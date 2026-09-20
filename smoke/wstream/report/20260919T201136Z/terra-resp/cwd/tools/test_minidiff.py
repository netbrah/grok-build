#!/usr/bin/env python3
"""Behavior checks for the minidiff command-line utility."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "tools" / "minidiff.py"


class MiniDiffTest(unittest.TestCase):
    def test_reports_word_substitutions_in_a_single_context_hunk(self) -> None:
        original = (
            "A weathered lighthouse watched over the rocky harbor.\n"
            "At dusk, its golden beam swept across the waves.\n"
            "The keeper polished the brass lantern each morning.\n"
            "Gulls circled the tower while fishermen returned home.\n"
            "By dawn, the steadfast light welcomed every vessel.\n"
        )
        revised = (
            "A weathered lighthouse watched over the rocky harbor.\n"
            "At dusk, its silver beam swept across the waves.\n"
            "The keeper polished the copper lantern each morning.\n"
            "Gulls circled the tower while fishermen returned home.\n"
            "By dawn, the guiding light welcomed every vessel.\n"
        )
        expected = (
            "--- a.txt\n"
            "+++ b.txt\n"
            "@@ -1,5 +1,5 @@\n"
            " A weathered lighthouse watched over the rocky harbor.\n"
            "-At dusk, its [-golden-] beam swept across the waves.\n"
            "+At dusk, its {+silver+} beam swept across the waves.\n"
            "-The keeper polished the [-brass-] lantern each morning.\n"
            "+The keeper polished the {+copper+} lantern each morning.\n"
            " Gulls circled the tower while fishermen returned home.\n"
            "-By dawn, the [-steadfast-] light welcomed every vessel.\n"
            "+By dawn, the {+guiding+} light welcomed every vessel.\n"
        )

        with tempfile.TemporaryDirectory() as directory:
            fixture_dir = Path(directory)
            (fixture_dir / "a.txt").write_text(original, encoding="utf-8")
            (fixture_dir / "b.txt").write_text(revised, encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(TOOL), "a.txt", "b.txt"],
                cwd=fixture_dir,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout, expected)

    def test_identical_files_are_silent_and_successful(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture_dir = Path(directory)
            (fixture_dir / "a.txt").write_text("The lamp is lit.\n", encoding="utf-8")
            (fixture_dir / "b.txt").write_text("The lamp is lit.\n", encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(TOOL), "a.txt", "b.txt"],
                cwd=fixture_dir,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main()
