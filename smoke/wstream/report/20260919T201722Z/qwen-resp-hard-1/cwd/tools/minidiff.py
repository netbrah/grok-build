#!/usr/bin/env python3
"""Word-level unified diff of two text files (stdlib only).

Usage:
    python3 minidiff.py FILE_A FILE_B

Prints only the changed hunks in unified style with one line of
context. Exits 1 when the files differ, 0 when identical, 2 on
usage or I/O errors.
"""

import difflib
import sys

CONTEXT = 1
# Sentinel standing in for each line's terminator; an object so it can
# never collide with a word from the input.
NL = object()


def read_lines(path):
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read().splitlines()


def tokenize(lines):
    """Words per line, each followed by the NL sentinel."""
    toks = []
    for line in lines:
        toks.extend(line.split())
        toks.append(NL)
    return toks


def line_bounds(toks, nlines):
    """Per-line token spans (start, end), each including the line's sentinel."""
    bounds = []
    pos = 0
    for _ in range(nlines):
        end = toks.index(NL, pos)
        bounds.append((pos, end + 1))
        pos = end + 1
    return bounds


def line_of(bounds, pos, toks_len):
    """Line index containing token pos, or len(bounds) if pos is past the end."""
    if pos >= toks_len:
        return len(bounds)
    for k, (s, e) in enumerate(bounds):
        if s <= pos < e:
            return k
    return len(bounds)


def analyze(lines_a, lines_b):
    """Word-level diff of two line lists.

    Returns (has_diff, events). Events, ordered on the old-line axis, are
    ("sub", old, new) for a changed line pair, ("del", old) for a removed
    line, ("ins", axis, new) for a line inserted before old line axis
    (axis == len(lines_a) means end of file), or ("ctx", old, new) for a
    word-identical line pair.

    Lines are paired through the word-level alignment (matched line
    terminators first, then matched words). A pair is reported as context
    only when both sides are word-identical, so unchanged lines stay
    context even when the LCS matches terminators across a gap.
    """
    toks_a, toks_b = tokenize(lines_a), tokenize(lines_b)
    bounds_a = line_bounds(toks_a, len(lines_a))
    bounds_b = line_bounds(toks_b, len(lines_b))
    matcher = difflib.SequenceMatcher(None, toks_a, toks_b, autojunk=False)
    opcodes = matcher.get_opcodes()
    has_diff = any(tag != "equal" for tag, *rest in opcodes)

    a2b = {}
    nl_pairs = []
    for tag, i1, i2, j1, j2 in opcodes:
        if tag == "equal":
            for k in range(i2 - i1):
                a2b[i1 + k] = j1 + k
                if toks_a[i1 + k] is NL:
                    nl_pairs.append((i1 + k, j1 + k))

    a2line = {}
    for i_tok, j_tok in nl_pairs:
        a2line[line_of(bounds_a, i_tok, len(toks_a))] = line_of(bounds_b, j_tok, len(toks_b))
    taken = set(a2line.values())
    for k in range(len(lines_a)):
        if k in a2line:
            continue
        for p in range(*bounds_a[k]):
            if p in a2b:
                m = line_of(bounds_b, a2b[p], len(toks_b))
                if m not in taken:
                    a2line[k] = m
                    taken.add(m)
                break

    events = []  # (axis, rank, kind, payload); lower rank sorts first
    for k in range(len(lines_a)):
        m = a2line.get(k)
        if m is None:
            events.append((k, 2, "del", (k,)))
        elif lines_a[k].split() == lines_b[m].split():
            events.append((k, 3, "ctx", (k, m)))
        else:
            events.append((k, 1, "sub", (k, m)))
    for m in range(len(lines_b)):
        if m in taken:
            continue
        # Insert after the last old line paired to an earlier new line, so
        # added lines keep their new-file order relative to paired lines.
        prev = -1
        for k in range(len(lines_a)):
            t = a2line.get(k)
            if t is not None and t < m:
                prev = k
        events.append((prev + 1, 0, "ins", (prev + 1, m)))
    events.sort(key=lambda ev: (ev[0], ev[1]))
    return has_diff, [(kind, *payload) for _, _, kind, payload in events]


def render_hunks(lines_a, lines_b, events):
    """Group change events <= CONTEXT context lines apart into hunks."""
    changes = [i for i, ev in enumerate(events) if ev[0] != "ctx"]
    groups = []
    for i in changes:
        if groups and events[i][1] - events[groups[-1][-1]][1] - 1 <= CONTEXT:
            groups[-1].append(i)
            continue
        groups.append([i])

    hunks = []
    for grp in groups:
        start = grp[0]
        lead = 0
        while start > 0 and lead < CONTEXT and events[start - 1][0] == "ctx":
            start -= 1
            lead += 1
        end = grp[-1] + 1
        trail = 0
        while end < len(events) and trail < CONTEXT and events[end][0] == "ctx":
            end += 1
            trail += 1

        rows = []  # (prefix, text, old_no, new_no)
        for ev in events[start:end]:
            kind = ev[0]
            if kind == "ctx":
                rows.append((" ", " ".join(lines_a[ev[1]].split()), ev[1] + 1, ev[2] + 1))
            elif kind == "sub":
                rows.append(("-", " ".join(lines_a[ev[1]].split()), ev[1] + 1, None))
                rows.append(("+", " ".join(lines_b[ev[2]].split()), None, ev[2] + 1))
            elif kind == "del":
                rows.append(("-", " ".join(lines_a[ev[1]].split()), ev[1] + 1, None))
            else:
                rows.append(("+", " ".join(lines_b[ev[2]].split()), None, ev[2] + 1))

        old_rows = [r for r in rows if r[0] in (" ", "-")]
        new_rows = [r for r in rows if r[0] in (" ", "+")]
        old_start = old_rows[0][2] if old_rows else events[grp[0]][1]
        new_start = new_rows[0][3] if new_rows else 0
        out = ["@@ -%d,%d +%d,%d @@" % (old_start, len(old_rows), new_start, len(new_rows))]
        out += ["%s%s" % (prefix, text) for prefix, text, _, _ in rows]
        hunks.append("\n".join(out))
    return hunks


def main(argv):
    if len(argv) != 3:
        print("usage: minidiff.py FILE_A FILE_B", file=sys.stderr)
        return 2
    try:
        lines_a = read_lines(argv[1])
        lines_b = read_lines(argv[2])
    except OSError as exc:
        print("minidiff: %s" % exc, file=sys.stderr)
        return 2
    has_diff, events = analyze(lines_a, lines_b)
    for hunk in render_hunks(lines_a, lines_b, events):
        print(hunk)
    return 1 if has_diff else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
