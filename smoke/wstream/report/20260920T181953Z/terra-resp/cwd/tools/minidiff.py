#!/usr/bin/env python3
"""Print word-level unified-style hunks for two text files."""

from __future__ import annotations

import difflib
import re
import sys
from collections.abc import Sequence


_WORD_PARTS = re.compile(r"\w+|\W+")


def format_range_unified(start: int, stop: int) -> str:
    """Return the range portion of a unified-diff hunk header."""
    beginning = start + 1
    length = stop - start
    if length == 1:
        return str(beginning)
    if length == 0:
        beginning -= 1
    return f"{beginning},{length}"


def highlight_words(before: str, after: str) -> tuple[str, str]:
    """Mark word-level differences in corresponding removed and added lines."""
    before_parts = _WORD_PARTS.findall(before)
    after_parts = _WORD_PARTS.findall(after)
    matcher = difflib.SequenceMatcher(a=before_parts, b=after_parts)
    marked_before: list[str] = []
    marked_after: list[str] = []

    for tag, before_start, before_stop, after_start, after_stop in matcher.get_opcodes():
        old = "".join(before_parts[before_start:before_stop])
        new = "".join(after_parts[after_start:after_stop])
        if tag == "equal":
            marked_before.append(old)
            marked_after.append(new)
        elif tag == "delete":
            marked_before.append(f"[-{old}-]")
        elif tag == "insert":
            marked_after.append(f"{{+{new}+}}")
        else:
            marked_before.append(f"[-{old}-]")
            marked_after.append(f"{{+{new}+}}")

    return "".join(marked_before), "".join(marked_after)


def output_line(prefix: str, text: str) -> str:
    """Prefix a diff line and represent a missing trailing newline explicitly."""
    if text.endswith("\n"):
        return f"{prefix}{text}"
    return f"{prefix}{text}\n\\ No newline at end of file\n"


def render_diff(before: str, after: str, context: int = 1) -> str:
    """Return unified-style word-level hunks without file header lines."""
    before_lines = before.splitlines(keepends=True)
    after_lines = after.splitlines(keepends=True)
    matcher = difflib.SequenceMatcher(a=before_lines, b=after_lines)
    rendered: list[str] = []

    for group in matcher.get_grouped_opcodes(context):
        first = group[0]
        last = group[-1]
        rendered.append(
            f"@@ -{format_range_unified(first[1], last[2])} "
            f"+{format_range_unified(first[3], last[4])} @@\n"
        )
        for tag, before_start, before_stop, after_start, after_stop in group:
            if tag == "equal":
                for line in before_lines[before_start:before_stop]:
                    rendered.append(output_line(" ", line))
            elif tag == "delete":
                for line in before_lines[before_start:before_stop]:
                    rendered.append(output_line("-", line))
            elif tag == "insert":
                for line in after_lines[after_start:after_stop]:
                    rendered.append(output_line("+", line))
            else:
                paired = min(before_stop - before_start, after_stop - after_start)
                for offset in range(paired):
                    old, new = highlight_words(
                        before_lines[before_start + offset],
                        after_lines[after_start + offset],
                    )
                    rendered.append(output_line("-", old))
                    rendered.append(output_line("+", new))
                for line in before_lines[before_start + paired:before_stop]:
                    rendered.append(output_line("-", line))
                for line in after_lines[after_start + paired:after_stop]:
                    rendered.append(output_line("+", line))

    return "".join(rendered)


def main(arguments: Sequence[str] | None = None) -> int:
    """Run the command-line interface."""
    arguments = sys.argv[1:] if arguments is None else arguments
    if len(arguments) != 2:
        print(f"usage: {PathLikeProgramName()} FIRST_FILE SECOND_FILE", file=sys.stderr)
        return 2

    try:
        before = open(arguments[0], encoding="utf-8").read()
        after = open(arguments[1], encoding="utf-8").read()
    except OSError as error:
        print(error, file=sys.stderr)
        return 2

    diff = render_diff(before, after)
    if diff:
        sys.stdout.write(diff)
        return 1
    return 0


def PathLikeProgramName() -> str:
    """Return the executable name for the usage message."""
    return sys.argv[0]


if __name__ == "__main__":
    raise SystemExit(main())
