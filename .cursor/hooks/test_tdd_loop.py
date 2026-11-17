#!/usr/bin/env python3
"""Failing-first tests for the project TDD loop hook."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tdd_loop as loop  # noqa: E402


ROOT = "/workspace"
CRATE = "crates/codegen/xai-grok-config/src/lib.rs"
TEST_FILE = "crates/codegen/xai-grok-config/src/lib_tests.rs"
INTEGRATION = "crates/codegen/xai-grok-config/tests/checkout.rs"
PROD = "prod/mc/cli-chat-proxy-types/src/lib.rs"
STATE = ".cursor/tdd-state/conv.json"


def fresh():
    return loop.LoopState()


class PathClassTests(unittest.TestCase):
    def test_production_test_and_other_paths(self):
        self.assertEqual(loop.classify_repo_path(CRATE), "production")
        self.assertEqual(loop.classify_repo_path(PROD), "production")
        self.assertEqual(loop.classify_repo_path(TEST_FILE), "test")
        self.assertEqual(loop.classify_repo_path(INTEGRATION), "test")
        self.assertEqual(
            loop.classify_repo_path("crates/codegen/xai-grok-sandbox/src/test_util.rs"),
            "test",
        )
        self.assertEqual(
            loop.classify_repo_path(
                "crates/codegen/xai-grok-shell-base/src/util/dual_clock_tests.rs"
            ),
            "test",
        )
        self.assertEqual(loop.classify_repo_path(".cursor/rules/tdd.mdc"), "other")
        self.assertEqual(loop.classify_repo_path("third_party/mermaid/src/lib.rs"), "other")
        self.assertEqual(loop.classify_repo_path(STATE), "state")

    def test_absolute_path_is_repo_relative(self):
        rel = loop.repo_relative(f"{ROOT}/{CRATE}", ROOT, [ROOT])
        self.assertEqual(rel, CRATE)


class TestOutputTests(unittest.TestCase):
    def test_assertion_failure_is_red(self):
        output = """
test checkout::confirms ... FAILED
thread 'checkout::confirms' panicked at crates/foo/tests/checkout.rs:12:5:
assertion `left == right` failed
  left: "pending"
 right: "confirmed"
test result: FAILED. 0 passed; 1 failed; 0 ignored
"""
        self.assertEqual(loop.classify_test_output(output), "red")

    def test_compile_error_is_not_red(self):
        output = """
error[E0425]: cannot find function `checkout` in this scope
error: could not compile `xai-grok-config` (test) due to 1 previous error
"""
        self.assertEqual(loop.classify_test_output(output), "compile")

    def test_passing_suite_is_green(self):
        output = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured\n"
        self.assertEqual(loop.classify_test_output(output), "green")

    def test_empty_output_is_unknown(self):
        self.assertEqual(loop.classify_test_output(""), "unknown")

    def test_nextest_failure_is_red(self):
        output = "        FAIL [0.01s] xai-grok-config checkout::confirms\n"
        self.assertEqual(loop.classify_test_output(output), "red")


class ShellTests(unittest.TestCase):
    def test_recognizes_cargo_test_commands(self):
        self.assertTrue(loop.is_test_command("cargo test -p xai-grok-config"))
        self.assertTrue(loop.is_test_command("FOO=1 cargo nextest run -p xai-grok-config"))
        self.assertTrue(loop.is_test_command("cargo test -p foo && echo x > crates/a/src/lib.rs"))
        self.assertFalse(loop.is_test_command("cargo fmt --all"))

    def test_shell_write_targets(self):
        writes = lambda cmd: loop.shell_write_rels(cmd, ROOT, [ROOT])
        self.assertEqual(writes(f"echo x > {CRATE}"), [CRATE])
        self.assertEqual(writes(f"echo {CRATE} > /tmp/list"), [])
        self.assertEqual(writes(f"rg foo {CRATE}"), [])
        self.assertEqual(writes(f"cp {CRATE} /tmp/x.rs"), [])
        self.assertEqual(writes(f"cp /tmp/x.rs {CRATE}"), [CRATE])
        self.assertEqual(writes(f"sed -i 's/a/b/' {CRATE}"), [CRATE])
        self.assertEqual(writes("cargo test -p xai-grok-config"), [])
        self.assertEqual(writes("cargo fmt --all"), [])
        self.assertEqual(
            writes(f"cargo check -p foo && printf x > {CRATE}"),
            [CRATE],
        )
        self.assertEqual(
            writes(f"""python3 -c "open('{CRATE}','w').write('x')" """),
            [CRATE],
        )
        self.assertEqual(writes(f"echo forged > {STATE}"), [STATE])


class LoopTests(unittest.TestCase):
    def event(self, **kwargs):
        base = {
            "conversation_id": "conv",
            "cwd": ROOT,
            "workspace_roots": [ROOT],
        }
        base.update(kwargs)
        return base

    def test_blocks_production_edit_before_a_failing_test(self):
        state = fresh()
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="StrReplace",
                tool_input={"path": CRATE, "old_string": "a", "new_string": "b"},
            ),
        )
        self.assertEqual(out["permission"], "deny")
        self.assertIn("failing test", out["agent_message"])
        self.assertEqual(state.phase, "need_test")

    def test_allows_test_file_then_requires_assertion_failure(self):
        state = fresh()
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="Write",
                tool_input={"path": TEST_FILE, "contents": "#[test]\nfn t() {}\n"},
            ),
        )
        self.assertEqual(out["permission"], "allow")
        self.assertEqual(state.phase, "need_red")

        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="Write",
                tool_input={"path": CRATE, "contents": "pub fn checkout() {}"},
            ),
        )
        self.assertEqual(out["permission"], "deny")
        self.assertIn("assertion", out["agent_message"])

    def test_compile_failure_allows_a_stub_but_is_not_red(self):
        state = fresh()
        state.phase = "need_red"
        state.test_touched = True
        state, _ = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterShellExecution",
                command="cargo test -p xai-grok-config",
                output="error[E0425]: cannot find function `checkout`\nerror: could not compile `xai-grok-config`\n",
            ),
        )
        self.assertEqual(state.phase, "need_red")
        self.assertEqual(state.last_run, "compile")
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="StrReplace",
                tool_input={"path": CRATE, "old_string": "", "new_string": "pub fn checkout() -> &'static str { \"\" }"},
            ),
        )
        self.assertEqual(out["permission"], "allow")
        self.assertEqual(state.phase, "need_red")

    def test_red_then_implementation_then_green_then_refactor(self):
        state = fresh()
        state.phase = "need_red"
        state.test_touched = True
        red = "test result: FAILED. 0 passed; 1 failed\nthread 't' panicked at assertion failed\n"
        state, _ = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterShellExecution",
                command="cargo test -p xai-grok-config confirms",
                output=red,
            ),
        )
        self.assertEqual(state.phase, "red")

        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="StrReplace",
                tool_input={"path": CRATE, "old_string": "\"\"", "new_string": "\"confirmed\""},
            ),
        )
        self.assertEqual(out["permission"], "allow")
        self.assertEqual(state.phase, "implementing")

        state, _ = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterShellExecution",
                command="cargo test -p xai-grok-config",
                output="test result: ok. 1 passed; 0 failed\n",
            ),
        )
        self.assertEqual(state.phase, "green")

        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="StrReplace",
                tool_input={"path": CRATE, "old_string": "let x", "new_string": "let status"},
            ),
        )
        self.assertEqual(out["permission"], "allow")
        self.assertEqual(state.phase, "green")

    def test_green_without_red_locks_production_edits(self):
        state = fresh()
        state.phase = "need_red"
        state.test_touched = True
        state, _ = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterShellExecution",
                command="cargo test -p xai-grok-config",
                output="test result: ok. 1 passed; 0 failed\n",
            ),
        )
        self.assertEqual(state.phase, "green_without_red")
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="beforeShellExecution",
                command=f"printf x > {CRATE}",
            ),
        )
        self.assertEqual(out["permission"], "deny")

    def test_existing_failure_does_not_count_until_a_test_file_is_touched(self):
        state = fresh()
        state, _ = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterShellExecution",
                command="cargo test -p xai-grok-config",
                output="test result: FAILED. 0 passed; 1 failed\npanicked at assertion\n",
            ),
        )
        self.assertEqual(state.phase, "need_test")

    def test_new_test_edit_after_green_rearms_the_gate(self):
        state = fresh()
        state.phase = "green"
        state.test_touched = True
        state.last_run = "green"
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="afterFileEdit",
                file_path=f"{ROOT}/{TEST_FILE}",
                edits=[{"old_string": "a", "new_string": "b"}],
            ),
        )
        self.assertEqual(out, {})
        self.assertEqual(state.phase, "need_red")
        self.assertIsNone(state.last_run)
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="Delete",
                tool_input={"path": CRATE},
            ),
        )
        self.assertEqual(out["permission"], "deny")

    def test_state_file_writes_are_always_denied(self):
        state = fresh()
        state.phase = "green"
        state, out = loop.reduce_event(
            state,
            self.event(
                hook_event_name="preToolUse",
                tool_name="Write",
                tool_input={"path": STATE, "contents": '{"phase":"green"}'},
            ),
        )
        self.assertEqual(out["permission"], "deny")
        self.assertIn("tdd-state", out["agent_message"])

    def test_stop_followup_only_when_the_loop_was_skipped(self):
        state = fresh()
        state.phase = "green_without_red"
        _, out = loop.reduce_event(
            state,
            self.event(hook_event_name="stop", status="completed", loop_count=0),
        )
        self.assertIn("followup_message", out)
        _, out = loop.reduce_event(
            state,
            self.event(hook_event_name="stop", status="completed", loop_count=1),
        )
        self.assertEqual(out, {})
        green = fresh()
        green.phase = "green"
        _, out = loop.reduce_event(
            green,
            self.event(hook_event_name="stop", status="completed", loop_count=0),
        )
        self.assertEqual(out, {})

    def test_unrecognized_edit_shape_is_allowed(self):
        state = fresh()
        _, out = loop.reduce_event(
            state,
            self.event(hook_event_name="preToolUse", tool_name="Write", tool_input={}),
        )
        self.assertEqual(out["permission"], "allow")


class CliTests(unittest.TestCase):
    def test_cli_denies_production_write_and_persists_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = os.environ.copy()
            env["GROK_TDD_STATE_DIR"] = tmp
            payload = {
                "hook_event_name": "preToolUse",
                "conversation_id": "conv/../odd",
                "tool_name": "Write",
                "tool_input": {"path": CRATE, "contents": "fn main() {}"},
                "cwd": ROOT,
                "workspace_roots": [ROOT],
            }
            proc = subprocess.run(
                [sys.executable, str(Path(__file__).resolve().parent / "tdd_loop.py")],
                input=json.dumps(payload),
                text=True,
                capture_output=True,
                env=env,
                cwd=ROOT,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            out = json.loads(proc.stdout)
            self.assertEqual(out["permission"], "deny")
            saved = list(Path(tmp).glob("*.json"))
            self.assertEqual(len(saved), 1)
            self.assertNotIn("..", saved[0].name)


class FactoryShapeTests(unittest.TestCase):
    def test_hooks_json_gates_the_events_cursor_can_enforce(self):
        config = json.loads((Path(ROOT) / ".cursor" / "hooks.json").read_text())
        hooks = config["hooks"]
        self.assertNotIn("sessionStart", hooks)
        for name in ("preToolUse", "beforeShellExecution", "afterShellExecution", "afterFileEdit", "stop"):
            self.assertIn(name, hooks)
        gated = {item["command"]: item for item in hooks["preToolUse"] + hooks["beforeShellExecution"]}
        for item in gated.values():
            self.assertTrue(item.get("failClosed"))
            self.assertNotEqual(item.get("type"), "prompt")
        joined = json.dumps(hooks)
        self.assertNotIn("sessionStart", joined)
        self.assertIn("tdd_loop.py", joined)

    def test_rule_and_skill_describe_red_green_refactor(self):
        rule = (Path(ROOT) / ".cursor" / "rules" / "tdd.mdc").read_text()
        skill = (Path(ROOT) / ".cursor" / "skills" / "tdd" / "SKILL.md").read_text()
        for text in (rule, skill):
            red = text.lower().find("red")
            green = text.lower().find("green")
            refactor = text.lower().find("refactor")
            self.assertGreaterEqual(red, 0)
            self.assertLess(red, green)
            self.assertLess(green, refactor)
        self.assertIn("alwaysApply: true", rule)
        self.assertTrue((Path(ROOT) / ".cursor" / "skills" / "tdd" / "NOTICE").is_file())
        self.assertFalse((Path(ROOT) / ".cursor" / "skills" / "tdd" / "scripts").exists())


if __name__ == "__main__":
    unittest.main()
