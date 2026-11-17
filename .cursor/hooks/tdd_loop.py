#!/usr/bin/env python3
"""Cursor hook for the red → green → refactor loop.

Enforces only what project hooks can see: edit-tool paths, recognizable shell
writes, and cargo test / nextest output. Judgement (the named break, seams,
tautologies) stays in the tdd rule and skill.
"""

from __future__ import annotations

import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

EDIT_TOOLS = {
    "Write",
    "StrReplace",
    "Delete",
    "EditNotebook",
    "ApplyPatch",
    "Edit",
    "MultiEdit",
    "search_replace",
}
PATH_KEYS = ("path", "file_path", "target_file", "file", "notebook_path")
PHASES = {
    "need_test",
    "need_red",
    "red",
    "implementing",
    "green",
    "green_without_red",
}
PERMISSION_EVENTS = {"preToolUse", "beforeShellExecution"}

NEED_TEST_MSG = (
    "Write the failing test in a test file (`*_tests.rs`, `*_test.rs`, `test_*.rs`, "
    "or under `tests/`) before editing production Rust. Run `cargo test -p <crate>` "
    "until an assertion fails. A compile error is not red."
)
NEED_RED_MSG = (
    "Production Rust is blocked until `cargo test` reports an assertion failure for "
    "the new test. A compile error is not red, and a passing run is not red."
)
SKIPPED_RED_MSG = (
    "The test passed without a recorded assertion failure. Revert the production "
    "change, make the assertion fail, and only then implement."
)
STATE_MSG = (
    "The TDD loop state under .cursor/tdd-state is maintained by the hook. Do not write it."
)


@dataclass
class LoopState:
    phase: str = "need_test"
    last_run: str | None = None
    test_touched: bool = False
    violation: bool = False

    def to_dict(self) -> dict:
        return {
            "phase": self.phase,
            "last_run": self.last_run,
            "test_touched": self.test_touched,
            "violation": self.violation,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "LoopState":
        phase = data.get("phase")
        if phase not in PHASES:
            phase = "need_test"
        last = data.get("last_run")
        if last not in ("red", "compile", "green", "unknown"):
            last = None
        return cls(
            phase=phase,
            last_run=last,
            test_touched=bool(data.get("test_touched")),
            violation=bool(data.get("violation")),
        )


def classify_repo_path(rel: str) -> str:
    rel = rel.replace("\\", "/")
    while rel.startswith("./"):
        rel = rel[2:]
    if rel == ".cursor/tdd-state" or rel.startswith(".cursor/tdd-state/"):
        return "state"
    if not rel.endswith(".rs"):
        return "other"
    name = rel.rsplit("/", 1)[-1]
    if "/tests/" in f"/{rel}" or rel.startswith("tests/"):
        return "test"
    if name.endswith("_tests.rs") or name.endswith("_test.rs") or name.startswith("test_"):
        return "test"
    if rel.startswith("crates/") or rel.startswith("prod/"):
        return "production"
    return "other"


def repo_relative(path: str, cwd: str, roots: list[str]) -> str:
    raw = (path or "").strip().strip('"').strip("'")
    if raw.startswith("file://"):
        raw = raw[len("file://") :]
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = Path(cwd) / candidate
    norm = Path(os.path.normpath(str(candidate)))
    bases = [Path(os.path.normpath(root)) for root in roots] or [Path(os.path.normpath(cwd))]
    cwd_path = Path(os.path.normpath(cwd))
    if cwd_path not in bases:
        bases.append(cwd_path)
    for base in bases:
        try:
            return norm.relative_to(base).as_posix()
        except ValueError:
            continue
    return norm.as_posix()


def classify_test_output(output: str) -> str:
    if not output or not output.strip():
        return "unknown"
    if re.search(r"test result: FAILED", output) or re.search(r"\bFAIL\s+\[", output):
        return "red"
    if re.search(r"could not compile|error\[E\d+\]", output):
        return "compile"
    if re.search(r"test result: ok\b", output):
        return "green"
    if re.search(r"\b0 failed\b", output) and re.search(r"\bpassed\b", output):
        return "green"
    return "unknown"


def split_segments(command: str) -> list[str]:
    parts: list[str] = []
    buf: list[str] = []
    quote = None
    i = 0
    while i < len(command):
        char = command[i]
        if quote:
            buf.append(char)
            if char == quote:
                quote = None
            i += 1
            continue
        if char in ("'", '"'):
            quote = char
            buf.append(char)
            i += 1
            continue
        if char in ("\n", ";"):
            parts.append("".join(buf))
            buf = []
            i += 1
            continue
        if char == "&" and i + 1 < len(command) and command[i + 1] == "&":
            parts.append("".join(buf))
            buf = []
            i += 2
            continue
        if char == "|" and i + 1 < len(command) and command[i + 1] == "|":
            parts.append("".join(buf))
            buf = []
            i += 2
            continue
        buf.append(char)
        i += 1
    if buf:
        parts.append("".join(buf))
    return [part.strip() for part in parts if part.strip()]


def tokenize(segment: str) -> list[str]:
    tokens: list[str] = []
    buf: list[str] = []
    quote = None
    for char in segment:
        if quote:
            if char == quote:
                quote = None
            else:
                buf.append(char)
            continue
        if char in ("'", '"'):
            quote = char
            continue
        if char.isspace():
            if buf:
                tokens.append("".join(buf))
                buf = []
            continue
        buf.append(char)
    if buf:
        tokens.append("".join(buf))
    return tokens


def command_argv(segment: str) -> list[str]:
    tokens = tokenize(segment)
    index = 0
    while index < len(tokens) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", tokens[index]):
        index += 1
    return tokens[index:]


def is_test_command(command: str) -> bool:
    for segment in split_segments(command):
        argv = command_argv(segment)
        if len(argv) >= 2 and argv[0] == "cargo" and argv[1] in ("test", "nextest"):
            return True
    return False


def _redirect_targets(tokens: list[str]) -> list[str]:
    targets: list[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token in (">", ">>", "1>", "1>>", "2>", "&>", "&>>") or re.match(r"^\d+>>?$", token):
            if index + 1 < len(tokens):
                targets.append(tokens[index + 1])
                index += 2
                continue
        glued = re.match(r"^(?:&>>?|\d*>>?)(.+)$", token)
        if glued and glued.group(1) and token not in (">", ">>"):
            targets.append(glued.group(1))
        index += 1
    return targets


def _tee_targets(tokens: list[str]) -> list[str]:
    if "tee" not in tokens:
        return []
    targets: list[str] = []
    for token in tokens[tokens.index("tee") + 1 :]:
        if token.startswith("-"):
            continue
        if token in ("|", "||", "&&", ";"):
            break
        targets.append(token)
    return targets


def _inplace_target(tokens: list[str]) -> str | None:
    if not tokens or tokens[0] not in ("sed", "perl"):
        return None
    inplace = any(
        token == "-i" or token.startswith("--in-place") or re.match(r"^-[A-Za-z]*i", token)
        for token in tokens[1:]
    )
    if not inplace:
        return None
    return tokens[-1]


def _copy_target(tokens: list[str]) -> str | None:
    if len(tokens) < 3 or tokens[0] not in ("cp", "mv", "install"):
        return None
    args = [token for token in tokens[1:] if not token.startswith("-")]
    if len(args) < 2:
        return None
    return args[-1]


def _python_write_targets(segment: str) -> list[str]:
    if not re.search(r"\bopen\s*\(|\.write(?:_text|_bytes)?\s*\(", segment):
        return []
    return re.findall(r"(?:crates|prod|\.cursor/tdd-state)/[A-Za-z0-9_./-]+", segment)


def segment_write_targets(segment: str) -> list[str]:
    tokens = tokenize(segment)
    targets = []
    targets.extend(_redirect_targets(tokens))
    targets.extend(_tee_targets(tokens))
    copied = _copy_target(tokens)
    if copied:
        targets.append(copied)
    edited = _inplace_target(tokens)
    if edited:
        targets.append(edited)
    targets.extend(_python_write_targets(segment))
    return targets


def shell_write_rels(command: str, cwd: str, roots: list[str]) -> list[str]:
    found: list[str] = []
    for segment in split_segments(command):
        for raw in segment_write_targets(segment):
            if raw in ("/dev/null",):
                continue
            rel = repo_relative(raw, cwd, roots)
            if classify_repo_path(rel) not in ("production", "state"):
                continue
            if rel not in found:
                found.append(rel)
    return found


def extract_tool_paths(tool_input) -> list[str]:
    if isinstance(tool_input, str):
        try:
            tool_input = json.loads(tool_input)
        except json.JSONDecodeError:
            return []
    if not isinstance(tool_input, dict):
        return []
    paths: list[str] = []

    def take(value) -> None:
        if isinstance(value, str):
            paths.append(value)

    for key in PATH_KEYS:
        take(tool_input.get(key))
    files = tool_input.get("files")
    if isinstance(files, list):
        for item in files:
            if isinstance(item, str):
                paths.append(item)
            elif isinstance(item, dict):
                for key in PATH_KEYS:
                    take(item.get(key))
    return paths


def allow() -> dict:
    return {"permission": "allow"}


def deny(message: str) -> dict:
    return {
        "permission": "deny",
        "user_message": message,
        "agent_message": message,
    }


def production_allowed(state: LoopState) -> bool:
    if state.phase in ("red", "implementing", "green"):
        return True
    return state.phase == "need_red" and state.last_run == "compile" and state.test_touched


def deny_message(state: LoopState) -> str:
    if state.phase == "green_without_red":
        return SKIPPED_RED_MSG
    if not state.test_touched or state.phase == "need_test":
        return NEED_TEST_MSG
    return NEED_RED_MSG


def note_test_edit(state: LoopState) -> None:
    state.test_touched = True
    if state.phase != "need_red":
        state.phase = "need_red"
        state.last_run = None


def note_production_edit(state: LoopState) -> None:
    if state.phase == "red":
        state.phase = "implementing"


def note_test_result(state: LoopState, kind: str) -> None:
    if kind == "unknown":
        return
    state.last_run = kind
    if kind == "red" and state.test_touched:
        state.phase = "red"
        state.violation = False
        return
    if kind == "green" and state.phase in ("red", "implementing"):
        state.phase = "green"
        return
    if kind == "green" and state.phase != "green" and state.test_touched:
        state.phase = "green_without_red"


def gate_paths(state: LoopState, rels: list[str]) -> tuple[dict, bool]:
    if not rels:
        return allow(), False
    kinds = {classify_repo_path(rel) for rel in rels}
    if "state" in kinds:
        return deny(STATE_MSG), False
    if "production" in kinds and not production_allowed(state):
        return deny(deny_message(state)), False
    return allow(), True


def apply_allowed(state: LoopState, rels: list[str]) -> None:
    kinds = {classify_repo_path(rel) for rel in rels}
    if "test" in kinds:
        note_test_edit(state)
    if "production" in kinds:
        note_production_edit(state)


def stop_output(state: LoopState, event: dict) -> dict:
    if event.get("status") != "completed":
        return {}
    try:
        loop_count = int(event.get("loop_count") or 0)
    except (TypeError, ValueError):
        loop_count = 0
    if loop_count >= 1:
        return {}
    if state.violation or state.phase == "green_without_red":
        return {
            "followup_message": (
                "A production Rust edit landed without a recorded assertion failure. "
                "Revert that production change, write the failing test, run "
                "`cargo test -p <crate>` until the assertion fails, and only then implement."
            )
        }
    if state.phase == "implementing":
        return {
            "followup_message": (
                "The failing test is recorded and the production edit is in progress. "
                "Run `cargo test -p <crate>` for the affected crate and get a green result "
                "before handing off. Refactor only after that run is green."
            )
        }
    return {}


def reduce_event(state: LoopState, event: dict) -> tuple[LoopState, dict]:
    name = event.get("hook_event_name") or ""
    cwd = event.get("cwd") or os.getcwd()
    roots = event.get("workspace_roots") or [cwd]
    if not isinstance(roots, list):
        roots = [cwd]

    if name == "preToolUse":
        tool = event.get("tool_name") or ""
        if tool not in EDIT_TOOLS:
            return state, allow()
        rels = [repo_relative(path, cwd, roots) for path in extract_tool_paths(event.get("tool_input"))]
        decision, ok = gate_paths(state, rels)
        if ok:
            apply_allowed(state, rels)
        return state, decision

    if name == "beforeShellExecution":
        rels = shell_write_rels(event.get("command") or "", cwd, roots)
        if not rels:
            return state, allow()
        decision, ok = gate_paths(state, rels)
        if ok:
            apply_allowed(state, rels)
        return state, decision

    if name == "afterShellExecution":
        command = event.get("command") or ""
        for rel in shell_write_rels(command, cwd, roots):
            kind = classify_repo_path(rel)
            if kind == "state" or (kind == "production" and not production_allowed(state)):
                state.violation = True
        if is_test_command(command):
            output = event.get("output")
            if output is None:
                output = event.get("stdout") or ""
            note_test_result(state, classify_test_output(str(output)))
        return state, {}

    if name == "afterFileEdit":
        rel = repo_relative(event.get("file_path") or "", cwd, roots)
        kind = classify_repo_path(rel)
        if kind == "test":
            note_test_edit(state)
        elif kind == "production":
            if production_allowed(state):
                note_production_edit(state)
            else:
                state.violation = True
        elif kind == "state":
            state.violation = True
        return state, {}

    if name == "stop":
        return state, stop_output(state, event)

    if name in PERMISSION_EVENTS:
        return state, allow()
    return state, {}


def sanitize_id(value: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "_", value or "default")
    while ".." in cleaned:
        cleaned = cleaned.replace("..", "_")
    cleaned = cleaned.strip("._") or "default"
    return cleaned[:120]


def state_file(conversation_id: str) -> Path:
    directory = Path(os.environ.get("GROK_TDD_STATE_DIR") or ".cursor/tdd-state")
    directory.mkdir(parents=True, exist_ok=True)
    return directory / f"{sanitize_id(conversation_id)}.json"


def load_state(conversation_id: str) -> LoopState:
    path = state_file(conversation_id)
    if not path.is_file():
        return LoopState()
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return LoopState()
    if not isinstance(data, dict):
        return LoopState()
    return LoopState.from_dict(data)


def save_state(conversation_id: str, state: LoopState) -> None:
    path = state_file(conversation_id)
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(state.to_dict()))
    temporary.replace(path)


def main() -> int:
    raw = sys.stdin.read()
    try:
        event = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        event = {}
    if not isinstance(event, dict):
        event = {}
    conversation_id = str(event.get("conversation_id") or "default")
    name = event.get("hook_event_name")
    try:
        state = load_state(conversation_id)
        state, output = reduce_event(state, event)
        save_state(conversation_id, state)
    except Exception as exc:  # noqa: BLE001 — a permission hook must still emit JSON
        sys.stderr.write(f"tdd-loop: {exc}\n")
        if name in PERMISSION_EVENTS:
            output = deny(
                "TDD hook failed closed. Fix .cursor/hooks/tdd_loop.py before editing production Rust."
            )
        else:
            output = {}
    json.dump(output, sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
