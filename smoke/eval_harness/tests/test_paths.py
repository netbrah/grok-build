"""Path-primitive tests for the evaluation harness (Task 2).

Covers RootedPath parsing (ROOT:POSIX_RELATIVE_PATH only), lexical
resolve_rooted containment (no symlink following), and normalize_path_token
(longest matching named root, equal-length alias failure, external
normalization, no filesystem access).

Run from the worktree root:
    python3.14 -m unittest -v smoke.eval_harness.tests.test_paths
"""
import os
import shutil
import tempfile
import unittest
from pathlib import Path

from smoke.eval_harness import contract, paths


class RootedPathParseTest(unittest.TestCase):
    def test_parse_valid_tokens(self):
        rp = paths.RootedPath.parse("worktree:smoke/redteam/run.py")
        self.assertEqual(rp.root, "worktree")
        self.assertEqual(rp.path, "smoke/redteam/run.py")
        rp = paths.RootedPath.parse("grok-home:bin/grok-dogfood")
        self.assertEqual((rp.root, rp.path), ("grok-home", "bin/grok-dogfood"))
        rp = paths.RootedPath.parse("logscale:bin/session-cost-attr.py")
        self.assertEqual((rp.root, rp.path), ("logscale", "bin/session-cost-attr.py"))

    def test_parse_rejects_only_rooted_posix_form(self):
        for bad in (
            "",                 # empty
            "worktree",         # no colon
            ":smoke/x",         # empty root
            "worktree:",        # empty path
            "Worktree:a/b",     # root not a lowercase slug
            "work tree:a/b",    # root with space
            "worktree:/abs",    # absolute path part
            "worktree:a\\b",    # backslash
            "worktree:a/../b",  # .. segment
            "worktree:a/b/..",  # trailing .. segment
            "worktree:./a",     # leading . segment
            "worktree:a/./b",   # interior . segment
            "worktree:a:b/c",   # colon inside the path part
            "worktree:",        # (redundant guard) empty path
            "a.b/c:x/y",        # dotted root segment (not a slug root form)
        ):
            with self.assertRaises(contract.ContractError, msg=repr(bad)):
                paths.RootedPath.parse(bad)

    def test_percent_escaped_segments_accepted(self):
        # The plans corpus contains %2F URL-escaped session segments
        # (e.g. provenance/fixtures/xreplay76/.../sessions/%2FUsers%2F...):
        # % is part of the path segment charset (review M1).
        rp = paths.RootedPath.parse(
            "plans:provenance/fixtures/xreplay76/mint/report/20260918T005237Z/"
            "home/sessions/%2FUsers%2Fpalanisd/summary.json"
        )
        self.assertEqual(
            rp.path,
            "provenance/fixtures/xreplay76/mint/report/20260918T005237Z/"
            "home/sessions/%2FUsers%2Fpalanisd/summary.json",
        )
        # % joins the charset; it does not loosen the rest of it.
        with self.assertRaises(contract.ContractError):
            paths.RootedPath("plans", "a/%2F b")

    def test_constructor_rejects_bad_root_or_path(self):
        with self.assertRaises(contract.ContractError):
            paths.RootedPath("Worktree", "a/b")
        with self.assertRaises(contract.ContractError):
            paths.RootedPath("worktree", "/abs")
        with self.assertRaises(contract.ContractError):
            paths.RootedPath("worktree", "a/../b")
        with self.assertRaises(contract.ContractError):
            paths.RootedPath("worktree", "")

    def test_construction_and_parse_share_validation(self):
        rp = paths.RootedPath.parse("plans:parity-formalism/FORMALISM.md")
        self.assertEqual(rp, paths.RootedPath("plans", "parity-formalism/FORMALISM.md"))
        with self.assertRaises(contract.ContractError):
            paths.RootedPath.parse("plans:FORMALISM.MD/../x")


class ResolveRootedTest(unittest.TestCase):
    def setUp(self):
        self.root_dir = Path(tempfile.mkdtemp(prefix="eval-harness-test-"))
        self.addCleanup(shutil.rmtree, self.root_dir, ignore_errors=True)
        self.file = self.root_dir / "smoke" / "redteam" / "run.py"
        self.file.parent.mkdir(parents=True)
        self.file.write_text("# runner\n", encoding="utf-8")
        self.symlink = self.root_dir / "smoke" / "run-link.py"
        os.symlink("redteam/run.py", self.symlink)
        self.dangling = self.root_dir / "smoke" / "dangling-link.py"
        os.symlink("no/such/target.py", self.dangling)

    def _roots(self):
        return {"worktree": self.root_dir}

    def test_resolves_regular_file_lexically(self):
        rp = paths.RootedPath.parse("worktree:smoke/redteam/run.py")
        out = paths.resolve_rooted(self._roots(), rp)
        self.assertEqual(out, self.root_dir / "smoke" / "redteam" / "run.py")
        self.assertTrue(os.path.isfile(out))

    def test_require_regular_accepts_file(self):
        rp = paths.RootedPath.parse("worktree:smoke/redteam/run.py")
        self.assertEqual(paths.resolve_rooted(self._roots(), rp, require_regular=True),
                         self.file)

    def test_require_regular_rejects_directory(self):
        rp = paths.RootedPath.parse("worktree:smoke")
        with self.assertRaises(contract.ContractError):
            paths.resolve_rooted(self._roots(), rp, require_regular=True)

    def test_inventory_symlink_not_followed(self):
        # Without require_regular the containment check is purely lexical:
        # even a dangling symlink resolves to its lexical location.
        rp = paths.RootedPath.parse("worktree:smoke/dangling-link.py")
        self.assertEqual(paths.resolve_rooted(self._roots(), rp), self.dangling)
        # With require_regular the entry must be a regular file per lstat:
        # a symlink (dangling or not) is not regular and its target is
        # never opened.
        rp = paths.RootedPath.parse("worktree:smoke/run-link.py")
        with self.assertRaises(contract.ContractError):
            paths.resolve_rooted(self._roots(), rp, require_regular=True)

    def test_root_current_directory_resolves_contained_relative(self):
        # A root value of "." (any spelling: ".", Path("."), "./", "") names
        # the current directory. A validated relative part cannot contain a
        # leading "/" or a ".." segment, so containment holds by
        # construction; this documents that invariant (review M-A3).
        cwd = os.getcwd()
        with tempfile.TemporaryDirectory(prefix="eval-harness-cwd-") as tmp:
            os.chdir(tmp)
            self.addCleanup(os.chdir, cwd)
            Path("probe.txt").write_text("probe", encoding="utf-8")
            for spelling in (".", Path("."), "./", ""):
                roots = {"worktree": spelling}
                rp = paths.RootedPath.parse("worktree:probe.txt")
                out = paths.resolve_rooted(roots, rp, require_regular=True)
                self.assertEqual(out, Path("probe.txt"), repr(spelling))
                self.assertTrue(out.is_file(), repr(spelling))

    def test_root_current_directory_lexical_containment(self):
        # Without require_regular the check never touches the filesystem:
        # a validated relative part under root "." is lexically contained.
        roots = {"worktree": "."}
        self.assertEqual(
            paths.resolve_rooted(roots, paths.RootedPath("worktree", "a/b/c.txt")),
            Path("a/b/c.txt"),
        )

    def test_unknown_root_rejected(self):
        rp = paths.RootedPath.parse("plans:a/b")
        with self.assertRaises(contract.ContractError):
            paths.resolve_rooted(self._roots(), rp)

    def test_missing_regular_target_rejected(self):
        rp = paths.RootedPath.parse("worktree:smoke/absent.py")
        with self.assertRaises(contract.ContractError):
            paths.resolve_rooted(self._roots(), rp, require_regular=True)

    def test_containment_is_lexical_not_resolved(self):
        # No .resolve() anywhere: the returned path is the lexical join of
        # the supplied root and the validated relative path, even when the
        # root itself is a symlink.
        real = self.root_dir / "realroot"
        real.mkdir()
        link_root = self.root_dir / "linkroot"
        os.symlink("realroot", link_root)
        (real / "x.py").write_text("x", encoding="utf-8")
        roots = {"worktree": link_root}
        rp = paths.RootedPath.parse("worktree:x.py")
        out = paths.resolve_rooted(roots, rp, require_regular=True)
        self.assertEqual(out, link_root / "x.py")
        self.assertIn("linkroot", str(out))


class NormalizePathTokenTest(unittest.TestCase):
    def test_relative_symlink_strings_remain_verbatim(self):
        roots = {"plans": "/home/op/grok/plans"}
        for token in ("parity-formalism/FORMALISM.md", "../escape/target.md",
                      "a/./b", "relative/link"):
            self.assertEqual(paths.normalize_path_token(token, roots), token)

    def test_absolute_token_under_root(self):
        roots = {"plans": "/home/op/grok/plans"}
        self.assertEqual(
            paths.normalize_path_token("/home/op/grok/plans/parity-formalism/Q-A.md",
                                       roots),
            "@plans/parity-formalism/Q-A.md",
        )

    def test_nested_roots_choose_longest_prefix(self):
        roots = {"a": "/x", "b": "/x/y"}
        self.assertEqual(paths.normalize_path_token("/x/y/z.md", roots), "@b/z.md")
        self.assertEqual(paths.normalize_path_token("/x/w.md", roots), "@a/w.md")

    def test_equal_length_aliases_fail(self):
        roots = {"r1": "/x", "r2": "/x"}
        with self.assertRaises(contract.ContractError):
            paths.normalize_path_token("/x/z.md", roots)
        roots = {"r1": "/x/y", "r2": "/x/y/"}  # same lexical path, trailing slash
        with self.assertRaises(contract.ContractError):
            paths.normalize_path_token("/x/y/z.md", roots)

    def test_external_paths_hash_original_token(self):
        import hashlib

        roots = {"plans": "/home/op/grok/plans"}
        token = "/opt/elsewhere/tool.md"
        out = paths.normalize_path_token(token, roots)
        self.assertEqual(out,
                         "@external/tool.md#"
                         + hashlib.sha256(token.encode("utf-8")).hexdigest())

    def test_normalization_never_touches_the_filesystem(self):
        roots = {
            "plans": "/nonexistent/plans-root-xyz",
            "nested": "/nonexistent/plans-root-xyz/deep",
        }
        self.assertEqual(
            paths.normalize_path_token("/nonexistent/plans-root-xyz/deep/f.md",
                                       roots),
            "@nested/f.md",
        )
        self.assertEqual(
            paths.normalize_path_token("/nonexistent/plans-root-xyz/f.md", roots),
            "@plans/f.md",
        )

    def test_dotdot_inside_root_normalizes_escaping_token_does_not_match(self):
        roots = {"r": "/a/b"}
        # Lexically collapses inside the root.
        self.assertEqual(paths.normalize_path_token("/a/b/x/../c.md", roots),
                         "@r/c.md")
        # Collapses above the root: no longer beneath it -> external.
        import hashlib

        token = "/a/b/x/../../c.md"
        self.assertEqual(
            paths.normalize_path_token(token, roots),
            "@external/c.md#" + hashlib.sha256(token.encode("utf-8")).hexdigest(),
        )

    def test_token_equal_to_root_is_external(self):
        import hashlib

        roots = {"r": "/a/b"}
        token = "/a/b"
        self.assertEqual(
            paths.normalize_path_token(token, roots),
            "@external/b#" + hashlib.sha256(token.encode("utf-8")).hexdigest(),
        )

    def test_empty_token_rejected(self):
        with self.assertRaises(contract.ContractError):
            paths.normalize_path_token("", {"r": "/a"})

    def test_contract_error_identity(self):
        # paths raises the canonical package's single validation exception.
        try:
            paths.normalize_path_token("", {"r": "/a"})
        except contract.ContractError as exc:
            self.assertIs(type(exc), contract.ContractError)
        else:
            self.fail("expected ContractError for empty token")


if __name__ == "__main__":
    unittest.main()
