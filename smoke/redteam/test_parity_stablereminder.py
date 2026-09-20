#!/usr/bin/env python3
"""Offline fixture tests for apex-ayl.110 (STABLE-REMINDER) — stdlib only.

Characterizes the mid-prefix D(developer-item) <-> R(system-reminder)
reorder that sticks the cached prefix: on the recorded capture the
developer item sits before the last reminder in req-004/req-006
(D<R), then a ONE-TIME swap moves it to the tail in req-007 (R<D), where
it stays permanently (req-007..009). From req-007 on, every request
re-sends the whole tail after the developer item cold — the cached
prefix cannot grow past the stuck byte boundary.

Fixtures (fixtures/parity/110/, see META.json for sources + sha256s):
  req-004_body.json (7 items)   D<R   dev@[5]       rems@[2,3,6]
  req-006_body.json (12 items)  D<R   dev@[10]      rems@[2,3,5,7,8,11]
  req-007_body.json (15 items)  R<D   dev@[14]      one-time D<->R swap
  req-008_body.json (18 items)  R<D   dev@[17]      permanent
  req-009_body.json (21 items)  R<D   dev@[20]      permanent
Bare compact request BODIES (top-level "input" array), document key order
preserved (order-faithful to the wire). req-004 vs req-006 common prefix:
27,280 B true serde bytes / 27,363 B python-normalized upper bound
(META.json); python-normalized element-wise recount here = 27,280 B.
Source capture: smoke/redteam/report/full-sweep-20260919T060150Z/
t21-resp-sol-mcp-tool/wire/

SDD:    grok/plans/xwire/110-stable-reminder-sdd-20260919.md
Ratify: grok/plans/xwire/110-stable-reminder-sdd-ratify-root-20260919.md
        (verdict CONCUR)

NOTE (fixture-sourced deviation): the task spec's reminder probe
'"<system-reminder>"' (quoted tag, closing quote included) never occurs in
these bodies — the tag opens a longer content string
("content":"<system-reminder>\\n...") with no closing quote. Per
"the fixture is the source of truth", the probe is the quote-anchored
opening form '"<system-reminder>' (a JSON string value starting with the
tag); on this capture both variants agree on the position sets above.

Run it (OFFLINE: no proxy key, no binary, no live calls, no cargo):

  python3 smoke/redteam/test_parity_stablereminder.py            # exit 0
  python3 smoke/redteam/test_parity_stablereminder.py --gate     # exit 1 today
  python3 smoke/redteam/test_parity_stablereminder.py --gate DIR

--gate checks a live wire capture dir (default:
smoke/redteam/report/full-sweep-20260919T060150Z/t21-resp-sol-mcp-tool/
wire, worktree-root-relative, CWD-relative fallback). Discovers BOTH
envelope files req-*.json (uses envelope["body"]) and bare bodies
req-00*_body.json (top-level is the body); keeps entries whose body has
an "input" list, sorted by request number (side-calls skipped). Pairs
are drawn from the main-loop subsequence only — conversation-flow
requests per the x-grok-conv-id header (set, and not a 'turn-summary-*'
synthetic id; title side-calls carry an empty conv-id) or, for bare
bodies without an envelope, a developer item in the input. Side-calls
and aux requests are outside the SDD §4(a) subset. Per consecutive
main-loop pair (prev, cur):
(a) ORDER FLIP — prev has devs[0] < rems[-1] and cur has rems[-1] <
devs[0]; (b) PREFIX — the element-wise common normalized prefix must
cover norm_bytes(prev_body["input"]) - B_D (input-only base; F1 fix
2026-09-20 — the old whole-body base included the ~47.6 KB tools array
and was structurally unreachable on live envelopes), where B_D is the
max normalized size of a single developer item anywhere in the capture
(fallback 139). Exit 0 = GREEN (post-cut acceptance), 1 = RED, 2 =
usage error. On the recorded fixture the 4->6 pair ALSO trips the
prefix check (27,280 B prefix vs ~33 KB input) — expected pre-cut;
acceptance = gate False today, gate True on post-cut captures.

Test map (characterization pins on the recorded capture — all GREEN today):

  DRSwapRederivation.test_order_early_d_r       GREEN (D before last R on
      req-004/req-006; prints actual dev/reminder positions)
  DRSwapRederivation.test_swap_at_req_007       GREEN (one-time D<->R swap;
      developer at the tail, rems[-1] < devs[0])
  DRSwapRederivation.test_permanence            GREEN (developer permanently
      at the tail on req-008/req-009 -> R<D permanent)
  DRSwapRederivation.test_stuck_prefix_bound    GREEN (common prefix of
      req-004 vs req-006 in [25000, norm_bytes(body4) - 2000))
  DRSwapRederivation.test_gate_on_recorded_fixture
      GREEN today — asserts gate(FIXTURE_DIR) is False (recorded RED
      pre-cut; the 6->7 order flip must be among the findings)
"""
import argparse
import json
import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE_DIR = os.path.join(HERE, "fixtures", "parity", "110")
# Worktree root per task spec: 4 levels up from this file's dir (dirname x4).
WORKTREE_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(HERE))))
DEFAULT_GATE_REL = "smoke/redteam/report/full-sweep-20260919T060150Z/t21-resp-sol-mcp-tool/wire"

# Item probes (substring tests on json.dumps(item)):
DEV_MARK = '"developer"'            # developer-role item
REMINDER_MARK = '"<system-reminder>'  # content string starting with the tag


def _load_json(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def norm_bytes(obj):
    """Python-normalized compact JSON byte size (ensure_ascii=True — an
    UPPER BOUND on the true serde wire bytes; META.json records the true
    numbers)."""
    return len(json.dumps(obj, separators=(",", ":")).encode("utf-8"))


def _body_positions(body):
    """Return (devs, rems): item indices of developer items and
    system-reminder items in body["input"]."""
    devs = []
    rems = []
    for index, item in enumerate(body["input"]):
        dumped = json.dumps(item)
        if DEV_MARK in dumped:
            devs.append(index)
        if REMINDER_MARK in dumped:
            rems.append(index)
    return devs, rems


def _common_prefix_bytes(body_a, body_b):
    """Element-wise common prefix: compare json.dumps(item, compact) per
    item index; total normalized bytes of the shared leading items."""
    total = 0
    for item_a, item_b in zip(body_a["input"], body_b["input"]):
        dumped_a = json.dumps(item_a, separators=(",", ":"))
        dumped_b = json.dumps(item_b, separators=(",", ":"))
        if dumped_a != dumped_b:
            break
        total += len(dumped_a.encode("utf-8"))
    return total


class DRSwapRederivation(unittest.TestCase):
    """Characterization pins on fixtures/parity/110 (must pass today)."""

    def _body(self, number):
        path = os.path.join(FIXTURE_DIR, "req-%03d_body.json" % number)
        return _load_json(path)

    def test_order_early_d_r(self):
        """req-004 and req-006: developer present, reminders present, and
        D before the last R (devs[0] < rems[-1]). Expected cross-check:
        req-004 dev@[5] rems@[2,3,6]; req-006 dev@[10] rems@[2,3,5,7,8,11]."""
        for number in (4, 6):
            body = self._body(number)
            devs, rems = _body_positions(body)
            print("req-%03d: items=%d devs@%s rems@%s" % (number, len(body["input"]), devs, rems))
            self.assertTrue(devs, "req-%03d: no developer item" % number)
            self.assertTrue(rems, "req-%03d: no system-reminder item" % number)
            self.assertLess(devs[0], rems[-1],
                            "req-%03d: D not before last R (devs@%s rems@%s)" % (number, devs, rems))

    def test_swap_at_req_007(self):
        """req-007: the one-time D<->R swap — developer now at the tail
        (devs == [len(items)-1]) with the last reminder before it
        (rems[-1] < devs[0])."""
        body = self._body(7)
        devs, rems = _body_positions(body)
        print("req-007: items=%d devs@%s rems@%s" % (len(body["input"]), devs, rems))
        self.assertEqual(devs, [len(body["input"]) - 1],
                         "req-007: developer not at the tail (devs@%s)" % devs)
        self.assertLess(rems[-1], devs[0],
                        "req-007: last reminder not before developer (devs@%s rems@%s)" % (devs, rems))

    def test_permanence(self):
        """req-008 and req-009: developer permanently at the tail
        (devs == [len(items)-1]) -> R<D is permanent from the swap on."""
        for number in (8, 9):
            body = self._body(number)
            devs, rems = _body_positions(body)
            print("req-%03d: items=%d devs@%s rems@%s" % (number, len(body["input"]), devs, rems))
            self.assertEqual(devs, [len(body["input"]) - 1],
                             "req-%03d: developer not at the tail (devs@%s)" % (number, devs))

    def test_stuck_prefix_bound(self):
        """Element-wise common prefix of req-004 vs req-006 (normalized
        bytes of the shared leading items): >= 25000 AND <
        norm_bytes(body4) - 2000. True wire: 27,280 B serde / 27,363 B
        python-normalized (META.json); element-wise recount = 27,280 B."""
        body4 = self._body(4)
        body6 = self._body(6)
        kept = _common_prefix_bytes(body4, body6)
        body4_bytes = norm_bytes(body4)
        print("common prefix req-004 vs req-006: %d bytes; req-004 normalized %d bytes"
              % (kept, body4_bytes))
        self.assertGreaterEqual(kept, 25000)
        self.assertLess(kept, body4_bytes - 2000)

    def test_gate_on_recorded_fixture(self):
        """gate(FIXTURE_DIR) returns (False, findings) — the recorded RED
        pre-cut is expected: the 6->7 order flip must be among them (the
        4->6 prefix-truncated finding is expected too; do not weaken)."""
        passed, findings = gate(FIXTURE_DIR)
        for finding in findings:
            print(finding)
        self.assertFalse(passed, "gate on the recorded fixture must be RED pre-cut")
        self.assertTrue(any("order flip" in f and "req6->req7" in f for f in findings),
                        "expected the D<->R order flip req6->req7 among findings: %r" % findings)


def _load_body(path):
    """Return (number, body, envelope) for a wire capture file, or None
    to skip. Envelope is the outer dict for live-capture envelope files
    ({"n", "method", "path", "ts", "headers", "body"}) and None for bare
    bodies (top-level "input" list; number from the req-NNN filename).
    A string body is json-decoded if possible. No "input" list -> None.
    """
    obj = _load_json(path)
    if not isinstance(obj, dict):
        return None
    number = obj.get("n")
    body = obj.get("body")
    envelope = None
    if isinstance(body, str):
        try:
            body = json.loads(body)
        except json.JSONDecodeError:
            return None
        envelope = obj
    elif body is None and isinstance(obj.get("input"), list):
        body = obj
    else:
        envelope = obj
    if not isinstance(body, dict) or not isinstance(body.get("input"), list):
        return None
    if not isinstance(number, int):
        match = re.match(r"req-(\d+)", os.path.basename(path))
        if not match:
            return None
        number = int(match.group(1))
    return number, body, envelope

TURN_SUMMARY_CONV_PREFIX = "turn-summary-"


def _is_main_loop(body, envelope):
    """Main-loop request = conversation-flow request (the SDD §4(a)
    subset). Envelope captures: x-grok-conv-id must be set and must not
    be a synthetic side-call id — title side-calls carry an EMPTY
    conv-id, the turn-summary side-call carries 'turn-summary-*'. Bare
    bodies (fixtures, no envelope): a developer item in the input
    suffices. No developer item -> never main-loop."""
    if not _body_positions(body)[0]:
        return False
    if envelope is None:
        return True
    headers = envelope.get("headers") or {}
    conv_id = headers.get("x-grok-conv-id")
    if not isinstance(conv_id, str) or not conv_id:
        return False
    return not conv_id.startswith(TURN_SUMMARY_CONV_PREFIX)


def gate(capture_dir):
    """Post-cut acceptance gate over a wire capture dir.

    Discovers envelope files req-*.json (uses envelope["body"]) and bare
    bodies req-00*_body.json (top-level is the body) under capture_dir;
    keeps entries whose body has an "input" list (side-calls skipped),
    sorted by request number. Pairs are drawn from the MAIN-LOOP
    subsequence only (see _is_main_loop: the x-grok-conv-id header must
    be set and not 'turn-summary-*'; bare bodies fall back to a
    developer item in the input). Side-calls and aux requests are
    outside the SDD §4(a) subset; before the 2026-09-20 F1/F2 fix they
    tripped the prefix check structurally (t21's req3->req4 "kept 0 of
    1316"; post-cut sol's req4->req5 title pair and the req18->req19
    turn-summary pair). Per consecutive main-loop pair (prev, cur):
      (a) ORDER FLIP — prev has devs[0] < rems[-1] AND cur has
          rems[-1] < devs[0];
      (b) PREFIX — element-wise common normalized prefix bytes must be
          >= norm_bytes(prev_body["input"]) - B_D. F1 fix 2026-09-20:
          the base is the input-only prev body; the old whole-body base
          (incl. the ~47.6 KB tools array on live envelopes) made the
          criterion structurally unreachable, pre- or post-cut. B_D is
          the max normalized size of a single developer item anywhere in
          the capture (fallback 139) — it tolerates exactly one in-place
          developer-item content update (the pinned D), nothing more.
    Returns (passed, findings).
    """
    entries = []
    for dirpath, _dirnames, filenames in os.walk(capture_dir):
        for name in sorted(filenames):
            if not re.match(r"req-\d+(_body)?\.json$", name):
                continue
            loaded = _load_body(os.path.join(dirpath, name))
            if loaded is not None:
                entries.append(loaded)
    if not entries:
        return (False, ["no request bodies with an input list found under %s (vacuous capture)" % capture_dir])
    entries.sort(key=lambda entry: entry[0])
    dev_bytes = 0
    for _number, body, _envelope in entries:
        for item in body["input"]:
            if DEV_MARK in json.dumps(item):
                dev_bytes = max(dev_bytes, norm_bytes(item))
    b_d = dev_bytes or 139
    main_loop = [(num, body) for num, body, envelope in entries if _is_main_loop(body, envelope)]
    findings = []
    for (prev_number, prev_body), (cur_number, cur_body) in zip(main_loop, main_loop[1:]):
        prev_devs, prev_rems = _body_positions(prev_body)
        cur_devs, cur_rems = _body_positions(cur_body)
        if prev_rems and cur_rems:
            if prev_devs[0] < prev_rems[-1] and cur_rems[-1] < cur_devs[0]:
                findings.append("D<->R order flip req%d->req%d (mid-prefix churn)" % (prev_number, cur_number))
        kept = _common_prefix_bytes(prev_body, cur_body)
        prev_input_bytes = norm_bytes(prev_body["input"])
        if kept < prev_input_bytes - b_d:
            findings.append("prefix truncated req%d->req%d (kept %d of %d input bytes)"
                            % (prev_number, cur_number, kept, prev_input_bytes))
    return (not findings, findings)


def _resolve_gate_dir(specified):
    """Resolve the --gate dir. An explicit DIR is tried as-is (absolute or
    CWD-relative), then worktree-root-relative; the default DIR is tried
    worktree-root-relative, then CWD-relative (spec fallback). Returns the
    first existing dir or None."""
    if specified:
        if os.path.isabs(specified):
            candidates = [specified]
        else:
            candidates = [os.path.join(os.getcwd(), specified), os.path.join(WORKTREE_ROOT, specified)]
    else:
        candidates = [os.path.join(WORKTREE_ROOT, DEFAULT_GATE_REL), os.path.join(os.getcwd(), DEFAULT_GATE_REL)]
    for candidate in candidates:
        if os.path.isdir(candidate):
            return candidate
    return None


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    parser = argparse.ArgumentParser(
        description="apex-ayl.110 STABLE-REMINDER offline fixture tests + post-cut gate",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gate", nargs="?", const=DEFAULT_GATE_REL, default=None, metavar="DIR",
                        help="run the gate over a wire capture dir instead of the fixture "
                             "characterization suite; default DIR = %s" % DEFAULT_GATE_REL)
    ns, _rest = parser.parse_known_args(argv)
    if ns.gate is None:
        unittest.main(module=__name__, argv=[sys.argv[0]] + argv)
    resolved = _resolve_gate_dir(ns.gate)
    if resolved is None:
        print("usage error: gate dir not found: %s (tried worktree root %s and CWD %s)"
              % (ns.gate, WORKTREE_ROOT, os.getcwd()))
        return 2
    passed, findings = gate(resolved)
    for finding in findings:
        print(finding)
    if passed:
        print("GATE GREEN: no D<->R order flips, no truncated prefixes (%s)" % resolved)
        return 0
    print("GATE RED: %d finding(s) in %s" % (len(findings), resolved))
    return 1


if __name__ == "__main__":
    sys.exit(main())
