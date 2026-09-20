#!/usr/bin/env python3
"""Word-level unified diff of two text files.

Usage:
    minidiff.py FILE_A FILE_B

Compares the files word by word and prints only the changed hunks in
unified style with one line of context. Within a line, substituted words
are shown inline as ``-old +new``; lines present on only one side are
shown as whole ``-``/``+`` lines. Exits 1 when the files differ, 0 when
they are identical, and 2 on usage or I/O errors.
"""

import sys
from difflib import SequenceMatcher

CONTEXT = 1


def word_align(line_a, line_b):
    """Align the words of two lines into a list of (op, word) pairs."""
    wa, wb = line_a.split(), line_b.split()
    out = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(
        None, wa, wb, autojunk=False
    ).get_opcodes():
        if tag == "equal":
            out.extend((" ", w) for w in wa[i1:i2])
        elif tag == "delete":
            out.extend(("-", w) for w in wa[i1:i2])
        elif tag == "insert":
            out.extend(("+", w) for w in wb[j1:j2])
        else:  # replace: removed words first, then added words
            out.extend(("-", w) for w in wa[i1:i2])
            out.extend(("+", w) for w in wb[j1:j2])
    return out


def render_mixed(pairs):
    parts = [w if op == " " else op + w for op, w in pairs]
    return " " + " ".join(parts)


def hunk_bounds(rows, lo, hi, idx):
    """Return (start, count) of file lines shown by a hunk, for side idx.

    A pure insertion/deletion hunk reports the line it starts just after
    with a count of zero, matching unified diff conventions.
    """
    first, count, prev = None, 0, 0
    for k, row in enumerate(rows):
        line_no = row[idx]
        if not line_no:
            continue
        if lo <= k <= hi:
            if first is None:
                first = line_no
            count += 1
        elif k < lo:
            prev = line_no
    if first is None:
        return prev, 0
    return first, count


def diff_lines(a, b):
    """Return (rows, changed).

    rows is a list of (a_line_no, b_line_no, kind, text) tuples where the
    unused side's line number is 0 and kind is ctx, mix, del, or add.
    """
    rows = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(
        None, a, b, autojunk=False
    ).get_opcodes():
        if tag == "equal":
            for i, j in zip(range(i1, i2), range(j1, j2)):
                rows.append((i + 1, j + 1, "ctx", " " + a[i]))
        elif tag == "replace" and i2 - i1 == j2 - j1:
            for i, j in zip(range(i1, i2), range(j1, j2)):
                if a[i] == b[j]:
                    rows.append((i + 1, j + 1, "ctx", " " + a[i]))
                else:
                    rows.append(
                        (i + 1, j + 1, "mix", render_mixed(word_align(a[i], b[j])))
                    )
        else:
            for i in range(i1, i2):
                rows.append((i + 1, 0, "del", "-" + a[i]))
            for j in range(j1, j2):
                rows.append((0, j + 1, "add", "+" + b[j]))
    changed = any(row[2] != "ctx" for row in rows)
    return rows, changed


def main(argv):
    if len(argv) != 3:
        print(f"usage: {argv[0]} FILE_A FILE_B", file=sys.stderr)
        return 2
    try:
        with open(argv[1], encoding="utf-8") as f:
            a = f.read().splitlines()
        with open(argv[2], encoding="utf-8") as f:
            b = f.read().splitlines()
    except OSError as exc:
        print(f"minidiff: {exc}", file=sys.stderr)
        return 2

    rows, changed = diff_lines(a, b)
    if not changed:
        return 0

    change_pos = [k for k, row in enumerate(rows) if row[2] != "ctx"]
    ranges = []
    for p in change_pos:
        lo, hi = max(0, p - CONTEXT), min(len(rows) - 1, p + CONTEXT)
        if ranges and lo <= ranges[-1][1]:
            ranges[-1][1] = max(ranges[-1][1], hi)
        else:
            ranges.append([lo, hi])

    out = [f"--- {argv[1]}", f"+++ {argv[2]}"]
    for lo, hi in ranges:
        a_start, a_count = hunk_bounds(rows, lo, hi, 0)
        b_start, b_count = hunk_bounds(rows, lo, hi, 1)
        out.append(f"@@ -{a_start},{a_count} +{b_start},{b_count} @@")
        out.extend(row[3] for row in rows[lo : hi + 1])
    print("\n".join(out))
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
