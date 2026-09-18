#!/usr/bin/env python3
"""Offline test suite for the golden-kind wire assert + switch_model op
(apex-ayl.79 XW-JIG-1 ratchet ①; the XW-FIXTURES-1 op side, apex-ayl.70).

Run it (OFFLINE: no proxy key, no binary, no live calls, no cargo):

  python3 smoke/redteam/test_run_golden.py -v

TDD evidence map (house SDD: RED-first; captured artifacts in
smoke/redteam/report-r1/):

  GoldenMechanismTest.test_case_passes_schema_validation   RED-1 (mechanism)
      cases/golden-smoke-01.json (kind=golden + the companion count pin)
      must pass the frozen schema + in-tree contract gate.
  GoldenMechanismTest.test_check_wire_golden_passes        RED-1 (mechanism)
      check_wire handles kind=golden against the case's real-capture wire
      dir: the canonical full-body pin holds (the mechanism works end to
      end, selection + normalize + compare).
  GoldenTeethTest                                          RED-2 (teeth)
      Corrupt one fixture field in a scratch copy -> the golden assert
      FAILs with the structured field-level diff written to the case
      report dir; restore -> PASS. A pin that cannot fail is not a pin.
  GoldenSelectionTest
      The count-tolerance selection mechanics are REUSED, not re-invented:
      where-filter scoping (headers.x-grok-session-id keeps a foreign
      session's same-shape request out), nth=0 newest / last:0 oldest on
      the time-sorted filtered set, empty filtered set fails closed, any
      form fails closed on an empty set.
  GoldenCanonicalTest
      Canonical JSON semantics: object key order insensitive (sort_keys),
      list order SIGNIFICANT (input[] is the conversation in order — no
      order-insensitive list normalization exists and may not be
      invented), normalize nulls (list-index paths; absent paths no-op,
      but a declared path absent from this run's capture is named in
      the detail + diff doc — MINOR-1: the false-RED hazard is
      diagnosable, never silent).
  SwitchModelOpTest
      Op wiring: STEP_OPS/WIRE_KINDS membership, switch_model step
      validation, the storage-form cell diff engine, and the ONE-engine
      ruling (R1C: no cell_diff wire kind, no second wire comparator).
  GoldenCaseOfflineRunTest                                 GREEN-1
      The full runner executes GOLDEN-SMOKE-01 (zero-step headless case:
      no binary launch, no model call — the wiretap sits idle on
      loopback) and the row verdict is PASS with both wire pins ok.
"""
import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import run  # noqa: E402  (the runner under test)

CASE_FILE = os.path.join(HERE, "cases", "golden-smoke-01.json")
CASE_WIRE = os.path.join(HERE, "cases", "golden-smoke-01", "wire")
CAPTURE_NAME = "req-010.json"
FIXTURE_NAME = "golden-smoke-01-expected.json"


def _case():
    with open(CASE_FILE) as fh:
        return json.load(fh)


def _specs_by_kind(kind):
    return [s for s in _case()["assert"]["wire"]
            if s.get("kind") == kind]


def _golden_spec():
    specs = _specs_by_kind("golden")
    if len(specs) != 1:
        raise AssertionError("expected exactly one golden assert")
    return specs[0]


def _count_spec():
    specs = _specs_by_kind("count")
    if len(specs) != 1:
        raise AssertionError("expected exactly one companion count assert")
    return specs[0]


class _ScratchWire:
    """A scratch copy of the case's wire dir under <tmp>/wire — the same
    shape as a run dir (run_dir/wire), so the golden diff artifact lands
    in the scratch root exactly like the case report dir."""

    def __init__(self, extra=None):
        self.root = tempfile.mkdtemp(prefix="golden-smoke-scratch-")
        self.wire = os.path.join(self.root, "wire")
        shutil.copytree(CASE_WIRE, self.wire)
        for name, doc in (extra or {}).items():
            with open(os.path.join(self.wire, name), "w") as fh:
                json.dump(doc, fh, indent=2)

    def touch(self, name, ts):
        os.utime(os.path.join(self.wire, name), (ts, ts))

    def corrupt_fixture(self, field, value):
        p = os.path.join(self.wire, FIXTURE_NAME)
        with open(p) as fh:
            doc = json.load(fh)
        cur = doc
        toks = field.split(".")
        for t in toks[:-1]:
            cur = cur[int(t)] if isinstance(cur, list) else cur[t]
        last = toks[-1]
        if isinstance(cur, list):
            cur[int(last)] = value
        else:
            cur[last] = value
        with open(p, "w") as fh:
            json.dump(doc, fh, indent=2)

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


class GoldenMechanismTest(unittest.TestCase):
    def test_case_passes_schema_validation(self):
        errs = run.validate_case_file(CASE_FILE)
        self.assertEqual(
            errs, [],
            "case contract violations:\n" + "\n".join(errs))

    def test_check_wire_golden_passes(self):
        r = run.check_wire(_golden_spec(), CASE_WIRE)
        self.assertIsInstance(r, run.AssertResult)
        self.assertEqual(r.kind, "wire.golden")
        self.assertTrue(r.ok, r.detail)
        self.assertTrue(run._cite_resolvable(r.cite, CASE_WIRE),
                        "golden PASS must carry a resolvable citation")


class GoldenTeethTest(unittest.TestCase):
    def test_fixture_corruption_fails_with_structured_diff(self):
        scratch = _ScratchWire()
        try:
            scratch.corrupt_fixture("reasoning.summary", "detailed")
            r = run.check_wire(_golden_spec(), scratch.wire)
            self.assertFalse(r.ok, "corrupted fixture must FAIL the pin")
            diff_name = "golden-diff-%s.json" % _golden_spec().get("id",
                                                                   "unnamed")
            diff_path = os.path.join(scratch.root, diff_name)
            self.assertTrue(os.path.isfile(diff_path),
                            "structured diff must be written to the case "
                            "report dir (the TDD RED artifact)")
            with open(diff_path) as fh:
                diff = json.load(fh)
            self.assertEqual(diff["divergences"][0]["path"],
                             "reasoning.summary")
            self.assertEqual(diff["divergences"][0]["want"], "detailed")
            self.assertEqual(diff["divergences"][0]["got"], "concise")
            self.assertIn("reasoning.summary", r.detail)
        finally:
            scratch.cleanup()

    def test_capture_corruption_fails_too(self):
        scratch = _ScratchWire()
        try:
            p = os.path.join(scratch.wire, CAPTURE_NAME)
            with open(p) as fh:
                doc = json.load(fh)
            doc["body"]["input"][16]["content"] = (
                "CORRUPTED: the terminal user prompt was tampered with")
            with open(p, "w") as fh:
                json.dump(doc, fh, indent=2)
            r = run.check_wire(_golden_spec(), scratch.wire)
            self.assertFalse(r.ok)
            diff = json.load(open(os.path.join(
                scratch.root, "golden-diff-%s.json"
                % _golden_spec().get("id", "unnamed"))))
            self.assertTrue(
                diff["divergences"][0]["path"].startswith("input.16."),
                diff["divergences"][0]["path"])
        finally:
            scratch.cleanup()

    def test_restored_fixture_passes(self):
        scratch = _ScratchWire()
        try:
            r = run.check_wire(_golden_spec(), scratch.wire)
            self.assertTrue(r.ok, r.detail)
        finally:
            scratch.cleanup()


class GoldenSelectionTest(unittest.TestCase):
    def _foreign_capture(self, session_id):
        with open(os.path.join(CASE_WIRE, CAPTURE_NAME)) as fh:
            doc = json.load(fh)
        doc["headers"]["x-grok-session-id"] = session_id
        return doc

    def test_session_scope_excludes_foreign_capture(self):
        scratch = _ScratchWire(extra={
            "req-011.json": self._foreign_capture(
                "00000000-0000-4000-8000-000000000000")})
        try:
            scratch.touch("req-011.json", time.time() + 100)
            r = run.check_wire(_golden_spec(), scratch.wire)
            self.assertTrue(r.ok,
                            "the foreign session's same-shape request must "
                            "be filtered out (x-grok-session-id scope)")
            rc = run.check_wire(_count_spec(), scratch.wire)
            self.assertTrue(rc.ok,
                            "count scope must still bound N=1 with the "
                            "foreign capture present: %s" % rc.detail)
        finally:
            scratch.cleanup()

    def test_unscoped_where_catches_foreign_capture(self):
        scratch = _ScratchWire(extra={
            "req-011.json": self._foreign_capture(
                "00000000-0000-4000-8000-000000000000")})
        try:
            spec = copy.deepcopy(_count_spec())
            del spec["where"]["headers.x-grok-session-id"]
            r = run.check_wire(spec, scratch.wire)
            self.assertFalse(r.ok,
                             "without the session scope the where-filter "
                             "matches 2 -> the count pin must FAIL "
                             "(this is the foreign-request hazard the "
                             "scope exists for)")
        finally:
            scratch.cleanup()

    def test_nth_zero_newest_last_zero_oldest(self):
        scratch = _ScratchWire(extra={
            "req-012.json": self._foreign_capture(
                "01a09ca8-be72-7eb3-99d6-b58a09566587")})
        try:
            scratch.touch("req-012.json", time.time() + 100)
            p = os.path.join(scratch.wire, "req-012.json")
            with open(p) as fh:
                doc = json.load(fh)
            doc["body"]["reasoning"]["summary"] = "detailed"
            with open(p, "w") as fh:
                json.dump(doc, fh, indent=2)
            spec = copy.deepcopy(_golden_spec())
            r_newest = run.check_wire(spec, scratch.wire)
            self.assertFalse(r_newest.ok,
                             "nth=0 (default) selects the NEWEST "
                             "where-filtered capture (req-012)")
            spec["last"] = 0
            r_oldest = run.check_wire(spec, scratch.wire)
            self.assertTrue(r_oldest.ok,
                             "last:0 selects the OLDEST (req-010, intact)")
        finally:
            scratch.cleanup()

    def test_empty_filtered_set_fails_closed(self):
        spec = copy.deepcopy(_golden_spec())
        spec["where"]["headers.x-grok-session-id"] = (
            "00000000-0000-4000-8000-00000000dead")
        r = run.check_wire(spec, CASE_WIRE)
        self.assertFalse(r.ok,
                         "an empty filtered set is a harness failure, "
                         "never a vacuous pass")

    def test_any_form(self):
        scratch = _ScratchWire()
        try:
            spec = copy.deepcopy(_golden_spec())
            spec["any"] = True
            r = run.check_wire(spec, scratch.wire)
            self.assertTrue(r.ok, r.detail)
            spec["where"]["headers.x-grok-session-id"] = (
                "00000000-0000-4000-8000-00000000dead")
            r2 = run.check_wire(spec, scratch.wire)
            self.assertFalse(r2.ok,
                             "any form fails closed on an empty set")
        finally:
            scratch.cleanup()


class GoldenCanonicalTest(unittest.TestCase):
    def _capture(self, scratch, body, name="req-100.json",
                 model="qwen3.8-27b", session=None):
        doc = {"n": 100, "method": "POST", "path": "/v1/responses",
               "ts": "2026-09-17T00:00:00Z",
               "headers": {"x-grok-session-id": session or "sess-a"},
               "body": body}
        p = os.path.join(scratch.wire, name)
        with open(p, "w") as fh:
            json.dump(doc, fh, indent=2)
        return p

    def _engine(self, scratch, fixture, normalize, name="req-100.json"):
        return run.golden_compare(
            os.path.join(scratch.wire, name),
            os.path.join(scratch.wire, fixture),
            normalize)

    def test_key_order_insensitive_list_order_significant(self):
        scratch = _ScratchWire()
        try:
            body = {"model": "m", "stream": True,
                    "input": [{"type": "message", "role": "user",
                               "content": "first"},
                              {"type": "message", "role": "assistant",
                               "content": "second"}]}
            self._capture(scratch, body)
            with open(os.path.join(scratch.wire, "fx.json"), "w") as fh:
                json.dump({"stream": True, "model": "m", "input": [
                    {"role": "user", "type": "message", "content": "first"},
                    {"role": "assistant", "type": "message",
                     "content": "second"}]}, fh)
            ok, detail, diff = self._engine(scratch, "fx.json", [])
            self.assertTrue(ok, "key order must not matter: %s" % detail)
            self.assertIsNone(diff)
            swapped = copy.deepcopy(body)
            swapped["input"] = list(reversed(swapped["input"]))
            self._capture(scratch, swapped, name="req-101.json")
            ok2, detail2, diff2 = self._engine(
                scratch, "fx.json", [], name="req-101.json")
            self.assertFalse(ok2,
                             "list order is the conversation order — it "
                             "must be significant (no order-insensitive "
                             "list normalization)")
            self.assertIsNotNone(diff2)
        finally:
            scratch.cleanup()

    def test_normalize_nulls_list_index_path(self):
        scratch = _ScratchWire()
        try:
            body = {"model": "m",
                    "input": [{"type": "reasoning", "id": "rs_aaa",
                               "content": "x"},
                              {"type": "reasoning", "id": "rs_bbb",
                               "content": "y"}]}
            self._capture(scratch, body)
            fixture = copy.deepcopy(body)
            fixture["input"][0]["id"] = None
            with open(os.path.join(scratch.wire, "fx.json"), "w") as fh:
                json.dump(fixture, fh)
            ok, detail, diff = self._engine(scratch, "fx.json",
                                            ["body.input.0.id"])
            self.assertTrue(ok,
                            "the normalize path body.input.0.id must null "
                            "the id in the capture before compare: %s"
                            % detail)
            self.assertIsNone(diff)
        finally:
            scratch.cleanup()

    def test_normalize_absent_path_is_noop(self):
        scratch = _ScratchWire()
        try:
            body = {"model": "m", "input": []}
            self._capture(scratch, body)
            with open(os.path.join(scratch.wire, "fx.json"), "w") as fh:
                json.dump(body, fh)
            ok, detail, diff = self._engine(
                scratch, "fx.json", ["body.input.9.id",
                                     "body.prompt_cache_key"])
            self.assertTrue(ok,
                            "absent normalize paths are a no-op (both "
                            "sides): %s" % detail)
        finally:
            scratch.cleanup()

    def test_normalize_unapplied_path_is_loud(self):
        scratch = _ScratchWire()
        try:
            # Declared paths: body.model is in the capture (applied);
            # body.prompt_cache_key and body.input.0.id are absent
            # this run (unapplied — the volatile field was not sent).
            body = {"model": "m", "stream": True}
            self._capture(scratch, body)
            norm = ["body.model", "body.prompt_cache_key",
                    "body.input.0.id"]
            want = {"model": None, "stream": True}
            with open(os.path.join(scratch.wire, "fx.json"), "w") as fh:
                json.dump(want, fh)
            ok, detail, diff = self._engine(scratch, "fx.json", norm)
            self.assertTrue(
                ok,
                "unapplied normalize paths must not flip a matching "
                "body (no hard-fail): %s" % detail)
            self.assertIsNone(diff)
            self.assertIn(
                "[normalize 1/3 applied; unapplied: body.prompt_cache_key"
                ", body.input.0.id]", detail)
            # All-applied form: every declared path present -> the bare
            # [N/N applied] note, no unapplied clause.
            ok3, detail3, diff3 = self._engine(
                scratch, "fx.json", ["body.model"])
            self.assertTrue(ok3, detail3)
            self.assertIn("[normalize 1/1 applied]", detail3)
            # Same declarations with a REAL divergence -> FAIL (the
            # verdict is still correct), the unapplied note + the
            # absent-this-run diagnosis in the detail, and the diff doc
            # carrying the unapplied set in declaration order.
            want2 = {"model": None, "stream": False}
            with open(os.path.join(scratch.wire, "fx2.json"), "w") as fh:
                json.dump(want2, fh)
            ok2, detail2, diff2 = self._engine(
                scratch, "fx2.json", norm)
            self.assertFalse(ok2,
                             "a real divergence must still FAIL with "
                             "unapplied normalize paths present: %s"
                             % detail2)
            self.assertIn("[normalize 1/3 applied; unapplied: "
                          "body.prompt_cache_key, body.input.0.id]",
                          detail2)
            self.assertIn("the volatile field was absent this run",
                          detail2)
            self.assertEqual(
                diff2["normalize_unapplied"],
                ["body.prompt_cache_key", "body.input.0.id"])
        finally:
            scratch.cleanup()


class SwitchModelOpTest(unittest.TestCase):
    def test_step_ops_and_wire_kinds_membership(self):
        self.assertIn("switch_model", run.STEP_OPS)
        self.assertIn("golden", run.WIRE_KINDS)
        self.assertNotIn(
            "cell_diff", run.WIRE_KINDS,
            "R1C: cell_diff is retired as a kind — the switch_model op's "
            "wire hook calls the golden engine (one engine)")

    def test_validate_step_switch_model(self):
        good = {"op": "switch_model", "model": "claude-sonnet-5",
                "via": "acp", "cell": "smoke/xwfix/cells/az-vxm"}
        errs = []
        run._validate_step("x steps[0]", good, errs)
        self.assertEqual(errs, [])
        errs = []
        st = dict(good)
        del st["cell"]
        run._validate_step("x steps[0]", st, errs)
        self.assertTrue(any("cell" in e for e in errs), errs)
        errs = []
        st = dict(good, via="teleport")
        run._validate_step("x steps[0]", st, errs)
        self.assertTrue(any("via" in e for e in errs), errs)
        errs = []
        st = dict(good, assert_form="jsonl")
        run._validate_step("x steps[0]", st, errs)
        self.assertTrue(any("assert_form" in e for e in errs), errs)
        errs = []
        run._validate_step("x steps[0]",
                           dict(good, assert_form="wire"), errs)
        self.assertEqual(errs, [])

    def test_storage_form_cell_diff_engine(self):
        root = tempfile.mkdtemp(prefix="xwfix-cell-")
        try:
            cell_dir = os.path.join(root, "cell")
            session_dir = os.path.join(root, "session")
            os.makedirs(cell_dir)
            os.makedirs(session_dir)
            expected = [{"type": "system", "content": "pin"}]
            with open(os.path.join(cell_dir, "expected.json"), "w") as fh:
                json.dump(expected, fh)
            with open(os.path.join(session_dir, "chat_history.jsonl"),
                      "w") as fh:
                fh.write(json.dumps(expected[0]) + "\n")
            r = run.xwfix_cell_diff_storage(cell_dir, session_dir)
            self.assertTrue(r.ok, r.detail)
            with open(os.path.join(session_dir, "chat_history.jsonl"),
                      "w") as fh:
                fh.write(json.dumps({"type": "system",
                                     "content": "rewritten"}) + "\n")
            r2 = run.xwfix_cell_diff_storage(cell_dir, session_dir)
            self.assertFalse(r2.ok)
            self.assertIn("first_diff_index=0", r2.detail)
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_one_golden_engine(self):
        self.assertTrue(hasattr(run, "golden_compare"),
                        "the golden-kind compare function must exist "
                        "(the one engine)")
        self.assertFalse(hasattr(run, "xwfix_cell_diff_wire"),
                        "NIT-3/R1C: no second wire comparator — the op's "
                        "wire hook routes through golden_compare")


class GoldenCaseOfflineRunTest(unittest.TestCase):
    def test_full_runner_passes_the_case(self):
        out = tempfile.mkdtemp(prefix="golden-smoke-out-")
        try:
            proc = subprocess.run(
                [sys.executable, os.path.join(HERE, "run.py"),
                 "GOLDEN-SMOKE-01", "--out", out,
                 "--live-home", run.DEFAULT_LIVE_HOME],
                capture_output=True, text=True, timeout=300, cwd=HERE)
            report = os.path.join(out, "report.json")
            self.assertTrue(
                os.path.isfile(report),
                "runner output:\n%s\n%s" % (proc.stdout[-2000:],
                                             proc.stderr[-2000:]))
            with open(report) as fh:
                doc = json.load(fh)
            rows = [r for r in doc.get("rows", [])
                    if r.get("id") == "GOLDEN-SMOKE-01"]
            self.assertEqual(len(rows), 1,
                             "GOLDEN-SMOKE-01 row missing: %s"
                             % json.dumps(doc.get("rows"))[:1000])
            row = rows[0]
            self.assertEqual(row["status"], "PASS",
                             "asserts: %s" % json.dumps(
                                 row.get("asserts"), indent=1)[:2000])
            by_kind = {}
            for a in row.get("asserts", []):
                by_kind[a.get("kind")] = a
            self.assertTrue(by_kind["wire.golden"]["ok"],
                            by_kind.get("wire.golden"))
            self.assertTrue(by_kind["wire.count"]["ok"],
                            by_kind.get("wire.count"))
        finally:
            shutil.rmtree(out, ignore_errors=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
