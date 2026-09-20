#!/usr/bin/env python3
"""Word-level unified diff of two text files.

Usage:
    minidiff.py FILE_A FILE_B

Only the changed hunks are printed, in unified style with one line of
context.  Words that changed inside a line are shown inline (git
--word-diff style): deletions as [-old-], insertions as {+new+}; such a
line is printed once, without a line prefix.  Lines that were added or
removed entirely keep the usual + / - prefixes.

Exit status: 0 when the files are identical, 1 when they differ,
2 on usage or I/O errors.
"""

import difflib
import re
import sys

CONTEXT = 1
_TOKEN = re.compile(r"\S+|\s+")
_PREFIX = {"ctx": " ", "del": "-", "add": "+", "both": ""}


def read_lines(path):
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read().splitlines()


def tokenize(line):
    """Split a line into word and whitespace tokens; join() rebuilds it."""
    return _TOKEN.findall(line)


def word_level_block(a_lines, b_lines):
    """Diff two line slices at word granularity.

    Returns (a_marks, b_marks, merged):
      a_marks / b_marks -- per-line list of (token, mark) pairs where
                           mark is 'del', 'ins', or None
      merged            -- token stream for one-line-for-one-line blocks,
                           or None
    """
    a_flat, a_off = [], []
    for line in a_lines:
        a_off.append(len(a_flat))
        a_flat.extend(tokenize(line))
    b_flat, b_off = [], []
    for line in b_lines:
        b_off.append(len(b_flat))
        b_flat.extend(tokenize(line))

    ops = difflib.SequenceMatcher(None, a_flat, b_flat, autojunk=False).get_opcodes()
    a_mark = [None] * len(a_flat)
    b_mark = [None] * len(b_flat)
    for tag, i1, i2, j1, j2 in ops:
        if tag in ("replace", "delete"):
            a_mark[i1:i2] = ["del"] * (i2 - i1)
        if tag in ("replace", "insert"):
            b_mark[j1:j2] = ["ins"] * (j2 - j1)

    def group(flat, marks, off, count):
        out = []
        for li in range(count):
            s = off[li]
            e = off[li + 1] if li + 1 < count else len(flat)
            out.append(list(zip(flat[s:e], marks[s:e])))
        return out

    a_marks = group(a_flat, a_mark, a_off, len(a_lines))
    b_marks = group(b_flat, b_mark, b_off, len(b_lines))

    merged = None
    if len(a_lines) == 1 and len(b_lines) == 1:
        merged = []
        for tag, i1, i2, j1, j2 in ops:
            if tag == "equal":
                merged += [(t, None) for t in a_flat[i1:i2]]
            elif tag == "replace":
                merged += [(t, "del") for t in a_flat[i1:i2]]
                merged += [(t, "ins") for t in b_flat[j1:j2]]
            elif tag == "delete":
                merged += [(t, "del") for t in a_flat[i1:i2]]
            else:
                merged += [(t, "ins") for t in b_flat[j1:j2]]
    return a_marks, b_marks, merged


def render_tokens(tokens):
    """Render (token, mark) pairs, collapsing plain runs back to text."""
    parts = []
    i, n = 0, len(tokens)
    while i < n:
        text, mark = tokens[i]
        if mark is None:
            j = i
            while j < n and tokens[j][1] is None:
                j += 1
            parts.append("".join(t for t, _ in tokens[i:j]))
            i = j
        elif mark == "del":
            parts.append("[-" + text + "-]")
            i += 1
        else:
            parts.append("{+" + text + "+}")
            i += 1
    return "".join(parts)


def has_marks(marks):
    return any(mark is not None for line in marks for _, mark in line)


def block_records(a_lines, b_lines, i1, i2, j1, j2):
    """Display records for one non-equal opcode: (kind, a_ln, b_ln, text)."""
    a_block = a_lines[i1:i2]
    b_block = b_lines[j1:j2]
    a_marks, b_marks, merged = word_level_block(a_block, b_block)

    def line_all_marked(line, want):
        return bool(line) and all(m == want for _, m in line)

    # A one-for-one line change renders as a single merged line, unless the
    # whole line was replaced (then plain -/+ lines are clearer).
    if (
        len(a_block) == 1
        and len(b_block) == 1
        and (has_marks(a_marks) or has_marks(b_marks))
        and merged is not None
        and any(m is None for _, m in merged)
    ):
        return [("both", i1 + 1, j1 + 1, render_tokens(merged))]

    recs = []
    for off, marks in enumerate(a_marks):
        if line_all_marked(marks, "del"):
            recs.append(("del", i1 + 1 + off, None, a_block[off]))
        elif any(m is not None for _, m in marks):
            recs.append(("del", i1 + 1 + off, None, render_tokens(marks)))
        else:
            b_ln = j1 + 1 + off if off < len(b_block) else None
            recs.append(("ctx", i1 + 1 + off, b_ln, a_block[off]))
    for off, marks in enumerate(b_marks):
        if line_all_marked(marks, "ins"):
            recs.append(("add", None, j1 + 1 + off, b_block[off]))
        elif any(m is not None for _, m in marks):
            recs.append(("add", None, j1 + 1 + off, render_tokens(marks)))
        elif off >= len(a_block) or any(m is not None for _, m in a_marks[off]):
            recs.append(("add", None, j1 + 1 + off, b_block[off]))
        # else: unchanged counterpart already emitted as context
    return recs


def build_hunks(a_lines, b_lines):
    """Line-level opcodes for hunk layout, word-level detail inside."""
    ops = difflib.SequenceMatcher(None, a_lines, b_lines, autojunk=False).get_opcodes()
    hunks = []
    prev_eq = None
    for idx, (tag, i1, i2, j1, j2) in enumerate(ops):
        if tag == "equal":
            prev_eq = (i1, i2, j1, j2)
            continue
        nxt = ops[idx + 1] if idx + 1 < len(ops) else None

        lead = []
        if prev_eq is not None:
            e1, e2, f1, f2 = prev_eq
            off = f1 - e1
            for k in range(max(e1, i1 - CONTEXT), i1):
                lead.append(("ctx", k + 1, k + 1 + off, a_lines[k]))
        body = block_records(a_lines, b_lines, i1, i2, j1, j2)
        trail = []
        if nxt is not None and nxt[0] == "equal":
            n1, n2, m1, m2 = nxt[1], nxt[2], nxt[3], nxt[4]
            off = m1 - n1
            for k in range(n1, min(n2, n1 + CONTEXT)):
                trail.append(("ctx", k + 1, k + 1 + off, a_lines[k]))

        if hunks and prev_eq is not None:
            gap = i1 - prev_eq[0]  # equal lines separating this block from the previous
            if gap == 0 or 2 * CONTEXT >= gap:
                lines = hunks[-1]["lines"]
                for rec in lead:
                    if lines and lines[-1][:3] == rec[:3]:
                        continue
                    lines.append(rec)
                lines.extend(body)
                lines.extend(trail)
                continue
        hunks.append({"lines": lead + body + trail})
    return hunks


def _span(start, count):
    return "%d" % start if count == 1 else "%d,%d" % (start, count)


def format_hunks(hunks):
    out = []
    for hunk in hunks:
        lines = hunk["lines"]
        a_lns = [a for _, a, _, _ in lines if a is not None]
        b_lns = [b for _, _, b, _ in lines if b is not None]
        a_start = min(a_lns) if a_lns else min(b_lns) - 1
        b_start = min(b_lns) if b_lns else min(a_lns) - 1
        out.append("@@ -%s +%s @@" % (_span(a_start, len(a_lns)), _span(b_start, len(b_lns))))
        for kind, _, _, text in lines:
            out.append(_PREFIX[kind] + text)
    return out


def main(argv):
    if len(argv) != 3:
        print("usage: minidiff.py FILE_A FILE_B", file=sys.stderr)
        return 2
    try:
        a_lines = read_lines(argv[1])
        b_lines = read_lines(argv[2])
    except (OSError, UnicodeDecodeError) as exc:
        print("minidiff: %s" % exc, file=sys.stderr)
        return 2
    out = format_hunks(build_hunks(a_lines, b_lines))
    for line in out:
        print(line)
    return 1 if out else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
