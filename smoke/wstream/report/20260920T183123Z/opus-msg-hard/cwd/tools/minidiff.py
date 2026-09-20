#!/usr/bin/env python3
"""Word-level unified diff of two text files.

Prints only changed hunks with one line of context. Exits 1 when the files
differ, 0 when they are identical.
"""

import difflib
import re
import sys

CONTEXT = 1
WORD_SPLIT = re.compile(r"(\s+)")


def tokenize(line):
    """Split a line into words and the whitespace runs between them."""
    return [tok for tok in WORD_SPLIT.split(line) if tok != ""]


def mark_words(old_line, new_line):
    """Return (old, new) renderings with differing words bracketed."""
    old_toks = tokenize(old_line)
    new_toks = tokenize(new_line)
    matcher = difflib.SequenceMatcher(a=old_toks, b=new_toks, autojunk=False)
    old_out = []
    new_out = []
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        old_chunk = "".join(old_toks[i1:i2])
        new_chunk = "".join(new_toks[j1:j2])
        if tag == "equal":
            old_out.append(old_chunk)
            new_out.append(new_chunk)
        else:
            if old_chunk:
                old_out.append("[-" + old_chunk + "-]")
            if new_chunk:
                new_out.append("{+" + new_chunk + "+}")
    return "".join(old_out), "".join(new_out)


def format_range(start, length):
    """Unified format omits the length when a range covers exactly one line."""
    pos = start + 1 if length else start
    return str(pos) if length == 1 else "{},{}".format(pos, length)


def hunk_header(group):
    """Build the @@ -a,b +c,d @@ header for a group of opcodes."""
    first, last = group[0], group[-1]
    a_range = format_range(first[1], last[2] - first[1])
    b_range = format_range(first[3], last[4] - first[3])
    return "@@ -{} +{} @@".format(a_range, b_range)


def diff_lines(a_lines, b_lines):
    """Yield unified-style output lines for the changed hunks."""
    matcher = difflib.SequenceMatcher(a=a_lines, b=b_lines, autojunk=False)
    for group in matcher.get_grouped_opcodes(CONTEXT):
        yield hunk_header(group)
        for tag, i1, i2, j1, j2 in group:
            if tag == "equal":
                for line in a_lines[i1:i2]:
                    yield " " + line
                continue
            if tag == "replace":
                paired = min(i2 - i1, j2 - j1)
                for offset in range(paired):
                    old_line, new_line = mark_words(a_lines[i1 + offset], b_lines[j1 + offset])
                    yield "-" + old_line
                    yield "+" + new_line
                for line in a_lines[i1 + paired:i2]:
                    yield "-" + line
                for line in b_lines[j1 + paired:j2]:
                    yield "+" + line
                continue
            for line in a_lines[i1:i2]:
                yield "-" + line
            for line in b_lines[j1:j2]:
                yield "+" + line


def read_lines(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return handle.read().splitlines()
    except OSError as err:
        sys.stderr.write("minidiff: cannot read {}: {}\n".format(path, err.strerror))
        raise SystemExit(2)


def main(argv):
    if len(argv) != 3:
        sys.stderr.write("usage: minidiff.py OLD NEW\n")
        return 2
    a_lines = read_lines(argv[1])
    b_lines = read_lines(argv[2])
    changed = False
    for line in diff_lines(a_lines, b_lines):
        changed = True
        print(line)
    return 1 if changed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
