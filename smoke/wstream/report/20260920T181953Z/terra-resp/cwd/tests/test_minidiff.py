"""Regression tests for the minidiff command-line tool."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "tools" / "minidiff.py"


class MinidiffTests(unittest.TestCase):
    def run_tool(self, before: str, after: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            directory_path = Path(directory)
            first = directory_path / "first.txt"
            second = directory_path / "second.txt"
            first.write_text(before, encoding="utf-8")
            second.write_text(after, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(TOOL), str(first), str(second)],
                capture_output=True,
                check=False,
                encoding="utf-8",
            )

    def test_reports_word_substitution_in_a_unified_hunk(self) -> None:
        result = self.run_tool(
            "The old lighthouse watched the sea.\nIts lamp glowed steadily.\n",
            "The old lighthouse watched the sea.\nIts lamp shone steadily.\n",
        )

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, "")
        self.assertIn("@@ -1,2 +1,2 @@", result.stdout)
        self.assertIn(" The old lighthouse watched the sea.\n", result.stdout)
        self.assertIn("-Its lamp [-glowed-] steadily.\n", result.stdout)
        self.assertIn("+Its lamp {+shone+} steadily.\n", result.stdout)

    def test_produces_no_output_for_identical_files(self) -> None:
        result = self.run_tool("No change.\n", "No change.\n")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main()
