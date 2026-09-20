#!/usr/bin/env python3
"""Dependency-free word-level diff CLI.

Usage: minidiff.py <file1> <file2>

Compares two text files at word granularity (whitespace-delimited tokens)
and prints only the changed hunks in unified-diff style, with 1 token of
context around each change. Exits 1 if any differences were found, 0
otherwise.
"""
import difflib
import sys


def read_words(path):
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()
    return text.split()


def main(argv):
    if len(argv) != 3:
        print(f"usage: {argv[0]} <file1> <file2>", file=sys.stderr)
        return 2

    file1, file2 = argv[1], argv[2]
    words1 = read_words(file1)
    words2 = read_words(file2)

    hunk_lines = list(
        difflib.unified_diff(
            words1,
            words2,
            fromfile=file1,
            tofile=file2,
            lineterm="",
            n=1,
        )
    )

    if not hunk_lines:
        return 0

    print("\n".join(hunk_lines))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
