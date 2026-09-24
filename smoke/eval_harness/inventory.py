"""Deterministic, race-safe inventory scopes (DESIGN.md §§2–4).

Produces the complete metadata inventory for every catalog-declared
scope: the recursive ``plans-corpus`` scope and the two declared-inputs
scopes. Scans traverse with sorted ``os.scandir`` entries and
``follow_symlinks=False``; a symlink is never resolved, opened, or
descended into — it is recorded with its raw link-target byte count and
SHA-256, and an absolute link string is normalized lexically against the
supplied roots (never the raw string). Regular files are hashed through
a file descriptor with pre/post ``lstat``/``fstat`` identity, size, and
mtime checks; symlink link bytes are re-read and compared. Any
disagreement raises :class:`SourceChangedError` and aborts the scan
(DESIGN.md §4: "fail if any source changes during the scan").

Git state is one of the exact five enums and comes from NUL-delimited
Git output scoped to the owning repository (``git -C <root>`` with
``GIT_OPTIONAL_LOCKS=0`` so the invocation never rewrites the index):

- ``git ls-files -z``                     -> tracked set
- ``git status --porcelain=v2 -z``        -> changed-tracked records
- ``git ls-files -z --others --exclude-standard``        -> untracked
- ``git ls-files -z --others --ignored --exclude-standard`` -> untracked+ignored

The state is parsed once per owning repository (chunked pathspec
batching merged into a single map). A path under a nested repository is
``outside-repository``.

Role, media type, generation state, and content policy are catalog
driven: curated sources are ``curated-document``/``curated-text``;
source entrypoints are ``source-code`` (``launcher`` for launcher
components) and ``source-authored``; the formalism-linter's two exact
worktree generated outputs are ``generated-source``/``declared-generated``
while the row's other worktree targets (the source-authored A3
projection-seam tests and the outbound-lint fixture evidence) are
``source-authored``;
semantic-json component inputs and curated-text component inputs scoped
to their exact ``.json`` tools files and ``<dir>/*/meta.json`` under a
declared fixtures directory (DESIGN.md §3) are ``semantic-json``;
everything else is ``metadata-only`` with a deterministic name/extension
role heuristic. Unknown extensions map to
``application/octet-stream``.

Missing cataloged items: a missing required source entrypoint, or a
missing declared generated target (a literal path, or a glob whose
anchor is absent or matches nothing), is a hard ``ContractError``
(DESIGN.md §8); a missing optional entrypoint, input, owner document,
or other glob match is skipped (a soft concern owned by the writer
boundary). A
scope digest is the SHA-256 of the canonical JSON array of the scope's
complete, sorted metadata records — the root-scope digest authority
(DESIGN.md §3).

Python 3.14 standard library only. Import only through the canonical
package name ``smoke.eval_harness.inventory``.
"""
from __future__ import annotations

import dataclasses
import fnmatch
import hashlib
import os
import re
import stat
import subprocess
from pathlib import Path
from typing import Any, Iterable, Iterator, Mapping

from .contract import (
    AdapterContext,
    ContractError,
    canonical_json_bytes,
    sensitive_matches,
)
from .paths import normalize_path_token

__all__ = [
    "CONTENT_POLICIES",
    "FILE_TYPES",
    "GENERATION_STATES",
    "GIT_STATES",
    "HashedRegular",
    "HashedSymlink",
    "InventorySnapshot",
    "MEDIA_TYPES",
    "ROLES",
    "SourceChangedError",
    "classify_git_state",
    "hash_regular_stable",
    "hash_symlink_stable",
    "inventory_all",
    "inventory_scope",
]

GIT_STATES = frozenset(
    {
        "tracked-clean",
        "tracked-modified",
        "untracked",
        "ignored",
        "outside-repository",
    }
)
GENERATION_STATES = frozenset(
    {"source-authored", "declared-generated", "unknown"}
)
MEDIA_TYPES = frozenset(
    {
        "application/json",
        "application/x-ndjson",
        "application/pdf",
        "application/octet-stream",
        "text/markdown",
        "text/plain",
        "text/x-python",
        "text/x-shellscript",
        "text/x-rust",
        "text/x-toml",
        "text/yaml",
        "text/html",
        "text/css",
        "text/javascript",
        "image/png",
        "image/svg+xml",
        "inode/symlink",
    }
)
ROLES = frozenset(
    {
        "curated-document",
        "source-code",
        "generated-source",
        "schema",
        "manifest",
        "fixture-metadata",
        "fixture-payload",
        "launcher",
        "capture",
        "report",
        "configuration",
        "other",
    }
)
CONTENT_POLICIES = frozenset(
    {"metadata-only", "semantic-json", "curated-text"}
)
FILE_TYPES = frozenset({"regular", "symlink"})

_EXTENSION_MEDIA: dict[str, str] = {
    ".json": "application/json",
    ".ndjson": "application/x-ndjson",
    ".pdf": "application/pdf",
    ".md": "text/markdown",
    ".txt": "text/plain",
    ".py": "text/x-python",
    ".sh": "text/x-shellscript",
    ".rs": "text/x-rust",
    ".toml": "text/x-toml",
    ".yml": "text/yaml",
    ".yaml": "text/yaml",
    ".html": "text/html",
    ".css": "text/css",
    ".js": "text/javascript",
    ".png": "image/png",
    ".svg": "image/svg+xml",
}

_HASH_CHUNK = 1024 * 1024
_GIT_PATH_CHUNK = 1000
_GIT_TIMEOUT_SECONDS = 300
_GIT_ENV = {"GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0"}

#: The catalog row that declares the generated-rule worktree targets
#: (DESIGN.md §2: the formalism-linter row names the generator and its
#: worktree outputs).
_GENERATED_TARGET_COMPONENT = "formalism-linter"

#: Closed enums mirroring ``schemas/catalog.schema.json`` entrypoint
#: discriminator values (round 12, M-2): a mangled discriminator must
#: fail loud rather than silently vacate the §8 required-entrypoint
#: gate through the same filter that legitimately skips in-enum rows.
_ENTRYPOINT_ROLES = frozenset({"installed", "selftest", "source"})
_ENTRYPOINT_AVAILABILITY = frozenset(
    {"health-only", "optional", "required"}
)

#: Closed enums mirroring ``schemas/catalog.schema.json`` for every
#: field whose mangled value would be conflated with a legitimate
#: filter (round 13, converged M-1/M-2/M-3): an unknown root is NOT a
#: cross-root skip, a mangled content_policy is NOT the metadata-only
#: default, and a mangled kind is NOT a non-launcher. Values are never
#: echoed. ``_KIND_RE`` mirrors the schema's ``kind`` slug pattern.
_DECLARED_ROOTS = frozenset(
    {"agents-home", "codex-home", "grok-home", "logscale", "plans",
     "worktree"}
)
_CONTENT_POLICIES = frozenset(
    {"curated-text", "metadata-only", "semantic-json"}
)
_KIND_RE = re.compile(r"^[a-z][a-z0-9-]*$")

#: The worktree targets the formalism-linter generator actually
#: produces, per the DESIGN.md §2 row annotations: ``rules_generated.rs``
#: and only ``hard_rules.json`` inside the ``outbound_lint`` fixture
#: corpus. The row's other declared worktree targets are
#: source-authored: ``projection_tests.rs`` (A3 projection-seam tests)
#: and the remaining outbound-lint fixtures (authored test evidence,
#: inventoried as metadata-only).
_GENERATED_TARGET_SPECS = frozenset(
    {
        "crates/codegen/xai-grok-sampling-types/src/conversation/"
        "rules_generated.rs",
        "crates/codegen/xai-grok-sampling-types/fixtures/outbound_lint/"
        "hard_rules.json",
    }
)


class SourceChangedError(Exception):
    """A source entry changed identity, size, mtime, or link bytes
    while it was being read. The scan aborts; nothing is emitted."""


@dataclasses.dataclass(frozen=True)
class HashedRegular:
    """Stable hash of a regular file plus safe identity metadata.

    ``bytes``/``size`` are the raw byte count read; ``dev``/``ino``/
    ``mtime_ns`` are internal race-check metadata and never enter a
    record.
    """

    bytes: int
    sha256: str
    size: int
    dev: int
    ino: int
    mtime_ns: int


@dataclasses.dataclass(frozen=True)
class HashedSymlink:
    """Stable hash of a symlink's raw link-target bytes.

    ``bytes`` is the raw UTF-8 link-target byte count; ``target`` is the
    safe token (a relative string verbatim, an absolute string
    normalized to ``@<root>/<rel>`` or ``@external/<base>#<sha256>``).
    The raw absolute string is never retained.
    """

    bytes: int
    sha256: str
    target: str


@dataclasses.dataclass(frozen=True)
class InventorySnapshot:
    """The sorted metadata records of one scope plus its canonical
    scope digest (SHA-256 over the canonical JSON record array)."""

    records: tuple
    sha256: str


# ---------------------------------------------------------------------------
# race-safe stable hashing
# ---------------------------------------------------------------------------


def _lstat(path: os.PathLike) -> os.stat_result:
    try:
        return os.lstat(path)
    except OSError as exc:
        raise SourceChangedError(
            f"source vanished or is unreadable at {path}: {exc.strerror}"
        ) from None


def _stat_flag(probe, path: os.PathLike) -> bool:
    """Race-safe DirEntry stat flag: an entry that vanishes or becomes
    unreadable mid-walk is a typed SourceChangedError, never a raw
    OSError."""
    try:
        return probe()
    except OSError as exc:
        raise SourceChangedError(
            f"source vanished or is unreadable at {path}: {exc.strerror}"
        ) from None


def _assert_same(
    before: os.stat_result, after: os.stat_result, path: os.PathLike, kind: str
) -> None:
    if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
        raise SourceChangedError(f"{kind} identity changed for {path} during read")
    if before.st_size != after.st_size:
        raise SourceChangedError(f"{kind} size changed for {path} during read")
    if before.st_mtime_ns != after.st_mtime_ns:
        raise SourceChangedError(f"{kind} mtime changed for {path} during read")


def hash_regular_stable(path: os.PathLike) -> HashedRegular:
    """Hash a regular file through an ``O_NOFOLLOW`` descriptor.

    Pre and post ``lstat`` are cross-checked with the mid-read
    ``fstat`` for identity (dev, ino), size, and mtime; any change
    raises :class:`SourceChangedError`. The path must already be known
    to be a regular file; a symlink or directory found here is a
    source change, never a target to follow.
    """
    pre = _lstat(path)
    if not stat.S_ISREG(pre.st_mode):
        raise SourceChangedError(
            f"source at {path} is no longer a regular file (not followed)"
        )
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise SourceChangedError(
            f"cannot open {path} for hashing: {exc.strerror}"
        ) from None
    digest = hashlib.sha256()
    try:
        opened = os.fstat(fd)
        _assert_same(pre, opened, path, "regular file")
        while True:
            chunk = os.read(fd, _HASH_CHUNK)
            if not chunk:
                break
            digest.update(chunk)
    except OSError as exc:
        raise SourceChangedError(
            f"read failed for {path} during hashing: {exc.strerror}"
        ) from None
    finally:
        os.close(fd)
    post = _lstat(path)
    _assert_same(pre, post, path, "regular file")
    return HashedRegular(
        bytes=pre.st_size,
        sha256=digest.hexdigest(),
        size=pre.st_size,
        dev=pre.st_dev,
        ino=pre.st_ino,
        mtime_ns=pre.st_mtime_ns,
    )


def hash_symlink_stable(path: os.PathLike, roots: Mapping[str, Any]) -> HashedSymlink:
    """Hash a symlink's raw link-target bytes without touching the
    target: the link string is read twice with ``readlink`` and compared
    against the pre/post ``lstat`` identity, size, and mtime.
    """
    pre = _lstat(path)
    if not stat.S_ISLNK(pre.st_mode):
        raise SourceChangedError(f"source at {path} is no longer a symlink")
    try:
        first = os.readlink(path)
    except OSError as exc:
        raise SourceChangedError(
            f"cannot read link at {path}: {exc.strerror}"
        ) from None
    post = _lstat(path)
    _assert_same(pre, post, path, "symlink")
    try:
        second = os.readlink(path)
    except OSError as exc:
        raise SourceChangedError(
            f"cannot re-read link at {path}: {exc.strerror}"
        ) from None
    if first != second:
        raise SourceChangedError(f"symlink target changed for {path} during read")
    if not first:
        raise ContractError(
            f"symlink at {path} has an empty link target", code="empty-symlink-target"
        )
    raw = first.encode("utf-8")
    return HashedSymlink(
        bytes=len(raw),
        sha256=hashlib.sha256(raw).hexdigest(),
        target=normalize_path_token(first, roots),
    )


# ---------------------------------------------------------------------------
# Git state classification (NUL-delimited, scoped to the owning repo)
# ---------------------------------------------------------------------------


def _run_git(repo_root: os.PathLike, argv: list[str]) -> bytes:
    env = dict(os.environ)
    env.update(_GIT_ENV)
    cmd = ["git", "-C", os.fspath(repo_root), "-c", "core.quotepath=false", *argv]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, env=env, timeout=_GIT_TIMEOUT_SECONDS
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise ContractError(f"git invocation failed: {exc}", code="git-failed") from None
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()[:400]
        raise ContractError(
            f"git {' '.join(argv[:2])} exited {proc.returncode}: {detail}",
            code="git-failed",
        )
    return proc.stdout


def _git_or_none(repo_root: os.PathLike, argv: list[str]) -> str | None:
    env = dict(os.environ)
    env.update(_GIT_ENV)
    cmd = ["git", "-C", os.fspath(repo_root), *argv]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, env=env, timeout=_GIT_TIMEOUT_SECONDS
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout.decode("utf-8", "replace").strip()


def _decode_git_path(token: bytes) -> str:
    if not token:
        raise ContractError("empty NUL-delimited Git path", code="git-status-shape")
    try:
        path = token.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ContractError(
            "Git path output is not valid UTF-8", code="git-status-shape"
        ) from exc
    if path.startswith('"'):
        raise ContractError(
            "Git path is C-quoted (core.quotepath must be off)",
            code="git-quoted-path",
        )
    return path


def _split_nul_paths(data: bytes) -> set:
    return {_decode_git_path(token) for token in data.split(b"\0") if token}


#: Fixed field count per changed-record type before the path. The path
#: is the remainder after the fixed fields and may itself contain
#: spaces, so it is taken as the split remainder, never as the last
#: whitespace-separated word:
#:   1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
#:   u <XY> <sub> <mH> <m1> <m2> <m3> <o1> <o2> <o3> <path>
#:   2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <score> <path>
#: Git 2.50's man page documents ``2`` exclusively as a rename or copy
#: (ninth field the ``R<N>``/``C<N>`` score, completed by the bare
#: NUL-separated source-path token); the legacy unmerged ``2`` shape
#: with thirteen fields is rejected — it is indistinguishable from a
#: rename whose new path contains three spaces.
_CHANGED_RECORD_FIXED_FIELDS = {"1": 8, "u": 10, "2": 9}

#: The ``R<N>``/``C<N>`` rename score in the ninth field of a ``2``
#: record (verified verbatim on Apple Git 2.50: ``2 R. N... 100644
#: 100644 100644 <h> <h> R100 a2.txt\0a.txt\0``).
_RENAME_SCORE_RE = re.compile(r"[RC][0-9]+")


def _parse_status_v2_nul(data: bytes) -> tuple[set, set]:
    """Parse NUL-terminated ``status --porcelain=v2 -z`` records.

    Returns ``(changed, untracked)`` path sets. Changed tracked files
    (record types ``1`` and ``u``, including unmerged paths, and
    ``2`` staged rename/copy records) go to ``changed``; ``?`` records
    to ``untracked``. ``!`` records are ignored entries: Git
    directory-collapses them, so per-file ignored state is derived from
    ``ls-files --ignored`` instead and these are discarded.

    For every changed record type the path is the remainder after the
    fixed fields (spaces inside the path are legal). A ``2`` record is
    exclusively a rename or copy: nine fixed fields whose ninth is the
    ``R<N>``/``C<N>`` score, then the new path, then the immediately
    following bare NUL token — the source path — which is consumed as
    part of the record. A rename record missing its source token, or
    carrying an empty or C-quoted source path, a changed record with an
    empty path, and a ``?``/``!`` record with an empty path are all
    malformed and fail loud.
    """
    changed: set = set()
    untracked: set = set()
    records = data.split(b"\0")
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if not record:
            continue
        try:
            text = record.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ContractError(
                "git status record is not valid UTF-8", code="git-status-shape"
            ) from exc
        head, sep, rest = text.partition(" ")
        if not sep:
            raise ContractError(
                f"malformed git status record {text!r}", code="git-status-shape"
            )
        source: bytes | None = None
        if head in _CHANGED_RECORD_FIXED_FIELDS:
            fixed = _CHANGED_RECORD_FIXED_FIELDS[head]
            parts = text.split(" ", fixed)
            if len(parts) != fixed + 1 or not parts[fixed]:
                raise ContractError(
                    f"malformed git status record {text!r}", code="git-status-shape"
                )
            if head == "2" and not _RENAME_SCORE_RE.fullmatch(parts[fixed - 1]):
                raise ContractError(
                    f"malformed git status record {text!r}", code="git-status-shape"
                )
            path = parts[fixed]
            if head == "2":
                if index >= len(records) or not records[index]:
                    raise ContractError(
                        f"rename record {text!r} is missing its source "
                        "path token",
                        code="git-status-shape",
                    )
                source = records[index]
                index += 1
        elif head in ("?", "!"):
            path = rest
            if not path:
                raise ContractError(
                    f"malformed git status record {text!r}",
                    code="git-status-shape",
                )
        else:
            raise ContractError(
                f"unknown git status record type {head!r}", code="git-status-shape"
            )
        if path.startswith('"'):
            raise ContractError(
                "git status path is C-quoted (core.quotepath must be off)",
                code="git-quoted-path",
            )
        if source is not None:
            # the bare source-path token must itself be a clean path
            _decode_git_path(source)
        if head in _CHANGED_RECORD_FIXED_FIELDS:
            changed.add(path)
        elif head == "?":
            untracked.add(path)
    return changed, untracked


def _inside_nested_repo(repo_root: Path, relpath: str) -> bool:
    """True when any strict ancestor directory of the path (below the
    repo root) contains a ``.git`` entry: the file belongs to a nested
    repository, not the owning one."""
    current = repo_root
    for part in relpath.split("/")[:-1]:
        current = current / part
        if os.path.lexists(os.path.join(current, ".git")):
            return True
    return False


def _chunks(items: list, size: int) -> Iterator[list]:
    for start in range(0, len(items), size):
        yield items[start : start + size]


def classify_git_state(repo_root: os.PathLike, paths: Iterable[str]) -> dict:
    """Classify repository-relative POSIX paths into exactly one of
    :data:`GIT_STATES`, parsed from NUL-delimited Git output scoped to
    ``repo_root``. The Git invocations are read-only queries
    (``GIT_OPTIONAL_LOCKS=0``); they never modify the repository.
    """
    path_list = list(paths)
    if not path_list:
        return {}
    states = {p: "outside-repository" for p in path_list}
    if _git_or_none(repo_root, ["rev-parse", "--show-toplevel"]) is None:
        return states
    repo_abs = Path(os.fspath(repo_root))
    nested = {p for p in path_list if _inside_nested_repo(repo_abs, p)}
    tracked: set = set()
    changed: set = set()
    untracked: set = set()
    ignored: set = set()
    for chunk in _chunks(path_list, _GIT_PATH_CHUNK):
        tracked |= _split_nul_paths(_run_git(repo_root, ["ls-files", "-z", "--", *chunk]))
        status_changed, status_untracked = _parse_status_v2_nul(
            _run_git(
                repo_root,
                ["status", "--porcelain=v2", "-z", "--untracked-files=all", "--", *chunk],
            )
        )
        changed |= status_changed
        untracked |= status_untracked
        others = _split_nul_paths(
            _run_git(
                repo_root,
                ["ls-files", "-z", "--others", "--exclude-standard", "--", *chunk],
            )
        )
        others_ignored = _split_nul_paths(
            _run_git(
                repo_root,
                [
                    "ls-files",
                    "-z",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--",
                    *chunk,
                ],
            )
        )
        untracked |= others
        ignored |= others_ignored - others
    for path in path_list:
        if path in nested:
            states[path] = "outside-repository"
        elif path in tracked:
            states[path] = "tracked-modified" if path in changed else "tracked-clean"
        elif path in ignored:
            states[path] = "ignored"
        else:
            # untracked (a `?` record, an --others entry, or a path the
            # repository neither tracks nor recognizes as ignored)
            states[path] = "untracked"
    return states


# ---------------------------------------------------------------------------
# scope traversal and catalog-driven selection
# ---------------------------------------------------------------------------


def _walk_files(root_dir: Path, prefix: str) -> list:
    """Collect ``(relpath, abspath)`` for every regular file and symlink
    under ``prefix``. Sorted ``os.scandir`` traversal,
    ``follow_symlinks=False``; symlinks are recorded and never
    descended into or opened."""
    found: list = []
    seen: set = set()

    def walk(directory: Path, current: str) -> None:
        try:
            with os.scandir(directory) as it:
                entries = sorted(it, key=lambda entry: entry.name)
        except OSError as exc:
            raise SourceChangedError(
                f"source vanished or is unreadable under {directory}: "
                f"{exc.strerror}"
            ) from None
        for entry in entries:
            rel = f"{current}/{entry.name}" if current else entry.name
            if _stat_flag(entry.is_symlink, entry.path):
                if rel in seen:
                    raise ContractError(
                        f"duplicate inventory path {rel!r}", code="duplicate-inventory"
                    )
                seen.add(rel)
                found.append((rel, Path(entry.path)))
            elif _stat_flag(
                lambda: entry.is_dir(follow_symlinks=False), entry.path
            ):
                walk(Path(entry.path), rel)
            elif _stat_flag(
                lambda: entry.is_file(follow_symlinks=False), entry.path
            ):
                if rel in seen:
                    raise ContractError(
                        f"duplicate inventory path {rel!r}", code="duplicate-inventory"
                    )
                seen.add(rel)
                found.append((rel, Path(entry.path)))
            else:
                raise ContractError(
                    f"unsupported file type in scope at {rel!r}",
                    code="unsupported-file-type",
                )

    walk(root_dir / prefix, prefix)
    return found


def _has_glob(text: str) -> bool:
    return any(ch in text for ch in "*?[")


def _match_segments(segs: list, i: int, pat: list, j: int) -> bool:
    while j < len(pat):
        token = pat[j]
        if token == "**":
            if j == len(pat) - 1:
                return True
            for k in range(i, len(segs) + 1):
                if _match_segments(segs, k, pat, j + 1):
                    return True
            return False
        if i >= len(segs) or not fnmatch.fnmatchcase(segs[i], token):
            return False
        i += 1
        j += 1
    return i == len(segs)


def _glob_match(relpath: str, pattern: str) -> bool:
    """Anchored POSIX segment matcher: ``*``/``?``/``[`` within a
    segment, ``**`` for zero or more whole segments."""
    return _match_segments(relpath.split("/"), 0, pattern.split("/"), 0)


def _expand_pattern(root_dir: Path, pattern: str) -> list:
    """Expand one include/exclude pattern to the matching file relpaths
    (regular files and symlinks under the pattern's literal anchor)."""
    if not _has_glob(pattern):
        target = root_dir / pattern
        try:
            info = os.lstat(target)
        except OSError:
            return []
        if stat.S_ISLNK(info.st_mode) or stat.S_ISREG(info.st_mode):
            return [pattern]
        if stat.S_ISDIR(info.st_mode):
            return [rel for rel, _ in _walk_files(root_dir, pattern)]
        raise ContractError(
            f"unsupported declared source type at {pattern!r}",
            code="unsupported-file-type",
        )
    parts = pattern.split("/")
    anchor_parts = []
    for part in parts:
        if _has_glob(part):
            break
        anchor_parts.append(part)
    anchor = "/".join(anchor_parts)
    base = root_dir if not anchor else root_dir / anchor
    try:
        info = os.lstat(base)
    except OSError:
        return []
    if not stat.S_ISDIR(info.st_mode):
        return []
    matches = {
        rel
        for rel, _ in _walk_files(root_dir, anchor)
        if _glob_match(rel, pattern)
    }
    return sorted(matches)


def _check_rooted_spec(spec: str, root_id: str, origin: str) -> None:
    """DESIGN §2: a scope never crosses its declared root. An absolute
    or `..`-segment spec would escape ``root_dir`` when joined — typed
    error, never a host read."""
    if spec.startswith("/") or ".." in spec.split("/"):
        raise ContractError(
            f"declared {origin} {spec!r} crosses scope root {root_id!r}",
            code="scope-crossing-input",
        )
    # Shape gate (round 12, M-2; widened round 14, GLM M-2 and round
    # 15, GLM F-1; mirrors the formalism adapter's
    # ``_catalog_spec_shape_violation``): an empty/whitespace body, ANY
    # whitespace character (the schema's path-token character class
    # forbids it — a padded spec never matches a record rel), ANY ``.``
    # segment (leading, interior or trailing) or any empty segment
    # never resolves to a canonical scope-relative path — it either
    # lstats the root directory itself (expanding the scope to the
    # WHOLE root including ``.git/`` machine-state files) or defeats the
    # plans-corpus prune prefix. Shape violations are malformed input,
    # not scope crossing; the value is never echoed.
    segments = spec.split("/")
    if (
        not spec.strip()
        or any(ch.isspace() for ch in spec)
        or "." in segments
        or "" in segments
    ):
        raise ContractError(
            f"malformed shape (empty, whitespace, or dot/empty-segment) "
            f"in declared {origin} for root {root_id!r}",
            code="malformed-catalog-input",
        )


def _validated_catalog_components(catalog: Mapping) -> list:
    """Guard the top-level ``components`` container (round 10,
    MAJOR-2): a null/non-list container must not escape as a raw
    TypeError or char-iterate into silent skips, and a non-dict row
    must not be silently dropped — one malformed-catalog-input."""
    raw = catalog.get("components", [])
    if not isinstance(raw, list):
        raise ContractError(
            "non-list components container", code="malformed-catalog-input"
        )
    for j, row in enumerate(raw):
        if not isinstance(row, dict):
            raise ContractError(
                f"non-dict component at components[{j}]",
                code="malformed-catalog-input",
            )
        # A malformed row-identifying id is conflated with an ABSENT
        # row — the generated-target origin check never fires and the
        # DESIGN §8 hard missing-generated-target gate is bypassed
        # (round 11, M-2). The value is never echoed.
        if not isinstance(row.get("id"), str) or not row["id"]:
            raise ContractError(
                f"non-string or empty component id at components[{j}]",
                code="malformed-catalog-input",
            )
        # NUL-tainted ids pass the string/emptiness guard yet never
        # equal the expected constant — conflated with an absent row
        # again (round 12, M-4; round-9 I-5 discipline on the id).
        _reject_nul(row["id"], f"component id at components[{j}]")
        # Row discriminator discipline (round 13, converged M-1/M-2/M-3
        # + m-1): the row's own ``root`` names the join target of its
        # bare inputs and role filters — a mangled value was conflated
        # with the legitimate cross-root skip, silently dropping the
        # row's declared specs and flipping their content policy
        # (asymmetric with the formalism adapter's F-1 one-hard). The
        # ``kind`` slug drives the launcher role; present-but-mangled
        # ``content_policy`` must not collapse to the metadata-only
        # default. Absent content_policy keeps the legitimate default
        # (the schema requires it, but hand-built catalogs may omit
        # optional metadata — 18 existing tests depend on the default).
        row_root = row.get("root")
        if (
            not isinstance(row_root, str)
            or row_root not in _DECLARED_ROOTS
        ):
            raise ContractError(
                f"invalid root at components[{j}]",
                code="malformed-catalog-input",
            )
        kind = row.get("kind")
        if (
            not isinstance(kind, str)
            or not _KIND_RE.fullmatch(kind)
        ):
            raise ContractError(
                f"invalid kind at components[{j}]",
                code="malformed-catalog-input",
            )
        if "content_policy" in row:
            policy = row["content_policy"]
            if (
                not isinstance(policy, str)
                or policy not in _CONTENT_POLICIES
            ):
                raise ContractError(
                    f"invalid content_policy at components[{j}]",
                    code="malformed-catalog-input",
                )
    return raw


def _validated_catalog_scopes(catalog: Mapping) -> list:
    """Guard the top-level ``inventory_scopes`` container (round 10,
    MAJOR-3): same container discipline, and a non-dict row must not be
    conflated with an ABSENT scope — that silently disabled the
    cross-scope plans-corpus pruning (scope expansion)."""
    raw = catalog.get("inventory_scopes", [])
    if not isinstance(raw, list):
        raise ContractError(
            "non-list inventory_scopes container",
            code="malformed-catalog-input",
        )
    for j, row in enumerate(raw):
        if not isinstance(row, dict):
            raise ContractError(
                f"non-dict scope at inventory_scopes[{j}]",
                code="malformed-catalog-input",
            )
        # A malformed row-identifying id is conflated with an ABSENT
        # scope: inventory_all silently drops the whole scope while
        # inventory_scope misroutes it to unknown-scope (round 11,
        # M-3). The id also flows into record["scope"] — an output
        # field — so a sensitive value is rejected like any other
        # (round-9 curated-rel parity). The value is never echoed.
        if not isinstance(row.get("id"), str) or not row["id"]:
            raise ContractError(
                f"non-string or empty scope id at inventory_scopes[{j}]",
                code="malformed-catalog-input",
            )
        if sensitive_matches(row["id"]):
            raise ContractError(
                f"sensitive value in scope id at inventory_scopes[{j}]",
                code="malformed-catalog-input",
            )
        # NUL is not a sensitive pattern yet rides verbatim into
        # record["scope"] — the round-9 I-5 discipline on the id
        # channel (round 12, M-4).
        _reject_nul(row["id"], f"scope id at inventory_scopes[{j}]")
        # Round 14, WH M-1 (K-3/F-5 parity on the sibling field of the
        # same row): the scope root flows verbatim into record["root"]
        # and the scope digest. Gate string values only — non-string
        # roots keep the existing typed missing-root outcome in
        # inventory_scope, and a clean unknown root stays there too.
        row_root = row.get("root")
        if isinstance(row_root, str):
            if sensitive_matches(row_root):
                raise ContractError(
                    f"sensitive value in scope root at "
                    f"inventory_scopes[{j}]",
                    code="malformed-catalog-input",
                )
            _reject_nul(row_root, f"scope root at inventory_scopes[{j}]")
    return raw


def _reject_nul(value: str, where: str) -> None:
    """A NUL byte is a malformed catalog shape, not a scope violation
    (round 9, I-5): it passes ``_check_rooted_spec`` and escapes as a
    raw ``ValueError`` from lstat downstream."""
    if "\x00" in value:
        raise ContractError(
            f"{where} contains a NUL byte", code="malformed-catalog-input"
        )


def _validated_entrypoint_item(entry: Mapping, comp_id: object, j: int) -> None:
    """Per-row entrypoint discipline (round 12, M-2/M-1): a mangled
    ``role``/``availability`` discriminator value or a non-string
    ``root``/``path`` on a declared entrypoint must fail loud — the
    filter comparisons otherwise conflate the mangle with a legitimate
    non-source/non-required/other-root row and the DESIGN §8 hard
    missing-entrypoint gate is vacuously satisfied with zero findings.
    Discriminators must name the schema's closed enums; the legitimate
    in-enum filtering stays at the call sites. Values are never echoed."""
    role = entry.get("role")
    if not isinstance(role, str) or role not in _ENTRYPOINT_ROLES:
        raise ContractError(
            f"invalid role at entrypoints[{j}] for component {comp_id!r}",
            code="malformed-catalog-input",
        )
    availability = entry.get("availability")
    if (
        not isinstance(availability, str)
        or availability not in _ENTRYPOINT_AVAILABILITY
    ):
        raise ContractError(
            f"invalid availability at entrypoints[{j}] for component "
            f"{comp_id!r}",
            code="malformed-catalog-input",
        )
    # Membership (not str-ness) is the discipline (round 13, converged
    # M-1): empty/NUL/unknown roots are conflated with the legitimate
    # declared-but-other-root skip and vacate the §8 required-entrypoint
    # gate; the schema closes this field to the declared root ids.
    if (
        not isinstance(entry.get("root"), str)
        or entry["root"] not in _DECLARED_ROOTS
    ):
        raise ContractError(
            f"invalid root at entrypoints[{j}] for component {comp_id!r}",
            code="malformed-catalog-input",
        )
    if not isinstance(entry.get("path"), str) or not entry["path"]:
        raise ContractError(
            f"non-string or empty entrypoint path at entrypoints[{j}] "
            f"for component {comp_id!r}",
            code="malformed-catalog-input",
        )
    _reject_nul(
        entry["path"],
        f"entrypoint path for component {comp_id!r} at entrypoints[{j}]",
    )


def _validated_scope_includes(raw: object, scope_id: str) -> list:
    """Type-guard a scope's includes container (round 9, I-3): a null
    value must not escape as a raw TypeError and a non-list container
    must not char-iterate into silent pruning — one
    malformed-catalog-input either way. Empty items are malformed too
    (schema minLength-1 discipline, round 9, I-1)."""
    if not isinstance(raw, list):
        raise ContractError(
            f"non-list includes for scope {scope_id!r}",
            code="malformed-catalog-input",
        )
    out: list = []
    for j, tree in enumerate(raw):
        if not isinstance(tree, str):
            raise ContractError(
                f"non-string include for scope {scope_id!r} at includes[{j}]",
                code="malformed-catalog-input",
            )
        if not tree.strip():
            raise ContractError(
                f"empty include for scope {scope_id!r} at includes[{j}]",
                code="malformed-catalog-input",
            )
        _reject_nul(tree, f"include for scope {scope_id!r} at includes[{j}]")
        out.append(tree)
    return out


def _validated_scope_excludes(raw: object, scope_id: str) -> list:
    """Type-guard a scope's excludes container (round 9, I-2): silently
    dropping a non-string exclude EXPANDS the scope toward
    machine-state files — non-list containers, non-string items, and
    empty items are all malformed-catalog-input."""
    if not isinstance(raw, list):
        raise ContractError(
            f"non-list excludes for scope {scope_id!r}",
            code="malformed-catalog-input",
        )
    out: list = []
    for j, item in enumerate(raw):
        if not isinstance(item, str):
            raise ContractError(
                f"non-string exclude for scope {scope_id!r} at excludes[{j}]",
                code="malformed-catalog-input",
            )
        if not item.strip():
            raise ContractError(
                f"empty exclude for scope {scope_id!r} at excludes[{j}]",
                code="malformed-catalog-input",
            )
        _reject_nul(item, f"exclude for scope {scope_id!r} at excludes[{j}]")
        # Shape gate (round 12, coordinator-proactive closure of the
        # round-9 I-2 family): a dot-leading or empty-segment pattern
        # matches no record rel — a silently no-op exclusion expands
        # the scope toward machine-state files just like a dropped one.
        ex_segments = item.split("/")
        # Round 13, GLM M-2: a ".."-shaped exclusion matches no
        # scope-relative rel — the machine-state protection silently
        # no-ops (the includes side already raises).
        if ".." in ex_segments:
            raise ContractError(
                f"parent-segment exclude for scope {scope_id!r} "
                f"at excludes[{j}]",
                code="malformed-catalog-input",
            )
        # Round 14, GLM M-2: one "/./" rewrite turned a live
        # machine-state exclusion into a pattern matching no record
        # rel — any ``.`` segment is malformed, not just leading.
        # Round 15, GLM F-1: so is any whitespace character (the
        # schema's path-token class forbids it; a padded pattern
        # silently matches nothing).
        if (
            any(ch.isspace() for ch in item)
            or "." in ex_segments
            or "" in ex_segments
        ):
            raise ContractError(
                f"malformed shape (whitespace/dot/empty-segment) in "
                f"exclude for scope {scope_id!r} at excludes[{j}]",
                code="malformed-catalog-input",
            )
        out.append(item)
    return out


def _declared_specs(catalog: Mapping, root_id: str) -> list:
    """Catalog-declared ``(spec, origin)`` pairs rooted at ``root_id``:
    component inputs, source entrypoints, and owner documents."""
    specs: list = []
    for component in _validated_catalog_components(catalog):
        if not isinstance(component, dict):
            continue
        comp_root = component.get("root")
        comp_id = component.get("id")
        inputs = component.get("inputs", [])
        if not isinstance(inputs, list):
            raise ContractError(
                f"non-list catalog inputs for component {comp_id!r}",
                code="malformed-catalog-input",
            )
        for j, item in enumerate(inputs):
            if not isinstance(item, str):
                raise ContractError(
                    f"non-string catalog input for component "
                    f"{comp_id!r} at inputs[{j}]",
                    code="malformed-catalog-input",
                )
            _reject_nul(
                item, f"catalog input for component {comp_id!r} at inputs[{j}]"
            )
            spec_root, sep, spec = item.partition(":")
            if sep:
                # Membership before the filter (round 13, GLM M-1): an
                # unknown (or empty) rooted-spec root silently skipped
                # the §8 generated target; a declared-but-other-root
                # spec keeps the legitimate silent skip.
                if spec_root not in _DECLARED_ROOTS:
                    raise ContractError(
                        f"invalid root in rooted input at inputs[{j}] "
                        f"for component {comp_id!r}",
                        code="malformed-catalog-input",
                    )
                if spec_root != root_id:
                    continue
            elif comp_root != root_id:
                continue
            target_root = spec_root if sep else comp_root
            origin = "input"
            if (
                comp_id == _GENERATED_TARGET_COMPONENT
                and target_root == "worktree"
            ):
                # the generated-rule worktree targets are hard-required
                # (DESIGN.md §8: a missing generated-rule target is a
                # hard error)
                origin = "generated-target"
            _check_rooted_spec(spec if sep else item, root_id, origin)
            specs.append((spec if sep else item, origin))
        entries = component.get("entrypoints", [])
        if not isinstance(entries, list):
            # A non-list container must not char-iterate (a string
            # char-iterates into non-dict skips) or raise a raw
            # TypeError (round 9, I-4).
            raise ContractError(
                f"non-list entrypoints for component {comp_id!r}",
                code="malformed-catalog-input",
            )
        for j, entry in enumerate(entries):
            if not isinstance(entry, dict):
                # A non-dict item must not be silently skipped — for a
                # mangled REQUIRED entrypoint that vacates the missing-
                # entrypoint gate (round 11, MAJOR-1).
                raise ContractError(
                    f"non-dict entrypoint at entrypoints[{j}] for "
                    f"component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            _validated_entrypoint_item(entry, comp_id, j)
            if entry.get("role") != "source":
                continue
            if entry.get("root") != root_id:
                continue
            if not isinstance(entry.get("path"), str):
                raise ContractError(
                    f"non-string entrypoint path for component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            _reject_nul(
                entry["path"], f"entrypoint path for component {comp_id!r}"
            )
            origin = f"entrypoint:{entry.get('availability')}"
            _check_rooted_spec(entry["path"], root_id, origin)
            specs.append((entry["path"], origin))
        docs = component.get("owner_docs", [])
        if not isinstance(docs, list):
            raise ContractError(
                f"non-list owner_docs for component {comp_id!r}",
                code="malformed-catalog-input",
            )
        for j, doc in enumerate(docs):
            if not isinstance(doc, dict):
                raise ContractError(
                    f"non-dict owner-doc at owner_docs[{j}] for "
                    f"component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            # Round 13, QC m-1: a mangled owner-doc root was conflated
            # with the cross-root skip — the record silently vanished.
            if (
                not isinstance(doc.get("root"), str)
                or doc["root"] not in _DECLARED_ROOTS
            ):
                raise ContractError(
                    f"invalid root at owner_docs[{j}] for component "
                    f"{comp_id!r}",
                    code="malformed-catalog-input",
                )
            if doc["root"] != root_id:
                continue
            if not isinstance(doc.get("path"), str):
                raise ContractError(
                    f"non-string owner-doc path for component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            _reject_nul(doc["path"], f"owner-doc path for component {comp_id!r}")
            _check_rooted_spec(doc["path"], root_id, "owner-doc")
            specs.append((doc["path"], "owner-doc"))
    return specs


def _expand_spec(root_dir: Path, spec: str, origin: str) -> list:
    rels = _expand_pattern(root_dir, spec)
    if rels:
        return rels
    # An empty expansion: a declared generated target is a hard
    # requirement (DESIGN.md §8) whether declared as a literal or as a
    # glob — a missing anchor directory or a glob matching nothing is a
    # hard error, not a silent skip.
    if origin == "generated-target":
        raise ContractError(
            f"declared generated target {spec!r} is missing",
            code="missing-generated-target",
        )
    if not _has_glob(spec) and origin == "entrypoint:required":
        raise ContractError(
            f"required source entrypoint {spec!r} is missing",
            code="missing-entrypoint",
        )
    # optional entrypoints, inputs, and owner documents: a missing
    # item is skipped (soft concern owned by the writer boundary)
    return []


def _candidate_rels(
    catalog: Mapping, scope: Mapping, root_dir: Path
) -> list:
    selection = scope.get("selection")
    root_id = scope.get("root")
    scope_id = scope.get("id")
    includes = _validated_scope_includes(scope.get("includes", []), scope_id)
    for include in includes:
        _check_rooted_spec(include, root_id, "scope-include")
    excludes = _validated_scope_excludes(scope.get("excludes", []), scope_id)
    rel_set: set = set()
    if selection == "recursive-all-regular-and-symlink":
        for include in includes:
            top = root_dir / include
            try:
                info = os.lstat(top)
            except OSError as exc:
                raise ContractError(
                    f"scope include {include!r} is missing: {exc.strerror}",
                    code="missing-scope-dir",
                ) from None
            if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
                raise ContractError(
                    f"scope include {include!r} is not a real directory",
                    code="include-not-directory",
                )
            rel_set.update(rel for rel, _ in _walk_files(root_dir, include))
    elif selection in (
        "catalog-static-sources-outside-plans-corpus",
        "catalog-static-sources-plus-indexer-source",
    ):
        for spec, origin in _declared_specs(catalog, root_id):
            rel_set.update(_expand_spec(root_dir, spec, origin))
        if selection == "catalog-static-sources-outside-plans-corpus":
            for other in _validated_catalog_scopes(catalog):
                if other.get("id") != "plans-corpus":
                    continue
                # Cross-scope includes are type-guarded like the scope's
                # own: null must not raise a raw TypeError and a
                # non-list container must not char-iterate into silent
                # pruning (round 9, I-3); a non-dict row must not be
                # conflated with an absent scope (round 10, MAJOR-3).
                for tree in _validated_scope_includes(
                    other.get("includes", []), "plans-corpus"
                ):
                    # Round 13, workhorse M-3: the prune-prefix join
                    # must be shape-guarded like the scope's own
                    # includes — a mangled item matched no record and
                    # silently disabled pruning (scope expansion).
                    _check_rooted_spec(tree, "plans-corpus",
                                       "scope-include")
                    prefix = tree.rstrip("/") + "/"
                    rel_set = {r for r in rel_set if not r.startswith(prefix)}
        else:
            for pattern in includes:
                if pattern == "catalog-declared":
                    continue
                rel_set.update(_expand_pattern(root_dir, pattern))
    else:
        raise ContractError(
            f"unknown scope selection {selection!r}", code="unknown-selection"
        )
    return [
        rel
        for rel in sorted(rel_set)
        if not any(_glob_match(rel, pattern) for pattern in excludes)
    ]


def _check_required_entrypoints(
    catalog: Mapping, scope: Mapping, candidates: list
) -> None:
    """A missing required source entrypoint is a hard error
    (DESIGN.md §8). Applies to entrypoints rooted at the scope root;
    entries filtered out of this scope by design (excludes, or the
    plans-corpus trees for plans-declared-inputs) are the other scope's
    concern and are skipped."""
    root_id = scope.get("root")
    selection = scope.get("selection")
    scope_id = scope.get("id")
    excludes = _validated_scope_excludes(scope.get("excludes", []), scope_id)
    corpus_prefixes: list = []
    if selection == "catalog-static-sources-outside-plans-corpus":
        for other in _validated_catalog_scopes(catalog):
            if other.get("id") != "plans-corpus":
                continue
            for tree in _validated_scope_includes(
                other.get("includes", []), "plans-corpus"
            ):
                # Round 13, workhorse M-3 (second site): same shape
                # gate on the corpus_prefixes join.
                _check_rooted_spec(tree, "plans-corpus", "scope-include")
                corpus_prefixes.append(tree.rstrip("/") + "/")
    # The recursive corpus scope only owns the entrypoints inside its own
    # include trees; declared-scope entrypoints outside them (e.g. the
    # parity-repro source entrypoint) are that scope's concern.
    include_prefixes: list = []
    if selection == "recursive-all-regular-and-symlink":
        for tree in _validated_scope_includes(
            scope.get("includes", []), scope_id
        ):
            include_prefixes.append(tree.rstrip("/") + "/")
    candidate_set = set(candidates)
    for component in _validated_catalog_components(catalog):
        if not isinstance(component, dict):
            continue
        entries = component.get("entrypoints", [])
        if not isinstance(entries, list):
            # Same container discipline as every other iteration site
            # (round 9, I-4): never char-iterate, never raw-escape.
            raise ContractError(
                f"non-list entrypoints for component "
                f"{component.get('id')!r}",
                code="malformed-catalog-input",
            )
        for j, entry in enumerate(entries):
            if not isinstance(entry, dict):
                # Same item discipline as every other iteration site
                # (round 11, MAJOR-1): a mangled required entrypoint
                # must not vacate the gate silently.
                raise ContractError(
                    f"non-dict entrypoint at entrypoints[{j}] for "
                    f"component {component.get('id')!r}",
                    code="malformed-catalog-input",
                )
            _validated_entrypoint_item(entry, component.get("id"), j)
            if isinstance(entry.get("path"), str):
                _reject_nul(
                    entry["path"],
                    f"entrypoint path for component {component.get('id')!r}",
                )
            if entry.get("role") != "source" or entry.get("availability") != "required":
                continue
            if entry.get("root") != root_id or not isinstance(entry.get("path"), str):
                continue
            rel = entry["path"]
            if rel in candidate_set:
                continue
            if any(_glob_match(rel, pattern) for pattern in excludes):
                continue
            if any(rel.startswith(prefix) for prefix in corpus_prefixes):
                continue
            if include_prefixes and not any(
                rel.startswith(prefix) for prefix in include_prefixes
            ):
                continue
            raise ContractError(
                f"required source entrypoint {rel!r} is missing",
                code="missing-entrypoint",
            )


# ---------------------------------------------------------------------------
# catalog-driven record attributes
# ---------------------------------------------------------------------------


class _CatalogIndex:
    """Precomputed catalog membership used for role, media type,
    generation state, and content policy derivation."""

    def __init__(self, catalog: Mapping) -> None:
        self.curated: set = set()
        curated_raw = catalog.get("curated_sources", [])
        if not isinstance(curated_raw, list):
            # Container discipline (round 9, I-4 class): a non-list
            # container must not char-iterate into a silent empty set.
            raise ContractError(
                "non-list curated_sources container",
                code="malformed-catalog-input",
            )
        for j, src in enumerate(curated_raw):
            if not isinstance(src, dict):
                # Item discipline (round 11, MAJOR-1): the formalism
                # adapter hard-fails the identical shape — a silent
                # skip here would be asymmetric.
                raise ContractError(
                    f"non-dict curated entry at curated_sources[{j}]",
                    code="malformed-catalog-input",
                )
            # Field discipline (round 12, M-3): a field-mangled dict
            # was silently skipped, flipping the record's content
            # policy (curated-text -> metadata-only) with zero
            # findings — asymmetric with the formalism adapter, which
            # hard-fails the identical shape at the field level.
            curated_root = src.get("root")
            curated_path = src.get("path")
            # Membership (round 13, GLM M-1): root="plans2" silently
            # dropped the designation — the record flipped
            # curated-text -> metadata-only (formalism hard-fails the
            # identical shape).
            if (
                not isinstance(curated_root, str)
                or curated_root not in _DECLARED_ROOTS
            ):
                raise ContractError(
                    f"invalid root at curated_sources[{j}]",
                    code="malformed-catalog-input",
                )
            if not isinstance(curated_path, str) or not curated_path:
                raise ContractError(
                    f"non-string or empty path at curated_sources[{j}]",
                    code="malformed-catalog-input",
                )
            _reject_nul(
                curated_path, f"curated path at curated_sources[{j}]"
            )
            # Round 14, GLM M-1: this was the only declared-path channel
            # without the shared shape gate — a mangle never matched a
            # record rel, so the designation silently vanished
            # (curated-text -> metadata-only, zero findings) while the
            # identical bytes hard-fail in the formalism curated walk.
            _check_rooted_spec(curated_path, curated_root, "curated-source")
            self.curated.add((curated_root, curated_path))
        self.entrypoint_kind: dict = {}
        self.semantic_specs: list = []
        self.generated_specs: list = []
        self.authored_specs: list = []
        for component in _validated_catalog_components(catalog):
            if not isinstance(component, dict):
                continue
            kind = component.get("kind")
            comp_root = component.get("root")
            comp_id = component.get("id")
            entries = component.get("entrypoints", [])
            if not isinstance(entries, list):
                # Same container discipline as every other iteration
                # site (round 9, I-4).
                raise ContractError(
                    f"non-list entrypoints for component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            for j, entry in enumerate(entries):
                if not isinstance(entry, dict):
                    # Item discipline (round 11, MAJOR-1): a non-dict
                    # item must not silently degrade the role of the
                    # file it would have claimed.
                    raise ContractError(
                        f"non-dict entrypoint at entrypoints[{j}] for "
                        f"component {comp_id!r}",
                        code="malformed-catalog-input",
                    )
                _validated_entrypoint_item(entry, comp_id, j)
                if (
                    entry.get("role") == "source"
                    and isinstance(entry.get("root"), str)
                    and isinstance(entry.get("path"), str)
                ):
                    _reject_nul(
                        entry["path"],
                        f"entrypoint path for component {comp_id!r}",
                    )
                    self.entrypoint_kind[(entry["root"], entry["path"])] = kind
            inputs = component.get("inputs", [])
            if not isinstance(inputs, list):
                raise ContractError(
                    f"non-list catalog inputs for component {comp_id!r}",
                    code="malformed-catalog-input",
                )
            for j, item in enumerate(inputs):
                if not isinstance(item, str):
                    raise ContractError(
                        f"non-string catalog input for component "
                        f"{comp_id!r} at inputs[{j}]",
                        code="malformed-catalog-input",
                    )
                _reject_nul(
                    item, f"catalog input for component {comp_id!r} at inputs[{j}]"
                )
                spec_root, sep, spec = item.partition(":")
                if sep and spec_root not in _DECLARED_ROOTS:
                    # Round 13, GLM M-1 (this site's twin of the
                    # _declared_specs gate).
                    raise ContractError(
                        f"invalid root in rooted input at inputs[{j}] "
                        f"for component {comp_id!r}",
                        code="malformed-catalog-input",
                    )
                target_root = spec_root if sep else comp_root
                if not isinstance(target_root, str):
                    continue
                rel = spec if sep else item
                if (
                    comp_id == _GENERATED_TARGET_COMPONENT
                    and target_root == "worktree"
                ):
                    if _has_glob(rel):
                        # a glob worktree target (the outbound_lint
                        # fixture corpus): the corpus is source-authored
                        # evidence, and only its exact generated members
                        # are generated outputs (DESIGN.md §2)
                        self.authored_specs.append((target_root, rel))
                        for generated in sorted(_GENERATED_TARGET_SPECS):
                            if _glob_match(generated, rel):
                                self.generated_specs.append(
                                    (target_root, generated)
                                )
                    elif rel in _GENERATED_TARGET_SPECS:
                        # the row's exact generated outputs (DESIGN.md §2)
                        self.generated_specs.append((target_root, rel))
                    else:
                        # the row's other worktree targets are
                        # source-authored: projection_tests.rs (DESIGN.md §2)
                        self.authored_specs.append((target_root, rel))
                if (
                    component.get("content_policy") in (
                        "semantic-json",
                        "curated-text",
                    )
                    and not _has_glob(rel)
                ):
                    # Semantic sources are declared by the semantic-json
                    # components and by the curated-text corpus
                    # components (DESIGN.md §3 allowlist items 2–3: the
                    # exact .json tools inputs and the declared fixtures
                    # directory's one-level meta.json files). Only exact
                    # file/directory specs qualify — glob specs like the
                    # outbound_lint fixture corpus are inventoried as
                    # authored test evidence, never semantic.
                    # semantic_match keeps the scope to those shapes
                    # only; the curated-text component's .md/.txt inputs
                    # are not semantic-json.
                    self.semantic_specs.append((target_root, rel))

    def generated_match(self, root_id: str, relpath: str) -> bool:
        for r, spec in self.generated_specs:
            if r != root_id:
                continue
            if _has_glob(spec):
                if _glob_match(relpath, spec):
                    return True
            elif spec == relpath:
                return True
        return False

    def authored_match(self, root_id: str, relpath: str) -> bool:
        """True when the path is one of the formalism-linter row's
        source-authored worktree targets (DESIGN.md §2): not a generated
        output, but still a declared static source of that row."""
        for r, spec in self.authored_specs:
            if r != root_id:
                continue
            if _has_glob(spec):
                if _glob_match(relpath, spec):
                    return True
            elif spec == relpath:
                return True
        return False

    def semantic_match(self, root_id: str, relpath: str) -> bool:
        for r, spec in self.semantic_specs:
            if r != root_id:
                continue
            if _has_glob(spec):
                if _glob_match(relpath, spec):
                    return True
            elif spec == relpath:
                # the exact .json tools files are semantic-json sources
                if relpath.endswith(".json"):
                    return True
            elif _glob_match(relpath, spec.rstrip("/") + "/*/meta.json"):
                # a declared fixtures directory: its one-level meta.json
                # files are semantic-json sources (DESIGN.md §3)
                return True
        return False


def _media_type_for(relpath: str) -> str:
    base = relpath.rsplit("/", 1)[-1]
    ext = os.path.splitext(base)[1].lower()
    return _EXTENSION_MEDIA.get(ext, "application/octet-stream")


def _heuristic_role(relpath: str) -> str:
    base = relpath.rsplit("/", 1)[-1]
    if base.endswith(".schema.json"):
        return "schema"
    if base in ("manifest.json", "catalog.json"):
        return "manifest"
    if "fixtures" in relpath.split("/"):
        return "fixture-metadata" if base == "meta.json" else "fixture-payload"
    ext = os.path.splitext(base)[1].lower()
    if ext in (".py", ".rs", ".sh"):
        return "source-code"
    if ext in (".md", ".txt"):
        return "curated-document"
    if ext == ".json":
        return "configuration"
    if ext in (".jsonl", ".ndjson"):
        return "capture"
    return "other"


def _derive_attributes(
    index: _CatalogIndex, root_id: str, relpath: str, file_type: str
) -> tuple:
    if file_type == "symlink":
        media_type = "inode/symlink"
    else:
        media_type = _media_type_for(relpath)
    role = None
    generation_state = "unknown"
    content_policy = "metadata-only"
    if (root_id, relpath) in index.curated:
        role = "curated-document"
        content_policy = "curated-text"
    elif (root_id, relpath) in index.entrypoint_kind:
        kind = index.entrypoint_kind[(root_id, relpath)]
        role = "launcher" if kind == "launcher" else "source-code"
        generation_state = "source-authored"
    elif index.generated_match(root_id, relpath):
        role = "generated-source"
        generation_state = "declared-generated"
    else:
        role = _heuristic_role(relpath)
        if index.authored_match(root_id, relpath):
            # source-authored worktree target of the formalism-linter
            # row (DESIGN.md §2): authored, not generated
            generation_state = "source-authored"
    if content_policy == "metadata-only" and index.semantic_match(
        root_id, relpath
    ):
        content_policy = "semantic-json"
    return role, media_type, generation_state, content_policy


# ---------------------------------------------------------------------------
# public scope entry points
# ---------------------------------------------------------------------------


def inventory_scope(ctx: AdapterContext, scope_id: str) -> InventorySnapshot:
    """Inventory one catalog-declared scope and return its sorted
    metadata records plus canonical scope digest."""
    scope = None
    for row in _validated_catalog_scopes(ctx.catalog):
        if row.get("id") == scope_id:
            scope = row
            break
    if scope is None:
        raise ContractError(
            f"unknown inventory scope {scope_id!r}", code="unknown-scope"
        )
    root_id = scope.get("root")
    if not isinstance(root_id, str) or root_id not in ctx.roots:
        raise ContractError(
            f"scope {scope_id!r} root {root_id!r} is not among the supplied roots",
            code="missing-root",
        )
    root_dir = Path(os.fspath(ctx.roots[root_id]))
    try:
        root_info = os.stat(root_dir)
    except OSError as exc:
        raise ContractError(
            f"scope root {root_id!r} is not a readable directory: {exc.strerror}",
            code="missing-root-dir",
        ) from None
    if not stat.S_ISDIR(root_info.st_mode):
        raise ContractError(
            f"scope root {root_id!r} is not a directory", code="missing-root-dir"
        )
    index = _CatalogIndex(ctx.catalog)
    rels = _candidate_rels(ctx.catalog, scope, root_dir)
    _check_required_entrypoints(ctx.catalog, scope, rels)
    # race-checked lstat classification of every candidate entry
    entries: list = []
    for rel in rels:
        abspath = root_dir / rel
        info = _lstat(abspath)
        if stat.S_ISREG(info.st_mode):
            entries.append((rel, abspath, "regular"))
        elif stat.S_ISLNK(info.st_mode):
            entries.append((rel, abspath, "symlink"))
        else:
            raise ContractError(
                f"unsupported file type in scope at {rel!r}",
                code="unsupported-file-type",
            )
    git_states = classify_git_state(root_dir, [rel for rel, _, _ in entries])
    records: list = []
    for rel, abspath, file_type in entries:
        role, media_type, generation_state, content_policy = _derive_attributes(
            index, root_id, rel, file_type
        )
        record: dict = {
            "scope": scope_id,
            "root": root_id,
            "path": rel,
            "file_type": file_type,
            "role": role,
            "media_type": media_type,
            "git_state": git_states[rel],
            "generation_state": generation_state,
            "content_policy": content_policy,
        }
        if file_type == "regular":
            hashed = hash_regular_stable(abspath)
            record["bytes"] = hashed.bytes
            record["sha256"] = hashed.sha256
        else:
            hashed = hash_symlink_stable(abspath, ctx.roots)
            record["bytes"] = hashed.bytes
            record["sha256"] = hashed.sha256
            record["symlink_target"] = hashed.target
        records.append(record)
    records.sort(key=lambda record: (record["root"], record["path"]))
    digest = hashlib.sha256(canonical_json_bytes(records)).hexdigest()
    return InventorySnapshot(records=tuple(records), sha256=digest)


def inventory_all(ctx: AdapterContext) -> tuple:
    """Inventory every catalog-declared scope, sorted by scope ID."""
    scope_ids = sorted(
        row.get("id")
        for row in _validated_catalog_scopes(ctx.catalog)
        if isinstance(row.get("id"), str)
    )
    return tuple(inventory_scope(ctx, scope_id) for scope_id in scope_ids)
