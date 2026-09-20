#!/usr/bin/env python3
"""Offline fixture tests for apex-ayl.101 (CACHEWRITE-DTO) — stdlib only.

Characterizes the recorded RED state of the cache_write_tokens drop: the
responses WIRE carries usage.input_tokens_details.cache_write_tokens on the
response.completed frame, but the harness usage seam
(xai-grok-shell/src/session/usage_file.rs, camelCase keys) never writes the
matching cacheCreationTokens row into usage.json.

Fixtures (fixtures/parity/101/, see META.json for sources + sha256s):
  response_completed_frame.json            main-turn frame (input_tokens
                                           16595, cached_tokens 0,
                                           cache_write_tokens 16592)
  response_completed_frame_sidecall.json   224-token warm side-call frame
                                           (cache_write_tokens 0)
  usage_recorded.json                      same session's usage.json
                                           (session/turns[0]
                                           cacheCreationTokens 0 = RED)
Source capture: smoke/wstream/report/20260919T060151Z/sol-resp/

SDD:    grok/plans/xwire/101-cachewrite-dto-sdd-20260919.md
Ratify: grok/plans/xwire/101-cachewrite-dto-sdd-ratify-root-20260919.md
        (verdict CONCUR)

Run it (OFFLINE: no proxy key, no binary, no live calls, no cargo):

  python3 smoke/redteam/test_parity_cachewrite.py          # exit 0 (fixtures)
  python3 smoke/redteam/test_parity_cachewrite.py --gate   # exit 1 today (RED)
  python3 smoke/redteam/test_parity_cachewrite.py --gate DIR

--gate checks a live capture dir (default:
smoke/wstream/report/20260919T060151Z/sol-resp, worktree-root-relative,
CWD-relative fallback): every response.completed frame in capture/resp-*.jsonl
with cache_write_tokens = N > 0 must have a usage.json row (session or
turns[i]) with inputTokens == frame input_tokens AND cacheCreationTokens ==
N (byte-exact). A capture with no N>0 frame is vacuous and fails. Exit 0 =
GREEN (post-cut acceptance), 1 = RED (drop still present), 2 = usage error.

Test map (characterization pins on the recorded capture — all GREEN today;
the documented RED is the harness drop, not a test failure):

  CacheWriteDtoReplay.test_wire_frame_carries_field      GREEN (wire has the
      field: cached_tokens 0 + cache_write_tokens 16592 on main, 0 on side)
  CacheWriteDtoReplay.test_input_tokens_byte_match       GREEN (16595 byte-
      equal between the wire frame and usage.json session.inputTokens)
  CacheWriteDtoReplay.test_harness_records_write_RED_state
      GREEN today — DOCUMENTS the RED state (wire-present 16592 AND
      harness-dropped 0); re-mint the fixture post-cut and this test's
      expected values flip with it
  CacheWriteDtoReplay.test_sidecall_zero_write_row       GREEN (224-token
      warm call, zero write row — the control case)
"""
import argparse
import json
import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE_DIR = os.path.join(HERE, "fixtures", "parity", "101")
# Worktree root per task spec: 4 levels up from this file's dir (dirname x4).
WORKTREE_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(HERE))))
DEFAULT_GATE_REL = "smoke/wstream/report/20260919T060151Z/sol-resp"


def _load_json(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def _iter_write_frames(capture_dir):
    """Yield (path, frame_index, input_tokens, cache_write_tokens) for every
    response.completed frame with cache_write_tokens = N > 0 in any
    resp-*.jsonl under capture_dir (recursive).

    Capture line shape: {"frame_index": N, "frame": "data: {json}"} — the
    SSE 'data:' prefix is stripped and 'data: [DONE]' terminators skipped;
    non-SSE lines (request-header records, plain bodies) are ignored.
    """
    for dirpath, _dirnames, filenames in os.walk(capture_dir):
        for name in sorted(filenames):
            if not re.match(r"resp-\d+\.jsonl$", name):
                continue
            path = os.path.join(dirpath, name)
            with open(path, "r", encoding="utf-8") as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    record = json.loads(line)
                    if not isinstance(record, dict):
                        continue
                    frame = record.get("frame")
                    if isinstance(frame, str):
                        if not frame.startswith("data:"):
                            continue
                        payload = frame[len("data:"):].strip()
                        if payload == "[DONE]":
                            continue
                        try:
                            frame = json.loads(payload)
                        except json.JSONDecodeError:
                            continue
                    if not isinstance(frame, dict) or frame.get("type") != "response.completed":
                        continue
                    usage = (frame.get("response") or {}).get("usage") or {}
                    details = usage.get("input_tokens_details") or {}
                    write_tokens = details.get("cache_write_tokens") or 0
                    if write_tokens > 0:
                        yield path, record.get("frame_index"), usage.get("input_tokens"), write_tokens


def _iter_usage_rows(capture_dir):
    """Yield (path, [(label, row), ...]) for every usage.json under
    capture_dir (recursive); rows = the session object plus each turns[]
    entry (the usage seam's camelCase rows)."""
    for dirpath, _dirnames, filenames in os.walk(capture_dir):
        for name in sorted(filenames):
            if name != "usage.json":
                continue
            path = os.path.join(dirpath, name)
            doc = _load_json(path)
            rows = []
            if isinstance(doc.get("session"), dict):
                rows.append(("session", doc["session"]))
            for index, turn in enumerate(doc.get("turns") or []):
                if isinstance(turn, dict):
                    rows.append(("turns[%d]" % index, turn))
            yield path, rows


class CacheWriteDtoReplay(unittest.TestCase):
    """Characterization pins on fixtures/parity/101 (must pass today)."""

    def setUp(self):
        self.main_frame = _load_json(os.path.join(FIXTURE_DIR, "response_completed_frame.json"))
        self.side_frame = _load_json(os.path.join(FIXTURE_DIR, "response_completed_frame_sidecall.json"))
        self.usage_recorded = _load_json(os.path.join(FIXTURE_DIR, "usage_recorded.json"))

    def _usage(self, frame):
        return frame["response"]["usage"]

    def test_wire_frame_carries_field(self):
        """The wire carries cache_write_tokens: main frame shows a 16592-token
        cold write with zero cached reads; the side-call frame shows zero."""
        details = self._usage(self.main_frame)["input_tokens_details"]
        self.assertEqual(details["cached_tokens"], 0)
        self.assertEqual(details["cache_write_tokens"], 16592)
        self.assertGreater(details["cache_write_tokens"], 0)
        side_details = self._usage(self.side_frame)["input_tokens_details"]
        self.assertEqual(side_details["cache_write_tokens"], 0)

    def test_input_tokens_byte_match(self):
        """Wire frame input_tokens == usage.json session-level inputTokens
        (16595) — the join key the gate matches rows on."""
        wire = self._usage(self.main_frame)["input_tokens"]
        recorded = self.usage_recorded["session"]["inputTokens"]
        print("wire input_tokens=%s, usage.json session.inputTokens=%s" % (wire, recorded))
        self.assertEqual(wire, 16595)
        self.assertEqual(wire, recorded)

    def test_harness_records_write_RED_state(self):
        """DOCUMENTS the RED state (wire-present AND harness-dropped): the
        wire shows cache_write_tokens=16592 for this session, yet the harness
        usage.json records cacheCreationTokens=0 at BOTH the session level
        and in turns[0]. Turns green when the .101 cut lands and a re-minted
        fixture shows the write row (expected values then flip with it)."""
        self.assertEqual(self.usage_recorded["session"]["cacheCreationTokens"], 0)
        self.assertEqual(self.usage_recorded["turns"][0]["cacheCreationTokens"], 0)

    def test_sidecall_zero_write_row(self):
        """Control case: the 224-token warm side-call carries a zero write
        row on the wire (nothing for the harness to record either way)."""
        usage = self._usage(self.side_frame)
        self.assertEqual(usage["input_tokens"], 224)
        self.assertEqual(usage["input_tokens_details"]["cache_write_tokens"], 0)


def gate(capture_dir):
    """Post-cut acceptance gate over a live capture dir.

    For every response.completed frame with cache_write_tokens = N > 0
    (capture/resp-*.jsonl, recursive), require a usage row (top-level
    session object or a turns[] entry) in some usage.json under capture_dir
    with inputTokens == frame input_tokens AND cacheCreationTokens == N
    (byte-exact ints). Returns (passed, findings); findings are
    human-readable strings carrying file paths. A capture with no N>0
    frame is vacuous and fails.
    """
    findings = []
    frames = list(_iter_write_frames(capture_dir))
    usages = list(_iter_usage_rows(capture_dir))
    if not frames:
        findings.append("no response.completed frame with cache_write_tokens > 0 under %s (vacuous capture)" % capture_dir)
        return (False, findings)
    for path, frame_index, input_tokens, write_tokens in frames:
        matched = []
        row_descs = []
        for usage_path, rows in usages:
            for label, row in rows:
                row_descs.append("%s:%s(inputTokens=%s, cacheCreationTokens=%s)"
                                 % (usage_path, label, row.get("inputTokens"), row.get("cacheCreationTokens")))
                if row.get("inputTokens") == input_tokens and row.get("cacheCreationTokens") == write_tokens:
                    matched.append("%s:%s" % (usage_path, label))
        if not matched:
            finding = "no usage row with inputTokens==%s and cacheCreationTokens==%s for the response.completed frame in %s (frame_index=%s)" \
                      % (input_tokens, write_tokens, path, frame_index)
            if row_descs:
                finding += "; rows scanned: " + ", ".join(row_descs)
            else:
                finding += "; no usage.json found under %s" % capture_dir
            findings.append(finding)
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
        description="apex-ayl.101 CACHEWRITE-DTO offline fixture tests + post-cut gate",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gate", nargs="?", const=DEFAULT_GATE_REL, default=None, metavar="DIR",
                        help="run the gate over a capture dir instead of the fixture "
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
        print("GATE GREEN: every cache_write_tokens frame has a matching usage row (%s)" % resolved)
        return 0
    print("GATE RED: %d finding(s) in %s" % (len(findings), resolved))
    return 1


if __name__ == "__main__":
    sys.exit(main())
