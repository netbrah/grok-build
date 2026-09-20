#!/usr/bin/env python3
"""Print a word-level, unified-style diff for two text files."""

from __future__ import annotations

import argparse
import difflib
from pathlib import Path
from typing import Iterable, Sequence


def mark_word_changes(old_line: str, new_line: str) -> tuple[str, str]:
    """Return line variants with changed words marked for unified output."""
    old_words = old_line.rstrip("\n").split()
    new_words = new_line.rstrip("\n").split()
    old_parts: list[str] = []
    new_parts: list[str] = []

    for tag, old_start, old_end, new_start, new_end in difflib.SequenceMatcher(
        a=old_words, b=new_words
    ).get_opcodes():
        if tag == "equal":
            old_parts.extend(old_words[old_start:old_end])
            new_parts.extend(new_words[new_start:new_end])
        else:
            old_parts.extend(f"[-{word}-]" for word in old_words[old_start:old_end])
            new_parts.extend(f"{{+{word}+}}" for word in new_words[new_start:new_end])

    return " ".join(old_parts), " ".join(new_parts)


def format_range(start: int, stop: int) -> str:
    """Format a unified-diff range from zero-based start and exclusive stop."""
    length = stop - start
    if length == 1:
        return str(start + 1)
    return f"{start + 1},{length}"


def hunk_header(group: Sequence[tuple[str, int, int, int, int]]) -> str:
    """Build the unified header for a SequenceMatcher opcode group."""
    _, old_start, _, new_start, _ = group[0]
    _, _, old_stop, _, new_stop = group[-1]
    return (
        f"@@ -{format_range(old_start, old_stop)} "
        f"+{format_range(new_start, new_stop)} @@\n"
    )


def unified_word_diff(
    old_lines: Sequence[str], new_lines: Sequence[str]
) -> Iterable[str]:
    """Yield changed unified-style hunks with one line of context."""
    matcher = difflib.SequenceMatcher(a=old_lines, b=new_lines)
    groups = list(matcher.get_grouped_opcodes(n=1))
    if not groups:
        return

    yield "--- a.txt\n"
    yield "+++ b.txt\n"
    for group in groups:
        yield hunk_header(group)
        for tag, old_start, old_stop, new_start, new_stop in group:
            if tag == "equal":
                for line in old_lines[old_start:old_stop]:
                    yield f" {line}"
            elif tag == "replace":
                old_segment = old_lines[old_start:old_stop]
                new_segment = new_lines[new_start:new_stop]
                paired_count = min(len(old_segment), len(new_segment))
                for index in range(paired_count):
                    old_line, new_line = mark_word_changes(
                        old_segment[index], new_segment[index]
                    )
                    yield f"-{old_line}\n"
                    yield f"+{new_line}\n"
                for line in old_segment[paired_count:]:
                    yield f"-{line}"
                for line in new_segment[paired_count:]:
                    yield f"+{line}"
            elif tag == "delete":
                for line in old_lines[old_start:old_stop]:
                    yield f"-{line}"
            elif tag == "insert":
                for line in new_lines[new_start:new_stop]:
                    yield f"+{line}"


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Print a word-level unified diff for two text files."
    )
    parser.add_argument("old_file")
    parser.add_argument("new_file")
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    old_file = Path(arguments.old_file)
    new_file = Path(arguments.new_file)
    old_lines = old_file.read_text(encoding="utf-8").splitlines(keepends=True)
    new_lines = new_file.read_text(encoding="utf-8").splitlines(keepends=True)
    diff = list(unified_word_diff(old_lines, new_lines))
    if diff:
        print("".join(diff), end="")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
