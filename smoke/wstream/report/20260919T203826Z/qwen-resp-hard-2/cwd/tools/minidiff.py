#!/usr/bin/env python3
"""Word-level diff of two text files, printed as unified-style hunks.

Usage:
    minidiff.py FILE1 FILE2

The files are compared word by word (whitespace-separated tokens), so a
difference exists only when the word sequences differ; re-wrapping text
across line boundaries is not reported. Output is unified style: a
`---`/`+++` header pair followed by only the changed hunks, each with
one line of context. If the line structure around the changed regions
does not line up between the files, the output falls back to ordinary
line-based unified diff for those files.

Exit status: 0 when the files match, 1 when they differ, 2 on usage
or I/O errors.
"""

import sys
from difflib import SequenceMatcher, unified_diff

CONTEXT = 1


def read_lines(path):
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read().splitlines()


def tokenize(lines):
    """Return (words, spans); spans holds a (start, end) token range per line."""
    words = []
    spans = []
    for line in lines:
        start = len(words)
        words.extend(line.split())
        spans.append((start, len(words)))
    return words, spans


def touched_lines(spans, i1, i2):
    """Indices of lines containing at least one token in [i1, i2)."""
    return [
        li
        for li, (s, e) in enumerate(spans)
        if s < e and s < i2 and e > i1
    ]


def fmt_range(indices):
    """Unified-diff hunk range: '3' for one line, '3,4' for four, '0,0' for none."""
    if not indices:
        return "0,0"
    lo, hi = min(indices), max(indices)
    if lo == hi:
        return str(lo + 1)
    return f"{lo + 1},{hi - lo + 1}"


def diff_files(old_lines, new_lines, name1, name2):
    """Return (output lines, different)."""
    old_words, old_spans = tokenize(old_lines)
    new_words, new_spans = tokenize(new_lines)
    opcodes = SequenceMatcher(
        None, old_words, new_words, autojunk=False
    ).get_opcodes()

    old_changed = [False] * len(old_lines)
    new_changed = [False] * len(new_lines)
    different = False
    for tag, i1, i2, j1, j2 in opcodes:
        if tag == "equal":
            continue
        different = True
        for li in touched_lines(old_spans, i1, i2):
            old_changed[li] = True
        for lj in touched_lines(new_spans, j1, j2):
            new_changed[lj] = True

    if not different:
        return [], False

    # Unchanged lines pair up one-to-one in order. Verify the pairing is
    # line-identical; if the line structure drifted, a word-level hunk has
    # no honest unified representation, so use line-based unified diff.
    old_u = [i for i, c in enumerate(old_changed) if not c]
    new_u = [j for j, c in enumerate(new_changed) if not c]
    if len(old_u) != len(new_u) or any(
        old_lines[a] != new_lines[b] for a, b in zip(old_u, new_u)
    ):
        body = list(
            unified_diff(
                old_lines, new_lines, name1, name2, n=CONTEXT, lineterm=""
            )
        )
        return body, True

    # Row sequence over the whole file: ctx/del/add rows carrying line
    # indices. Between two unchanged line pairs every line is changed, so
    # the walk below emits del rows for the old gap, add rows for the new
    # gap, then the unchanged pair as a context row.
    rows = []
    i = j = k = 0
    n_old, n_new = len(old_lines), len(new_lines)
    while i < n_old or j < n_new:
        if k < len(old_u):
            a, b = old_u[k], new_u[k]
            rows.extend(("del", x, None) for x in range(i, a))
            rows.extend(("add", None, x) for x in range(j, b))
            rows.append(("ctx", a, b))
            i, j, k = a + 1, b + 1, k + 1
        else:
            rows.extend(("del", x, None) for x in range(i, n_old))
            rows.extend(("add", None, x) for x in range(j, n_new))
            break

    # Group rows into hunks: maximal changed blocks extended by CONTEXT
    # context rows on each side, merged when the row ranges overlap.
    hunks = []
    start = 0
    last = len(rows) - 1
    while start <= last:
        if rows[start][0] == "ctx":
            start += 1
            continue
        end = start
        while end < last and rows[end + 1][0] != "ctx":
            end += 1
        hs, he = max(0, start - CONTEXT), min(last, end + CONTEXT)
        if hunks and hs <= hunks[-1][1]:
            hunks[-1][1] = he
        else:
            hunks.append([hs, he])
        start = end + 1

    out = [f"--- {name1}", f"+++ {name2}"]
    for hs, he in hunks:
        hunk = rows[hs : he + 1]
        old_idx = [r[1] for r in hunk if r[1] is not None]
        new_idx = [r[2] for r in hunk if r[2] is not None]
        out.append(f"@@ -{fmt_range(old_idx)} +{fmt_range(new_idx)} @@")
        for kind, a, b in hunk:
            if kind == "ctx":
                out.append(" " + old_lines[a])
            elif kind == "del":
                out.append("-" + old_lines[a])
            else:
                out.append("+" + new_lines[b])
    return out, True


def main(argv):
    if len(argv) != 3:
        print(f"usage: {argv[0]} FILE1 FILE2", file=sys.stderr)
        return 2
    try:
        old_lines = read_lines(argv[1])
        new_lines = read_lines(argv[2])
    except OSError as exc:
        print(f"minidiff: {exc}", file=sys.stderr)
        return 2
    out, different = diff_files(old_lines, new_lines, argv[1], argv[2])
    if out:
        print("\n".join(out))
    return 1 if different else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
