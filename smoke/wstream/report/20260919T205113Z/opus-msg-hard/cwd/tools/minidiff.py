#!/usr/bin/env python3
"""Word-level unified diff of two text files.

Prints only changed hunks with one line of context. Exits 1 when the files
differ, 0 when they are identical.
"""

import difflib
import re
import sys

CONTEXT = 1
TOKEN_RE = re.compile(r"\s+|\S+")


def read_lines(path):
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read().splitlines()


def tokenize(line):
    return TOKEN_RE.findall(line)


def mark_words(old_line, new_line):
    """Return (old, new) rendered with changed words bracketed."""
    old_tokens = tokenize(old_line)
    new_tokens = tokenize(new_line)
    matcher = difflib.SequenceMatcher(a=old_tokens, b=new_tokens, autojunk=False)
    old_out = []
    new_out = []
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        old_chunk = "".join(old_tokens[i1:i2])
        new_chunk = "".join(new_tokens[j1:j2])
        if tag == "equal":
            old_out.append(old_chunk)
            new_out.append(new_chunk)
            continue
        if old_chunk:
            old_out.append("[-%s-]" % old_chunk)
        if new_chunk:
            new_out.append("{+%s+}" % new_chunk)
    return "".join(old_out), "".join(new_out)


def hunk_range(start, length):
    """Unified-diff range: 1-based start, or 0 for an empty side."""
    if length == 0:
        return "%d,0" % start
    if length == 1:
        return "%d" % (start + 1)
    return "%d,%d" % (start + 1, length)


def format_hunk(group, a_lines, b_lines):
    a_start = group[0][1]
    a_end = group[-1][2]
    b_start = group[0][3]
    b_end = group[-1][4]
    out = [
        "@@ -%s +%s @@"
        % (hunk_range(a_start, a_end - a_start), hunk_range(b_start, b_end - b_start))
    ]
    for tag, i1, i2, j1, j2 in group:
        if tag == "equal":
            out.extend(" " + line for line in a_lines[i1:i2])
            continue
        if tag == "replace" and (i2 - i1) == (j2 - j1):
            for old_line, new_line in zip(a_lines[i1:i2], b_lines[j1:j2]):
                marked_old, marked_new = mark_words(old_line, new_line)
                out.append("-" + marked_old)
                out.append("+" + marked_new)
            continue
        out.extend("-" + line for line in a_lines[i1:i2])
        out.extend("+" + line for line in b_lines[j1:j2])
    return out


def diff(a_path, b_path):
    a_lines = read_lines(a_path)
    b_lines = read_lines(b_path)
    matcher = difflib.SequenceMatcher(a=a_lines, b=b_lines, autojunk=False)
    out = []
    for group in matcher.get_grouped_opcodes(CONTEXT):
        out.extend(format_hunk(group, a_lines, b_lines))
    return out


def main(argv):
    if len(argv) != 3:
        sys.stderr.write("usage: %s FILE1 FILE2\n" % argv[0])
        return 2
    try:
        lines = diff(argv[1], argv[2])
    except OSError as err:
        sys.stderr.write("minidiff: %s\n" % err)
        return 2
    if not lines:
        return 0
    sys.stdout.write("--- %s\n+++ %s\n" % (argv[1], argv[2]))
    sys.stdout.write("\n".join(lines) + "\n")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
