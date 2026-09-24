"""Inventory scope tests for the evaluation harness (Task 3).

Covers, against a temporary Git repository fixture and a static sample
catalog:

- complete, sorted, deterministic traversal of the plans-corpus scope
  (regular files and symlinks, ``follow_symlinks=False``, never
  following a symlink),
- exact Git state classification from NUL-delimited Git output scoped
  to the owning repository (tracked-clean / tracked-modified /
  untracked / ignored / outside-repository),
- race-safe stable hashing with pre/post lstat/fstat identity, size,
  mtime, and link-byte checks (SourceChangedError),
- catalog-driven role, media type, generation state, and content
  policy,
- the canonical scope digest over sorted metadata records,
- adapters.plan_corpus.discover metadata-only behavior (no entities,
  no section:* entities, run overlays rejected),
- a runtime set-equality assertion that an independent non-following
  walk of the live plans corpus yields exactly the (root, path,
  file_type) set of the plans-corpus scope (read-only; volatile corpus
  counts are never pinned).

Run from the worktree root:
    python3.14 -m unittest -v smoke.eval_harness.tests.test_inventory
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from smoke.eval_harness import contract, inventory, paths
from smoke.eval_harness.adapters import plan_corpus

_FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures" / "inventory"
_EMPTY_SHA256 = (
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
)


def _git_available() -> bool:
    return shutil.which("git") is not None


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------------------
# temporary Git repository fixture with every required case
# ---------------------------------------------------------------------------


def _run_git(repo: Path, *argv: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), *argv], capture_output=True, text=True
    )


def _build_git_fixture() -> tuple[Path, Path]:
    """Build a temporary Git repository covering the checklist cases.

    Returns ``(repo, outside)`` where ``outside`` holds targets that
    escape the repository (absolute-external symlinks, a directory
    target whose contents must never be inventoried).
    """
    repo = Path(tempfile.mkdtemp(prefix="eval-harness-inv-repo-", dir="/tmp"))
    outside = Path(tempfile.mkdtemp(prefix="eval-harness-inv-outside-", dir="/tmp"))
    assert _run_git(repo, "init", "-q", "-b", "main").returncode == 0
    corpus = repo / "corpus"
    for sub in (
        "curation/tools/fixtures/case-1",
        "ignored-dir",
        "scripts",
        "subdir",
        "tools",
    ):
        (corpus / sub).mkdir(parents=True)

    def write(rel: str, data: bytes) -> None:
        (corpus / rel).write_bytes(data)

    # committed entries
    write(".gitignore", b"ignored-dir/\n*.cache\n")
    write("tracked-clean.txt", b"a\n")
    write("tracked-modified.txt", b"b\n")
    write("renamed-old.txt", b"r\n")
    # space-named paths: porcelain v2 paths may contain spaces
    write("my report.txt", b"rep\n")
    write("spaced-src.txt", b"t\n")
    write("unknown.xyz", b"x\n")
    write("empty.bin", b"")
    write("unicode-\u00fc-\u65e5\u672c\u8a9e.md", "u\n".encode("utf-8"))
    write("subdir/nested-deep.json", b"{}\n")
    write("tools/rules.json", b'{"schema_version": 1}\n')
    write("curation/NOTES.md", b"# Notes\n")
    write("curation/tools/expected.json", b'{"schema_version": 1}\n')
    write("curation/tools/fixtures/case-1/meta.json", b'{"fixture_id": "case-1"}\n')
    write("curation/tools/fixtures/case-1/turn1.json", b'{"request": 1}\n')
    write("CURATED.md", b"# Fixture\n")
    write("scripts/tool.py", b"print('tool')\n")
    (corpus / "rel-link.txt").symlink_to("tracked-clean.txt")
    (corpus / "abs-link-under-root.txt").symlink_to(
        str(corpus / "subdir" / "nested-deep.json")
    )
    assert _run_git(repo, "add", "-A").returncode == 0
    assert (
        _run_git(
            repo,
            "-c",
            "user.name=eval-harness",
            "-c",
            "user.email=eval-harness@example.invalid",
            "commit",
            "-q",
            "-m",
            "fixture baseline",
        ).returncode
        == 0
    )
    # post-commit mutations: modified, untracked, and ignored entries
    write("tracked-modified.txt", b"b changed\n")
    write("untracked.txt", b"u\n")
    write("ignored-dir/cache.cache", b"c\n")
    # a staged rename (git mv): porcelain v2 emits a ten-token ``2``
    # record naming the new path plus a bare NUL-separated source-path
    # token that belongs to the record
    assert (
        _run_git(
            repo, "mv", "corpus/renamed-old.txt", "corpus/renamed-new.txt"
        ).returncode
        == 0
    )
    # a staged rename whose new path has three spaces: the record then
    # has thirteen whitespace-separated words, which must still parse as
    # the rename shape (path is the remainder, not the last word)
    assert (
        _run_git(
            repo, "mv", "corpus/spaced-src.txt",
            "corpus/new name with spaces.txt",
        ).returncode
        == 0
    )
    # a tracked-modified file with a space in its name: the ``1`` record
    # carries the path with the space as the remainder after the eight
    # fixed fields
    write("my report.txt", b"rep changed\n")
    (outside / "outside-target.txt").write_bytes(b"outside\n")
    (outside / "dir-target").mkdir()
    (outside / "dir-target" / "hidden.txt").write_bytes(b"h\n")
    (corpus / "abs-link-external.txt").symlink_to(
        str(outside / "outside-target.txt")
    )
    (corpus / "dir-link").symlink_to(str(outside / "dir-target"))
    return repo, outside


class GitFixtureCase(unittest.TestCase):
    """Shared setUp for tests that need the temporary Git repository."""

    def setUp(self) -> None:
        if not _git_available():
            self.skipTest("git CLI is required for the fixture tests")
        self.repo, self.outside = _build_git_fixture()
        self.addCleanup(shutil.rmtree, self.repo, True)
        self.addCleanup(shutil.rmtree, self.outside, True)
        self.ctx = contract.AdapterContext(
            roots={"plans": self.repo},
            catalog=_corpus_catalog(),
        )


def _corpus_catalog() -> dict:
    return {
        "schema_version": 1,
        "roots": [{"id": "plans", "required_for": ["build", "health"]}],
        "inventory_scopes": [
            {
                "id": "plans-corpus",
                "root": "plans",
                "includes": ["corpus"],
                "selection": "recursive-all-regular-and-symlink",
            }
        ],
        "curated_sources": [
            {
                "root": "plans",
                "path": "corpus/CURATED.md",
                "selectors": [{"kind": "heading", "text": "Fixture"}],
            }
        ],
        "components": [
            {
                "id": "corpus-tools",
                "kind": "tool",
                "root": "plans",
                "adapter": "plan_corpus",
                "content_policy": "semantic-json",
                "entrypoints": [
                    {
                        "root": "plans",
                        "path": "corpus/scripts/tool.py",
                        "role": "source",
                        "availability": "required",
                    }
                ],
                "inputs": ["corpus/tools/rules.json"],
                "outputs": [],
                "lifecycle": {"registry": None, "reason": "fixture"},
                "install": None,
                "owner_docs": [],
            },
            {
                # a curated-text corpus component (the catalog shape of
                # the parity-formalism row): its declared inputs carry
                # the DESIGN.md §3 allowlisted semantic sources
                "id": "corpus-curation",
                "kind": "corpus",
                "root": "plans",
                "adapter": "plan_corpus",
                "content_policy": "curated-text",
                "entrypoints": [],
                "inputs": [
                    "corpus/curation/NOTES.md",
                    "corpus/curation/tools/expected.json",
                    "corpus/curation/tools/fixtures",
                ],
                "outputs": [],
                "lifecycle": {"registry": None, "reason": "fixture"},
                "install": None,
                "owner_docs": [],
            },
        ],
    }


_EXPECTED_PATHS = (
    "corpus/.gitignore",
    "corpus/CURATED.md",
    "corpus/abs-link-external.txt",
    "corpus/abs-link-under-root.txt",
    "corpus/curation/NOTES.md",
    "corpus/curation/tools/expected.json",
    "corpus/curation/tools/fixtures/case-1/meta.json",
    "corpus/curation/tools/fixtures/case-1/turn1.json",
    "corpus/dir-link",
    "corpus/my report.txt",
    "corpus/new name with spaces.txt",
    "corpus/empty.bin",
    "corpus/ignored-dir/cache.cache",
    "corpus/rel-link.txt",
    "corpus/renamed-new.txt",
    "corpus/scripts/tool.py",
    "corpus/subdir/nested-deep.json",
    "corpus/tracked-clean.txt",
    "corpus/tracked-modified.txt",
    "corpus/tools/rules.json",
    "corpus/unicode-\u00fc-\u65e5\u672c\u8a9e.md",
    "corpus/unknown.xyz",
    "corpus/untracked.txt",
)

_EXPECTED_GIT_STATES = {
    "corpus/.gitignore": "tracked-clean",
    "corpus/CURATED.md": "tracked-clean",
    "corpus/abs-link-external.txt": "untracked",
    "corpus/abs-link-under-root.txt": "tracked-clean",
    "corpus/curation/NOTES.md": "tracked-clean",
    "corpus/curation/tools/expected.json": "tracked-clean",
    "corpus/curation/tools/fixtures/case-1/meta.json": "tracked-clean",
    "corpus/curation/tools/fixtures/case-1/turn1.json": "tracked-clean",
    "corpus/dir-link": "untracked",
    "corpus/my report.txt": "tracked-modified",
    "corpus/new name with spaces.txt": "tracked-modified",
    "corpus/empty.bin": "tracked-clean",
    "corpus/ignored-dir/cache.cache": "ignored",
    "corpus/rel-link.txt": "tracked-clean",
    "corpus/renamed-new.txt": "tracked-modified",
    "corpus/scripts/tool.py": "tracked-clean",
    "corpus/subdir/nested-deep.json": "tracked-clean",
    "corpus/tracked-clean.txt": "tracked-clean",
    "corpus/tracked-modified.txt": "tracked-modified",
    "corpus/tools/rules.json": "tracked-clean",
    "corpus/unicode-\u00fc-\u65e5\u672c\u8a9e.md": "tracked-clean",
    "corpus/unknown.xyz": "tracked-clean",
    "corpus/untracked.txt": "untracked",
}


# ---------------------------------------------------------------------------
# race-safe stable hashing
# ---------------------------------------------------------------------------


class StableHashTest(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(
            tempfile.mkdtemp(prefix="eval-harness-inv-hash-", dir="/tmp")
        )
        self.addCleanup(shutil.rmtree, self.root, True)
        self.file = self.root / "data.txt"
        self.file.write_bytes(b"payload\n")
        self.link = self.root / "link.txt"
        self.link.symlink_to("data.txt")
        self.roots = {"plans": self.root}

    def test_regular_file_hash_and_safe_metadata(self):
        out = inventory.hash_regular_stable(self.file)
        self.assertEqual(out.bytes, len(b"payload\n"))
        self.assertEqual(out.sha256, _sha256(b"payload\n"))
        self.assertEqual(out.size, len(b"payload\n"))
        real = os.lstat(self.file)
        self.assertEqual(out.dev, real.st_dev)
        self.assertEqual(out.ino, real.st_ino)
        self.assertEqual(out.mtime_ns, real.st_mtime_ns)

    def test_regular_file_empty(self):
        empty = self.root / "empty.bin"
        empty.write_bytes(b"")
        out = inventory.hash_regular_stable(empty)
        self.assertEqual(out.bytes, 0)
        self.assertEqual(out.sha256, _EMPTY_SHA256)

    def test_regular_file_refuses_symlink(self):
        with self.assertRaises(inventory.SourceChangedError):
            inventory.hash_regular_stable(self.link)

    def test_regular_file_identity_change_detected(self):
        real = os.lstat(self.file)
        fake = SimpleNamespace(
            st_mode=real.st_mode,
            st_ino=real.st_ino + 1,
            st_dev=real.st_dev,
            st_size=real.st_size,
            st_mtime_ns=real.st_mtime_ns,
        )
        with mock.patch("os.fstat", return_value=fake):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.hash_regular_stable(self.file)

    def test_regular_file_size_change_detected(self):
        real = os.lstat(self.file)
        changed = SimpleNamespace(
            st_mode=real.st_mode,
            st_ino=real.st_ino,
            st_dev=real.st_dev,
            st_size=real.st_size + 3,
            st_mtime_ns=real.st_mtime_ns,
        )
        with mock.patch("os.lstat", side_effect=[real, changed]):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.hash_regular_stable(self.file)

    def test_regular_file_mtime_change_detected(self):
        real = os.lstat(self.file)
        changed = SimpleNamespace(
            st_mode=real.st_mode,
            st_ino=real.st_ino,
            st_dev=real.st_dev,
            st_size=real.st_size,
            st_mtime_ns=real.st_mtime_ns + 1,
        )
        with mock.patch("os.lstat", side_effect=[real, changed]):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.hash_regular_stable(self.file)

    def test_symlink_link_bytes_hash_and_verbatim_target(self):
        out = inventory.hash_symlink_stable(self.link, self.roots)
        self.assertEqual(out.bytes, len(b"data.txt"))
        self.assertEqual(out.sha256, _sha256(b"data.txt"))
        # a relative link string is carried verbatim
        self.assertEqual(out.target, "data.txt")

    def test_symlink_absolute_under_root_normalized(self):
        under = self.root / "abs-link.txt"
        under.symlink_to(str(self.root / "data.txt"))
        out = inventory.hash_symlink_stable(under, self.roots)
        self.assertEqual(out.target, "@plans/data.txt")
        self.assertEqual(out.bytes, len(str(self.root / "data.txt").encode("utf-8")))
        self.assertEqual(
            out.sha256, _sha256(str(self.root / "data.txt").encode("utf-8"))
        )

    def test_symlink_absolute_external_normalized(self):
        external = self.root / "ext-link.txt"
        external.symlink_to("/opt/elsewhere/tool.md")
        out = inventory.hash_symlink_stable(external, self.roots)
        self.assertEqual(
            out.target,
            "@external/tool.md#" + _sha256(b"/opt/elsewhere/tool.md"),
        )
        self.assertEqual(out.bytes, len(b"/opt/elsewhere/tool.md"))
        self.assertEqual(out.sha256, _sha256(b"/opt/elsewhere/tool.md"))

    def test_symlink_target_change_detected(self):
        with mock.patch("os.readlink", side_effect=["data.txt", "other.txt"]):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.hash_symlink_stable(self.link, self.roots)

    def test_symlink_refuses_regular_file(self):
        with self.assertRaises(inventory.SourceChangedError):
            inventory.hash_symlink_stable(self.file, self.roots)


# ---------------------------------------------------------------------------
# NUL-delimited Git record parsing
# ---------------------------------------------------------------------------


class GitRecordParsingTest(unittest.TestCase):
    def test_status_v2_nul_records_parsed(self):
        # git 2.50 (Apple Git-155) shapes, observed verbatim:
        #   1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
        #   u <XY> <sub> <mH> <m1> <m2> <m3> <o1> <o2> <o3> <path>
        # plus the staged rename/copy ``2`` shape (see the dedicated
        # tests below). The legacy unmerged ``2`` form with thirteen
        # fields is no longer accepted: git 2.50's man page documents
        # ``2`` exclusively as renamed/copied, and the legacy shape
        # collides with a rename whose new path has three spaces.
        changed_rec = (
            b"1 M. N... 100644 100644 100644 "
            + b"a" * 40
            + b" "
            + b"b" * 40
            + b" changed.txt\0"
        )
        unmerged_rec = (
            b"u UU N... 100644 100644 100644 100644 "
            + b"c" * 40
            + b" "
            + b"d" * 40
            + b" "
            + b"e" * 40
            + b" conflicted.txt\0"
        )
        legacy_unmerged_rec = (
            b"2 UU 100000 100644 100644 "
            + b"f" * 40
            + b" "
            + b"g" * 40
            + b" "
            + b"h" * 40
            + b" "
            + b"i" * 40
            + b" "
            + b"j" * 40
            + b" "
            + b"k" * 40
            + b" "
            + b"l" * 40
            + b" legacy.txt\0"
        )
        untracked_rec = b"? new file.txt\0"
        ignored_rec = b"! ignored-dir/\0"
        data = changed_rec + unmerged_rec + untracked_rec + ignored_rec
        changed, untracked = inventory._parse_status_v2_nul(data)
        self.assertEqual(changed, {"changed.txt", "conflicted.txt"})
        self.assertEqual(untracked, {"new file.txt"})
        # the legacy thirteen-field unmerged ``2`` form now fails loud
        # instead of being parsed as a changed record
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(legacy_unmerged_rec)

    def test_staged_rename_copy_records_consume_source_path(self):
        # Apple Git 2.50 shapes, observed verbatim: a staged rename/copy
        # is a ten-token ``2`` record (ninth field an ``R<N>``/``C<N>``
        # score) whose last token is the new path, followed by a bare
        # NUL-separated source-path token that is part of the record.
        rename_rec = (
            b"2 R. N... 100644 100644 100644 "
            + b"a" * 40
            + b" "
            + b"b" * 40
            + b" R100 a2.txt\0a.txt\0"
        )
        copy_rec = (
            b"2 C. N... 100644 100644 100644 "
            + b"c" * 40
            + b" "
            + b"d" * 40
            + b" C100 c2.txt\0c.txt\0"
        )
        # a lower score and spaces inside both paths are legal: only the
        # nine fixed fields are split off, the remainder is the path
        spaced_rename_rec = (
            b"2 R. N... 100644 100644 100644 "
            + b"e" * 40
            + b" "
            + b"f" * 40
            + b" R093 renamed plan.txt\0my plan.txt\0"
        )
        # a new path with three spaces makes the record thirteen
        # whitespace-separated words: it must still parse as the rename
        # shape (path is the remainder after the nine fixed fields, the
        # bare source token is consumed), never as a legacy 13-token form
        three_space_rename_rec = (
            b"2 R. N... 100644 100644 100644 "
            + b"g" * 40
            + b" "
            + b"h" * 40
            + b" R093 new name with spaces.txt\0spaced-src.txt\0"
        )
        changed, untracked = inventory._parse_status_v2_nul(
            rename_rec
            + copy_rec
            + spaced_rename_rec
            + three_space_rename_rec
        )
        self.assertEqual(
            changed,
            {"a2.txt", "c2.txt", "renamed plan.txt", "new name with spaces.txt"},
        )
        # the source paths are consumed by their records, never parsed
        # as records of their own
        self.assertFalse(
            {"a.txt", "c.txt", "my plan.txt", "spaced-src.txt"} & changed
        )
        self.assertEqual(untracked, set())

    def test_space_in_path_records_parsed(self):
        # the path is the remainder after the fixed fields, so spaces
        # inside it are legal for every changed record type
        modified_rec = (
            b"1 .M N... 100644 100644 100644 "
            + b"a" * 40
            + b" "
            + b"b" * 40
            + b" my report.txt\0"
        )
        unmerged_rec = (
            b"u UU N... 100644 100644 100644 100644 "
            + b"c" * 40
            + b" "
            + b"d" * 40
            + b" "
            + b"e" * 40
            + b" conflicted file.txt\0"
        )
        changed, untracked = inventory._parse_status_v2_nul(
            modified_rec + unmerged_rec
        )
        self.assertEqual(changed, {"my report.txt", "conflicted file.txt"})
        self.assertEqual(untracked, set())

    def test_empty_untracked_or_ignored_path_rejected(self):
        # a ``?``/``!`` record with an empty path is malformed, never an
        # empty-string entry in the untracked set
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b"? \0")
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b"! \0")
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b"?\0? real.txt\0")

    def test_malformed_rename_records_rejected(self):
        # a ten-field ``2`` record whose ninth field is not an R/C score
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(
                b"2 M. N... 100644 100644 100644 "
                + b"a" * 40
                + b" "
                + b"b" * 40
                + b" M a2.txt\0a.txt\0"
            )
        # a rename record missing its bare source-path token
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(
                b"2 R. N... 100644 100644 100644 "
                + b"a" * 40
                + b" "
                + b"b" * 40
                + b" R100 a2.txt\0"
            )
        # a rename record with an empty source-path token
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(
                b"2 R. N... 100644 100644 100644 "
                + b"a" * 40
                + b" "
                + b"b" * 40
                + b" R100 a2.txt\0\0? after.txt\0"
            )

    def test_malformed_status_record_rejected(self):
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b"9 weird\0")
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b"1 missing-fields\0")

    def test_quoted_path_rejected(self):
        with self.assertRaises(contract.ContractError):
            inventory._parse_status_v2_nul(b'? "quoted.txt"\0')

    def test_nul_path_set_parsed(self):
        out = inventory._split_nul_paths(b"a.txt\0b/c.txt\0")
        self.assertEqual(out, {"a.txt", "b/c.txt"})
        self.assertEqual(inventory._split_nul_paths(b""), set())


# ---------------------------------------------------------------------------
# Git state classification against the temporary repository
# ---------------------------------------------------------------------------


class ClassifyGitStateTest(GitFixtureCase):
    def test_exact_state_mapping(self):
        states = inventory.classify_git_state(
            self.repo, list(_EXPECTED_PATHS)
        )
        self.assertEqual(states, _EXPECTED_GIT_STATES)

    def test_outside_repository_when_not_a_repo(self):
        plain = Path(tempfile.mkdtemp(prefix="eval-harness-inv-plain-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, plain, True)
        (plain / "a.txt").write_bytes(b"a")
        self.assertEqual(
            inventory.classify_git_state(plain, ["a.txt"]),
            {"a.txt": "outside-repository"},
        )

    def test_staged_rename_new_name_modified_old_path_no_phantom(self):
        states = inventory.classify_git_state(
            self.repo,
            [
                "corpus/renamed-new.txt",
                "corpus/renamed-old.txt",
                "corpus/new name with spaces.txt",
                "corpus/spaced-src.txt",
            ],
        )
        self.assertEqual(
            states,
            {
                # the rename record's last token (the new path) is the
                # changed entry
                "corpus/renamed-new.txt": "tracked-modified",
                # the consumed source path left the index and must not
                # resurrect as a phantom changed/tracked entry
                "corpus/renamed-old.txt": "untracked",
                # the three-space new name is the remainder after the
                # nine fixed fields, not the last word
                "corpus/new name with spaces.txt": "tracked-modified",
                "corpus/spaced-src.txt": "untracked",
            },
        )

    def test_nested_repository_is_outside_repository(self):
        nested = self.repo / "corpus" / "subdir" / "nested"
        nested.mkdir()
        assert _run_git(nested, "init", "-q", "-b", "main").returncode == 0
        (nested / "inner.txt").write_bytes(b"i\n")
        states = inventory.classify_git_state(
            self.repo, ["corpus/tracked-clean.txt", "corpus/subdir/nested/inner.txt"]
        )
        self.assertEqual(states["corpus/tracked-clean.txt"], "tracked-clean")
        self.assertEqual(states["corpus/subdir/nested/inner.txt"], "outside-repository")

    def test_empty_path_list(self):
        self.assertEqual(inventory.classify_git_state(self.repo, []), {})


# ---------------------------------------------------------------------------
# plans-corpus scope end to end on the temporary repository
# ---------------------------------------------------------------------------


class PlansCorpusScopeTest(GitFixtureCase):
    def test_sorted_complete_coverage(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        recorded = [r["path"] for r in snap.records]
        self.assertEqual(set(recorded), set(_EXPECTED_PATHS))
        self.assertEqual(
            [(r["root"], r["path"]) for r in snap.records],
            sorted((r["root"], r["path"]) for r in snap.records),
        )
        self.assertTrue(all(r["root"] == "plans" for r in snap.records))
        self.assertTrue(all(r["scope"] == "plans-corpus" for r in snap.records))

    def test_symlinks_not_followed_and_digests_raw_link_bytes(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        # symlink-to-directory is recorded, never descended into
        self.assertNotIn("corpus/dir-link/hidden.txt", by_path)
        dir_link = by_path["corpus/dir-link"]
        self.assertEqual(dir_link["file_type"], "symlink")
        self.assertEqual(dir_link["media_type"], "inode/symlink")
        for rel, target, expected in (
            (
                "corpus/rel-link.txt",
                "tracked-clean.txt",
                "tracked-clean.txt",
            ),
            (
                "corpus/abs-link-under-root.txt",
                str(self.repo / "corpus" / "subdir" / "nested-deep.json"),
                "@plans/corpus/subdir/nested-deep.json",
            ),
            (
                "corpus/abs-link-external.txt",
                str(self.outside / "outside-target.txt"),
                "@external/outside-target.txt#"
                + _sha256(str(self.outside / "outside-target.txt").encode("utf-8")),
            ),
            (
                "corpus/dir-link",
                str(self.outside / "dir-target"),
                "@external/dir-target#"
                + _sha256(str(self.outside / "dir-target").encode("utf-8")),
            ),
        ):
            record = by_path[rel]
            self.assertEqual(record["file_type"], "symlink")
            self.assertEqual(record["media_type"], "inode/symlink")
            self.assertEqual(record["bytes"], len(target.encode("utf-8")))
            self.assertEqual(record["sha256"], _sha256(target.encode("utf-8")))
            self.assertEqual(record["symlink_target"], expected)
        # regular records never carry a symlink target
        for record in by_path.values():
            if record["file_type"] == "regular":
                self.assertNotIn("symlink_target", record)
        # no file from the external directory ever entered the scope
        self.assertNotIn(
            "corpus/outside-target.txt", by_path
        )

    def test_exact_git_state_enum_values(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        self.assertEqual(
            {p: r["git_state"] for p, r in by_path.items()}, _EXPECTED_GIT_STATES
        )

    def test_vanished_entry_raises_typed_source_changed(self):
        # GLM R3 m-2: an entry vanishing between traversal and the
        # classification lstat must surface as the module's typed
        # SourceChangedError, never a raw uncaught OSError. The flaky
        # lstat passes the include-directory check through and fails
        # only for a fixture entry file.
        real_lstat = os.lstat

        def flaky(path, *args, **kwargs):
            if str(path).endswith(".txt") and "corpus" in str(path):
                raise FileNotFoundError(2, "No such file or directory")
            return real_lstat(path, *args, **kwargs)

        with mock.patch.object(inventory.os, "lstat", side_effect=flaky):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.inventory_scope(self.ctx, "plans-corpus")

    def test_curated_text_component_semantic_sources(self):
        # DESIGN.md §3 semantic-content allowlist items 2–3, catalog
        # driven: the exact .json tools input of a curated-text corpus
        # component and its declared fixtures directory's one-level
        # meta.json files are semantic-json at the inventory layer.
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        self.assertEqual(
            by_path["corpus/curation/tools/expected.json"]["content_policy"],
            "semantic-json",
        )
        self.assertEqual(
            by_path["corpus/curation/tools/fixtures/case-1/meta.json"][
                "content_policy"
            ],
            "semantic-json",
        )
        # adjacent non-allowlisted fixture content stays metadata-only
        # (DESIGN.md §3 item 3: turn*.json and wire bodies are excluded)
        self.assertEqual(
            by_path["corpus/curation/tools/fixtures/case-1/turn1.json"][
                "content_policy"
            ],
            "metadata-only",
        )
        # a curated-text component's non-.json inputs that are not
        # curated sources remain metadata-only, not semantic-json
        self.assertEqual(
            by_path["corpus/curation/NOTES.md"]["content_policy"],
            "metadata-only",
        )

    def test_staged_rename_recorded_modified_without_phantom(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        renamed = by_path["corpus/renamed-new.txt"]
        self.assertEqual(renamed["git_state"], "tracked-modified")
        self.assertEqual(renamed["sha256"], _sha256(b"r\n"))
        # the old path must not resurrect as a phantom record
        self.assertNotIn("corpus/renamed-old.txt", by_path)
        # the three-space rename is recorded under its full new name
        spaced = by_path["corpus/new name with spaces.txt"]
        self.assertEqual(spaced["git_state"], "tracked-modified")
        self.assertEqual(spaced["sha256"], _sha256(b"t\n"))
        self.assertNotIn("corpus/spaced-src.txt", by_path)
        # the space-named modified file keeps its full name and content
        report = by_path["corpus/my report.txt"]
        self.assertEqual(report["git_state"], "tracked-modified")
        self.assertEqual(report["sha256"], _sha256(b"rep changed\n"))

    def test_exact_role_media_generation_policy_enums(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        for record in snap.records:
            self.assertIn(record["git_state"], inventory.GIT_STATES)
            self.assertIn(record["generation_state"], inventory.GENERATION_STATES)
            self.assertIn(record["media_type"], inventory.MEDIA_TYPES)
            self.assertIn(record["role"], inventory.ROLES)
            self.assertIn(record["content_policy"], inventory.CONTENT_POLICIES)
            self.assertIn(record["file_type"], inventory.FILE_TYPES)
        # unknown extension maps deterministically to octet-stream
        self.assertEqual(
            by_path["corpus/unknown.xyz"]["media_type"], "application/octet-stream"
        )
        # the empty file commits to zero bytes and the empty digest
        self.assertEqual(by_path["corpus/empty.bin"]["bytes"], 0)
        self.assertEqual(by_path["corpus/empty.bin"]["sha256"], _EMPTY_SHA256)
        self.assertEqual(
            by_path["corpus/tracked-clean.txt"]["sha256"], _sha256(b"a\n")
        )
        self.assertEqual(
            by_path["corpus/unicode-\u00fc-\u65e5\u672c\u8a9e.md"]["media_type"],
            "text/markdown",
        )
        # catalog-driven roles / generation state / content policy
        curated = by_path["corpus/CURATED.md"]
        self.assertEqual(curated["role"], "curated-document")
        self.assertEqual(curated["content_policy"], "curated-text")
        tool = by_path["corpus/scripts/tool.py"]
        self.assertEqual(tool["role"], "source-code")
        self.assertEqual(tool["generation_state"], "source-authored")
        self.assertEqual(tool["content_policy"], "metadata-only")
        rules = by_path["corpus/tools/rules.json"]
        self.assertEqual(rules["content_policy"], "semantic-json")
        for other in by_path.values():
            if other["path"] not in (
                "corpus/CURATED.md",
                "corpus/tools/rules.json",
                "corpus/curation/tools/expected.json",
                "corpus/curation/tools/fixtures/case-1/meta.json",
            ):
                self.assertEqual(other["content_policy"], "metadata-only")
            if other["path"] != "corpus/scripts/tool.py":
                self.assertEqual(other["generation_state"], "unknown")

    def test_canonical_scope_digest_over_sorted_records(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        self.assertRegex(snap.sha256, r"^[0-9a-f]{64}$")
        expected = _sha256(
            contract.canonical_json_bytes([dict(r) for r in snap.records])
        )
        self.assertEqual(snap.sha256, expected)
        # the digest commits to record order: reordering changes it
        shuffled = list(snap.records)
        shuffled[0], shuffled[1] = shuffled[1], shuffled[0]
        self.assertNotEqual(
            snap.sha256, _sha256(contract.canonical_json_bytes(shuffled))
        )

    def test_deterministic_across_runs(self):
        first = inventory.inventory_scope(self.ctx, "plans-corpus")
        second = inventory.inventory_scope(self.ctx, "plans-corpus")
        self.assertEqual(first.records, second.records)
        self.assertEqual(first.sha256, second.sha256)

    def test_source_changed_during_scan_propagates(self):
        with mock.patch.object(
            inventory,
            "hash_regular_stable",
            side_effect=inventory.SourceChangedError("source changed"),
        ):
            with self.assertRaises(inventory.SourceChangedError):
                inventory.inventory_scope(self.ctx, "plans-corpus")

    def test_missing_include_directory_rejected(self):
        catalog = _corpus_catalog()
        catalog["inventory_scopes"][0]["includes"] = ["absent-tree"]
        ctx = contract.AdapterContext(roots={"plans": self.repo}, catalog=catalog)
        with self.assertRaises(contract.ContractError):
            inventory.inventory_scope(ctx, "plans-corpus")

    def test_unknown_scope_rejected(self):
        with self.assertRaises(contract.ContractError):
            inventory.inventory_scope(self.ctx, "no-such-scope")

    def test_missing_required_entrypoint_rejected(self):
        catalog = _corpus_catalog()
        (self.repo / "corpus" / "scripts" / "tool.py").unlink()
        ctx = contract.AdapterContext(roots={"plans": self.repo}, catalog=catalog)
        with self.assertRaises(contract.ContractError):
            inventory.inventory_scope(ctx, "plans-corpus")


# ---------------------------------------------------------------------------
# declared-inputs scope selection (static sample catalog fixture)
# ---------------------------------------------------------------------------


class DeclaredScopeSelectionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.catalog = json.loads(
            (_FIXTURES_DIR / "sample_catalog.json").read_text(encoding="utf-8")
        )
        self.plans = Path(tempfile.mkdtemp(prefix="eval-harness-inv-plans-", dir="/tmp"))
        self.worktree = Path(
            tempfile.mkdtemp(prefix="eval-harness-inv-wt-", dir="/tmp")
        )
        self.addCleanup(shutil.rmtree, self.plans, True)
        self.addCleanup(shutil.rmtree, self.worktree, True)
        self.ctx = contract.AdapterContext(
            roots={"plans": self.plans, "worktree": self.worktree},
            catalog=self.catalog,
        )

    def _build_plans_tree(self) -> None:
        for rel in (
            "alpha/CURATED.md",
            "alpha/in-corpus.md",
            "alpha/other.md",
            "extra/doc.md",
            "extra/owner.md",
            "extra/undeclared.txt",
            "tool/entry.sh",
        ):
            path = self.plans / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"x\n")

    def _build_worktree_tree(self) -> None:
        for rel in (
            "declared-input.txt",
            "smoke/harness/README.md",
            "smoke/harness/top.py",
            "smoke/harness/adapters/a.py",
            "smoke/harness/notes.txt",
            "smoke/harness/tests/t.py",
            "smoke/harness/tests/fixtures/f.json",
            "smoke/harness/site/site.json",
            "smoke/harness/__pycache__/top.pyc",
        ):
            path = self.worktree / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"x\n")

    def test_worktree_selection_includes_and_excludes(self):
        self._build_worktree_tree()
        snap = inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        recorded = [r["path"] for r in snap.records]
        self.assertEqual(
            set(recorded),
            {
                "declared-input.txt",
                "smoke/harness/README.md",
                "smoke/harness/top.py",
                "smoke/harness/tests/t.py",
                "smoke/harness/tests/fixtures/f.json",
            },
        )
        self.assertEqual(
            recorded,
            [
                "declared-input.txt",
                "smoke/harness/README.md",
                "smoke/harness/tests/fixtures/f.json",
                "smoke/harness/tests/t.py",
                "smoke/harness/top.py",
            ],
        )

    def test_worktree_records_outside_repository(self):
        self._build_worktree_tree()
        snap = inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        self.assertTrue(
            all(
                r["git_state"] == "outside-repository" for r in snap.records
            )
        )

    def test_worktree_entrypoint_role_and_generated_target_policy(self):
        self._build_worktree_tree()
        snap = inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        by_path = {r["path"]: r for r in snap.records}
        top = by_path["smoke/harness/top.py"]
        self.assertEqual(top["role"], "source-code")
        self.assertEqual(top["generation_state"], "source-authored")
        self.assertEqual(top["media_type"], "text/x-python")

    def test_worktree_missing_required_entrypoint_rejected(self):
        self._build_worktree_tree()
        (self.worktree / "smoke/harness/top.py").unlink()
        with self.assertRaises(contract.ContractError):
            inventory.inventory_scope(self.ctx, "worktree-declared-inputs")

    def test_plans_declared_inputs_exclude_corpus_trees(self):
        self._build_plans_tree()
        snap = inventory.inventory_scope(self.ctx, "plans-declared-inputs")
        self.assertEqual(
            [r["path"] for r in snap.records],
            ["extra/doc.md", "extra/owner.md", "tool/entry.sh"],
        )
        by_path = {r["path"]: r for r in snap.records}
        self.assertEqual(by_path["tool/entry.sh"]["role"], "source-code")
        self.assertEqual(by_path["extra/doc.md"]["content_policy"], "metadata-only")


# ---------------------------------------------------------------------------
# formalism-linter worktree generated targets (DESIGN.md §2 row)
# ---------------------------------------------------------------------------

_GENERATED_CRATE = "crates/codegen/xai-grok-sampling-types"


def _generated_target_catalog() -> dict:
    """Minimal catalog declaring the formalism-linter worktree targets
    exactly as the live catalog does (DESIGN.md §2): the generator
    output, the source-authored A3 projection-seam tests, and the
    outbound-lint fixture corpus glob."""
    return {
        "schema_version": 1,
        "roots": [
            {"id": "plans", "required_for": ["build", "health"]},
            {"id": "worktree", "required_for": ["build", "health"]},
        ],
        "inventory_scopes": [
            {
                "id": "worktree-declared-inputs",
                "root": "worktree",
                "selection": "catalog-static-sources-plus-indexer-source",
                "includes": ["catalog-declared"],
            }
        ],
        "components": [
            {
                "id": "formalism-linter",
                "kind": "tool",
                "root": "plans",
                "adapter": "formalism",
                "content_policy": "semantic-json",
                "entrypoints": [],
                "inputs": [
                    f"worktree:{_GENERATED_CRATE}/src/conversation/"
                    "rules_generated.rs",
                    f"worktree:{_GENERATED_CRATE}/src/conversation/"
                    "projection_tests.rs",
                    f"worktree:{_GENERATED_CRATE}/fixtures/outbound_lint/**",
                ],
                "outputs": [],
                "lifecycle": {"registry": None, "reason": "fixture"},
                "install": None,
                "owner_docs": [],
            }
        ],
    }


class GeneratedTargetSelectionTest(unittest.TestCase):
    def setUp(self):
        self.worktree = Path(
            tempfile.mkdtemp(prefix="eval-harness-inv-genwt-", dir="/tmp")
        )
        self.addCleanup(shutil.rmtree, self.worktree, True)
        self.ctx = contract.AdapterContext(
            roots={"worktree": self.worktree},
            catalog=_generated_target_catalog(),
        )

    def _build_worktree_tree(self) -> None:
        for rel, data in (
            (
                f"{_GENERATED_CRATE}/src/conversation/rules_generated.rs",
                b"// generated\n",
            ),
            (
                f"{_GENERATED_CRATE}/src/conversation/projection_tests.rs",
                b"// source-authored A3 projection-seam tests\n",
            ),
            (
                f"{_GENERATED_CRATE}/fixtures/outbound_lint/hard_rules.json",
                b'{"rules": []}\n',
            ),
            (
                f"{_GENERATED_CRATE}/fixtures/outbound_lint/bodies/b1.json",
                b'{"body": 1}\n',
            ),
            (
                f"{_GENERATED_CRATE}/fixtures/outbound_lint/headers/h1.json",
                b'{"header": 1}\n',
            ),
        ):
            path = self.worktree / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

    def test_generated_targets_and_authored_evidence_labels(self):
        # DESIGN.md §2 row annotations: only rules_generated.rs and
        # outbound_lint/hard_rules.json are generator output;
        # projection_tests.rs and the remaining outbound-lint fixtures
        # are source-authored test evidence, inventoried as metadata-only
        self._build_worktree_tree()
        snap = inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        by_path = {r["path"]: r for r in snap.records}
        for path in (
            f"{_GENERATED_CRATE}/src/conversation/rules_generated.rs",
            f"{_GENERATED_CRATE}/fixtures/outbound_lint/hard_rules.json",
        ):
            record = by_path[path]
            self.assertEqual(record["role"], "generated-source")
            self.assertEqual(record["generation_state"], "declared-generated")
            self.assertEqual(record["content_policy"], "metadata-only")
        for path in (
            f"{_GENERATED_CRATE}/src/conversation/projection_tests.rs",
            f"{_GENERATED_CRATE}/fixtures/outbound_lint/bodies/b1.json",
            f"{_GENERATED_CRATE}/fixtures/outbound_lint/headers/h1.json",
        ):
            record = by_path[path]
            self.assertEqual(record["generation_state"], "source-authored", path)
            self.assertEqual(record["content_policy"], "metadata-only", path)

    def test_missing_generated_glob_anchor_raises(self):
        # a declared generated target is hard-required (DESIGN.md §8)
        # whether it is a literal path or a glob: a missing anchor
        # directory for the outbound_lint/** declaration fails loud
        self._build_worktree_tree()
        shutil.rmtree(self.worktree / f"{_GENERATED_CRATE}/fixtures")
        with self.assertRaises(contract.ContractError) as ctx:
            inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        self.assertEqual(ctx.exception.code, "missing-generated-target")

    def test_missing_generated_glob_expansion_raises(self):
        # an anchor that exists but expands to no files is equally a
        # missing generated target
        self._build_worktree_tree()
        lint = self.worktree / f"{_GENERATED_CRATE}/fixtures/outbound_lint"
        for entry in sorted(lint.iterdir()):
            if entry.is_dir():
                shutil.rmtree(entry)
            else:
                entry.unlink()
        with self.assertRaises(contract.ContractError) as ctx:
            inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        self.assertEqual(ctx.exception.code, "missing-generated-target")

    def test_missing_generated_literal_raises(self):
        # the literal generated target keeps its existing hard-missing
        # semantics
        self._build_worktree_tree()
        (
            self.worktree
            / f"{_GENERATED_CRATE}/src/conversation/rules_generated.rs"
        ).unlink()
        with self.assertRaises(contract.ContractError) as ctx:
            inventory.inventory_scope(self.ctx, "worktree-declared-inputs")
        self.assertEqual(ctx.exception.code, "missing-generated-target")


# ---------------------------------------------------------------------------
# plan_corpus adapter
# ---------------------------------------------------------------------------


class PlanCorpusAdapterTest(GitFixtureCase):
    def test_discover_rejects_requested_runs(self):
        with self.assertRaises(contract.ContractError):
            plan_corpus.discover(
                self.ctx, (paths.RootedPath("plans", "corpus/any.txt"),)
            )

    def test_discover_emits_metadata_only(self):
        result = plan_corpus.discover(self.ctx)
        self.assertIsInstance(result, contract.AdapterResult)
        self.assertEqual(result.entities, ())
        self.assertEqual(result.relationships, ())
        self.assertEqual(result.runs, ())
        self.assertEqual(result.findings, ())
        # no section:* entities: Task 4's formalism adapter owns them
        for entity in result.entities:
            self.assertFalse(str(entity.get("id", "")).startswith("section:"))

    def test_discover_runs_the_corpus_inventory(self):
        sentinel = inventory.InventorySnapshot((), "0" * 64)
        with mock.patch.object(
            inventory, "inventory_scope", return_value=sentinel
        ) as patched:
            plan_corpus.discover(self.ctx)
        patched.assert_called_once_with(self.ctx, "plans-corpus")


# ---------------------------------------------------------------------------
# live plans-corpus integration (runtime-computed, read-only)
# ---------------------------------------------------------------------------


def _real_plans_root() -> Path | None:
    worktree = Path(__file__).resolve().parents[3]
    candidate = os.path.normpath(
        os.path.join(str(worktree), "..", "..", "grok", "plans")
    )
    return Path(candidate) if Path(candidate).is_dir() else None


def _independent_walk(root: Path, includes: list[str]) -> set[tuple[str, str, str]]:
    """A deliberately separate non-following walk for cross-checking."""
    found: set[tuple[str, str, str]] = set()
    for include in includes:
        base = root / include
        if not base.is_dir():
            raise AssertionError(f"plans-corpus tree {include!r} is missing")
        stack: list[tuple[Path, str]] = [(base, include)]
        while stack:
            directory, prefix = stack.pop()
            with os.scandir(directory) as it:
                for entry in it:
                    rel = f"{prefix}/{entry.name}"
                    if entry.is_symlink():
                        found.add(("plans", rel, "symlink"))
                    elif entry.is_dir(follow_symlinks=False):
                        stack.append((Path(entry.path), rel))
                    elif entry.is_file(follow_symlinks=False):
                        found.add(("plans", rel, "regular"))
    return found


class RealCorpusIntegrationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.plans_root = _real_plans_root()
        if self.plans_root is None:
            self.skipTest("the plans repository is not available on this host")
        self.catalog = contract.load_catalog()
        self.includes = [
            row
            for row in self.catalog["inventory_scopes"]
            if row["id"] == "plans-corpus"
        ][0]["includes"]
        self.ctx = contract.AdapterContext(
            roots={"plans": self.plans_root, "worktree": Path(".")},
            catalog=self.catalog,
        )

    def test_independent_non_following_walk_set_equality(self):
        # The corpus is LIVE: other campaigns write to it concurrently.
        # Equality is computed at runtime as a set of (root, path,
        # file_type) triples; volatile counts are never pinned. The
        # supplied plans root arrives only through AdapterContext roots;
        # nothing below writes there.
        snap = None
        recorded: set[tuple[str, str, str]] = set()
        walked: set[tuple[str, str, str]] = set()
        for _attempt in range(2):
            snap = inventory.inventory_scope(self.ctx, "plans-corpus")
            recorded = {
                (r["root"], r["path"], r["file_type"]) for r in snap.records
            }
            walked = _independent_walk(self.plans_root, self.includes)
            if recorded == walked:
                break
        self.assertIsNotNone(snap)
        self.assertTrue(recorded, "the plans corpus must not be empty")
        self.assertEqual(
            recorded,
            walked,
            "plans-corpus records diverge from the independent walk; "
            f"missing={sorted(recorded - walked)[:25]} "
            f"extra={sorted(walked - recorded)[:25]}",
        )

    def test_real_corpus_records_enums_sorted_and_read_only_digest(self):
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        self.assertTrue(snap.records)
        for record in snap.records:
            self.assertEqual(record["root"], "plans")
            self.assertIn(record["git_state"], inventory.GIT_STATES)
            self.assertIn(record["media_type"], inventory.MEDIA_TYPES)
            self.assertIn(record["role"], inventory.ROLES)
            self.assertIn(record["content_policy"], inventory.CONTENT_POLICIES)
        keys = [(r["root"], r["path"]) for r in snap.records]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(
            snap.sha256,
            _sha256(contract.canonical_json_bytes([dict(r) for r in snap.records])),
        )

    def test_real_corpus_allowlisted_semantic_sources(self):
        # DESIGN.md §3 semantic-content allowlist items 2–3 against the
        # live catalog: the exact .json tools input and the declared
        # fixtures directory's one-level meta.json files are
        # semantic-json at the inventory layer; every other file in the
        # fixtures tree (wire bodies, turn*.json, ...) stays
        # metadata-only.
        snap = inventory.inventory_scope(self.ctx, "plans-corpus")
        by_path = {r["path"]: r for r in snap.records}
        verdicts = by_path.get(
            "parity-formalism/tools/expected_verdicts_ev12.json"
        )
        self.assertIsNotNone(
            verdicts, "allowlisted expected-verdicts input is missing"
        )
        self.assertEqual(verdicts["content_policy"], "semantic-json")
        # the catalog's formalism-linter selftest names this exact
        # fixture directory, so its meta.json is a stable pin
        self.assertIn(
            "parity-formalism/tools/fixtures/at-strict-xwire-01/meta.json",
            by_path,
            "catalog selftest fixture dir is missing from the corpus",
        )
        meta = [
            r
            for r in snap.records
            if inventory._glob_match(
                r["path"], "parity-formalism/tools/fixtures/*/meta.json"
            )
        ]
        self.assertTrue(meta, "no fixtures/*/meta.json records in the corpus")
        for record in meta:
            self.assertEqual(record["content_policy"], "semantic-json", record["path"])
        for record in snap.records:
            path = record["path"]
            if not path.startswith("parity-formalism/tools/fixtures/"):
                continue
            if inventory._glob_match(
                path, "parity-formalism/tools/fixtures/*/meta.json"
            ):
                continue
            self.assertEqual(record["content_policy"], "metadata-only", path)


class RealWorktreeScopeTest(unittest.TestCase):
    def setUp(self):
        worktree = Path(__file__).resolve().parents[3]
        if not (worktree / "crates").is_dir():
            self.skipTest("the live worktree is not available on this host")
        self.ctx = contract.AdapterContext(
            roots={"worktree": worktree},
            catalog=contract.load_catalog(),
        )

    def _scan(self, scope_id):
        for _attempt in range(3):
            try:
                return inventory.inventory_scope(self.ctx, scope_id)
            except inventory.SourceChangedError:
                continue
        self.fail("the live worktree changed during every scan attempt")

    def test_outbound_lint_generated_and_authored_labels(self):
        # DESIGN.md §2 row annotations, pinned against the live catalog
        # and worktree: only rules_generated.rs and outbound_lint/
        # hard_rules.json are generator output; projection_tests.rs and
        # the remaining outbound-lint fixtures are source-authored test
        # evidence, inventoried as metadata-only.
        snap = None
        for _attempt in range(3):
            try:
                snap = inventory.inventory_scope(
                    self.ctx, "worktree-declared-inputs"
                )
                break
            except inventory.SourceChangedError:
                continue
        self.assertIsNotNone(
            snap, "the live worktree changed during every scan attempt"
        )
        by_path = {r["path"]: r for r in snap.records}
        hard_rules = f"{_GENERATED_CRATE}/fixtures/outbound_lint/hard_rules.json"
        for path in (
            f"{_GENERATED_CRATE}/src/conversation/rules_generated.rs",
            hard_rules,
        ):
            record = by_path.get(path)
            self.assertIsNotNone(record, f"generated target {path} is missing")
            self.assertEqual(record["role"], "generated-source", path)
            self.assertEqual(
                record["generation_state"], "declared-generated", path
            )
            self.assertEqual(record["content_policy"], "metadata-only", path)
        projection = by_path.get(
            f"{_GENERATED_CRATE}/src/conversation/projection_tests.rs"
        )
        self.assertIsNotNone(projection, "projection_tests.rs is missing")
        self.assertEqual(projection["generation_state"], "source-authored")
        self.assertEqual(projection["content_policy"], "metadata-only")
        lint_files = [
            r
            for r in snap.records
            if r["path"].startswith(f"{_GENERATED_CRATE}/fixtures/outbound_lint/")
        ]
        self.assertTrue(lint_files, "the outbound_lint fixture corpus is missing")
        for record in lint_files:
            if record["path"] == hard_rules:
                continue
            self.assertEqual(
                record["generation_state"], "source-authored", record["path"]
            )
            self.assertEqual(
                record["content_policy"], "metadata-only", record["path"]
            )

    def test_indexer_self_membership_and_excludes(self):
        # DESIGN.md §2: the catalog includes the indexer's own sources and
        # "explicitly includes itself so its own bytes are pinned by
        # catalog_sha256". Losing that membership would silently unpin the
        # registry, so it is asserted against the live scope rather than a
        # sample catalog.
        snap = self._scan("worktree-declared-inputs")
        by_path = {r["path"] for r in snap.records}
        for path in (
            "smoke/eval_harness/catalog.json",
            "smoke/eval_harness/README.md",
            "smoke/eval_harness/inventory.py",
            "smoke/eval_harness/contract.py",
            "smoke/eval_harness/paths.py",
        ):
            self.assertIn(path, by_path, f"{path} is not inventoried")
        for record in snap.records:
            path = record["path"]
            self.assertFalse(
                path.startswith(("smoke/eval_harness/site/",
                                 "smoke/eval_harness/report/")),
                f"excluded tree leaked into the scope: {path}",
            )
            self.assertNotIn("__pycache__", path, path)
            self.assertFalse(path.endswith(".pyc"), path)


class RealPlansDeclaredInputsScopeTest(unittest.TestCase):
    """The plans-declared-inputs scope against the LIVE plans repository.

    The synthetic sample catalog covers this scope's traversal rules; this
    class is what proves the declared plans sources actually resolve on
    this host, and that the required-entrypoint hard gate (DESIGN.md §8)
    can fire there.
    """

    def setUp(self):
        self.plans_root = _real_plans_root()
        if self.plans_root is None:
            self.skipTest("the plans repository is not available on this host")
        self.catalog = contract.load_catalog()
        self.ctx = contract.AdapterContext(
            roots={"plans": self.plans_root, "worktree": Path(".")},
            catalog=self.catalog,
        )
        corpus = [
            row
            for row in self.catalog["inventory_scopes"]
            if row["id"] == "plans-corpus"
        ][0]
        self.corpus_prefixes = [
            tree.rstrip("/") + "/" for tree in corpus["includes"]
        ]

    def _scan(self, catalog=None):
        ctx = self.ctx if catalog is None else contract.AdapterContext(
            roots=dict(self.ctx.roots), catalog=catalog
        )
        for _attempt in range(3):
            try:
                return inventory.inventory_scope(
                    ctx, "plans-declared-inputs"
                )
            except inventory.SourceChangedError:
                continue
        self.fail("the live plans corpus changed during every scan attempt")

    def _plans_declared_specs_outside_corpus(self):
        """Every component input declared under the plans root that this
        scope owns: no cross-root ``other:`` prefix, no glob, and not
        inside a plans-corpus tree (that scope's concern)."""
        specs = []
        for component in self.catalog["components"]:
            if component.get("root") != "plans":
                continue
            for spec in component.get("inputs", []):
                if not isinstance(spec, str) or "*" in spec:
                    continue
                if ":" in spec.split("/")[0]:
                    continue
                if any(spec.startswith(p) for p in self.corpus_prefixes):
                    continue
                specs.append((component["id"], spec))
        return specs

    def test_scope_is_plans_rooted_and_non_empty(self):
        snap = self._scan()
        self.assertTrue(snap.records, "plans-declared-inputs is empty")
        for record in snap.records:
            self.assertEqual(record["root"], "plans", record["path"])
            self.assertEqual(record["content_policy"], "metadata-only",
                             record["path"])

    def test_design_named_parity_repro_entrypoint_is_recorded(self):
        # DESIGN.md §1 item 10 names this plans-rooted entrypoint
        # explicitly; it is the only required source entrypoint this scope
        # enforces (the three formalism-linter ones sit inside the corpus
        # trees), so its absence would otherwise pass unnoticed.
        snap = self._scan()
        by_path = {r["path"] for r in snap.records}
        self.assertIn("codex-parity-smoke/bin/parity-repro", by_path)

    def test_every_declared_plans_input_resolves(self):
        snap = self._scan()
        recorded = sorted({r["path"] for r in snap.records})
        for component_id, spec in self._plans_declared_specs_outside_corpus():
            # A spec may name a directory (or an anchor whose contents are
            # what matters), so a recorded path equal to the spec or
            # beneath it satisfies the declaration.
            prefix = spec.rstrip("/") + "/"
            self.assertTrue(
                any(path == spec or path.startswith(prefix)
                    for path in recorded),
                f"component {component_id!r} declares plans input "
                f"{spec!r} which resolves to nothing in the scope",
            )

    def test_missing_required_plans_entrypoint_fails_the_scope(self):
        catalog = json.loads(json.dumps(self.catalog))
        target = next(
            component for component in catalog["components"]
            if component["id"] == "parity-repro"
        )
        real = next(
            entry for entry in target["entrypoints"]
            if entry.get("role") == "source"
            and entry.get("availability") == "required"
            and entry.get("root") == "plans"
        )
        bogus = "codex-parity-smoke/bin/ayl137-no-such-entrypoint"
        target["entrypoints"].append(dict(real, path=bogus))
        with self.assertRaises(contract.ContractError) as caught:
            self._scan(catalog)
        self.assertEqual(caught.exception.code, "missing-entrypoint")
        # The unedited catalog must resolve every real declared
        # entrypoint, otherwise this test would pass on an unrelated
        # missing path and prove nothing about the one it injected.
        self._scan()
        self.assertIn(bogus, str(caught.exception))


# ---------------------------------------------------------------------------
# inventory_all (the Task 3 interface: snapshots sorted by scope ID)
# ---------------------------------------------------------------------------


class InventoryAllTest(unittest.TestCase):
    def test_inventory_all_returns_snapshots_sorted_by_scope_id(self):
        catalog = json.loads(
            (_FIXTURES_DIR / "sample_catalog.json").read_text(encoding="utf-8")
        )
        ctx = contract.AdapterContext(
            roots={"plans": Path("/"), "worktree": Path("/")},
            catalog=catalog,
        )
        # each scope gets a distinct sentinel snapshot; the mapping back
        # to a scope goes through the sentinel's digest
        sentinels = {
            "plans-corpus": inventory.InventorySnapshot((), "1" * 64),
            "plans-declared-inputs": inventory.InventorySnapshot(
                (), "2" * 64
            ),
            "worktree-declared-inputs": inventory.InventorySnapshot(
                (), "3" * 64
            ),
        }
        with mock.patch.object(
            inventory, "inventory_scope", autospec=True
        ) as patched:
            patched.side_effect = lambda context, scope_id: sentinels[scope_id]
            snaps = inventory.inventory_all(ctx)
        self.assertEqual(
            [snap.sha256 for snap in snaps],
            ["1" * 64, "2" * 64, "3" * 64],
            "snapshots must be sorted by scope ID",
        )
        # and each scope was inventoried exactly once, in that order
        self.assertEqual(
            [call.args[1] for call in patched.call_args_list],
            [
                "plans-corpus",
                "plans-declared-inputs",
                "worktree-declared-inputs",
            ],
        )


class Round6MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-6 review findings (GLM R6 m-1/m-2):
    walk and catalog-walk failures fail loud with a typed error, never a
    silent skip."""

    def test_walk_files_unreadable_subtree_raises_source_changed(self):
        tree = Path(tempfile.mkdtemp(prefix="inv-r6-perm-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, tree, True)
        locked = tree / "locked"
        locked.mkdir()
        (locked / "inner.txt").write_bytes(b"x")
        os.chmod(locked, 0o000)
        try:
            with self.assertRaises(inventory.SourceChangedError):
                inventory._walk_files(tree, "")
        finally:
            os.chmod(locked, 0o755)

    def test_non_string_catalog_input_raises_contract_error(self):
        catalog = {
            "roots": [{"id": "worktree", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "wt-scope",
                    "root": "worktree",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "bad-inputs",
                    "kind": "tool",
                    "root": "worktree",
                    "adapter": "formalism",
                    "content_policy": "semantic-json",
                    "entrypoints": [],
                    "inputs": ["worktree:a.json", 12345],
                    "outputs": [],
                    "lifecycle": {"registry": None, "reason": "fixture"},
                    "install": None,
                    "owner_docs": [],
                }
            ],
        }
        root = Path(tempfile.mkdtemp(prefix="inv-r6-badcat-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        ctx = contract.AdapterContext(roots={"worktree": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("bad-inputs", str(caught.exception))

    def test_declared_specs_non_string_input_raises_too(self):
        """m-5 mirror: _declared_specs must not silently skip a
        non-string input item (the _CatalogIndex twin raises)."""
        catalog = {
            "components": [
                {
                    "id": "bad-inputs",
                    "kind": "tool",
                    "root": "worktree",
                    "adapter": "formalism",
                    "entrypoints": [],
                    "inputs": ["a.json", 12345],
                    "outputs": [],
                    "owner_docs": [],
                }
            ],
        }
        with self.assertRaises(contract.ContractError) as caught:
            inventory._declared_specs(catalog, "worktree")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_absolute_declared_input_crosses_root_and_raises(self):
        """M-4b: a declared input whose spec is absolute crosses the
        declared scope root (DESIGN §2) — typed error, never a host read."""
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "crossing",
                    "kind": "tool",
                    "root": "plans",
                    "adapter": "formalism",
                    "entrypoints": [],
                    "inputs": ["plans:/nonexistent-r7-probe/outside.txt"],
                    "outputs": [],
                    "owner_docs": [],
                }
            ],
        }
        root = Path(tempfile.mkdtemp(prefix="inv-r7-absroot-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_dotdot_declared_input_crosses_root_and_raises(self):
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "crossing",
                    "kind": "tool",
                    "root": "plans",
                    "adapter": "formalism",
                    "entrypoints": [],
                    "inputs": ["plans:parity/../../outside.txt"],
                    "outputs": [],
                    "owner_docs": [],
                }
            ],
        }
        root = Path(tempfile.mkdtemp(prefix="inv-r7-dotdot-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_absolute_entrypoint_path_crosses_root_and_raises(self):
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "crossing",
                    "kind": "tool",
                    "root": "plans",
                    "adapter": "formalism",
                    "entrypoints": [
                        {
                            "root": "plans",
                            "path": "/nonexistent-r7-probe/entry.txt",
                            "role": "source",
                            "availability": "required",
                        }
                    ],
                    "inputs": [],
                    "outputs": [],
                    "owner_docs": [],
                }
            ],
        }
        root = Path(tempfile.mkdtemp(prefix="inv-r7-absentry-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_absolute_scope_include_crosses_root_and_raises(self):
        """M-8b: inventory_scopes[].includes joins catalog data to the
        root — an absolute include must raise before any host read."""
        root = Path(tempfile.mkdtemp(prefix="inv-r8-include-root-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        outside = Path(tempfile.mkdtemp(prefix="inv-r8-include-out-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, outside, True)
        (outside / "secret.txt").write_bytes(b"outside")
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "recursive-all-regular-and-symlink",
                    "includes": [str(outside)],
                }
            ],
            "components": [],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_dotdot_scope_include_crosses_root_and_raises(self):
        base = Path(tempfile.mkdtemp(prefix="inv-r8-dotdot-base-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, base, True)
        root = base / "in"
        root.mkdir()
        outside = base / "out"
        outside.mkdir()
        (outside / "secret.txt").write_bytes(b"outside")
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "recursive-all-regular-and-symlink",
                    "includes": ["../out"],
                }
            ],
            "components": [],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_non_list_scope_includes_raises(self):
        root = Path(tempfile.mkdtemp(prefix="inv-r8-nlinc-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": "abc",
                }
            ],
            "components": [],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_non_string_scope_include_raises(self):
        root = Path(tempfile.mkdtemp(prefix="inv-r8-nsinc-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared", 42],
                }
            ],
            "components": [],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_non_string_entrypoint_path_raises(self):
        """Round-8 converged minor: a non-string entrypoint path must not
        silently skip (the round-7 m-5 class on the entrypoint origin)."""
        root = Path(tempfile.mkdtemp(prefix="inv-r8-nsep-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "bad-entrypoint",
                    "kind": "tool",
                    "root": "plans",
                    "adapter": "formalism",
                    "entrypoints": [
                        {
                            "root": "plans",
                            "path": 42,
                            "role": "source",
                            "availability": "required",
                        }
                    ],
                    "inputs": [],
                    "outputs": [],
                    "owner_docs": [],
                }
            ],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("bad-entrypoint", str(caught.exception))

    def test_non_string_owner_doc_path_raises(self):
        root = Path(tempfile.mkdtemp(prefix="inv-r8-nsod-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        catalog = {
            "roots": [{"id": "plans", "required_for": []}],
            "inventory_scopes": [
                {
                    "id": "plans-corpus",
                    "root": "plans",
                    "selection": "catalog-static-sources-plus-indexer-source",
                    "includes": ["catalog-declared"],
                }
            ],
            "components": [
                {
                    "id": "bad-owndoc",
                    "kind": "tool",
                    "root": "plans",
                    "adapter": "formalism",
                    "entrypoints": [],
                    "inputs": [],
                    "outputs": [],
                    "owner_docs": [{"root": "plans", "path": 42}],
                }
            ],
        }
        ctx = contract.AdapterContext(roots={"plans": root}, catalog=catalog)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(ctx, "plans-corpus")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("bad-owndoc", str(caught.exception))


class Round9MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-9 review findings (I-1..I-5):
    empty includes, unguarded excludes containers, cross-scope includes
    reads, non-list entrypoints/owner_docs containers, and NUL bytes in
    items all fail loud with malformed-catalog-input — never a raw
    escape, silent prune, or scope expansion."""

    def _catalog(self, scope=None, components=None, extra_scopes=()):
        scope = scope if scope is not None else {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        }
        components = components if components is not None else [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        return {
            "roots": [{"id": "worktree", "required_for": []}],
            "inventory_scopes": [scope, *extra_scopes],
            "components": components,
        }

    def _ctx(self, catalog):
        root = Path(tempfile.mkdtemp(prefix="inv-r9-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        return contract.AdapterContext(roots={"worktree": root}, catalog=catalog)

    def _declared_scope(self):
        return {
            "id": "declared-scope",
            "root": "worktree",
            "selection": "catalog-static-sources-outside-plans-corpus",
            "includes": ["catalog-declared"],
        }

    def _plans_corpus_scope(self, includes):
        return {
            "id": "plans-corpus",
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": includes,
        }

    def test_empty_include_raises_malformed(self):
        """I-1: an empty include currently resolves to the root itself
        and scans the WHOLE root (including .git/) with zero findings."""
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["includes"] = [""]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_whitespace_include_raises_malformed(self):
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["includes"] = ["   "]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_non_list_excludes_container_raises(self):
        """I-2: a string excludes container char-iterates into no-op
        glob patterns — silently, today."""
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["excludes"] = "plans"
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("wt-scope", str(caught.exception))

    def test_non_string_exclude_item_raises(self):
        """I-2: a non-string exclude is silently dropped today, which
        EXPANDS the scope toward machine-state files."""
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["excludes"] = [42]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("excludes[0]", str(caught.exception))

    def test_empty_exclude_item_raises(self):
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["excludes"] = [""]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_cross_scope_null_includes_raises(self):
        """I-3: a null includes value on the plans-corpus scope escapes
        as a raw TypeError from the cross-scope read today."""
        catalog = self._catalog(
            scope=self._declared_scope(),
            extra_scopes=[self._plans_corpus_scope(None)],
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("plans-corpus", str(caught.exception))

    def test_cross_scope_string_includes_raises(self):
        """I-3: a string includes container char-iterates into per-char
        prefixes that silently prune the candidate set today."""
        catalog = self._catalog(
            scope=self._declared_scope(),
            extra_scopes=[self._plans_corpus_scope("plans")],
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("plans-corpus", str(caught.exception))

    def test_non_list_entrypoints_container_raises(self):
        """I-4: a string entrypoints container char-iterates into
        non-dict items and silently bypasses the required-entrypoint
        hard gate today."""
        components = [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": "plans/required.md",
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        catalog = self._catalog(components=components)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("c1", str(caught.exception))

    def test_non_list_owner_docs_container_raises(self):
        """I-4: an int owner_docs container is not iterable at all —
        raw TypeError today."""
        components = [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": [],
                "outputs": [],
                "owner_docs": 42,
            }
        ]
        catalog = self._catalog(
            scope=self._declared_scope(), components=components
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("c1", str(caught.exception))

    def test_null_byte_in_include_raises(self):
        """I-5: a NUL byte passes the rooted-spec gate and escapes as
        a raw ValueError from lstat today."""
        catalog = self._catalog()
        catalog["inventory_scopes"][0]["includes"] = ["plans\x00corpus"]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_null_byte_in_declared_input_raises(self):
        components = [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": ["worktree:a\x00b.json"],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        catalog = self._catalog(
            scope=self._declared_scope(), components=components
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_catalog_index_non_list_entrypoints_raises(self):
        """I-4 (direct unit): the _CatalogIndex iteration site must
        guard the container, not just the per-entry shape."""
        components = [
            {
                "id": "bad-ep",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": 42,
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        catalog = self._catalog(components=components)
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("bad-ep", str(caught.exception))

    def test_catalog_index_non_list_curated_sources_raises(self):
        """Same container-discipline class (coordinator-proactive
        closure): a non-list curated_sources container must not
        char-iterate into a silent empty curated set."""
        catalog = self._catalog()
        catalog["curated_sources"] = "plans/EXEMPLARS.md"
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")


class Round10MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-10 review findings: the two
    remaining unguarded top-level catalog containers (``components``,
    ``inventory_scopes``) and the residual NUL-byte channel in
    ``_CatalogIndex``/``_check_required_entrypoints`` all fail loud with
    malformed-catalog-input — never a raw escape, silent empty
    inventory, or silent scope expansion."""

    def _catalog(self, scope=None, components=None):
        scope = scope if scope is not None else {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        }
        components = components if components is not None else [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        return {
            "roots": [{"id": "worktree", "required_for": []}],
            "inventory_scopes": [scope],
            "components": components,
        }

    def _ctx(self, catalog):
        root = Path(tempfile.mkdtemp(prefix="inv-r10-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        return contract.AdapterContext(roots={"worktree": root}, catalog=catalog)

    def test_components_null_container_raises(self):
        """MAJOR-2: a null components container escapes as a raw
        TypeError from _CatalogIndex today."""
        catalog = self._catalog()
        catalog["components"] = None
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components", str(caught.exception))

    def test_components_string_container_raises(self):
        """MAJOR-2: a string components container char-iterates into
        non-dict skips — the scope silently inventories zero records
        with zero findings and the required-entrypoint gate is
        vacuously satisfied."""
        catalog = self._catalog(components="c1")
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components", str(caught.exception))

    def test_components_non_dict_item_raises(self):
        """MAJOR-2: a non-dict row among components is silently skipped
        today (declared data vanishes, zero findings)."""
        catalog = self._catalog(components=[42])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_inventory_scopes_null_raises(self):
        """MAJOR-3: a null inventory_scopes container escapes as a raw
        TypeError from the inventory_scope lookup today."""
        catalog = self._catalog()
        catalog["inventory_scopes"] = None
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes", str(caught.exception))

    def test_inventory_scopes_string_inventory_all_raises(self):
        """MAJOR-3: a string inventory_scopes container makes
        inventory_all silently return an empty tuple today — the
        entire inventory vanishes with zero findings."""
        catalog = self._catalog()
        catalog["inventory_scopes"] = "wt-scope"
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_all(self._ctx(catalog))
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes", str(caught.exception))

    def test_inventory_scopes_non_dict_row_raises(self):
        """MAJOR-3 (concordance probe): a non-dict row among the scope
        rows is conflated with an absent scope — the cross-scope
        plans-corpus pruning is silently disabled (scope expansion)."""
        catalog = self._catalog()
        catalog["inventory_scopes"] = [
            catalog["inventory_scopes"][0],
            42,
        ]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes[1]", str(caught.exception))

    def test_catalog_index_nul_input_raises(self):
        """MINOR-1 (NUL class closure): a NUL byte in an input item is
        inert in _CatalogIndex today (no typed check at this site)."""
        catalog = self._catalog(components=[
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": ["worktree:a\x00b.json"],
                "outputs": [],
                "owner_docs": [],
            }
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("NUL", str(caught.exception))

    def test_required_entrypoint_nul_path_raises(self):
        """MINOR-1 (NUL class closure): a NUL byte in a required
        entrypoint path on a recursive scope passes silently today —
        the NUL path never matches an include prefix, so the missing-
        entrypoint gate simply does not fire."""
        catalog = self._catalog(components=[
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [{
                    "role": "source",
                    "availability": "required",
                    "root": "worktree",
                    "path": "plans\x00x.md",
                }],
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("NUL", str(caught.exception))


class Round11MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-11 review findings: malformed
    row-identifying ``id`` fields in ``components``/``inventory_scopes``
    (conflation with an ABSENT row bypasses the DESIGN §8 hard gates and
    silently vanishes whole scopes) and non-dict item rows in the
    ``entrypoints``/``owner_docs``/``curated_sources`` item containers
    (silent skip / gate vacuity at five sites) all fail loud with
    malformed-catalog-input."""

    def _catalog(self, scope=None, components=None):
        scope = scope if scope is not None else {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        }
        components = components if components is not None else [
            {
                "id": "c1",
                "kind": "corpus",
                "root": "worktree",
                "adapter": "formalism",
                "entrypoints": [],
                "inputs": [],
                "outputs": [],
                "owner_docs": [],
            }
        ]
        return {
            "roots": [{"id": "worktree", "required_for": []}],
            "inventory_scopes": [scope],
            "components": components,
        }

    def _ctx(self, catalog):
        root = Path(tempfile.mkdtemp(prefix="inv-r11-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        return contract.AdapterContext(roots={"worktree": root}, catalog=catalog)

    def test_components_row_non_string_id_raises(self):
        """M-2: a non-string row id is conflated with an ABSENT linter
        row — the generated-target origin check never fires and the
        DESIGN §8 hard missing-generated-target gate is bypassed."""
        catalog = self._catalog(components=[{
            "id": 42,
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_components_row_missing_id_raises(self):
        catalog = self._catalog(components=[{
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_components_row_empty_id_raises(self):
        catalog = self._catalog(components=[{
            "id": "",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_scope_row_non_string_id_raises(self):
        """M-3: a non-string scope id is conflated with an absent scope
        in the id lookup (unknown-scope) — a malformed row must be
        loud at the container, not misrouted."""
        catalog = self._catalog(scope={
            "id": 42,
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        })
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes[0]", str(caught.exception))

    def test_inventory_all_missing_scope_id_raises(self):
        """M-3: a missing scope id made inventory_all silently drop the
        whole scope (zero findings) while inventory_scope was loud for
        identical bytes."""
        catalog = self._catalog()
        del catalog["inventory_scopes"][0]["id"]
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_all(self._ctx(catalog))
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes[0]", str(caught.exception))

    def test_scope_row_sensitive_id_raises_without_echo(self):
        """M-3: a raw-key-shaped scope id would land verbatim in
        record["scope"] of every record — rejected like any other
        sensitive catalog value, and the message must not echo it."""
        raw = "sk-externalkey12345678901"
        catalog = self._catalog(scope={
            "id": raw,
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        })
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), raw)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes[0]", str(caught.exception))
        self.assertNotIn(raw, str(caught.exception))

    def test_required_entrypoint_gate_vacuity_non_dict_entry_raises(self):
        """MAJOR-1 (N1): replacing a required entrypoint row with a
        non-dict item makes the missing-entrypoint gate vacuous — the
        mangled entry vanishes and the scope inventories with zero
        findings."""
        components = [{
            "id": "c1",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [42],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }]
        catalog = self._catalog(
            scope={
                "id": "declared-scope",
                "root": "worktree",
                "selection": "catalog-static-sources-plus-indexer-source",
                "includes": ["catalog-declared"],
            },
            components=components,
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    def test_non_dict_owner_doc_item_raises(self):
        """MAJOR-1 (N2): a non-dict owner-doc item is silently skipped
        today (zero findings)."""
        components = [{
            "id": "c1",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [42],
        }]
        catalog = self._catalog(
            scope={
                "id": "declared-scope",
                "root": "worktree",
                "selection": "catalog-static-sources-plus-indexer-source",
                "includes": ["catalog-declared"],
            },
            components=components,
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "declared-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("owner_docs[0]", str(caught.exception))

    def test_catalog_index_non_dict_curated_item_raises(self):
        """MAJOR-1 (N3): a non-dict curated item is silently skipped in
        _CatalogIndex today — asymmetric with the formalism adapter,
        which hard-fails the identical shape."""
        catalog = self._catalog()
        catalog["curated_sources"] = [42]
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("curated_sources[0]", str(caught.exception))

    def test_catalog_index_non_dict_entrypoint_item_raises(self):
        """MAJOR-1 (N4): a non-dict entrypoint item silently degrades
        the role of the file it would have claimed today."""
        components = [{
            "id": "c1",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [42],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }]
        catalog = self._catalog(components=components)
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    def test_check_required_entrypoints_non_dict_entry_raises(self):
        """MAJOR-1 (N5, direct unit): the guard must also hold at the
        _check_required_entrypoints site (defense in depth; the
        _CatalogIndex guard reaches it first in the end-to-end flow)."""
        components = [{
            "id": "c1",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [
                {
                    "role": "source",
                    "availability": "required",
                    "root": "worktree",
                    "path": "plans/README.md",
                },
                42,
            ],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }]
        catalog = self._catalog(components=components)
        scope = catalog["inventory_scopes"][0]
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_required_entrypoints(
                catalog, scope, ["plans/README.md"]
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[1]", str(caught.exception))


class Round12MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-12 review findings: mangled
    entrypoint discriminator VALUES (role/availability) and item fields
    (root/path) silently vacate the DESIGN §8 required-entrypoint hard
    gate and drop curated designations; empty/``.``-leading/empty-segment
    bare specs expand a scope to the whole declared root including
    ``.git/``; NUL-tainted row ids bypass the row-id guards (generated-
    target gate bypass, NUL riding into record["scope"]). All must fail
    loud with malformed-catalog-input, never echoing the bad value."""

    def _scope(self, **over):
        scope = {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "recursive-all-regular-and-symlink",
            "includes": ["plans"],
            "excludes": [],
        }
        scope.update(over)
        return scope

    def _component(self, **over):
        comp = {
            "id": "c1",
            "kind": "corpus",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }
        comp.update(over)
        return comp

    def _catalog(self, scope=None, components=None):
        return {
            "roots": [{"id": "worktree", "required_for": []}],
            "inventory_scopes": [scope if scope is not None else self._scope()],
            "components": (
                components if components is not None else [self._component()]
            ),
        }

    def _ctx(self, catalog):
        root = Path(tempfile.mkdtemp(prefix="inv-r12-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        (root / ".git").mkdir()
        (root / ".git" / "HEAD").write_bytes(b"ref: refs/heads/main\n")
        return contract.AdapterContext(roots={"worktree": root}, catalog=catalog)

    def _entry(self, **over):
        entry = {
            "role": "source",
            "availability": "required",
            "root": "worktree",
            "path": "plans/MISSING-R12.md",
        }
        entry.update(over)
        return entry

    # -- F-2: entrypoint discriminator values vacate the §8 gate --------------

    def test_entrypoint_bad_availability_value_raises(self):
        """GLM M-2: a REQUIRED source entrypoint (file absent) with
        availability=42 behaved as optional — hard gate vacuously
        satisfied, zero findings."""
        catalog = self._catalog(components=[
            self._component(entrypoints=[self._entry(availability=42)]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    def test_entrypoint_bad_role_value_raises(self):
        catalog = self._catalog(components=[
            self._component(entrypoints=[self._entry(role="REQUIRED")]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    def test_entrypoint_availability_key_absent_raises(self):
        entry = self._entry()
        del entry["availability"]
        catalog = self._catalog(components=[
            self._component(entrypoints=[entry]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_entrypoint_in_enum_values_pass_filter(self):
        """Discriminators naming in-enum non-source/non-required values
        stay the legitimate silent filter path (no raise)."""
        catalog = self._catalog(components=[
            self._component(entrypoints=[
                self._entry(role="selftest", availability="optional",
                            path="plans/README.md"),
                self._entry(role="installed", availability="health-only",
                            path="plans/README.md"),
            ]),
        ])
        snap = inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertTrue(snap.records)

    def test_check_required_entrypoints_non_string_path_raises(self):
        """Qwen-conc M-1 (direct unit): the gate site must reject a
        non-string path on a required source row rather than skipping
        it vacuously."""
        valid = self._entry(path="plans/README.md")
        mangled = self._entry(path=42)
        catalog = self._catalog(components=[
            self._component(entrypoints=[valid, mangled]),
        ])
        scope = catalog["inventory_scopes"][0]
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_required_entrypoints(
                catalog, scope, ["plans/README.md"]
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[1]", str(caught.exception))

    def test_entrypoint_path_channel_symmetry(self):
        """Qwen-conc M-1 (end-to-end): the identical mangle is loud on
        the recursive channel too (was: silent — _declared_specs, which
        raises, never runs for a recursive scope)."""
        catalog = self._catalog(components=[
            self._component(entrypoints=[self._entry(path=None)]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_entrypoint_empty_path_raises(self):
        catalog = self._catalog(components=[
            self._component(entrypoints=[self._entry(path="")]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    # -- F-3: empty/dot bare specs expand the scope to the whole root ---------

    def test_declared_input_empty_spec_raises(self):
        static = self._scope(
            selection="catalog-static-sources-plus-indexer-source",
            includes=["catalog-declared"],
        )
        catalog = self._catalog(
            scope=static,
            components=[self._component(inputs=[""])],
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_declared_input_dot_spec_raises(self):
        static = self._scope(
            selection="catalog-static-sources-plus-indexer-source",
            includes=["catalog-declared"],
        )
        catalog = self._catalog(
            scope=static,
            components=[self._component(inputs=["."])],
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_declared_input_empty_segment_raises(self):
        static = self._scope(
            selection="catalog-static-sources-plus-indexer-source",
            includes=["catalog-declared"],
        )
        catalog = self._catalog(
            scope=static,
            components=[self._component(inputs=["plans//README.md"])],
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_recursive_include_dot_raises(self):
        """Qwen-conc M-2: includes=["."] lstat the root directory and
        walk the WHOLE root including .git/ — malformed shape."""
        catalog = self._catalog(
            scope=self._scope(includes=["."]),
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_scope_exclude_dot_shape_raises(self):
        """Coordinator-proactive (round-9 I-2 family): a "."-shaped
        exclude matches no record rel — a silently no-op exclusion
        expands the scope toward machine-state files."""
        catalog = self._catalog(
            scope=self._scope(includes=["plans"], excludes=["."]),
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    # -- F-4: field-mangled rows silently dropped in _CatalogIndex ------------

    def test_catalog_index_curated_non_string_root_raises(self):
        """Qwen-conc M-3: the formalism adapter hard-fails this shape;
        the silent skip flipped record content policy with zero
        findings."""
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(
                {"curated_sources": [{"root": 42, "path": "x.md"}]}
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("curated_sources[0]", str(caught.exception))

    def test_catalog_index_curated_empty_path_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(
                {"curated_sources": [{"root": "plans", "path": ""}]}
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("curated_sources[0]", str(caught.exception))

    def test_catalog_index_entrypoint_non_string_root_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex({"components": [
                self._component(entrypoints=[self._entry(root=42)]),
            ]})
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    # -- F-5: NUL-tainted row ids bypass the row-id guards --------------------

    def test_component_id_nul_raises(self):
        """Qwen-conc M-4: "formalism-linter\\x00" is conflated with an
        absent linter row — the §8 missing-generated-target gate is
        bypassed (round-9 I-5 NUL discipline, row-id channel)."""
        catalog = self._catalog(components=[
            self._component(id="formalism-linter\x00"),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_scope_id_nul_raises(self):
        """Qwen-conc M-4: an NUL in a scope id rides verbatim into
        record["scope"] — serialized into the canonical output."""
        catalog = self._catalog(scope=self._scope(id="wt\x00"))
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_all(self._ctx(catalog))
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inventory_scopes[0]", str(caught.exception))


class Round13MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-13 review findings (converged
    across three seats): root identifiers and component-row discriminator
    fields (``root``/``kind``/``content_policy``) were type-only or
    unguarded — a mangled value was conflated with a legitimate
    cross-root/other-policy skip, silently dropping declared records,
    flipping record content policy, or vacating the DESIGN §8 hard gates;
    the excludes shape gate missed ``..`` segments (a no-op exclusion);
    and the cross-scope plans-corpus include reads skipped the shape gate
    (a mangled prune prefix silently expanded the scope). All must fail
    loud with malformed-catalog-input, never echoing the bad value."""

    def _scope(self, **over):
        scope = {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "catalog-static-sources-plus-indexer-source",
            "includes": ["catalog-declared"],
        }
        scope.update(over)
        return scope

    def _component(self, **over):
        comp = {
            "id": "c1",
            "kind": "tool",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }
        comp.update(over)
        return comp

    def _catalog(self, scopes=None, components=None):
        return {
            "roots": [
                {"id": "worktree", "required_for": []},
                {"id": "plans", "required_for": []},
            ],
            "inventory_scopes": (
                scopes if scopes is not None else [self._scope()]
            ),
            "components": (
                components if components is not None
                else [self._component()]
            ),
        }

    def _ctx(self, catalog):
        root = Path(tempfile.mkdtemp(prefix="inv-r13-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        (root / "docs").mkdir()
        (root / "docs" / "owner.md").write_bytes(b"# owner\n")
        return contract.AdapterContext(roots={"worktree": root}, catalog=catalog)

    def _scope_call(self, catalog):
        return inventory.inventory_scope(self._ctx(catalog), "wt-scope")

    # -- G-1: entrypoint root membership --------------------------------------

    def test_entrypoint_root_empty_raises(self):
        """M-1 (3 seats): root="" on a REQUIRED source row is swallowed by
        the cross-root filter — the §8 gate is vacated."""
        entry = {
            "role": "source", "availability": "required",
            "root": "", "path": "plans/MISSING-R13.md",
        }
        catalog = self._catalog(components=[
            self._component(entrypoints=[entry]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("entrypoints[0]", str(caught.exception))

    def test_entrypoint_root_nul_raises(self):
        entry = {
            "role": "source", "availability": "required",
            "root": "worktree\x00", "path": "plans/MISSING-R13.md",
        }
        catalog = self._catalog(components=[
            self._component(entrypoints=[entry]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_entrypoint_root_unknown_raises(self):
        """GLM M-1: a hashable-but-unknown root (schema-closed enum) must
        not be conflated with the declared-but-other-root skip."""
        entry = {
            "role": "source", "availability": "required",
            "root": "plans2", "path": "plans/MISSING-R13.md",
        }
        catalog = self._catalog(components=[
            self._component(entrypoints=[entry]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_entrypoint_declared_other_root_stays_silent(self):
        """Over-correction guard: a declared-but-other-root required row
        stays the legitimate silent cross-scope skip."""
        entry = {
            "role": "source", "availability": "required",
            "root": "plans", "path": "plans/MISSING-R13.md",
        }
        catalog = self._catalog(components=[
            self._component(entrypoints=[entry]),
        ])
        snap = self._scope_call(catalog)
        self.assertIsNotNone(snap)

    # -- G-1: component-row root/kind/content_policy ---------------------------

    def _assert_component_gate(self, comp_over, needle=None):
        # comp_over is a FULLY BUILT component dict (mutated or built via
        # self._component(**overrides)); passing overrides through the
        # builder again would re-supply deleted defaults.
        catalog = self._catalog(components=[dict(comp_over)])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        if needle is not None:
            self.assertIn(needle, str(caught.exception))

    def test_component_root_empty_raises(self):
        """M-2 (3 seats): root="" silently drops the row's declared
        specs AND flips their content policy."""
        catalog = self._catalog(components=[self._component(
            id="parity-formalism", root="",
            inputs=["plans/README.md"],
            content_policy="semantic-json",
        )])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("components[0]", str(caught.exception))

    def test_component_root_unknown_raises(self):
        self._assert_component_gate(
            self._component(root="nope-root"), "components[0]"
        )

    def test_component_root_missing_raises(self):
        comp = self._component()
        del comp["root"]
        self._assert_component_gate(comp, "components[0]")

    def test_component_root_nul_raises(self):
        self._assert_component_gate(self._component(root="worktree\x00"))

    def test_component_kind_int_raises(self):
        """QC m-1 (riding the pass): a mangled kind flips the record
        role launcher -> source-code with zero findings."""
        self._assert_component_gate(
            self._component(id="dogfood", kind=42), "components[0]"
        )

    def test_component_kind_non_slug_raises(self):
        self._assert_component_gate(self._component(kind="Not A Slug"))

    def test_component_kind_missing_raises(self):
        comp = self._component()
        del comp["kind"]
        self._assert_component_gate(comp, "components[0]")

    def test_component_content_policy_mangled_raises(self):
        """M-3 (QC): present-but-mangled content_policy flips live
        records to metadata-only — the F-4 e2e class on the component
        channel. Absent keeps the default (18 existing tests omit it)."""
        self._assert_component_gate(self._component(content_policy=42))

    def test_content_policy_absent_flows(self):
        """Over-correction guard: absent content_policy keeps the
        legitimate metadata-only default (existing tests depend on it)."""
        comp = self._component(inputs=["plans/README.md"])
        catalog = self._catalog(components=[comp])
        snap = self._scope_call(catalog)
        self.assertTrue(snap.records)

    # -- G-1: rooted input spec_root and owner-doc root ------------------------

    def test_rooted_input_unknown_spec_root_raises(self):
        """GLM M-1: "worktree2:..." silently skipped the §8 generated
        target — unknown rooted-spec roots must fail loud."""
        catalog = self._catalog(components=[
            self._component(inputs=["worktree2:docs/owner.md"]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("inputs[0]", str(caught.exception))

    def test_rooted_input_empty_spec_root_raises(self):
        """"":x" (empty spec_root) is malformed, not a silent skip."""
        catalog = self._catalog(components=[
            self._component(inputs=[":docs/owner.md"]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_rooted_input_declared_other_root_stays_silent(self):
        """Over-correction guard: plans-rooted spec in a worktree scope
        stays the legitimate silent skip."""
        catalog = self._catalog(components=[
            self._component(inputs=["plans:plans/README.md"]),
        ])
        snap = self._scope_call(catalog)
        self.assertIsNotNone(snap)

    def test_owner_doc_root_mangled_raises(self):
        """QC m-1: owner-doc root=42 conflated with the cross-root skip —
        the record silently vanished (soft channel)."""
        catalog = self._catalog(components=[
            self._component(owner_docs=[
                {"root": 42, "path": "docs/owner.md", "heading": "h"},
            ]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("owner_docs[0]", str(caught.exception))

    # -- G-1: curated root membership ------------------------------------------

    def test_curated_root_unknown_raises(self):
        """GLM M-1: root="plans2" silently dropped the designation — the
        record flipped curated-text -> metadata-only (identical bytes
        hard-fail in the formalism adapter)."""
        with self.assertRaises(contract.ContractError) as caught:
            inventory._CatalogIndex(
                {"curated_sources": [{"root": "plans2", "path": "x.md"}]}
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertIn("curated_sources[0]", str(caught.exception))

    # -- G-2: excludes parent segments ------------------------------------------

    def test_exclude_parent_segment_raises(self):
        """GLM M-2: a ".."-shaped exclusion matches no record rel — the
        machine-state protection silently no-ops (includes side raises)."""
        catalog = self._catalog(
            scopes=[self._scope(
                selection="recursive-all-regular-and-symlink",
                includes=["plans"], excludes=["../plans/**"],
            )],
        )
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    # -- G-3: cross-scope include shape gate ------------------------------------

    def test_cross_scope_include_shape_raises(self):
        """Workhorse M-3: plans-corpus includes reach the prune-prefix
        reads of OTHER scopes without _check_rooted_spec — a mangled item
        disabled pruning and duplicated 167 corpus records with zero
        findings."""
        scopes = [
            {
                "id": "plans-corpus",
                "root": "worktree",
                "selection": "recursive-all-regular-and-symlink",
                "includes": ["/abs/outside"],
                "excludes": [],
            },
            {
                "id": "outside-scope",
                "root": "worktree",
                "selection": (
                    "catalog-static-sources-outside-plans-corpus"
                ),
                "includes": ["catalog-declared"],
            },
        ]
        catalog = self._catalog(scopes=scopes)
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(self._ctx(catalog), "outside-scope")
        self.assertEqual(caught.exception.code, "scope-crossing-input")


class Round14MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-14 review findings (GLM seat
    M-1/M-2, workhorse seat M-1). The ``curated_sources[].path`` channel
    skipped the shared shape gate entirely — a mangle silently lost the
    designation (curated-text -> metadata-only, zero findings). The
    shared shape predicates only rejected a DOT-LEADING segment, so an
    interior/trailing ``.`` segment survived and defeated the round-13
    plans-corpus prune (166 corpus records leaked with zero findings)
    and silently no-oped excludes. And the scope row's own ``root``
    flowed verbatim into ``record["root"]`` and the scope digest with no
    sensitive scan or NUL rejection — the K-3/F-5 discipline on the
    sibling field of the same row. All must fail loud
    (malformed-catalog-input / scope-crossing-input as the channel
    dictates), never echoing the bad value."""

    def _scope(self, **over):
        scope = {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "catalog-static-sources-plus-indexer-source",
            "includes": ["catalog-declared"],
        }
        scope.update(over)
        return scope

    def _corpus_scope(self, includes):
        return {
            "id": "plans-corpus",
            "root": "plans",
            "selection": "recursive-all-regular-and-symlink",
            "includes": includes,
        }

    def _outside_scope(self):
        return {
            "id": "outside-scope",
            "root": "worktree",
            "selection": (
                "catalog-static-sources-outside-plans-corpus"
            ),
            "includes": ["catalog-declared"],
        }

    def _component(self, **over):
        comp = {
            "id": "c1",
            "kind": "tool",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }
        comp.update(over)
        return comp

    def _catalog(self, scopes=None, components=None, curated=None):
        catalog = {
            "roots": [
                {"id": "worktree", "required_for": []},
                {"id": "plans", "required_for": []},
            ],
            "inventory_scopes": (
                scopes if scopes is not None else [self._scope()]
            ),
            "components": (
                components if components is not None
                else [self._component()]
            ),
        }
        if curated is not None:
            catalog["curated_sources"] = curated
        return catalog

    def _tree(self):
        root = Path(tempfile.mkdtemp(prefix="inv-r14-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        (root / "docs").mkdir()
        (root / "docs" / "owner.md").write_bytes(b"# owner\n")
        return root

    def _ctx(self, catalog):
        return contract.AdapterContext(
            roots={"worktree": self._tree()}, catalog=catalog
        )

    def _ctx_keyed(self, catalog, root_key):
        return contract.AdapterContext(
            roots={root_key: self._tree()}, catalog=catalog
        )

    def _scope_call(self, catalog, scope_id="wt-scope"):
        return inventory.inventory_scope(self._ctx(catalog), scope_id)

    # -- H-1: curated_sources path shape gate -------------------------------

    def _curated_index_call(self, path):
        catalog = self._catalog(curated=[
            {"root": "worktree", "path": path, "selectors": ["# h"]},
        ])
        return inventory._CatalogIndex(catalog)

    def test_curated_path_dotlead_raises(self):
        """GLM M-1: './x' on the one ungated path channel silently lost
        the designation (curated-text -> metadata-only)."""
        with self.assertRaises(contract.ContractError) as caught:
            self._curated_index_call("./docs/owner.md")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_curated_path_absolute_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            self._curated_index_call("/abs/docs/owner.md")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_curated_path_parent_segment_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            self._curated_index_call("../outside/owner.md")
        self.assertEqual(caught.exception.code, "scope-crossing-input")

    def test_curated_path_empty_segment_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            self._curated_index_call("docs//owner.md")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_curated_path_interior_dot_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            self._curated_index_call("docs/./owner.md")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_curated_clean_path_still_designates(self):
        """Over-correction guard: the legitimate designation still
        flows after the gate."""
        catalog = self._catalog(
            components=[self._component(inputs=["docs/owner.md"])],
            curated=[
                {"root": "worktree", "path": "docs/owner.md",
                 "selectors": ["# owner"]},
            ],
        )
        snap = self._scope_call(catalog)
        record = next(r for r in snap.records if r["path"] == "docs/owner.md")
        self.assertEqual(record["content_policy"], "curated-text")
        self.assertEqual(record["role"], "curated-document")

    # -- H-2: interior/trailing '.' segments in the shared predicates -------

    def test_shape_interior_dot_raises(self):
        """GLM M-2: only a DOT-LEADING '.' was rejected; 'a/./b' survived
        every consumer of the shared gate."""
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_rooted_spec("docs/./owner.md", "plans", "probe")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_shape_trailing_dot_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_rooted_spec("parity-formalism/.", "plans", "probe")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_shape_leading_dot_still_raises(self):
        """Guard: the round-12 leading-dot case keeps raising."""
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_rooted_spec("./docs/owner.md", "plans", "probe")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_shape_substring_globs_still_flow(self):
        """Over-correction guards: '.'-substring names, dotfile globs and
        '**' are legal and must keep flowing (round-8 MINOR-1)."""
        for spec in ("a..b/c", "a/..b/c", ".hidden/x", "a..b/**", "**",
                     "trailing.dot."):
            inventory._check_rooted_spec(spec, "plans", "probe")

    def test_excludes_interior_dot_raises(self):
        """GLM M-2: one '/./' rewrite turned a live machine-state
        exclusion into a silent no-op."""
        with self.assertRaises(contract.ContractError) as caught:
            inventory._validated_scope_excludes(
                ["plans/./README.md"], "wt-scope"
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_excludes_trailing_dot_raises(self):
        with self.assertRaises(contract.ContractError) as caught:
            inventory._validated_scope_excludes(["plans/."], "wt-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_excludes_substring_dotfile_patterns_still_flow(self):
        """Over-correction guard: substring and dotfile excludes are
        legitimate patterns (round-12/13 adjudications)."""
        patterns = ["a..b/**", ".hidden/**", "docs/**", "trailing.dot./**"]
        self.assertEqual(
            inventory._validated_scope_excludes(patterns, "wt-scope"),
            patterns,
        )

    def test_prune_include_interior_dot_raises_site1(self):
        """GLM M-2 / probe A: an interior-dot corpus include survived the
        round-13 site-1 gate and duplicated 166 corpus records with zero
        findings (baseline scope held at 18)."""
        scopes = [
            self._corpus_scope(["parity-formalism/."]),
            self._outside_scope(),
        ]
        catalog = self._catalog(
            scopes=scopes,
            components=[self._component(inputs=["docs/owner.md"])],
        )
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog, "outside-scope")
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_prune_include_interior_dot_raises_site2(self):
        """Site 2 is pre-empted by site 1 on the public path — probe the
        gate directly at `_check_required_entrypoints`."""
        scopes = [
            self._corpus_scope(["parity-formalism/."]),
            self._outside_scope(),
        ]
        catalog = self._catalog(scopes=scopes)
        with self.assertRaises(contract.ContractError) as caught:
            inventory._check_required_entrypoints(
                catalog, scopes[1], []
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    # -- H-3: scope-row root sensitive/NUL discipline ------------------------

    def test_scope_root_sensitive_raises(self):
        """WH M-1: the scope root flows verbatim into record["root"] and
        the scope digest — K-3 parity on the sibling field of the same
        row demands the sensitive scan, value never echoed."""
        root_key = "sk-rawkey0123456789abcd"
        catalog = self._catalog(scopes=[self._scope(root=root_key)])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(
                self._ctx_keyed(catalog, root_key), "wt-scope"
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertNotIn(root_key, str(caught.exception))

    def test_scope_root_canary_raises(self):
        catalog = self._catalog(
            scopes=[self._scope(root=contract.CANARY_TOKEN)]
        )
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(
                self._ctx_keyed(catalog, contract.CANARY_TOKEN), "wt-scope"
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")
        self.assertNotIn("DO-NOT-LEAK", str(caught.exception))

    def test_scope_root_nul_raises(self):
        root_key = "worktree\x00"
        catalog = self._catalog(scopes=[self._scope(root=root_key)])
        with self.assertRaises(contract.ContractError) as caught:
            inventory.inventory_scope(
                self._ctx_keyed(catalog, root_key), "wt-scope"
            )
        self.assertEqual(caught.exception.code, "malformed-catalog-input")

    def test_scope_root_unsupplied_still_missing_root(self):
        """Guards: the existing typed `missing-root` outcomes for
        non-string and unsupplied roots are untouched — the new gate
        covers string values only, and `plans2`-style unknown-but-clean
        roots stay in the existing channel."""
        for bad_root in (42, "outside", "plans2", ""):
            catalog = self._catalog(scopes=[self._scope(root=bad_root)])
            with self.assertRaises(contract.ContractError) as caught:
                self._scope_call(catalog)
            self.assertEqual(caught.exception.code, "missing-root")

    def test_scope_root_clean_still_flows(self):
        """Guard: the legitimate clean-root flow is unchanged."""
        snap = self._scope_call(
            self._catalog(components=[
                self._component(inputs=["docs/owner.md"])
            ])
        )
        self.assertEqual(
            {r["root"] for r in snap.records}, {"worktree"}
        )


class Round15MirrorFixTest(unittest.TestCase):
    """Inventory-side mirror of the round-15 GLM finding: the shared
    shape predicates rejected only an EMPTY/WHITESPACE-ONLY body, so a
    PADDED or internal-space spec survived — while the schema's
    path-token character class (`[A-Za-z0-9._*%-]`) forbids whitespace
    anywhere. A padded curated path silently flipped a live record
    curated-text -> metadata-only (zero findings); a padded exclude
    silently no-oped (round-13 M-2 mechanism). A disk file NAMED with
    spaces is a different matter: record paths are disk-derived and
    must keep flowing untouched."""

    def _scope(self, **over):
        scope = {
            "id": "wt-scope",
            "root": "worktree",
            "selection": "catalog-static-sources-plus-indexer-source",
            "includes": ["catalog-declared"],
        }
        scope.update(over)
        return scope

    def _component(self, **over):
        comp = {
            "id": "c1",
            "kind": "tool",
            "root": "worktree",
            "adapter": "formalism",
            "entrypoints": [],
            "inputs": [],
            "outputs": [],
            "owner_docs": [],
        }
        comp.update(over)
        return comp

    def _catalog(self, scopes=None, components=None, curated=None):
        catalog = {
            "roots": [
                {"id": "worktree", "required_for": []},
                {"id": "plans", "required_for": []},
            ],
            "inventory_scopes": (
                scopes if scopes is not None else [self._scope()]
            ),
            "components": (
                components if components is not None
                else [self._component()]
            ),
        }
        if curated is not None:
            catalog["curated_sources"] = curated
        return catalog

    def _tree(self):
        root = Path(tempfile.mkdtemp(prefix="inv-r15-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, root, True)
        (root / "plans").mkdir()
        (root / "plans" / "README.md").write_bytes(b"# readme\n")
        (root / "docs").mkdir()
        (root / "docs" / "owner.md").write_bytes(b"# owner\n")
        (root / "corpus").mkdir()
        (root / "corpus" / "my report.txt").write_bytes(b"# spaced\n")
        return root

    def _ctx(self, catalog):
        return contract.AdapterContext(
            roots={"worktree": self._tree()}, catalog=catalog
        )

    def _scope_call(self, catalog, scope_id="wt-scope"):
        return inventory.inventory_scope(self._ctx(catalog), scope_id)

    # -- R-1: padded/internal-space specs must raise ------------------------

    def test_curated_path_padded_raises(self):
        """GLM F-1: ' parity-formalism/FORMALISM.md' on the live-shaped
        catalog silently flipped curated-text 6->5, zero findings."""
        for path in (
            " docs/owner.md", "docs/owner.md ", "docs/ owner.md",
            "\tdocs/owner.md", "docs/owner.md\t",
        ):
            catalog = self._catalog(curated=[
                {"root": "worktree", "path": path, "selectors": ["# h"]},
            ])
            with self.assertRaises(
                contract.ContractError, msg=path
            ) as caught:
                inventory._CatalogIndex(catalog)
            self.assertEqual(
                caught.exception.code, "malformed-catalog-input", path
            )
            self.assertNotIn("owner.md", str(caught.exception))

    def test_rooted_spec_padded_raises(self):
        for spec in (
            " docs/owner.md", "docs/owner.md ", "docs/owner md",
            "docs\towner.md", "docs\nowner.md",
        ):
            with self.assertRaises(
                contract.ContractError, msg=repr(spec)
            ) as caught:
                inventory._check_rooted_spec(spec, "plans", "probe")
            self.assertEqual(
                caught.exception.code, "malformed-catalog-input", spec
            )

    def test_excludes_padded_raises(self):
        for pattern in (
            " docs/**", "docs/** ", "docs/**  ", "docs/ **", "d ocs/**",
            "docs/\t**",
        ):
            with self.assertRaises(
                contract.ContractError, msg=repr(pattern)
            ) as caught:
                inventory._validated_scope_excludes([pattern], "wt-scope")
            self.assertEqual(
                caught.exception.code, "malformed-catalog-input", pattern
            )

    # -- guards: nothing legitimate shifts -----------------------------------

    def test_clean_specs_still_flow(self):
        for spec in ("docs/owner.md", "a..b/c", ".hidden/x", "**",
                     "a..b/**", "trailing.dot.", "plans/README.md"):
            inventory._check_rooted_spec(spec, "plans", "probe")
        patterns = ["docs/**", "a..b/**", ".hidden/**", "**/*.pyc",
                    "trailing.dot./**"]
        self.assertEqual(
            inventory._validated_scope_excludes(patterns, "wt-scope"),
            patterns,
        )

    def test_clean_curated_designation_still_flows(self):
        catalog = self._catalog(
            components=[self._component(inputs=["docs/owner.md"])],
            curated=[
                {"root": "worktree", "path": "docs/owner.md",
                 "selectors": ["# owner"]},
            ],
        )
        snap = self._scope_call(catalog)
        record = next(
            r for r in snap.records if r["path"] == "docs/owner.md"
        )
        self.assertEqual(record["content_policy"], "curated-text")

    def test_disk_file_named_with_spaces_still_inventorys(self):
        """Record paths are DISK-derived: a corpus file whose NAME
        contains spaces is legitimate and must keep flowing with its
        exact path (the shape gates apply to DECLARED specs only)."""
        catalog = self._catalog(scopes=[
            self._scope(
                selection="recursive-all-regular-and-symlink",
                includes=["corpus"],
            )
        ])
        snap = self._scope_call(catalog)
        self.assertEqual(
            [r["path"] for r in snap.records], ["corpus/my report.txt"]
        )

    def test_padded_declared_input_raises_e2e(self):
        """The declared-input channel rides the same predicate: a padded
        bare input must fail loud instead of silently matching nothing."""
        catalog = self._catalog(components=[
            self._component(inputs=[" docs/owner.md"]),
        ])
        with self.assertRaises(contract.ContractError) as caught:
            self._scope_call(catalog)
        self.assertEqual(
            caught.exception.code, "malformed-catalog-input"
        )


if __name__ == "__main__":
    unittest.main()
