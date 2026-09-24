"""Formalism adapter tests (Task 4, Bead apex-ayl.137).

Parses the parity-formalism corpus into namespaced, source-linked entities
(DESIGN.md §3): the 10 Python-linter rules (H-1..H-6, S-1..S-4), H-7 with
its three enforcement-layer states and three Rust projection-seam symbol
locators, 14 EV records, three boundary classes, 17 fixture arms, 67 request
expectations, 15 adjudications, C1..C7 from the single FORMALISM §2 category
table, typed EV/document/artifact/open-question citations, H-2/H-5 current +
superseded clause entities joined with ``supersedes`` edges, the generated-
rule coupling gate (pure ``derive()`` seam only, never the generator CLI or
the linter), resolvable xwavec71 / projection-test pins, and the adversarial
extraction behavior.

Isolated-copy mutations run on /tmp copies of the real corpus; the real
corpus and worktree are never written. The no-write proof is a complete
non-following byte/link snapshot compared before and after within one run.

Run from the worktree root:
    PYTHONDONTWRITEBYTECODE=1 \
        python3.14 -m unittest -v smoke.eval_harness.tests.test_formalism
"""
from __future__ import annotations

import copy
import hashlib
import json
import os
import re
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path

from smoke.eval_harness import contract, paths
from smoke.eval_harness.adapters import formalism

# --- root resolution --------------------------------------------------------
# Never hardcoded machine paths: the worktree root is the repository root
# (three levels above this file), plans is the sibling campaign plans root
# exactly as DESIGN.md §5 invokes it from the worktree root (../../grok/plans).
# Environment overrides exist for operators with a nonstandard layout.


def _worktree_root() -> Path:
    override = os.environ.get("EVAL_WORKTREE_ROOT")
    if override:
        return Path(override)
    return Path(__file__).resolve().parents[3]


def _plans_root() -> Path:
    override = os.environ.get("EVAL_PLANS_ROOT")
    if override:
        return Path(override)
    return _worktree_root() / ".." / ".." / "grok" / "plans"


FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures" / "formalism"

# --- DESIGN.md §3 cardinality and semantic pins -----------------------------

PIN_RULE_IDS = ["H-1", "H-2", "H-3", "H-4", "H-5", "H-6",
                "S-1", "S-2", "S-3", "S-4"]
PIN_EV_IDS = [f"EV-{n}" for n in range(1, 15)]
PIN_BOUNDARY_CLASSES = ["AzStrict", "VLLenient", "Vertex"]
PIN_FIXTURE_ARM_COUNT = 17
PIN_REQUEST_COUNT = 67
PIN_ADJUDICATION_COUNT = 15
PIN_CATEGORY_IDS = ["C1", "C2", "C3", "C4", "C5", "C6", "C7"]
PIN_KINDS = ["verbatim-copy", "document-sourced", "record-sourced-stub",
             "synthesized"]
PIN_H7_LAYERS = {
    "A1": "excluded-pending-T8-EV-12",
    "A2": "not-applicable-history-shape",
    "A3": "enforced-by-rust-projection-seam-test",
}
PIN_H7_SYMBOLS = [
    "xw_proj_orphaned_result_direction",
    "xw_proj_surviving_call_keeps_result",
    "xw_proj_carrier_survival",
]

# Worktree generated/target paths (catalog formalism-linter row declares the
# same relative paths; the tests use the design-pinned literals).
CRATE = "crates/codegen/xai-grok-sampling-types"
RULES_GENERATED_RS = f"{CRATE}/src/conversation/rules_generated.rs"
PROJECTION_TESTS_RS = f"{CRATE}/src/conversation/projection_tests.rs"
HARD_RULES_JSON = f"{CRATE}/fixtures/outbound_lint/hard_rules.json"
OUTBOUND_LINT_DIR = f"{CRATE}/fixtures/outbound_lint"

# Plans corpus relative paths.
RULES_JSON = "parity-formalism/tools/invariant_rules.json"
EXPECTED_VERDICTS_JSON = "parity-formalism/tools/expected_verdicts_ev12.json"
GENERATOR_PY = "parity-formalism/tools/generate_outbound_lint_rules.py"
LINTER_PY = "parity-formalism/tools/invariant_lint.py"
FORMALISM_MD = "parity-formalism/FORMALISM.md"
HARDENING_MD = "parity-formalism/HARDENING-SPEC.md"
QA_MD = "parity-formalism/Q-A.md"
EXEMPLARS_MD = "parity-formalism/EXEMPLARS.md"
INTEL_02_MD = "parity-formalism/intel/02-qwen-codexfam-reasoning-parity.md"
XWAVEC71_REPORT = "xwavec71/wavec71-green-report-qwen-20260918.md"
SDD_71 = "xwire/sdd-71-projector.md"

HEADING_CATEGORY = '2. Category decomposition (the "math" of a parity diff)'
HEADING_2_1 = ("2.1 Hard invariants (I^h; default artifact set A1 + A2 + A3 "
               "— a row may override, see H-7)")
HEADING_2_2A = ("2.2a Strict-row enc re-scope — exact clauses "
                "(T8 OQ-T8-1/2 adjudication, 2026-09-23; bead apex-ayl.126.8.5)")

ARTIFACT_RULES_JSON = f"artifact:plans:{RULES_JSON}"
ARTIFACT_GENERATOR = f"artifact:plans:{GENERATOR_PY}"
ARTIFACT_LINTER = f"artifact:plans:{LINTER_PY}"
ARTIFACT_RULES_GENERATED = f"artifact:worktree:{RULES_GENERATED_RS}"
ARTIFACT_PROJECTION_TESTS = f"artifact:worktree:{PROJECTION_TESTS_RS}"
ARTIFACT_HARD_RULES = f"artifact:worktree:{HARD_RULES_JSON}"
ARTIFACT_XWAVEC71 = f"artifact:plans:{XWAVEC71_REPORT}"
ARTIFACT_SDD_71 = f"artifact:plans:{SDD_71}"


# --- shared helpers ---------------------------------------------------------


def _ctx(plans: Path, worktree: Path) -> contract.AdapterContext:
    return contract.AdapterContext(
        roots={"worktree": worktree, "plans": plans},
        catalog=contract.load_catalog(),
    )


def _discover(plans: Path, worktree: Path):
    return formalism.discover(_ctx(plans, worktree), ())


def _by_kind(res, kind: str) -> list:
    return [e for e in res.entities if e.get("kind") == kind]


def _by_id(res, entity_id: str):
    matches = [e for e in res.entities if e.get("id") == entity_id]
    assert len(matches) <= 1, f"duplicate entity id {entity_id!r}"
    return matches[0] if matches else None


def _rels(res, kind: str) -> list:
    return [r for r in res.relationships if r.get("kind") == kind]


def _findings(res, code: str) -> list:
    return [f for f in res.findings if f.get("code") == code]


def _hard_findings(res) -> list:
    return [f for f in res.findings if f.get("impact") == "hard"]


def _serialize(res) -> str:
    return json.dumps(
        {
            "entities": list(res.entities),
            "relationships": list(res.relationships),
            "runs": list(res.runs),
            "findings": list(res.findings),
        },
        sort_keys=True,
        ensure_ascii=False,
    )


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _snapshot_tree(root: Path) -> dict:
    """Complete non-following byte/link snapshot of one tree.

    Every directory, regular file (full SHA-256 + size + mode), and symlink
    (raw link string) is recorded; ``__pycache__`` is included, never
    followed. Deterministic and exhaustive (DESIGN.md §8 no-write proof).
    """
    out: dict[str, tuple] = {}
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        base = Path(dirpath)
        for name in dirnames:
            out[f"dir/{(base / name).relative_to(root).as_posix()}"] = ("dir",)
        for name in sorted(filenames):
            p = base / name
            rel = p.relative_to(root).as_posix()
            st = os.lstat(p)
            if stat.S_ISLNK(st.st_mode):
                out[rel] = ("link", os.readlink(p))
            elif stat.S_ISREG(st.st_mode):
                out[rel] = ("file", st.st_mode, st.st_size,
                            hashlib.sha256(p.read_bytes()).hexdigest())
            else:
                out[rel] = ("other", st.st_mode)
    return out


def _snapshot_corpus(plans: Path) -> dict:
    """Snapshot the complete plans-corpus trees (plans-corpus scope)."""
    out: dict[str, tuple] = {}
    for tree in ("parity-formalism", "provenance", "xwire"):
        root = plans / tree
        if root.is_dir():
            for rel, value in _snapshot_tree(root).items():
                out[f"{tree}/{rel}"] = value
    return out


def _copy_corpus(tag: str) -> tuple[Path, Path]:
    """Copy the discovery-relevant corpus slice to a fresh /tmp tree.

    Returns ``(plans_root, worktree_root)``. The real corpus is never
    mutated; every test that mutates works on its own fresh copy.
    """
    base = Path(tempfile.mkdtemp(prefix=f"formalism-{tag}-"))
    plans = base / "plans"
    worktree = base / "worktree"
    shutil.copytree(_plans_root() / "parity-formalism",
                    plans / "parity-formalism", symlinks=True)
    (plans / "xwavec71").mkdir(parents=True)
    shutil.copy2(_plans_root() / XWAVEC71_REPORT, plans / XWAVEC71_REPORT)
    (plans / "xwire").mkdir(parents=True)
    shutil.copy2(_plans_root() / SDD_71, plans / SDD_71)
    for rel in (RULES_GENERATED_RS, PROJECTION_TESTS_RS, OUTBOUND_LINT_DIR):
        dest = worktree / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(_worktree_root() / rel, dest, symlinks=True) \
            if (_worktree_root() / rel).is_dir() \
            else shutil.copy2(_worktree_root() / rel, dest)
    return plans, worktree


# --- extraction unit tests (committed synthetic fixtures) --------------------


class ExtractionUnitTest(unittest.TestCase):
    def setUp(self):
        self.heading_text = (FIXTURES_DIR / "heading-doc.md").read_text(
            encoding="utf-8")
        self.question_text = (FIXTURES_DIR / "question-doc.md").read_text(
            encoding="utf-8")

    def test_extract_heading_range_single_match(self):
        # The range spans from the heading line through the line before the
        # next heading line of any level.
        rng = formalism.extract_heading_range(self.heading_text, "B. Beta")
        self.assertEqual(rng, "## B. Beta\n\nBeta body.\n")

    def test_extract_heading_range_stops_at_next_heading_any_level(self):
        # The committed fixture has a duplicate "A. Alpha" on purpose for the
        # duplicate test; the unique sub-heading's range still ends at the
        # next heading line of any level.
        rng = formalism.extract_heading_range(self.heading_text,
                                              "A.1 Alpha sub")
        self.assertEqual(rng, "### A.1 Alpha sub\n\nSub body line.\n")

    def test_extract_heading_requires_exact_normalized_text(self):
        for near_miss in ("a. Alpha", "A. Alpha ", "A. AlphA",
                          "T. Root extra", "B. Betas", "  B. Beta"):
            with self.assertRaises(
                    formalism.SelectorError, msg=repr(near_miss)):
                formalism.extract_heading_range(self.heading_text, near_miss)

    def test_extract_heading_range_missing_raises(self):
        with self.assertRaises(formalism.SelectorError) as ctx:
            formalism.extract_heading_range(self.heading_text, "C. Gamma")
        self.assertEqual(ctx.exception.reason, "missing")

    def test_extract_heading_range_duplicate_raises(self):
        with self.assertRaises(formalism.SelectorError) as ctx:
            formalism.extract_heading_range(self.heading_text, "A. Alpha")
        self.assertEqual(ctx.exception.reason, "duplicate")

    def test_extract_question_range_single_match_to_next_marker(self):
        # The range starts at the marker line (from ``**Q:``) and extends
        # through the line before the next ``**Q:`` marker.
        marker = 'Q: "First question — spanning two lines?"'
        rng = formalism.extract_question_range(self.question_text, marker)
        self.assertEqual(rng,
                         '**Q: "First question —\nspanning two lines?"**\n'
                         "A: First answer.\n")

    def test_extract_question_range_last_marker_extends_to_eof(self):
        marker = 'Q: "Second question?"'
        rng = formalism.extract_question_range(self.question_text, marker)
        self.assertIn("A: Second answer.", rng)
        self.assertIn("**Carry-forward notes** (tail):", rng)
        self.assertTrue(rng.endswith("tail content.\n"))

    def test_extract_question_range_missing_raises(self):
        with self.assertRaises(formalism.SelectorError) as ctx:
            formalism.extract_question_range(
                self.question_text, 'Q: "Third question?"')
        self.assertEqual(ctx.exception.reason, "missing")

    def test_extract_question_range_duplicate_raises(self):
        text = self.question_text + (
            '\n**Q: "Second question?"**\nA: duplicate.\n')
        with self.assertRaises(formalism.SelectorError) as ctx:
            formalism.extract_question_range(text, 'Q: "Second question?"')
        self.assertEqual(ctx.exception.reason, "duplicate")

    def test_question_marker_normalization_collapses_internal_whitespace(self):
        text = self.question_text
        # The raw marker spans two lines; normalization collapses the wrap.
        self.assertIn('**Q: "First question —\nspanning two lines?"**', text)
        rng = formalism.extract_question_range(
            text, 'Q: "First question — spanning two lines?"')
        self.assertTrue(rng)

    def test_snippet_under_cap_not_truncated(self):
        text = "small curated range\n"
        snippet = formalism.build_snippet(text, source_sha256="0" * 64,
                                          locator={"root": "plans",
                                                   "path": "a.md"})
        self.assertEqual(snippet["truncated"], False)
        self.assertEqual(snippet["bytes"], len(text.encode("utf-8")))
        self.assertEqual(snippet["text"], text)
        self.assertEqual(snippet["source_sha256"], "0" * 64)
        self.assertEqual(snippet["locator"], {"root": "plans", "path": "a.md"})

    def test_snippet_over_cap_truncates_at_codepoint_boundary(self):
        # 70_000 two-byte code points (140_000 UTF-8 bytes) force a cut in
        # the middle of the buffer; the cut may only land on a code-point
        # boundary and must stay within the 65,536-byte cap.
        text = "é" * 70_000
        cap = formalism.SNIPPET_CAP_BYTES
        self.assertEqual(cap, 65_536)
        snippet = formalism.build_snippet(text, source_sha256="1" * 64,
                                          locator={"root": "plans",
                                                   "path": "b.md"})
        self.assertTrue(snippet["truncated"])
        self.assertEqual(snippet["bytes"], len(text.encode("utf-8")))
        out = snippet["text"].encode("utf-8")
        self.assertLessEqual(len(out), cap)
        self.assertGreater(len(out), cap - 4)  # cut happened, not early
        self.assertEqual(snippet["text"], "é" * (len(out) // 2))
        # A 65,536-byte cut of 2-byte chars lands exactly on a boundary;
        # force an odd case with a trailing 3-byte char sequence.
        text3 = "é" * 32_767 + "€"  # 65,534 + 3 = 65,537 bytes > cap
        snippet3 = formalism.build_snippet(text3, source_sha256="2" * 64,
                                           locator={"root": "plans",
                                                    "path": "c.md"})
        out3 = snippet3["text"].encode("utf-8")
        self.assertLessEqual(len(out3), cap)
        snippet3["text"].encode("utf-8").decode("utf-8")  # must round-trip

    def test_snippet_carries_original_byte_count(self):
        text = "abcé"
        raw = text.encode("utf-8")
        snippet = formalism.build_snippet(text, source_sha256="3" * 64,
                                          locator={"root": "plans",
                                                   "path": "d.md"})
        self.assertEqual(snippet["bytes"], len(raw))
        self.assertEqual(snippet["bytes"], 5)


# --- real-corpus semantic pins ------------------------------------------------


class RealCorpusPinTest(unittest.TestCase):
    """Assert the DESIGN.md §3 pins against the live corpus.

    The corpus is live: only design-pinned semantic counts are asserted,
    never file counts/sizes/mtimes. Discovery runs once per class.
    """

    @classmethod
    def setUpClass(cls):
        cls.plans = _plans_root()
        cls.worktree = _worktree_root()
        assert (cls.plans / "parity-formalism").is_dir(), \
            f"plans root missing: {cls.plans} (set EVAL_PLANS_ROOT)"
        assert cls.worktree.is_dir(), \
            f"worktree root missing: {cls.worktree} (set EVAL_WORKTREE_ROOT)"
        cls.res = _discover(cls.plans, cls.worktree)
        cls.catalog = contract.load_catalog()

    # -- cardinality pins ----------------------------------------------------

    def test_ten_python_linter_rules(self):
        rules = [e for e in _by_kind(self.res, "rule")
                 if e.get("status") == "current"
                 and e.get("source", {}).get("path") == RULES_JSON]
        self.assertEqual(sorted(r["rule_id"] for r in rules),
                         sorted(PIN_RULE_IDS))
        self.assertEqual(len(rules), 10)

    def test_h7_rule_with_three_exact_layer_states(self):
        h7 = _by_id(self.res, "rule:H-7")
        self.assertIsNotNone(h7, "H-7 rule entity missing")
        layers = {l["layer"]: l["state"]
                  for l in h7.get("enforcement_layers", [])}
        self.assertEqual(layers, PIN_H7_LAYERS)
        self.assertEqual(len(h7["enforcement_layers"]), 3)

    def test_h7_three_rust_symbol_locators(self):
        enforced = [r for r in _rels(self.res, "enforced-by")
                    if r["source"] == "rule:H-7"]
        self.assertEqual(len(enforced), 3)
        self.assertEqual({r["locator"]["symbol"] for r in enforced},
                         set(PIN_H7_SYMBOLS))
        self.assertTrue(all(r["target"] == ARTIFACT_PROJECTION_TESTS
                            for r in enforced))
        self.assertTrue(all(r["locator"]["root"] == "worktree"
                            and r["locator"]["path"] == PROJECTION_TESTS_RS
                            for r in enforced))

    def test_fourteen_evidence_records(self):
        evs = _by_kind(self.res, "evidence-record")
        # Numeric sort: lexicographic order would mis-pin EV-10..EV-14
        # against the natural EV-1..EV-14 sequence.
        self.assertEqual(
            sorted((e["ev_id"] for e in evs), key=lambda v: int(v.split("-")[1])),
            PIN_EV_IDS)
        self.assertEqual(len(evs), 14)

    def test_three_boundary_classes(self):
        bounds = _by_kind(self.res, "boundary-class")
        self.assertEqual(sorted(b["name"] for b in bounds),
                         PIN_BOUNDARY_CLASSES)

    def test_seventeen_fixture_arms(self):
        arms = _by_kind(self.res, "fixture-arm")
        self.assertEqual(len(arms), PIN_FIXTURE_ARM_COUNT)
        ev_arms = [e for e in _by_kind(self.res, "request-expectation")]
        self.assertEqual(len({a["fixture_id"] for a in arms}), 17)
        self.assertTrue(ev_arms)

    def test_meta_kind_exact_four_values(self):
        arms = {a["fixture_id"]: a for a in _by_kind(self.res, "fixture-arm")}
        seen = {a["native_kind"] for a in arms.values()}
        self.assertEqual(seen, set(PIN_KINDS))
        # Every arm's native kind matches its real meta.json byte-for-byte.
        for arm_id, arm in arms.items():
            meta = self.plans / "parity-formalism" / "tools" / "fixtures" \
                / arm_id / "meta.json"
            self.assertEqual(arm["native_kind"],
                             json.loads(meta.read_text("utf-8"))["kind"],
                             arm_id)

    def test_sixty_seven_request_expectations(self):
        reqs = _by_kind(self.res, "request-expectation")
        self.assertEqual(len(reqs), PIN_REQUEST_COUNT)
        for req in reqs:
            m = re.fullmatch(r"request:([a-z0-9-]+):(\d{3})", req["id"])
            self.assertTrue(m, req["id"])
            self.assertEqual(req["fixture_id"], m.group(1))
            self.assertEqual(req["n"], int(m.group(2)))

    def test_fifteen_adjudications(self):
        adjs = _by_kind(self.res, "adjudication")
        self.assertEqual(len(adjs), PIN_ADJUDICATION_COUNT)
        oq = _by_id(self.res, "adjudication:OQ-T8-1")
        self.assertEqual(oq["fixture_id"], "sight-1-mxai-c04")
        self.assertEqual(oq["n"], 4)
        self.assertEqual(oq["rule"], "H-2")
        self.assertEqual(oq["hits"], 7)
        self.assertEqual(oq["class"], "superseded-scope-artifact")

    def test_categories_c1_c7_from_single_table(self):
        cats = _by_kind(self.res, "category")
        self.assertEqual(sorted(c["category_id"] for c in cats),
                         PIN_CATEGORY_IDS)
        names = {c["category_id"]: c["name"] for c in cats}
        self.assertEqual(names["C1"], "Envelope")
        self.assertEqual(names["C7"], "Nondeterminism envelope")
        # All seven come from the single FORMALISM §2 heading range.
        self.assertTrue(all(
            c.get("source", {}).get("heading") == HEADING_CATEGORY
            for c in cats))
        section_ids = {_by_id(self.res,
                              f"section:plans:{FORMALISM_MD}:{_selector_hash(HEADING_CATEGORY)}")["id"]}
        section = _by_id(self.res,
                         f"section:plans:{FORMALISM_MD}:{_selector_hash(HEADING_CATEGORY)}")
        self.assertIsNotNone(section, "FORMALISM §2 section entity missing")
        defines = [r for r in _rels(self.res, "defines")
                   if r["target"].startswith("category:")]
        self.assertEqual({r["source"] for r in defines}, section_ids)
        self.assertEqual(len(defines), 7)

    # -- typed citations -------------------------------------------------------

    def test_typed_evidence_citations(self):
        h1 = _by_id(self.res, "rule:H-1")
        self.assertEqual(h1["citations"],
                         [{"kind": "evidence-id", "id": "EV-1"}])
        h2 = _by_id(self.res, "rule:H-2")
        self.assertEqual(
            sorted(c["id"] for c in h2["citations"]), ["EV-13", "EV-9"])

    def test_typed_document_anchor_citations(self):
        s2 = _by_id(self.res, "rule:S-2")
        anchors = [c for c in s2["citations"]
                   if c["kind"] == "document-anchor"]
        self.assertEqual(len(anchors), 2)
        paths = {a["path"] for a in anchors}
        self.assertIn("parity-formalism/intel/02-qwen-codexfam-reasoning-"
                      "parity.md", paths)
        self.assertIn("parity-formalism/EXEMPLARS.md", paths)
        for anchor in anchors:
            self.assertEqual(anchor["root"], "plans")
            self.assertTrue(anchor["heading"])
        s3 = _by_id(self.res, "rule:S-3")
        obs = [c for c in s3["citations"]
               if c["kind"] == "document-anchor"]
        self.assertEqual(
            {a["heading"] for a in obs},
            {"1. Envelope diff (C1)",
             "(d) Headers + body envelope diff, `/v1/responses` POSTs"})

    def test_typed_open_question_citation(self):
        arm = _by_id(self.res, "fixture:mgw-toolctl-01")
        # OQ-f is a typed open-question citation in the arm's stamps (both
        # the meta.json ev_stamps and the verdict table); it must not be
        # relabeled as an unknown EV id.
        all_stamps = ([s for v in arm.get("ev_stamps", {}).values()
                       for s in v]
                      + list(arm.get("expected_ev_stamps", [])))
        self.assertTrue(any(s.startswith("OQ-f") for s in all_stamps),
                        all_stamps)
        self.assertIsNone(_by_id(self.res, "evidence:OQ-f"),
                          "OQ-f must not be relabeled as an EV id")
        cites = {r["target"] for r in _rels(self.res, "cites")
                 if r["source"] == "fixture:mgw-toolctl-01"}
        h23 = ("section:plans:" + HARDENING_MD + ":" + _selector_hash(
            "2.3 Open questions — NOT enforced (wire evidence owed first)"))
        self.assertIn(h23, cites,
                      "OQ-f must resolve to the §2.3 section (its namespace)")

    def test_typed_artifact_locator_citations(self):
        oq = _by_id(self.res, "adjudication:OQ-T8-1")
        ev = oq["evidence"]
        first = ev[0]
        self.assertEqual(first, {
            "kind": "artifact-locator",
            "root": "plans",
            "path": "parity-formalism/tools/fixtures/sight-1-mxai-c04/"
                    "wire/req-004.json",
            "request_n": 4,
        })
        self.assertEqual(len(ev), 3)
        for item in ev[1:]:
            self.assertEqual(item["kind"], "artifact-locator")
            self.assertNotIn("request_n", item)

    # -- supersession ----------------------------------------------------------

    def test_h2_h5_current_and_superseded_entities_with_edges(self):
        for rule_id in ("H-2", "H-5"):
            current = _by_id(self.res, f"rule:{rule_id}")
            superseded = _by_id(self.res, f"rule:{rule_id}:superseded")
            self.assertIsNotNone(current, rule_id)
            self.assertIsNotNone(superseded, f"{rule_id}:superseded")
            self.assertIn("SUPERSEDED-INTERIM", superseded["clause"])
            # Both generations carry locators into the authoritative §2.2a.
            for entity in (current, superseded):
                headings = {loc.get("heading")
                            for loc in entity.get("locators", [])}
                self.assertIn(HEADING_2_2A, headings, rule_id)
        sup = _rels(self.res, "supersedes")
        self.assertEqual(
            {(r["source"], r["target"]) for r in sup},
            {("rule:H-2", "rule:H-2:superseded"),
             ("rule:H-5", "rule:H-5:superseded")})
        self.assertTrue(all(r.get("locator", {}).get("heading") == HEADING_2_2A
                            for r in sup))

    # -- generated-rule artifacts ------------------------------------------------

    def test_generated_links_exactly_two_targets(self):
        generated = [e for e in _by_kind(self.res, "artifact")
                     if e.get("generation_state") == "declared-generated"]
        self.assertEqual(
            {e["id"] for e in generated},
            {ARTIFACT_RULES_GENERATED, ARTIFACT_HARD_RULES})
        gen_from = _rels(self.res, "generated-from")
        self.assertEqual(
            {(r["source"], r["target"]) for r in gen_from},
            {(ARTIFACT_RULES_GENERATED, ARTIFACT_RULES_JSON),
             (ARTIFACT_RULES_GENERATED, ARTIFACT_GENERATOR),
             (ARTIFACT_HARD_RULES, ARTIFACT_RULES_JSON),
             (ARTIFACT_HARD_RULES, ARTIFACT_GENERATOR)})

    def test_core_artifact_roles_survive_catalog_dedup(self):
        # The catalog walk registers the same paths with default roles
        # first; the design-pinned explicit roles must survive dedup
        # instead of being swallowed by the early return.
        for aid, role in (
            (ARTIFACT_RULES_JSON, "rule-source"),
            (ARTIFACT_GENERATOR, "tool-source"),
            (ARTIFACT_LINTER, "tool-source"),
            (ARTIFACT_RULES_GENERATED, "generated-target"),
            (ARTIFACT_HARD_RULES, "generated-target"),
        ):
            entity = _by_id(self.res, aid)
            self.assertIsNotNone(entity, aid)
            self.assertEqual(entity.get("role"), role, aid)

    def test_source_dirs_are_rooted_externalized_or_omitted(self):
        # M-B: raw source_dir strings are never copied. Path-shaped
        # values beneath no named root become a soft finding plus hash
        # with the value omitted; absolute tokens are externalized with
        # the same finding; free-text kind annotations pass through as
        # safe references (coordinator adjudication).
        arms = _by_kind(self.res, "fixture-arm")
        self.assertEqual(len(arms), 17)
        unrooted = _findings(self.res, "formalism-source-dir-unrooted")
        self.assertEqual(len(unrooted), 10)
        for f in unrooted:
            self.assertEqual(f["level"], "warning")
            self.assertEqual(f["impact"], "soft")
            self.assertEqual(len(f["detail"][0]["sha256"]), 64)
        text_dirs = [a["source_dir"] for a in arms
                     if isinstance(a.get("source_dir"), str)]
        ext_dirs = [a["source_dir"] for a in arms
                    if isinstance(a.get("source_dir"), list)]
        omitted = [a for a in arms if a.get("source_dir") is None]
        self.assertEqual((len(text_dirs), len(ext_dirs), len(omitted)),
                         (7, 1, 9))
        path_like = re.compile(
            r"^[A-Za-z0-9._*%-]+(?:/[A-Za-z0-9._*%-]+)*/?$")
        for sd in text_dirs:
            self.assertFalse(path_like.fullmatch(sd), sd)
        for tokens in ext_dirs:
            self.assertTrue(all(t.startswith("@external/")
                                for t in tokens))
        # The raw relative report paths are gone from source_dir (they
        # survive only as provenance text, a separate channel).
        for a in arms:
            sd = a.get("source_dir")
            if isinstance(sd, str):
                self.assertNotIn(
                    "wt/grok-build-responses/smoke/redteam/", sd)
            elif isinstance(sd, list):
                self.assertTrue(all(
                    "wt/grok-build-responses/smoke/redteam/" not in t
                    for t in sd))

    def test_remaining_outbound_lint_payloads_are_authored(self):
        base = self.worktree / OUTBOUND_LINT_DIR
        payload_files = []
        for dirpath, _dirnames, filenames in os.walk(base, followlinks=False):
            for name in filenames:
                p = Path(dirpath) / name
                rel = p.relative_to(self.worktree).as_posix()
                if rel != HARD_RULES_JSON:
                    payload_files.append(rel)
        self.assertTrue(payload_files)
        for rel in payload_files:
            entity = _by_id(self.res, f"artifact:worktree:{rel}")
            self.assertIsNotNone(entity, rel)
            self.assertEqual(entity["generation_state"], "source-authored",
                             rel)
            self.assertNotEqual(entity.get("role"), "generated-source")

    def test_boundary_map_symbolic_values_and_source_locators(self):
        rules_artifact = _by_id(self.res, ARTIFACT_RULES_JSON)
        bmap = rules_artifact.get("boundary_map", {})
        self.assertEqual(bmap.get("symbolic_values", {}).get("xai_prefix"),
                         "grok")
        self.assertEqual(
            bmap.get("symbolic_values", {}).get("openai_prefix"), "gpt-")
        src = bmap.get("source_files", {})
        self.assertEqual(
            src.get("model_boundary_class"),
            {"root": "worktree",
             "path": f"{CRATE}/src/conversation/projection.rs"})
        self.assertEqual(
            src.get("catalog_family"),
            {"root": "worktree", "path": f"{CRATE}/src/catalog_wire.rs"})
        self.assertEqual(
            src.get("is_anthropic_model"),
            {"root": "worktree",
             "path": f"{CRATE}/src/messages_model.rs"})

    def test_xwavec71_and_projection_pins_resolvable(self):
        report = _by_id(self.res, ARTIFACT_XWAVEC71)
        self.assertIsNotNone(report, "xwavec71 report artifact missing")
        sdd = _by_id(self.res, ARTIFACT_SDD_71)
        self.assertIsNotNone(sdd, "sdd-71 artifact missing")
        h7 = _by_id(self.res, "rule:H-7")
        pins = {p["path"]: p for p in h7.get("code_pins", [])}
        self.assertIn(XWAVEC71_REPORT, pins)
        self.assertEqual(pins[XWAVEC71_REPORT]["root"], "plans")
        self.assertEqual(pins[XWAVEC71_REPORT].get("line"), 75)
        self.assertIn(SDD_71, pins)
        # Resolvable: the report has a 75th line and sdd-71 exists.
        lines = (self.plans / XWAVEC71_REPORT).read_text("utf-8").split("\n")
        self.assertGreaterEqual(len(lines), 75)
        self.assertTrue((self.plans / SDD_71).is_file())
        proj = _by_id(self.res, ARTIFACT_PROJECTION_TESTS)
        self.assertEqual(proj.get("symbols"), PIN_H7_SYMBOLS)
        # Symbols verified without emitting Rust bodies: the bare names are
        # present, no Rust source text is.
        blob = _serialize(self.res)
        for symbol in PIN_H7_SYMBOLS:
            self.assertIn(symbol, blob)
        self.assertNotIn("fn xw_proj_", blob)
        self.assertNotIn("#[test]", blob)

    # -- corpus hygiene ---------------------------------------------------------

    def test_real_corpus_yields_no_hard_findings(self):
        hard = _hard_findings(self.res)
        self.assertEqual(
            hard, [],
            "unexpected hard findings on the unmutated corpus: "
            + json.dumps([{k: f.get(k) for k in
                           ("code", "component", "level", "impact", "source")}
                          for f in hard], ensure_ascii=False))

    def test_discover_emits_no_runs(self):
        self.assertEqual(self.res.runs, ())

    def test_no_wire_bodies_frames_or_errors_in_semantic_output(self):
        blob = _serialize(self.res)
        # (1) A distinctive interior window of a real fixture wire body must
        # never enter the index.
        req1 = (self.plans / "parity-formalism" / "tools" / "fixtures"
                / "ev3-empty-id-400" / "wire" / "req-001.json").read_bytes()
        window = req1[400:480].decode("utf-8", errors="ignore")
        self.assertNotIn(window, blob)
        # (2) Worktree body-fixture prompt text is never copied.
        body = (self.worktree / OUTBOUND_LINT_DIR / "bodies"
                / "h1-accept-cw1-req007-EV-1.json").read_text("utf-8")
        self.assertNotIn("You are Grok released by xAI", blob)
        # (3) meta.json non-allowlisted error/observation text is excluded:
        # the full verbatim_400 body (longer than the EV-3 fact truncation)
        # and the live_behavior observation.
        self.assertNotIn(
            "but this value contained additional characters. "
            "(litellm: AzureException BadRequestError)", blob)
        self.assertNotIn("classifier F3 -> strip 5 empty-id reasoning items",
                         blob)
        # (4) The linter's own stdout/turn-file adjacent text stays out.
        self.assertNotIn("stdout.ndjson", blob)

    def test_external_absolute_paths_are_normalized_not_copied(self):
        blob = _serialize(self.res)
        self.assertNotIn("/tmp/tagprobe", blob)
        ev6 = _by_id(self.res, "evidence:EV-6")
        self.assertIn("@external/turn1..4.json#", ev6["fact"])
        self.assertIn("@external/tagprobe-wire#", ev6["fact"])
        arm = _by_id(self.res, "fixture:tagprobe-2")
        prov = arm["provenance"]["text"]
        self.assertIsInstance(prov, dict)
        for value in prov.values():
            self.assertNotIn("/tmp/", value)
            self.assertIn("@external/", value)
        for value in prov.values():
            m = re.search(r"@external/([A-Za-z0-9._%-]+)#([0-9a-f]{64})",
                          value)
            self.assertTrue(m, value)

    def test_catalog_curated_sections_are_emitted(self):
        # Every catalog-declared heading/question selector for the formalism
        # corpus resolves to a document-section entity (no orphan selectors).
        for source in self.catalog.get("curated_sources", []):
            if source.get("root") != "plans":
                continue
            path = source["path"]
            for selector in source.get("selectors", []):
                kind = selector.get("kind")
                if kind not in ("heading", "question"):
                    continue
                section = _by_id(
                    self.res,
                    f"section:plans:{path}:{_selector_hash(selector.get('text') or selector.get('marker'))}")
                self.assertIsNotNone(section,
                                     f"selector {selector!r} in {path}")
                self.assertEqual(section["selector"]["kind"], kind)
                snippet = section["snippet"]
                self.assertIn("text", snippet)
                self.assertIn("truncated", snippet)
                self.assertIn("bytes", snippet)
                self.assertIn("source_sha256", snippet)
                self.assertEqual(snippet["source_sha256"],
                                 _sha256(self.plans / path))


def _selector_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


# --- generated-rule coupling gate (isolated copies) ---------------------------


class GeneratedRuleDriftTest(unittest.TestCase):
    """Mutate isolated /tmp copies; never the real corpus or worktree."""

    def _ctx(self, plans: Path, worktree: Path):
        return contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=contract.load_catalog(),
        )

    def test_unmutated_copy_has_no_drift_finding(self):
        plans, worktree = _copy_corpus("unmutated")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        res = formalism.discover(self._ctx(plans, worktree), ())
        self.assertEqual(_findings(res, "formalism-generated-rule-drift"), [])
        self.assertEqual(_hard_findings(res), [])

    def test_derive_bytes_match_committed_targets(self):
        # Independently prove the pure derive() seam bytes equal the committed
        # worktree targets (the same seam the adapter gate uses).
        import importlib.util
        plans, worktree = _copy_corpus("derive")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        gen_path = plans / GENERATOR_PY
        spec = importlib.util.spec_from_file_location(
            "formalism-test-generator-seam", str(gen_path))
        module = importlib.util.module_from_spec(spec)
        prev = sys.dont_write_bytecode
        sys.dont_write_bytecode = True
        try:
            spec.loader.exec_module(module)
            fixture_bytes, table_bytes = module.derive(plans / RULES_JSON)
        finally:
            sys.dont_write_bytecode = prev
        self.assertEqual(fixture_bytes, (worktree / HARD_RULES_JSON).read_bytes())
        self.assertEqual(table_bytes, (worktree / RULES_GENERATED_RS).read_bytes())
        # The seam must not have written bytecode next to the generator.
        pycache = plans / "parity-formalism" / "tools" / "__pycache__"
        self.assertEqual(
            {p.name for p in pycache.glob("formalism*")}, set())

    def test_mutated_rules_generated_yields_hard_drift_finding(self):
        plans, worktree = _copy_corpus("drift-rs")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        target = worktree / RULES_GENERATED_RS
        data = bytearray(target.read_bytes())
        data[128] ^= 0xFF
        target.write_bytes(bytes(data))
        before = _snapshot_tree(plans)
        res = formalism.discover(self._ctx(plans, worktree), ())
        after = _snapshot_tree(plans)
        self.assertEqual(before, after,
                         "discovery modified the plans corpus")
        drifts = _findings(res, "formalism-generated-rule-drift")
        self.assertTrue(drifts, "no generated-rule-drift finding emitted")
        locs = {(f["source"]["root"], f["source"]["path"]) for f in drifts}
        self.assertIn(("worktree", RULES_GENERATED_RS), locs)
        self.assertNotIn(("worktree", HARD_RULES_JSON), locs)
        for f in drifts:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["level"], "error")
            self.assertEqual(f["component"], "formalism-linter")

    def test_mutated_hard_rules_yields_hard_drift_finding(self):
        plans, worktree = _copy_corpus("drift-json")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        target = worktree / HARD_RULES_JSON
        data = bytearray(target.read_bytes())
        data[64] ^= 0xFF
        target.write_bytes(bytes(data))
        res = formalism.discover(self._ctx(plans, worktree), ())
        drifts = _findings(res, "formalism-generated-rule-drift")
        self.assertTrue(drifts)
        locs = {(f["source"]["root"], f["source"]["path"]) for f in drifts}
        self.assertIn(("worktree", HARD_RULES_JSON), locs)
        self.assertNotIn(("worktree", RULES_GENERATED_RS), locs)

    def test_generator_contact_is_pure_seam_only(self):
        plans, worktree = _copy_corpus("seam")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        before_modules = set(sys.modules)
        res = formalism.discover(self._ctx(plans, worktree), ())
        self.assertEqual(_findings(res, "formalism-generated-rule-drift"), [])
        self.assertEqual(_hard_findings(res), [])
        # The generator seam leaves no process-visible residue: the seam
        # module is popped from sys.modules once discover() returns.
        self.assertNotIn(formalism.GENERATOR_MODULE_NAME, sys.modules)
        # ...and no linter module was ever imported.
        linter_modules = [m for m in sys.modules
                          if "invariant_lint" in m]
        self.assertEqual(linter_modules, [])
        # No CLI was run and no module leaked: the seam name is absent
        # from the sys.modules diff and no harness module was imported.
        new = set(sys.modules) - before_modules
        self.assertNotIn(formalism.GENERATOR_MODULE_NAME, new)
        self.assertFalse(any(m.startswith("smoke.") for m in new))
        # No bytecode cache entry was created for the generator.
        pycache = plans / "parity-formalism" / "tools" / "__pycache__"
        names = {p.name for p in pycache.iterdir()} \
            if pycache.is_dir() else set()
        self.assertFalse(any("formalism" in n for n in names))


# --- adversarial extraction (mutated isolated copies) --------------------------


class AdversarialMutationTest(unittest.TestCase):
    """Each test takes a fresh /tmp copy of the corpus and mutates it."""

    def _run(self, plans: Path, worktree: Path, extra_roots=None):
        roots = {"worktree": worktree, "plans": plans}
        if extra_roots:
            roots.update(extra_roots)
        ctx = contract.AdapterContext(roots=roots,
                                      catalog=contract.load_catalog())
        return formalism.discover(ctx, ())

    def test_missing_heading_yields_hard_finding(self):
        plans, worktree = _copy_corpus("no-heading")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / FORMALISM_MD
        text = p.read_text("utf-8")
        text = text.replace(f"## {HEADING_CATEGORY}",
                            f"## 2. Category decomposition (renamed)")
        p.write_text(text, "utf-8")
        res = self._run(plans, worktree)
        missing = _findings(res, "formalism-selector-missing")
        self.assertTrue(missing)
        self.assertTrue(all(f["impact"] == "hard" for f in missing))
        self.assertTrue(any(f["source"]["path"] == FORMALISM_MD
                            for f in missing))

    def test_duplicate_heading_yields_hard_finding(self):
        plans, worktree = _copy_corpus("dup-heading")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / FORMALISM_MD
        text = p.read_text("utf-8")
        line = f"## {HEADING_CATEGORY}\n"
        self.assertIn(line, text)
        p.write_text(text.replace(line, line + line, 1), "utf-8")
        res = self._run(plans, worktree)
        dups = _findings(res, "formalism-selector-duplicate")
        self.assertTrue(dups)
        self.assertTrue(all(f["impact"] == "hard" for f in dups))
        self.assertTrue(any(f["source"]["path"] == FORMALISM_MD
                            for f in dups))

    def test_missing_question_marker_yields_hard_finding(self):
        plans, worktree = _copy_corpus("no-question")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / QA_MD
        lines = p.read_text("utf-8").split("\n")
        # Delete the last question block: from its marker line to EOF.
        start = None
        for i, ln in enumerate(lines):
            if ln.startswith("**Q:"):
                start = i
        self.assertIsNotNone(start)
        p.write_text("\n".join(lines[:start]) + "\n", "utf-8")
        res = self._run(plans, worktree)
        missing = _findings(res, "formalism-selector-missing")
        self.assertTrue(any(
            f["source"]["path"] == QA_MD and
            (f.get("source") or {}).get("question") for f in missing))

    def test_duplicate_question_marker_yields_hard_finding(self):
        plans, worktree = _copy_corpus("dup-question")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / QA_MD
        text = p.read_text("utf-8")
        marker = '**Q: "And across codex and claude we can start ' \
                 'systematically — how are\nencrypted_content and thinking ' \
                 'signatures etc?"**'
        self.assertIn(marker, text)
        p.write_text(text + "\n" + marker + "\nA: duplicate.\n", "utf-8")
        res = self._run(plans, worktree)
        dups = _findings(res, "formalism-selector-duplicate")
        self.assertTrue(any(f["source"]["path"] == QA_MD for f in dups))

    def test_malformed_rules_json_yields_hard_finding(self):
        plans, worktree = _copy_corpus("bad-json")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / RULES_JSON
        p.write_text(p.read_text("utf-8")[:50], "utf-8")  # truncated JSON
        res = self._run(plans, worktree)
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(bad)
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        self.assertTrue(any(f["source"]["path"] == RULES_JSON for f in bad))

    def test_unknown_citation_namespace_yields_hard_finding(self):
        plans, worktree = _copy_corpus("unknown-cite")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / RULES_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["rules"][0]["ev"] = ["EV-99", "FOO 3"]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        unknown = _findings(res, "formalism-unknown-citation")
        self.assertTrue(unknown)
        self.assertTrue(all(f["impact"] == "hard" for f in unknown))
        blob = _serialize(res)
        self.assertNotIn("EV-99", blob)
        self.assertNotIn("FOO 3", blob)

    def test_unresolvable_document_anchor_yields_hard_finding(self):
        plans, worktree = _copy_corpus("bad-anchor")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / RULES_JSON
        doc = json.loads(p.read_text("utf-8"))
        for rule in doc["rules"]:
            if rule["id"] == "S-2":
                rule["ev"] = ["EV-13", "EXEMPLARS 99"]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        unknown = _findings(res, "formalism-unknown-citation")
        self.assertTrue(unknown)
        self.assertTrue(all(f["impact"] == "hard" for f in unknown))

    def test_duplicate_stable_id_yields_hard_finding(self):
        plans, worktree = _copy_corpus("dup-id")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["adjudications"].append(dict(doc["adjudications"][0]))
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        collisions = _findings(res, "formalism-id-collision")
        self.assertTrue(collisions)
        self.assertTrue(all(f["impact"] == "hard" for f in collisions))
        ids = [e["id"] for e in res.entities]
        self.assertEqual(len(ids), len(set(ids)))

    def test_nested_root_longest_prefix_wins(self):
        plans, worktree = _copy_corpus("nested-root")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        nested = plans / "parity-formalism"
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "ev3-empty-id-400" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["copied_from"] = str(nested / "Q-A.md")  # absolute, nested root
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree,
                        extra_roots={"plans2": nested})
        arm = _by_id(res, "fixture:ev3-empty-id-400")
        self.assertEqual(arm["provenance"], {"text": "@plans2/Q-A.md"})
        self.assertNotIn(str(nested), _serialize(res))

    def test_raw_key_injection_is_hard_and_suppressed(self):
        plans, worktree = _copy_corpus("raw-key")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        secret = "sk-" + "A1" * 16
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "h2-synthetic-strict-enc" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["supersession_note"] = f"LEAK {secret} END"
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sensitive = _findings(res, "formalism-sensitive-output")
        self.assertTrue(sensitive)
        self.assertTrue(all(f["impact"] == "hard" for f in sensitive))
        blob = _serialize(res)
        self.assertNotIn(secret, blob)
        self.assertNotIn("LEAK", blob)

    def test_canary_injection_is_hard_and_suppressed(self):
        plans, worktree = _copy_corpus("canary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "h2-synthetic-strict-enc" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["supersession_note"] = f"X {contract.CANARY_TOKEN} Y"
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sensitive = _findings(res, "formalism-sensitive-output")
        self.assertTrue(sensitive)
        self.assertTrue(all(f["impact"] == "hard" for f in sensitive))
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))

    def test_html_payload_is_emitted_as_plain_text(self):
        plans, worktree = _copy_corpus("html")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        html = "<b>B</b><script>alert('x')</script>PLAIN"
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "h2-synthetic-strict-enc" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["supersession_note"] = html
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        arm = _by_id(res, "fixture:h2-synthetic-strict-enc")
        # Curated allowlisted text survives verbatim as data; HTML is never
        # interpreted by the index (the site inserts through textContent).
        self.assertEqual(arm["supersession"], html)
        self.assertEqual(_findings(res, "formalism-sensitive-output"), [])
        self.assertEqual(_hard_findings(res), [])

    def test_unexpected_meta_field_is_never_copied(self):
        plans, worktree = _copy_corpus("extra-field")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "h2a-synthetic-mint-tag-own-blob" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["injected_extra"] = "INJ-VALUE-777"
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        self.assertNotIn("injected_extra", blob)
        self.assertNotIn("INJ-VALUE-777", blob)
        arm = _by_id(res, "fixture:h2a-synthetic-mint-tag-own-blob")
        self.assertNotIn("injected_extra", arm)

    def test_meta_kind_outside_enum_is_hard(self):
        plans, worktree = _copy_corpus("bad-kind")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        meta = plans / "parity-formalism" / "tools" / "fixtures" \
            / "ev3-empty-id-400" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["kind"] = "mystery-kind"
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = _findings(res, "formalism-kind-enum-violation")
        self.assertTrue(bad)
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        self.assertNotIn("mystery-kind", _serialize(res))

    def test_absolute_and_escape_pins_yield_hard_findings(self):
        # M1: the H-7 code-pin channel is a structured locator channel.
        # An absolute pin and a relative-escape (`..`) pin are hard
        # findings, never copied raw; an absolute pin beneath the plans
        # root is emitted only in its rooted form.
        plans, worktree = _copy_corpus("h7-abs-pin")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / HARDENING_MD
        text = p.read_text("utf-8")
        marker = f"`{XWAVEC71_REPORT}:75`"
        abs_under_root = str(plans / "parity-formalism" / "Q-A.md")
        injections = (f" `{abs_under_root}`"
                      f" `/tmp/abs-evil.md`"
                      f" `../../escape.md`")
        out_lines = []
        replaced = False
        for line in text.splitlines(keepends=True):
            if line.startswith("| H-7") and marker in line \
                    and not replaced:
                out_lines.append(line.replace(marker, marker + injections))
                replaced = True
            else:
                out_lines.append(line)
        self.assertTrue(replaced)
        p.write_text("".join(out_lines), "utf-8")
        res = self._run(plans, worktree)
        aliases = _findings(res, "formalism-path-alias")
        self.assertGreaterEqual(len(aliases), 3)
        for f in aliases:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["level"], "error")
        # The H-7 pin findings are rooted at the HARDENING-SPEC source.
        self.assertTrue(any(
            f["source"].get("root") == "plans"
            and f["source"].get("path") == HARDENING_MD
            for f in aliases))
        # No raw absolute token anywhere in the builder output.
        blob = _serialize(res)
        self.assertNotIn("/tmp/abs-evil.md", blob)
        self.assertNotIn(abs_under_root, blob)
        # No raw escape token in any structured locator.
        h7 = _by_id(res, "rule:H-7")
        for pin in h7["code_pins"]:
            self.assertFalse(pin["path"].startswith("/"), pin)
            self.assertNotIn("..", pin["path"], pin)
        # The beneath-root absolute pin survives, normalized and rooted.
        self.assertIn(
            {"root": "plans", "path": "parity-formalism/Q-A.md"},
            h7["code_pins"])
        # The clean corpus pins are intact.
        self.assertIn(
            {"root": "plans", "path": XWAVEC71_REPORT, "line": 75},
            h7["code_pins"])
        self.assertIsNone(_by_id(res, "artifact:plans:../../escape.md"))
        self.assertIsNone(_by_id(res, "artifact:plans:/tmp/abs-evil.md"))

    def test_absolute_source_files_value_yields_hard_finding(self):
        # M1: boundary source_files values are structured locator
        # tokens; an absolute value yields a hard finding and is never
        # copied raw or mis-attributed to the first probed root.
        plans, worktree = _copy_corpus("abs-srcfile")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        rules_path = plans / RULES_JSON
        rules = json.loads(rules_path.read_text("utf-8"))
        raw_abs = str(worktree / f"{CRATE}/src/catalog_wire.rs")
        rules["boundary_slug_map"]["source_files"][
            "catalog_family"] = raw_abs
        rules_path.write_text(json.dumps(rules, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        aliases = [
            f for f in _findings(res, "formalism-path-alias")
            if f["source"].get("key", "").startswith(
                "boundary_slug_map.source_files.")
        ]
        self.assertEqual(len(aliases), 1)
        self.assertEqual(aliases[0]["impact"], "hard")
        self.assertEqual(aliases[0]["level"], "error")
        self.assertEqual(aliases[0]["component"], "formalism-linter")
        # The raw absolute path is never copied into the output.
        self.assertNotIn(raw_abs, _serialize(res))
        # The entry is normalized to its rooted form (the same locator
        # the clean relative value yields on the real corpus).
        bmap = _by_id(res, ARTIFACT_RULES_JSON)
        entry = bmap["boundary_map"]["source_files"].get("catalog_family")
        self.assertEqual(
            entry,
            {"root": "worktree", "path": f"{CRATE}/src/catalog_wire.rs"})

    def test_absolute_evidence_files_yield_hard_findings(self):
        # M-workhorse (round 2): adjudication evidence_files are
        # structured locator tokens; an absolute value and a `..` value
        # are hard findings, never concatenated raw into the locator
        # path, and the clean locators survive.
        plans, worktree = _copy_corpus("abs-evfile")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        idx = next(i for i, a in enumerate(adjs)
                   if isinstance(a, dict) and a.get("evidence_files"))
        target = adjs[idx]
        clean = [f for f in target["evidence_files"] if isinstance(f, str)]
        self.assertTrue(clean)
        target["evidence_files"] = clean + ["/tmp/abs-ev.json",
                                            "../../escape-ev.md"]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        aliases = [
            f for f in _findings(res, "formalism-path-alias")
            if f["source"].get("key", "").startswith("adjudications[")
        ]
        self.assertEqual(len(aliases), 2)
        for f in aliases:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["level"], "error")
            self.assertEqual(f["component"], "formalism-linter")
        # No raw token anywhere in the builder output.
        blob = _serialize(res)
        self.assertNotIn("/tmp/abs-ev.json", blob)
        self.assertNotIn("escape-ev", blob)
        # No adjudication locator carries an absolute, escaping, or
        # double-slashed path.
        all_locs = []
        for e in res.entities:
            if e.get("kind") == "adjudication":
                all_locs.extend(e.get("evidence") or [])
        self.assertTrue(all_locs)
        for item in all_locs:
            self.assertEqual(item["kind"], "artifact-locator", item)
            self.assertFalse(item["path"].startswith("/"), item)
            self.assertNotIn("..", item["path"], item)
            self.assertNotIn("//", item["path"], item)
        # The mutated adjudication keeps exactly its clean locators.
        self.assertEqual(len(_by_kind(res, "adjudication")),
                         PIN_ADJUDICATION_COUNT)
        adj = _by_id(res, f"adjudication:{target['id']}")
        self.assertEqual(len(adj["evidence"]), len(clean))
        for f in clean:
            self.assertTrue(any(
                item["root"] == "plans"
                and item["path"] == f"parity-formalism/tools/{f}"
                for item in adj["evidence"]), f)

    def test_h7_pin_root_resolves_owning_root(self):
        # m-workhorse (round 2): a relative H-7 pin resolving under a
        # different root carries that root in the pin record, not the
        # "plans" default.
        plans, worktree = _copy_corpus("h7-pin-root")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        note_rel = f"{CRATE}/src/conversation/pin-note.md"
        (worktree / note_rel).parent.mkdir(parents=True, exist_ok=True)
        (worktree / note_rel).write_text("# pin note\n", "utf-8")
        p = plans / HARDENING_MD
        text = p.read_text("utf-8")
        marker = f"`{XWAVEC71_REPORT}:75`"
        out_lines = []
        replaced = False
        for line in text.splitlines(keepends=True):
            if line.startswith("| H-7") and marker in line \
                    and not replaced:
                out_lines.append(line.replace(
                    marker, marker + f" `{note_rel}`"))
                replaced = True
            else:
                out_lines.append(line)
        self.assertTrue(replaced)
        p.write_text("".join(out_lines), "utf-8")
        res = self._run(plans, worktree)
        h7 = _by_id(res, "rule:H-7")
        self.assertIn(
            {"root": "worktree", "path": note_rel},
            h7["code_pins"])

    def test_non_utf8_curated_doc_yields_hard_finding(self):
        # m-concordance (round 2): a non-UTF-8 curated document is a hard
        # malformed-source finding, never an unhandled exception.
        plans, worktree = _copy_corpus("non-utf8")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        (plans / FORMALISM_MD).write_bytes(
            b"\xff\xfe\x00broken \x80\x81\n")
        res = self._run(plans, worktree)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("root") == "plans"
            and f["source"].get("path") == FORMALISM_MD
        ]
        self.assertTrue(bad)
        self.assertTrue(all(f["impact"] == "hard" for f in bad))

    def test_meta_ev_stamps_keys_are_hygienized(self):
        # M-A: meta.json ev_stamps pathspec KEYS get the same treatment
        # as values: absolute tokens are externalized, sensitive
        # content is a hard finding with the entry dropped.
        plans, worktree = _copy_corpus("meta-stamp-key")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        meta_path = plans / "parity-formalism" / "tools" / "fixtures" \
            / "at-strict-xwire-01" / "meta.json"
        meta = json.loads(meta_path.read_text("utf-8"))
        meta["ev_stamps"] = {
            contract.CANARY_TOKEN: ["EV-1"],
            "/tmp/abs-stamp/req-001.json": ["EV-2"],
        }
        meta_path.write_text(json.dumps(meta, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sens = _findings(res, "formalism-sensitive-output")
        self.assertTrue(sens)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        arm = _by_id(res, "fixture:at-strict-xwire-01")
        self.assertIsNotNone(arm)
        keys = list((arm.get("ev_stamps") or {}).keys())
        self.assertNotIn(contract.CANARY_TOKEN, keys)
        self.assertNotIn("/tmp/abs-stamp/req-001.json", keys)
        ext = [k for k in keys if k.startswith("@external/")]
        self.assertEqual(len(ext), 1)
        sha = hashlib.sha256(b"/tmp/abs-stamp/req-001.json").hexdigest()
        self.assertEqual(ext[0], f"@external/req-001.json#{sha}")
        self.assertEqual(arm["ev_stamps"][ext[0]], ["EV-2"])
        blob = _serialize(res)
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        self.assertNotIn("/tmp/abs-stamp", blob)

    def test_verdict_table_ev_stamps_are_hygienized(self):
        # M-A: verdict-table arm ev_stamps pass through safe_text
        # before reaching expected_ev_stamps; a canary stamp is a hard
        # finding and is dropped.
        plans, worktree = _copy_corpus("verdict-stamp")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        idx = next(i for i, a in enumerate(doc["arms"])
                   if a.get("fixture") == "crosswire-1-rt-xreplay1")
        doc["arms"][idx]["ev_stamps"] = ["EV-1", contract.CANARY_TOKEN]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sens = [f for f in _findings(res, "formalism-sensitive-output")
                if f.get("component") == "formalism-linter"]
        self.assertTrue(sens)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        arm = _by_id(res, "fixture:crosswire-1-rt-xreplay1")
        self.assertIsNotNone(arm)
        self.assertEqual(arm.get("expected_ev_stamps"), ["EV-1"])
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))

    def test_absolute_source_dir_is_externalized_with_finding(self):
        # M-B: an absolute source_dir is externalized (hash form) and
        # the non-convertible value still carries the required finding.
        plans, worktree = _copy_corpus("abs-srcdir")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        idx = next(i for i, a in enumerate(doc["arms"])
                   if a.get("fixture") == "crosswire-1-rt-xreplay1")
        raw = "/tmp/abs-srcdir/report.json"
        doc["arms"][idx]["source_dir"] = raw
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        arm = _by_id(res, "fixture:crosswire-1-rt-xreplay1")
        sha = hashlib.sha256(raw.encode("utf-8")).hexdigest()
        self.assertEqual(arm["source_dir"],
                         [f"@external/report.json#{sha}"])
        unrooted = _findings(res, "formalism-source-dir-unrooted")
        self.assertTrue(any(f["detail"][0]["sha256"] == sha
                            for f in unrooted))
        self.assertTrue(all(f["level"] == "warning"
                            and f["impact"] == "soft"
                            for f in unrooted))
        self.assertNotIn(raw, _serialize(res))

    def test_dot_and_double_slash_evidence_tokens_yield_hard_findings(self):
        # m-2: dot-segment-prefixed or double-slashed relative tokens
        # are refused with a hard path-alias finding (structured
        # channel discipline), never joined raw into a locator.
        plans, worktree = _copy_corpus("dot-evidence")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        idx = next(i for i, a in enumerate(adjs)
                   if isinstance(a, dict) and a.get("evidence_files"))
        adjs[idx]["evidence_files"] = [
            "./fixtures/sight-1-mxai-c04/wire/req-004.json",
            "fixtures//sight-1-mxai-c04/wire/resp-004.jsonl",
            "fixtures/sight-1-mxai-c04/wire/req-004.json",
        ]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        aliases = [
            f for f in _findings(res, "formalism-path-alias")
            if f["source"].get("key", "").startswith("adjudications[")
        ]
        self.assertEqual(len(aliases), 2)
        self.assertTrue(all(f["impact"] == "hard" for f in aliases))
        adj = _by_id(res, f"adjudication:{adjs[idx]['id']}")
        self.assertEqual(len(adj["evidence"]), 1)
        self.assertEqual(
            adj["evidence"][0]["path"],
            "parity-formalism/tools/fixtures/sight-1-mxai-c04/wire/"
            "req-004.json")
        for e in res.entities:
            if e.get("kind") == "adjudication":
                for item in e.get("evidence") or []:
                    self.assertNotIn("//", item["path"], item)
                    self.assertNotIn("/./", item["path"], item)

    def test_malformed_evidence_entries_yield_findings(self):
        # m-3: non-string/empty evidence_files entries are hard
        # malformed-source findings at their raw index, not silent
        # skips.
        plans, worktree = _copy_corpus("bad-evidence")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        idx = next(i for i, a in enumerate(adjs)
                   if isinstance(a, dict) and a.get("evidence_files"))
        adjs[idx]["evidence_files"] = [
            123, "", "fixtures/sight-1-mxai-c04/wire/req-004.json"]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key", "").startswith(
                f"adjudications[{idx}].evidence_files[")
        ]
        self.assertEqual(len(bad), 2)
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        adj = _by_id(res, f"adjudication:{adjs[idx]['id']}")
        self.assertEqual(len(adj["evidence"]), 1)
        self.assertEqual(
            adj["evidence"][0]["path"],
            "parity-formalism/tools/fixtures/sight-1-mxai-c04/wire/"
            "req-004.json")


# --- round 4: raw-emit gating + malformed-data discipline ------------------------


class Round4RawEmitAndMalformedTest(unittest.TestCase):
    """Batch A round 4 (bead apex-ayl.137): the hygiene class closed in
    rounds 1-3 on the remaining sibling channels, plus malformed-data
    discipline.

    Per-field-family injections (canary, absolute path, wrong type) mirror
    ``test_verdict_table_ev_stamps_are_hygienized``: every rejected value is
    a hard finding, is absent from the serialized output, leaves clean
    siblings intact, and mints no entity id nor relationship target. Each
    test mutates a fresh /tmp copy; the real corpus is never written.
    """

    def _run(self, plans: Path, worktree: Path, extra_roots=None):
        roots = {"worktree": worktree, "plans": plans}
        if extra_roots:
            roots.update(extra_roots)
        ctx = contract.AdapterContext(roots=roots,
                                      catalog=contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _copy_boundary_source_files(worktree: Path) -> None:
        # _copy_corpus copies only the drift-gate slice; the boundary
        # source_files resolution probes these worktree files, so the
        # three real files are copied at their live relative paths.
        live = json.loads(
            (_plans_root() / RULES_JSON).read_text("utf-8"))
        for rel in live["boundary_slug_map"]["source_files"].values():
            src = _worktree_root() / rel
            dst = worktree / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)

    # -- 1. rules[].id (id minting) + class/severity/check -------------------

    def test_rule_id_and_field_channels_are_gated(self):
        plans, worktree = _copy_corpus("r4-rule-fields")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / RULES_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["rules"][0]["id"] = contract.CANARY_TOKEN       # H-1: id
        doc["rules"][1]["class"] = "/tmp/r4-abs-cls/x.json"  # H-2: class
        doc["rules"][2]["severity"] = 42                     # H-3: severity
        doc["rules"][3]["ev"] = "EV-1"                       # H-4: ev
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        # id: canary is a hard sensitive finding at the exact index; no id
        # is minted and the record is dropped.
        sens = [f for f in _findings(res, "formalism-sensitive-output")
                if f.get("component") == "formalism-linter"]
        self.assertEqual(len(sens), 1)
        self.assertEqual(sens[0]["impact"], "hard")
        self.assertEqual(sens[0]["source"].get("index"), 0)
        self.assertIsNone(_by_id(res, f"rule:{contract.CANARY_TOKEN}"))
        self.assertIsNone(_by_id(res, "rule:H-1"))
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        # class: absolute token rewritten in place (escaped text).
        h2 = _by_id(res, "rule:H-2")
        self.assertIsNotNone(h2)
        sha = hashlib.sha256(b"/tmp/r4-abs-cls/x.json").hexdigest()
        self.assertEqual(h2["class"], f"@external/x.json#{sha}")
        self.assertNotIn("/tmp/r4-abs-cls/x.json", blob)
        # severity: wrong type is ONE hard malformed finding at the exact
        # index/key; the field is dropped, the record kept.
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"].get("path") == RULES_JSON
               and f["source"].get("index") == 2]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["source"].get("key"), "severity")
        h3 = _by_id(res, "rule:H-3")
        self.assertIsNotNone(h3)
        self.assertNotIn("severity", h3)
        # ev string container: ONE hard malformed finding — never iterated
        # character-wise (the old behavior minted one unknown-citation
        # finding per character).
        ev_bad = [f for f in _findings(res, "formalism-malformed-source")
                  if f["source"].get("path") == RULES_JSON
                  and f["source"].get("index") == 3]
        self.assertEqual(len(ev_bad), 1)
        self.assertEqual(ev_bad[0]["source"].get("key"), "ev")
        self.assertEqual(_findings(res, "formalism-unknown-citation"), [])
        self.assertEqual(_by_id(res, "rule:H-4")["citations"], [])
        # clean sibling record intact.
        h4 = _by_id(res, "rule:H-4")
        self.assertEqual(h4["class"], "all-responses")
        self.assertEqual(h4["severity"], "hard")
        self.assertEqual(h4["check"], "store_false")

    # -- 2. boundary_slug_map keys / source_files names / classes ------------

    def test_boundary_slug_map_channels_are_gated(self):
        plans, worktree = _copy_corpus("r4-boundary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        self._copy_boundary_source_files(worktree)
        p = plans / RULES_JSON
        doc = json.loads(p.read_text("utf-8"))
        bm = doc["boundary_slug_map"]
        bm[contract.CANARY_TOKEN] = "EV-1"
        bm["/tmp/r4-abs-bm/x.json"] = "sym"
        bm["o_series_first_char"] = 42
        bm["source_files"][contract.CANARY_TOKEN] = (
            "crates/codegen/xai-grok-sampling-types/src/conversation/"
            "projection.rs")
        bm["source_files"]["/tmp/r4-abs-name/x.json"] = (
            "crates/codegen/xai-grok-sampling-types/src/conversation/"
            "projection.rs")
        bm["classes"] = bm["classes"] + [
            contract.CANARY_TOKEN, "/tmp/r4-abs-cls/x.json", 42]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        # Three canary injections (symbolic key, source_files name, class
        # entry): three hard sensitive findings, raw token nowhere.
        sens = _findings(res, "formalism-sensitive-output")
        self.assertEqual(len(sens), 3)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        bmap = _by_id(res, ARTIFACT_RULES_JSON)
        symbolic = bmap["boundary_map"]["symbolic_values"]
        # symbolic: canary key dropped, absolute key rewritten in place,
        # non-string value a hard malformed finding at the exact key.
        self.assertNotIn(contract.CANARY_TOKEN, symbolic)
        sha = hashlib.sha256(b"/tmp/r4-abs-bm/x.json").hexdigest()
        self.assertEqual(symbolic.get(f"@external/x.json#{sha}"), "sym")
        self.assertNotIn("o_series_first_char", symbolic)
        self.assertEqual(symbolic["xai_prefix"], "grok")
        self.assertEqual(symbolic["openai_prefix"], "gpt-")
        self.assertTrue(any(
            f["source"].get("key") == "boundary_slug_map.o_series_first_char"
            and f["impact"] == "hard"
            for f in _findings(res, "formalism-malformed-source")))
        # source_files: name keys gated the same way; clean entries intact.
        files = bmap["boundary_map"]["source_files"]
        self.assertNotIn(contract.CANARY_TOKEN, files)
        nsha = hashlib.sha256(b"/tmp/r4-abs-name/x.json").hexdigest()
        self.assertEqual(
            files.get(f"@external/x.json#{nsha}"),
            {"root": "worktree",
             "path": "crates/codegen/xai-grok-sampling-types/"
                     "src/conversation/projection.rs"})
        self.assertEqual(
            files["model_boundary_class"],
            {"root": "worktree",
             "path": "crates/codegen/xai-grok-sampling-types/"
                     "src/conversation/projection.rs"})
        self.assertNotIn("/tmp/r4-abs-name/x.json", blob)
        # classes: no entity id minted from any rejected value; the three
        # pinned classes intact; two hard malformed findings at the exact
        # entry indices (rewritten-absolute fails the id shape, 42 is not a
        # string).
        bounds = _by_kind(res, "boundary-class")
        self.assertEqual(sorted(b["name"] for b in bounds),
                         PIN_BOUNDARY_CLASSES)
        self.assertIsNone(_by_id(res, f"boundary:{contract.CANARY_TOKEN}"))
        self.assertNotIn("boundary:42", blob)
        cls_bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "boundary_slug_map.classes"]
        self.assertEqual(len(cls_bad), 2)
        self.assertTrue(all(f["impact"] == "hard" for f in cls_bad))
        self.assertNotIn("/tmp/r4-abs-cls/x.json", blob)

    # -- 3. request expectations + arm verdict fields -------------------------

    def test_request_expectation_channels_are_gated(self):
        plans, worktree = _copy_corpus("r4-requests")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        arm = doc["arms"][0]
        self.assertEqual(arm["fixture"], "sight-1-msw-a")
        reqs = arm["requests"]
        reqs[0]["method"] = contract.CANARY_TOKEN
        reqs[1]["path"] = "/tmp/r4-abs-route/x.json"
        reqs[2]["model"] = "/tmp/r4-abs-model/x.json"
        reqs[3]["row_class"] = contract.CANARY_TOKEN
        reqs[4]["verdict"] = 5
        reqs[5]["hard"] = [["H-1", "input[0].content", "EV-1"],
                           contract.CANARY_TOKEN]
        reqs[5]["soft"] = "S-2"
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        fid = "sight-1-msw-a"
        # method/row_class canaries: two hard sensitive findings; the
        # fields are dropped, the records kept.
        sens = [f for f in _findings(res, "formalism-sensitive-output")
                if f.get("component") == "formalism-linter"]
        self.assertEqual(len(sens), 2)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        r1 = _by_id(res, f"request:{fid}:001")
        self.assertIsNotNone(r1)
        self.assertNotIn("method", r1)
        self.assertEqual(r1["verdict"], "PASS")
        r4 = _by_id(res, f"request:{fid}:004")
        self.assertNotIn("row_class", r4)
        # path: scan-only (wire route = data, coordinator adjudication):
        # kept verbatim, no rewrite, no finding.
        r2 = _by_id(res, f"request:{fid}:002")
        self.assertEqual(r2["path"], "/tmp/r4-abs-route/x.json")
        # model: full safe_text — absolute token rewritten in place.
        r3 = _by_id(res, f"request:{fid}:003")
        sha = hashlib.sha256(b"/tmp/r4-abs-model/x.json").hexdigest()
        self.assertEqual(r3["model"], f"@external/x.json#{sha}")
        self.assertNotIn("/tmp/r4-abs-model/x.json", blob)
        # verdict wrong type: hard malformed at the exact request; field
        # dropped, record kept.
        r5 = _by_id(res, f"request:{fid}:005")
        self.assertIsNotNone(r5)
        self.assertNotIn("verdict", r5)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"].get("path") == EXPECTED_VERDICTS_JSON
               and f["source"].get("index") == 0]
        self.assertEqual(len(bad), 3)
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        self.assertTrue(any(f["source"].get("key") == "verdict"
                            and f["source"].get("request_n") == 5
                            for f in bad))
        # hard: a non-list entry is ONE hard malformed finding, the clean
        # entry intact. soft: a string container is ONE hard malformed
        # finding (never iterated character-wise).
        r6 = _by_id(res, f"request:{fid}:006")
        self.assertEqual(r6["hard"], [["H-1", "input[0].content", "EV-1"]])
        self.assertEqual(r6["soft"], [])
        self.assertTrue(any(f["source"].get("key") == "hard[1]"
                            and f["source"].get("request_n") == 6
                            for f in bad))
        self.assertTrue(any(f["source"].get("key") == "soft"
                            and f["source"].get("request_n") == 6
                            for f in bad))
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        # clean sibling requests of the arm untouched.
        all_reqs = _by_kind(res, "request-expectation")
        self.assertEqual(len(all_reqs), PIN_REQUEST_COUNT)

    def test_arm_fixture_id_and_verdict_fields_are_gated(self):
        plans, worktree = _copy_corpus("r4-arm-ids")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        arms = doc["arms"]
        n_reqs = {a["fixture"]: len(a.get("requests", [])) for a in arms}
        arms[0]["fixture"] = contract.CANARY_TOKEN
        arms[1]["fixture"] = "/tmp/r4-abs-fid/x.json"
        arms[2]["fixture"] = 7
        arms[3]["expected_exit_code"] = "0"
        arms[4]["expected_drift_findings"] = True
        arms[5]["ev_stamps"] = "EV-2"
        arms[6]["ev_stamps"] = [123, "EV-1"]
        arms[7]["requests"] = "[]"
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        # No request id minted from a rejected fixture value, and no
        # relationship rides into a rejected fixture target.
        self.assertIsNone(_by_id(res, f"request:{contract.CANARY_TOKEN}:001"))
        for r in res.relationships:
            self.assertNotIn(contract.CANARY_TOKEN, r["source"])
            self.assertNotIn(contract.CANARY_TOKEN, r["target"])
        self.assertNotIn(f"fixture:{contract.CANARY_TOKEN}", blob)
        self.assertNotIn("/tmp/r4-abs-fid/x.json", blob)
        # arms[7] requests as a string container: ONE hard malformed
        # finding (never iterated character-wise), its requests mint
        # nothing.
        self.assertTrue(any(f["source"].get("index") == 7 and
                            f["source"].get("key") == "requests"
                            for f in bad))
        total = PIN_REQUEST_COUNT - (
            n_reqs["sight-1-msw-a"] + n_reqs["sight-1-msw-c"]
            + n_reqs["sight-1-swb-run2"]
            + n_reqs["crosswire-1-rt-xreplay1"])
        self.assertEqual(len(_by_kind(res, "request-expectation")), total)
        # The fixture directory arms still exist (meta-driven), just
        # without the rejected verdict linkage.
        self.assertIsNotNone(_by_id(res, "fixture:sight-1-msw-a"))
        # expected_exit_code str / expected_drift_findings bool: hard
        # malformed at the exact arm index/key; keys omitted.
        self.assertTrue(any(f["source"].get("index") == 3 and
                            f["source"].get("key") == "expected_exit_code"
                            for f in bad))
        self.assertTrue(any(f["source"].get("index") == 4 and
                            f["source"].get("key") == "expected_drift_findings"
                            for f in bad))
        self.assertNotIn("expected_exit_code",
                         _by_id(res, "fixture:sight-1-swb-run3"))
        self.assertNotIn("expected_drift_findings",
                         _by_id(res, "fixture:sight-1-mxai-c04"))
        # ev_stamps string container: exactly ONE hard malformed finding —
        # never iterated character-wise — and no expected stamps.
        stamps_container = [
            f for f in bad
            if f["source"].get("key") == "ev_stamps"
            and f["source"].get("index") == 5]
        self.assertEqual(len(stamps_container), 1)
        self.assertNotIn("expected_ev_stamps",
                         _by_id(res, "fixture:sight-1-mxai-c07"))
        # ev_stamps entry wrong type: hard finding at the exact entry
        # index; the clean stamp survives.
        stamps_entry = [
            f for f in bad
            if f["source"].get("key") == "ev_stamps"
            and f["source"].get("index") == 6]
        self.assertEqual(len(stamps_entry), 1)
        self.assertEqual(stamps_entry[0]["source"].get("entry"), 0)
        self.assertEqual(_by_id(res, "fixture:at-strict-xwire-01")
                         .get("expected_ev_stamps"), ["EV-1"])
        self.assertNotIn(contract.CANARY_TOKEN, blob)

    # -- 4. adjudications -------------------------------------------------------

    def test_adjudication_channels_are_gated(self):
        plans, worktree = _copy_corpus("r4-adjudications")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        adjs[0]["id"] = contract.CANARY_TOKEN
        adjs[1]["arm"] = contract.CANARY_TOKEN
        adjs[2]["n"] = "4"
        adjs[3]["hits"] = True
        adjs[4]["rule"] = "/tmp/r4-abs-rule/x.json"
        adjs[5]["class"] = contract.CANARY_TOKEN
        doc["adjudications"].append("not-a-dict")
        doc["arms"].append("not-an-arm")
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        sens = _findings(res, "formalism-sensitive-output")
        self.assertEqual(len(sens), 3)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        # id: no adjudication id minted, record dropped.
        self.assertIsNone(_by_id(res, f"adjudication:{contract.CANARY_TOKEN}"))
        self.assertEqual(len(_by_kind(res, "adjudication")),
                         PIN_ADJUDICATION_COUNT - 1)
        # arm: sensitive field dropped, record kept.
        a1 = _by_id(res, "adjudication:OQ-T8-2")
        self.assertIsNotNone(a1)
        self.assertNotIn("fixture_id", a1)
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        # n str / hits bool: hard malformed at the exact index/key; keys
        # omitted.
        self.assertTrue(any(f["source"].get("index") == 2 and
                            f["source"].get("key") == "n" for f in bad))
        self.assertTrue(any(f["source"].get("index") == 3 and
                            f["source"].get("key") == "hits" for f in bad))
        self.assertNotIn("n", _by_id(res, "adjudication:PIN-H1"))
        self.assertNotIn("hits", _by_id(res, "adjudication:PIN-H3-EV3"))
        # rule: absolute token rewritten in place.
        a4 = _by_id(res, "adjudication:PIN-H5")
        sha = hashlib.sha256(b"/tmp/r4-abs-rule/x.json").hexdigest()
        self.assertEqual(a4["rule"], f"@external/x.json#{sha}")
        self.assertNotIn("/tmp/r4-abs-rule/x.json", blob)
        # class: sensitive field dropped.
        a5 = _by_id(res, "adjudication:PIN-H2")
        self.assertNotIn("class", a5)
        # non-dict arm and adjudication entries: hard malformed at their
        # exact indices; clean records intact.
        self.assertTrue(any(f["source"].get("index") == 15 for f in bad))
        self.assertTrue(any(f["source"].get("index") == 17 for f in bad))
        self.assertEqual(len(_by_kind(res, "fixture-arm")),
                         PIN_FIXTURE_ARM_COUNT)
        self.assertNotIn(contract.CANARY_TOKEN, blob)

    # -- 5/6. category name cell + H-7 class cell --------------------------------

    def test_category_and_h7_name_cells_are_gated(self):
        plans, worktree = _copy_corpus("r4-cells")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / FORMALISM_MD
        text = p.read_text("utf-8")
        self.assertIn("| C1 | Envelope |", text)
        text = text.replace("| C1 | Envelope |",
                            f"| C1 | {contract.CANARY_TOKEN} |", 1)
        self.assertIn("| C2 | System/context payload |", text)
        text = text.replace("| C2 | System/context payload |",
                            "| C2 | /tmp/r4-abs-cat/x.json |", 1)
        p.write_text(text, "utf-8")
        h = plans / HARDENING_MD
        htext = h.read_text("utf-8")
        self.assertIn("| H-7 | all |", htext)
        htext = htext.replace("| H-7 | all |",
                              f"| H-7 | {contract.CANARY_TOKEN} |", 1)
        h.write_text(htext, "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        # C1 name canary + the curated FORMALISM §2 section snippet
        # carrying it + the H-7 class canary + the curated HARDENING §2.1
        # section snippet carrying it: four hard sensitive findings.
        sens = _findings(res, "formalism-sensitive-output")
        self.assertEqual(len(sens), 4)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        c1 = _by_id(res, "category:C1")
        self.assertIsNotNone(c1)
        self.assertNotIn("name", c1)
        # C2 name: absolute token rewritten in place (same gating as the
        # row's what/cost cells).
        c2 = _by_id(res, "category:C2")
        sha = hashlib.sha256(b"/tmp/r4-abs-cat/x.json").hexdigest()
        self.assertEqual(c2["name"], f"@external/x.json#{sha}")
        self.assertNotIn("/tmp/r4-abs-cat/x.json", blob)
        # clean siblings intact.
        self.assertEqual(_by_id(res, "category:C3")["name"], "Tool surface")
        self.assertEqual(_by_id(res, "category:C7")["name"],
                         "Nondeterminism envelope")
        # H-7 class cell: hard sensitive finding; the field is dropped,
        # the record (clause, layers, pins) kept.
        h7 = _by_id(res, "rule:H-7")
        self.assertIsNotNone(h7)
        self.assertNotIn("class", h7)
        self.assertEqual(len(h7["enforcement_layers"]), 3)
        self.assertEqual(len(h7["code_pins"]), 2)

    # -- 7. resolve_doc_anchor heading (scan-only) ------------------------------

    def test_doc_anchor_headings_are_scan_only(self):
        plans, worktree = _copy_corpus("r4-headings")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        # (a) canary in a resolved heading: hard sensitive, the citation is
        # rejected, the heading never copied.
        x = plans / EXEMPLARS_MD
        xtext = x.read_text("utf-8")
        heading = ("5. C4 instance — the 4-leg replay table "
                   "(reasoning representation × replay policy)")
        self.assertIn(f"## {heading}\n", xtext)
        xtext = xtext.replace(f"## {heading}\n",
                              f"## 5. C4 {contract.CANARY_TOKEN} instance\n",
                              1)
        x.write_text(xtext, "utf-8")
        # (b) absolute token in a resolved heading: scan-only — document
        # text is data (wire routes live in headings): no rewrite, no
        # finding, heading kept verbatim.
        i2 = plans / INTEL_02_MD
        itext = i2.read_text("utf-8")
        h2 = "(d) Headers + body envelope diff, `/v1/responses` POSTs"
        self.assertIn(f"## {h2}\n", itext)
        itext = itext.replace(f"## {h2}\n",
                              f"## {h2} /tmp/r4-abs-head/x.json\n", 1)
        i2.write_text(itext, "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        sens = _findings(res, "formalism-sensitive-output")
        self.assertTrue(sens)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        # (a) the EXEMPLARS citation is rejected; the intel/02 sibling
        # citation survives.
        s2 = _by_id(res, "rule:S-2")
        anchors = [c for c in s2["citations"]
                   if c["kind"] == "document-anchor"]
        self.assertEqual([a["path"] for a in anchors], [INTEL_02_MD])
        # (b) the heading with the absolute-looking wire route is copied
        # verbatim, with no path-alias or sensitive finding.
        s3 = _by_id(res, "rule:S-3")
        i2_anchors = [c for c in s3["citations"]
                      if c["kind"] == "document-anchor"
                      and c["path"] == INTEL_02_MD]
        self.assertEqual(
            [a["heading"] for a in i2_anchors],
            ["(d) Headers + body envelope diff, `/v1/responses` POSTs "
             "/tmp/r4-abs-head/x.json"])
        self.assertEqual(
            [f for f in _findings(res, "formalism-path-alias")
             if f["source"].get("path") == INTEL_02_MD], [])
        self.assertEqual(
            [f for f in _findings(res, "formalism-sensitive-output")
             if f["source"].get("path") == INTEL_02_MD], [])

    # -- 8. source_dir convertible branch + non-string --------------------------

    def test_source_dir_convertible_branch_and_malformed(self):
        plans, worktree = _copy_corpus("r4-srcdir")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        arms = doc["arms"]
        # path-shaped and resolvable beneath a supplied root: a rooted
        # locator dict, never the raw string.
        arms[0]["source_dir"] = RULES_JSON
        # non-string: hard malformed at the exact arm index/key, key
        # omitted.
        arms[1]["source_dir"] = 42
        # dot segment: path-traversal shape under the round-5 closed
        # free-text allowance: soft unrooted finding + hash, raw omitted.
        arms[2]["source_dir"] = "a/./b/"
        # clean path shape beneath no supplied root: the existing soft
        # unrooted finding + omission still applies.
        arms[3]["source_dir"] = "no/such/dir.json"
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        a0 = _by_id(res, "fixture:sight-1-msw-a")
        self.assertEqual(
            a0["source_dir"],
            {"root": "plans", "path": RULES_JSON})
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        self.assertTrue(any(f["source"].get("index") == 1 and
                            f["source"].get("key") == "source_dir"
                            for f in bad))
        self.assertNotIn("source_dir", _by_id(res, "fixture:sight-1-msw-c"))
        a2 = _by_id(res, "fixture:sight-1-swb-run2")
        self.assertNotIn("source_dir", a2)
        sha_a2 = hashlib.sha256(b"a/./b/").hexdigest()
        self.assertTrue(
            any(f["source"].get("index") == 2
                and f["detail"][0]["sha256"] == sha_a2
                for f in _findings(res, "formalism-source-dir-unrooted")))
        self.assertNotIn("a/./b/", _serialize(res))
        unrooted = [f for f in _findings(res, "formalism-source-dir-unrooted")
                    if f["source"].get("index") == 3]
        self.assertEqual(len(unrooted), 1)
        self.assertEqual(unrooted[0]["impact"], "soft")
        self.assertNotIn("source_dir", _by_id(res, "fixture:sight-1-swb-run3"))
        # the live unrooted census is 10 (9 relative + the tagprobe
        # absolute); arms[0] becomes convertible, arms[1] malformed,
        # arms[2] takes the unrooted finding, and arms[3] stays
        # unrooted: 10 - 2.
        self.assertEqual(len(_findings(res, "formalism-source-dir-unrooted")),
                         8)

    # -- 9. provenance copied_from: in-place rewrite + malformed ----------------

    def test_provenance_copied_from_in_place_and_malformed(self):
        plans, worktree = _copy_corpus("r4-prov")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = plans / "parity-formalism" / "tools" / "fixtures"
        # multi-token item: tokens rewritten IN PLACE, the source-authored
        # surrounding reference text retained.
        meta = base / "h2-synthetic-strict-enc" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["copied_from"] = ("see /tmp/r4-abs-prov/report.json then "
                              "/tmp/r4-abs-prov2/other.md for details")
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        # non-string dict item: hard malformed at the exact key; the key
        # is dropped from the provenance (stated choice: drop with finding,
        # never a null value).
        meta2 = base / "ev3-empty-id-400" / "meta.json"
        doc2 = json.loads(meta2.read_text("utf-8"))
        doc2["copied_from"] = {"turn1.json": 123}
        meta2.write_text(json.dumps(doc2, indent=2) + "\n", "utf-8")
        # non-string, non-dict container: hard malformed, nothing copied.
        meta3 = base / "ev4-encitem-503-stub" / "meta.json"
        doc3 = json.loads(meta3.read_text("utf-8"))
        doc3["copied_from"] = ["/tmp/r4-abs-prov3/a.json"]
        meta3.write_text(json.dumps(doc3, indent=2) + "\n", "utf-8")
        # sensitive item: hard sensitive, nothing copied.
        meta4 = base / "h2a-synthetic-mint-tag-own-blob" / "meta.json"
        doc4 = json.loads(meta4.read_text("utf-8"))
        doc4["copied_from"] = f"LEAK {contract.CANARY_TOKEN} END"
        meta4.write_text(json.dumps(doc4, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        blob = _serialize(res)
        s1 = hashlib.sha256(b"/tmp/r4-abs-prov/report.json").hexdigest()
        s2 = hashlib.sha256(b"/tmp/r4-abs-prov2/other.md").hexdigest()
        a = _by_id(res, "fixture:h2-synthetic-strict-enc")
        self.assertEqual(
            a["provenance"]["text"],
            f"see @external/report.json#{s1} then @external/other.md#{s2} "
            "for details")
        self.assertNotIn("/tmp/r4-abs-prov", blob)
        # a clean arm's source-authored relative reference survives
        # verbatim (safe references retained).
        b = _by_id(res, "fixture:sight-1-msw-c")
        self.assertIsInstance(b["provenance"]["text"], str)
        self.assertTrue(b["provenance"]["text"])
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        self.assertTrue(any(f["source"].get("key") == "copied_from.turn1.json"
                            for f in bad))
        self.assertTrue(any(f["source"].get("key") == "copied_from"
                            for f in bad))
        self.assertNotIn("provenance",
                         _by_id(res, "fixture:ev3-empty-id-400"))
        self.assertNotIn("provenance",
                         _by_id(res, "fixture:ev4-encitem-503-stub"))
        sens = _findings(res, "formalism-sensitive-output")
        self.assertTrue(sens)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        self.assertNotIn("provenance",
                         _by_id(res, "fixture:h2a-synthetic-mint-tag-own-blob"))
        self.assertNotIn(contract.CANARY_TOKEN, blob)

    # -- 10. meta ev_stamps shapes + non-dict meta --------------------------------

    def test_meta_ev_stamps_and_meta_shape_malformed(self):
        plans, worktree = _copy_corpus("r4-meta-stamps")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = plans / "parity-formalism" / "tools" / "fixtures"
        # string container: ONE hard malformed finding, never iterated
        # character-wise.
        meta = base / "at-strict-xwire-01" / "meta.json"
        doc = json.loads(meta.read_text("utf-8"))
        doc["ev_stamps"] = "EV-2"
        meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        # non-list value: hard malformed at the exact entry.
        meta2 = base / "ev3-empty-id-400" / "meta.json"
        doc2 = json.loads(meta2.read_text("utf-8"))
        doc2["ev_stamps"] = {"wire/req-001.json": "EV-3"}
        meta2.write_text(json.dumps(doc2, indent=2) + "\n", "utf-8")
        # non-string stamp entry: hard malformed at the exact stamp index;
        # the clean stamp survives.
        meta3 = base / "ev4-encitem-503-stub" / "meta.json"
        doc3 = json.loads(meta3.read_text("utf-8"))
        doc3["ev_stamps"] = {"wire/req-001.json": [123, "EV-4"]}
        meta3.write_text(json.dumps(doc3, indent=2) + "\n", "utf-8")
        # valid JSON but non-dict meta: hard malformed at its own path; the
        # arm is dropped.
        meta4 = base / "h3-synthetic-unknown-id" / "meta.json"
        meta4.write_text("[]\n", "utf-8")
        res = self._run(plans, worktree)
        bad = _findings(res, "formalism-malformed-source")
        self.assertTrue(all(f["impact"] == "hard" for f in bad))
        container = [f for f in bad
                     if f["source"].get("key") == "ev_stamps"
                     and "entry" not in f["source"]]
        self.assertEqual(len(container), 1)
        value = [f for f in bad
                 if f["source"].get("key") == "ev_stamps"
                 and f["source"].get("entry") == 0
                 and "stamp" not in f["source"]]
        self.assertEqual(len(value), 1)
        stamp = [f for f in bad if f["source"].get("stamp") == 0]
        self.assertEqual(len(stamp), 1)
        self.assertTrue(any(
            f["source"].get("path", "").endswith(
                "fixtures/h3-synthetic-unknown-id/meta.json") for f in bad))
        self.assertNotIn("ev_stamps",
                         _by_id(res, "fixture:at-strict-xwire-01"))
        self.assertNotIn("ev_stamps",
                         _by_id(res, "fixture:ev3-empty-id-400"))
        self.assertEqual(_by_id(res, "fixture:ev4-encitem-503-stub")
                         .get("ev_stamps"),
                         {"wire/req-001.json": ["EV-4"]})
        self.assertIsNone(_by_id(res, "fixture:h3-synthetic-unknown-id"))
        self.assertEqual(len(_by_kind(res, "fixture-arm")),
                         PIN_FIXTURE_ARM_COUNT - 1)

    # -- 11. locator_channel "." segments + _SOURCE_DIR_PATH_RE ------------------

    def test_locator_channel_rejects_dot_segments(self):
        plans, worktree = _copy_corpus("r4-dot-seg")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        self._copy_boundary_source_files(worktree)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        idx = next(i for i, a in enumerate(adjs)
                   if isinstance(a, dict) and a.get("evidence_files"))
        adjs[idx]["evidence_files"] = [
            "fixtures/./sight-1-mxai-c04/wire/req-004.json",
            "fixtures/sight-1-mxai-c04/wire/req-004.json",
        ]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        rp = plans / RULES_JSON
        rules = json.loads(rp.read_text("utf-8"))
        rules["boundary_slug_map"]["source_files"][
            "model_boundary_class"] = (
                "crates/./codegen/xai-grok-sampling-types/"
                "src/conversation/projection.rs")
        rp.write_text(json.dumps(rules, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        aliases = _findings(res, "formalism-path-alias")
        self.assertTrue(aliases)
        self.assertTrue(all(f["impact"] == "hard" for f in aliases))
        ev = [f for f in aliases
              if f["source"].get("key", "").startswith("adjudications[")]
        self.assertEqual(len(ev), 1)
        sf = [f for f in aliases
              if f["source"].get("key", "").startswith(
                  "boundary_slug_map.source_files.")]
        self.assertEqual(len(sf), 1)
        adj = _by_id(res, f"adjudication:{adjs[idx]['id']}")
        self.assertEqual(len(adj["evidence"]), 1)
        self.assertEqual(
            adj["evidence"][0]["path"],
            "parity-formalism/tools/fixtures/sight-1-mxai-c04/wire/"
            "req-004.json")
        bmap = _by_id(res, ARTIFACT_RULES_JSON)
        self.assertNotIn("model_boundary_class",
                         bmap["boundary_map"]["source_files"])
        self.assertNotIn("crates/./", _serialize(res))


class Round5ResidualEdgesTest(unittest.TestCase):
    """Batch A round 5 (bead apex-ayl.137): narrow residual edges of the
    raw-emit / no-finding classes closed in rounds 1-4.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The non-echoing locator rule holds throughout: a rejected
    raw value never appears in its own finding's source, and sensitive
    content never reaches the serialized output.
    """

    def _run(self, plans: Path, worktree: Path, extra_roots=None):
        roots = {"worktree": worktree, "plans": plans}
        if extra_roots:
            roots.update(extra_roots)
        ctx = contract.AdapterContext(roots=roots,
                                      catalog=contract.load_catalog())
        return formalism.discover(ctx, ())

    # -- 1. non-dict top-level JSON docs (M1) ---------------------------------

    def test_non_dict_rules_json_is_a_hard_finding(self):
        plans, worktree = _copy_corpus("r5-rules-nondict")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        res0 = self._run(plans, worktree)
        p = plans / RULES_JSON
        p.write_text("[1, 2, 3]\n", "utf-8")
        # A mutated hard_rules.json must not pass silently either: with a
        # non-dict rules file the coupling gate has no valid input and is
        # skipped, but the malformed file itself is a hard finding, so the
        # build fails regardless.
        (worktree / HARD_RULES_JSON).write_bytes(b'{"rules": []}\n')
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"] == {"root": "plans", "path": RULES_JSON}]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(_findings(res, "formalism-generated-rule-drift"), [])
        self.assertTrue(_hard_findings(res))
        self.assertLess(len(res.entities), len(res0.entities))
        # Every parsed entity from the rules doc is gone; only the
        # design-pinned artifact record (source locator = the file) remains.
        self.assertEqual(
            [e for e in res.entities
             if e.get("source", {}).get("path") == RULES_JSON
             and e.get("kind") != "artifact"], [])
        self.assertIsNotNone(_by_id(res, ARTIFACT_RULES_JSON))

    def test_non_dict_verdicts_json_is_a_hard_finding(self):
        plans, worktree = _copy_corpus("r5-verdicts-nondict")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        p.write_text("[1, 2, 3]\n", "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"] ==
               {"root": "plans", "path": EXPECTED_VERDICTS_JSON}]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(_by_kind(res, "request-expectation"), [])
        self.assertEqual(_by_kind(res, "adjudication"), [])
        self.assertTrue(_hard_findings(res))

    # -- 2. read_json parse-success-null vs read failure (M5) -----------------

    def test_json_null_docs_are_hard_findings(self):
        plans, worktree = _copy_corpus("r5-null-docs")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        (plans / RULES_JSON).write_text("null\n", "utf-8")
        (plans / EXPECTED_VERDICTS_JSON).write_text("null\n", "utf-8")
        res = self._run(plans, worktree)
        for rel in (RULES_JSON, EXPECTED_VERDICTS_JSON):
            bad = [f for f in _findings(res, "formalism-malformed-source")
                   if f["source"] == {"root": "plans", "path": rel}]
            self.assertEqual(len(bad), 1, rel)
            self.assertEqual(bad[0]["impact"], "hard")
            self.assertEqual(bad[0]["component"], "formalism-linter")

    # -- 3. ev_registry key echo (M2) ------------------------------------------

    def test_ev_registry_key_canary_is_not_echoed(self):
        plans, worktree = _copy_corpus("r5-evkey-canary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / RULES_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["ev_registry"][contract.CANARY_TOKEN] = "canary key fact"
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f.get("component") == "formalism-linter"
               and f["source"].get("path") == RULES_JSON
               and f["source"].get("key") == "ev_registry"]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertIsInstance(bad[0]["source"].get("index"), int)
        # Zero sensitive findings: the key is shape-checked before any
        # scan, so no sensitive finding carries the raw key.
        self.assertEqual(_findings(res, "formalism-sensitive-output"), [])
        self.assertIsNone(_by_id(res, f"evidence:{contract.CANARY_TOKEN}"))
        self.assertEqual(len(_by_kind(res, "evidence-record")),
                         len(PIN_EV_IDS))
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))

    # -- 4. source_dir traversal shapes (M3) + directory conversion (M9) ------

    def test_source_dir_traversal_shapes_are_findings_not_raw(self):
        plans, worktree = _copy_corpus("r5-srcdir-traversal")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        probes = [
            "../outside/peer-dir",
            "./rel-dir",
            "a//double",
            "a..b",
            "trailing.dot.",
        ]
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        for i, probe in enumerate(probes):
            doc["arms"][i]["source_dir"] = probe
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        for i, probe in enumerate(probes):
            arm = _by_id(res, f"fixture:{doc['arms'][i]['fixture']}")
            self.assertNotIn("source_dir", arm, probe)
            sha = hashlib.sha256(probe.encode("utf-8")).hexdigest()
            unrooted = [
                f for f in _findings(res, "formalism-source-dir-unrooted")
                if f["source"].get("index") == i
                and f["detail"][0]["sha256"] == sha
            ]
            self.assertEqual(len(unrooted), 1, probe)
            self.assertEqual(unrooted[0]["impact"], "soft")
            self.assertEqual(unrooted[0]["level"], "warning")
        blob = _serialize(res)
        for probe in probes:
            self.assertNotIn(probe, blob)
        # The closed free-text allowance still admits the descriptive
        # annotation strings of the live corpus (prose may carry a slash,
        # e.g. "EV-13/EV-9-stamped", but no traversal shape).
        free = _by_id(res, "fixture:h2-synthetic-strict-enc")
        self.assertTrue(free["source_dir"].startswith("SYNTHESIZED"))
        free2 = _by_id(res, "fixture:h2a-synthetic-mint-tag-own-blob")
        self.assertIn("EV-13/EV-9-stamped", free2["source_dir"])
        self.assertEqual(len(_findings(res, "formalism-source-dir-unrooted")),
                         10)

    def test_source_dir_directory_is_convertible(self):
        plans, worktree = _copy_corpus("r5-srcdir-dir")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        cap_rel = "smoke/redteam/report/r5-probe-capture/wire"
        (worktree / cap_rel).mkdir(parents=True)
        (worktree / cap_rel / "req-001.json").write_text("{}", "utf-8")
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["arms"][5]["source_dir"] = cap_rel
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        arm = _by_id(res, f"fixture:{doc['arms'][5]['fixture']}")
        self.assertEqual(arm.get("source_dir"),
                         {"root": "worktree", "path": cap_rel})
        self.assertEqual(
            [f for f in _findings(res, "formalism-source-dir-unrooted")
             if f["source"].get("index") == 5], [])

    # -- 5. fixture meta non-string fields (M4) --------------------------------

    def test_fixture_meta_non_string_fields_are_hard_findings(self):
        plans, worktree = _copy_corpus("r5-meta-fields")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = plans / "parity-formalism" / "tools" / "fixtures"
        cases = [
            ("ev3-empty-id-400", "kind", 42),
            ("ev4-encitem-503-stub", "copied_at", ["2026-09-23"]),
            ("h2-synthetic-strict-enc", "supersession_note", 7),
        ]
        for arm_id, key, value in cases:
            meta = base / arm_id / "meta.json"
            doc = json.loads(meta.read_text("utf-8"))
            doc[key] = value
            meta.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        for arm_id, key, _ in cases:
            bad = [f for f in _findings(res, "formalism-malformed-source")
                   if f.get("component") == "parity-formalism"
                   and f["source"].get("key") == key
                   and f["source"].get("path", "").endswith(
                       f"fixtures/{arm_id}/meta.json")]
            self.assertEqual(len(bad), 1, key)
            self.assertEqual(bad[0]["impact"], "hard")
        self.assertNotIn("native_kind",
                         _by_id(res, "fixture:ev3-empty-id-400"))
        self.assertNotIn("copied_at",
                         _by_id(res, "fixture:ev4-encitem-503-stub"))
        self.assertNotIn("supersession",
                         _by_id(res, "fixture:h2-synthetic-strict-enc"))

    # -- 6. duplicate verdict fixture id (M6) ----------------------------------

    def test_duplicate_verdict_fixture_id_is_hard(self):
        plans, worktree = _copy_corpus("r5-dup-arm")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        dup = copy.deepcopy(doc["arms"][0])
        dup["expected_exit_code"] = 99
        doc["arms"].append(dup)
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f.get("component") == "formalism-linter"
               and f["source"].get("path") == EXPECTED_VERDICTS_JSON
               and f["source"].get("key") == "fixture"
               and f["source"].get("index") == len(doc["arms"]) - 1]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        # Stated choice: keep-first — the first arm's verdict fields win
        # the arms map; the duplicate's are not copied over (mirrors the
        # entity id-collision rule).
        first = doc["arms"][0]
        arm = _by_id(res, f"fixture:{first['fixture']}")
        self.assertEqual(arm.get("expected_exit_code"),
                         first.get("expected_exit_code"))
        self.assertNotEqual(arm.get("expected_exit_code"), 99)

    # -- 7. negative request n / adjudication counts (M7) ----------------------

    def test_negative_request_and_hit_counts_are_rejected(self):
        plans, worktree = _copy_corpus("r5-negative-n")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["arms"][0]["requests"][0]["n"] = -1
        doc["adjudications"][0]["hits"] = -3
        doc["adjudications"][1]["n"] = -2
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        self.assertNotIn("request:sight-1-msw-a:-01", _serialize(res))
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f.get("component") == "formalism-linter"
               and f["source"].get("path") == EXPECTED_VERDICTS_JSON]
        req = [f for f in bad if f["source"].get("key") == "n"
               and f["source"].get("entry") == 0
               and f["source"].get("index") == 0]
        self.assertEqual(len(req), 1)
        self.assertEqual(req[0]["impact"], "hard")
        hits = [f for f in bad if f["source"].get("key") == "hits"
                and f["source"].get("index") == 0]
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]["impact"], "hard")
        adjn = [f for f in bad if f["source"].get("key") == "n"
                and f["source"].get("index") == 1
                and "entry" not in f["source"]]
        self.assertEqual(len(adjn), 1)
        self.assertEqual(adjn[0]["impact"], "hard")
        self.assertNotIn("hits", _by_id(res, "adjudication:OQ-T8-1"))
        self.assertNotIn("n", _by_id(res, "adjudication:OQ-T8-2"))

    # -- 8. sensitive scan on the locator channel + artifact registration (M8) -

    def test_relative_locator_channel_is_sensitive_scanned(self):
        plans, worktree = _copy_corpus("r5-locscan")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        Round4RawEmitAndMalformedTest._copy_boundary_source_files(worktree)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        adjs = doc["adjudications"]
        idx = next(i for i, a in enumerate(adjs)
                   if isinstance(a, dict) and a.get("evidence_files"))
        clean = adjs[idx]["evidence_files"][-1]
        canary_rel = f"fixtures/{contract.CANARY_TOKEN}/req-001.json"
        adjs[idx]["evidence_files"] = [canary_rel, clean]
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        rp = plans / RULES_JSON
        rules = json.loads(rp.read_text("utf-8"))
        rules["boundary_slug_map"]["source_files"]["r5-probe"] = (
            f"crates/{contract.CANARY_TOKEN}/x.rs")
        rp.write_text(json.dumps(rules, indent=2) + "\n", "utf-8")
        # H-7 pin: append a canary-named pin to the H-7 row.
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        marker = f"`{XWAVEC71_REPORT}:75`"
        out_lines = []
        replaced = False
        for line in text.splitlines(keepends=True):
            if line.startswith("| H-7") and marker in line \
                    and not replaced:
                out_lines.append(line.replace(
                    marker, marker + f" `{contract.CANARY_TOKEN}.md`"))
                replaced = True
            else:
                out_lines.append(line)
        self.assertTrue(replaced)
        hp.write_text("".join(out_lines), "utf-8")
        res = self._run(plans, worktree)
        # Four findings: the canary pin (locator channel), the curated
        # §2.1 section snippet (the mutated H-7 row sits inside the range),
        # the boundary source_files entry, and the adjudication evidence
        # entry. All hard.
        sens = _findings(res, "formalism-sensitive-output")
        self.assertEqual(len(sens), 4)
        self.assertTrue(all(f["impact"] == "hard" for f in sens))
        self.assertEqual(
            [f for f in sens
             if f["source"].get("key")
             == f"adjudications[{idx}].evidence_files[0]"],
            [f for f in sens
             if f["source"].get("key", "").startswith("adjudications[")])
        self.assertEqual(
            [f for f in sens
             if f["source"].get("key")
             == "boundary_slug_map.source_files.r5-probe"],
            [f for f in sens
             if f["source"].get("key", "").startswith(
                 "boundary_slug_map.source_files.")])
        adj = _by_id(res, f"adjudication:{adjs[idx]['id']}")
        self.assertEqual(len(adj["evidence"]), 1)
        self.assertEqual(adj["evidence"][0]["path"],
                         f"parity-formalism/tools/{clean}")
        bmap = _by_id(res, ARTIFACT_RULES_JSON)
        self.assertNotIn("r5-probe",
                         bmap["boundary_map"]["source_files"])
        h7 = _by_id(res, "rule:H-7")
        self.assertEqual([pin["path"] for pin in h7["code_pins"]],
                         [SDD_71, XWAVEC71_REPORT])
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))

    def test_artifact_registration_scans_walked_filenames(self):
        plans, worktree = _copy_corpus("r5-artifact-canary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        canary_rel = f"{OUTBOUND_LINT_DIR}/{contract.CANARY_TOKEN}.json"
        (worktree / canary_rel).write_text("{}", "utf-8")
        res = self._run(plans, worktree)
        sens = [f for f in _findings(res, "formalism-sensitive-output")
                if f["source"].get("root") == "worktree"
                and f["source"].get("path") == OUTBOUND_LINT_DIR]
        self.assertEqual(len(sens), 1)
        self.assertEqual(sens[0]["impact"], "hard")
        self.assertIsNotNone(sens[0].get("rel_sha256"))
        self.assertIsNone(_by_id(res, f"artifact:worktree:{canary_rel}"))
        # Sibling walked artifacts are unaffected.
        self.assertIsNotNone(_by_id(res, ARTIFACT_HARD_RULES))
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))

    # -- 9. present-but-empty text values (M10) --------------------------------

    def test_empty_text_values_are_rejected_with_findings(self):
        plans, worktree = _copy_corpus("r5-empty-text")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        rp = plans / RULES_JSON
        rules = json.loads(rp.read_text("utf-8"))
        rules["boundary_slug_map"]["o_series_first_char"] = ""
        rp.write_text(json.dumps(rules, indent=2) + "\n", "utf-8")
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["arms"][0]["source_dir"] = ""
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        meta = (plans / "parity-formalism" / "tools" / "fixtures"
                / "at-strict-xwire-01" / "meta.json")
        m = json.loads(meta.read_text("utf-8"))
        m["ev_stamps"]["wire/req-006.json"] = [""]
        m["copied_at"] = ""
        meta.write_text(json.dumps(m, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = _findings(res, "formalism-malformed-source")
        bm = [f for f in bad
              if f["source"].get("key")
              == "boundary_slug_map.o_series_first_char"]
        self.assertEqual(len(bm), 1)
        self.assertEqual(bm[0]["impact"], "hard")
        artifact = _by_id(res, ARTIFACT_RULES_JSON)
        self.assertNotIn(
            "o_series_first_char",
            artifact["boundary_map"]["symbolic_values"])
        sd = [f for f in bad if f["source"].get("key") == "source_dir"
              and f["source"].get("index") == 0
              and f["source"].get("path") == EXPECTED_VERDICTS_JSON]
        self.assertEqual(len(sd), 1)
        self.assertEqual(sd[0]["impact"], "hard")
        self.assertNotIn("source_dir",
                         _by_id(res, "fixture:sight-1-msw-a"))
        arm = _by_id(res, "fixture:at-strict-xwire-01")
        self.assertNotIn("copied_at", arm)
        ca = [f for f in bad if f["source"].get("key") == "copied_at"
              and f["source"].get("path", "").endswith(
                  "fixtures/at-strict-xwire-01/meta.json")]
        self.assertEqual(len(ca), 1)
        self.assertEqual(ca[0]["impact"], "hard")
        self.assertEqual(list(arm["ev_stamps"].keys()),
                         ["wire/req-002.json..wire/req-005.json"])
        stamp = [f for f in bad if f["source"].get("key") == "ev_stamps"
                 and f["source"].get("stamp") == 0]
        self.assertEqual(len(stamp), 1)
        self.assertEqual(stamp[0]["impact"], "hard")

    # -- 10. clean-path gate aligned with paths._validate_posix_relative (M11) -

    def test_source_dir_clean_gate_aligned_with_validator(self):
        st = formalism._State(contract.AdapterContext(
            roots={"plans": Path("/tmp")}, catalog={}))

        def validator_clean(value: str) -> bool:
            probe = value[:-1] if value.endswith("/") else value
            try:
                paths._validate_posix_relative(probe)
            except contract.ContractError:
                return False
            return True

        values = [
            "a/b", "a/b/", "a/b/c", "x", ".hidden", "a/.hidden/b",
            "trailing.dot.", "a..b", "a/..b/c", "a/./b", "a/./b/",
            "a//b", "../x", "./x", "/abs", "a\\b", "", "a/", "a.b",
        ]
        for value in values:
            self.assertEqual(st._source_dir_clean(value),
                             validator_clean(value), value)
        # The previously divergent cases, pinned explicitly (m11): a
        # trailing dot is a clean segment per the normative validator;
        # a dot segment or a '..' run is not.
        self.assertTrue(st._source_dir_clean("trailing.dot."))
        self.assertTrue(st._source_dir_clean("a/b/"))
        self.assertFalse(st._source_dir_clean("a/./b/"))
        self.assertFalse(st._source_dir_clean("a..b"))
        self.assertFalse(st._source_dir_clean("a/..b/c"))


class Round6ResidualEdgesTest(unittest.TestCase):
    """Batch A round 6 (bead apex-ayl.137): residual gaps verified in the
    round-5 bytes by three review seats:

    - MAJOR-A: the ``_source_dir`` absolute-token branch emitted
      normalized ``@root/...`` / ``@external/...`` tokens with NO
      sensitive scan (a planted canary directory name or raw-key
      basename reached the serialized output with zero findings);
    - MAJOR-B: a truncated (sub-4-cell) ``| H-`` row, or a C-category
      row failing the exact four-column shape, vanished with zero
      findings (DESIGN.md §8: malformed data never silently
      disappears);
    - m-1: ``_scan_only`` accepted a present-but-empty string;
    - m-2: the converted (rooted) ``source_dir`` emit kept a trailing
      slash the normative validator rejects;
    - m-3: a non-string catalog component input was silently skipped.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The non-echoing locator rule holds throughout: rejected
    values and sensitive content never reach the serialized output.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    # -- MAJOR-A: sensitive scan on the absolute-token branch ----------------

    def test_source_dir_under_root_absolute_canary_is_dropped(self):
        plans, worktree = _copy_corpus("r6-srcdir-abs-canary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        cap_rel = f"capture/{contract.CANARY_TOKEN}/wire"
        (worktree / cap_rel).mkdir(parents=True)
        (worktree / cap_rel / "req-001.json").write_text("{}", "utf-8")
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        # An ABSOLUTE source_dir pointing at the canary-named directory
        # in the worktree copy: the under-root normalized token
        # (``@worktree/...``) must be scanned like every emitted path.
        doc["arms"][0]["source_dir"] = str(worktree / cap_rel)
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sens = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("path") == EXPECTED_VERDICTS_JSON
            and f["source"].get("index") == 0
            and f["source"].get("key") == "source_dir"
        ]
        self.assertEqual(len(sens), 1)
        self.assertEqual(sens[0]["impact"], "hard")
        self.assertEqual(sens[0]["component"], "formalism-linter")
        self.assertEqual(sens[0]["detail"][0]["pattern"], "canary")
        # The token is dropped; with no surviving token the field is
        # omitted entirely (the relative branch's drop semantics).
        arm = _by_id(res, "fixture:sight-1-msw-a")
        self.assertNotIn("source_dir", arm)
        self.assertEqual(
            [f for f in _findings(res, "formalism-source-dir-unrooted")
             if f["source"].get("index") == 0], [])
        blob = _serialize(res)
        self.assertNotIn(contract.CANARY_TOKEN, blob)
        self.assertNotIn(str(worktree), blob)

    def test_source_dir_out_of_root_raw_key_basename_is_dropped(self):
        plans, worktree = _copy_corpus("r6-srcdir-ext-key")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        # A basename that matches the pinned RAW_KEY_PATTERN
        # (sk-[A0-9a-z]{16,}: 'A', digits, lowercase only).
        raw_key = "sk-externalkey12345678901"
        clean_abs = "/abs/probe-clean-a/probe-clean-b"
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        # An out-of-root absolute path whose BASENAME is the raw key:
        # the ``@external/<base>#<sha>`` form would expose the basename
        # beside only the soft unrooted finding.
        doc["arms"][1]["source_dir"] = f"/abs/elsewhere/{raw_key}"
        # A sibling arm with a clean out-of-root token: the @external
        # form and the soft unrooted finding still apply when the token
        # survives.
        doc["arms"][2]["source_dir"] = clean_abs
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        sens = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("path") == EXPECTED_VERDICTS_JSON
            and f["source"].get("index") == 1
            and f["source"].get("key") == "source_dir"
        ]
        self.assertEqual(len(sens), 1)
        self.assertEqual(sens[0]["impact"], "hard")
        self.assertEqual(sens[0]["component"], "formalism-linter")
        self.assertEqual(sens[0]["detail"][0]["pattern"], "raw-key")
        arm = _by_id(res, "fixture:sight-1-msw-c")
        self.assertNotIn("source_dir", arm)
        self.assertEqual(
            [f for f in _findings(res, "formalism-source-dir-unrooted")
             if f["source"].get("index") == 1], [])
        self.assertNotIn(raw_key, _serialize(res))
        clean_arm = _by_id(res, "fixture:sight-1-swb-run2")
        sha = hashlib.sha256(clean_abs.encode("utf-8")).hexdigest()
        self.assertEqual(clean_arm.get("source_dir"),
                         [f"@external/probe-clean-b#{sha}"])
        unrooted = [
            f for f in _findings(res, "formalism-source-dir-unrooted")
            if f["source"].get("index") == 2
        ]
        self.assertEqual(len(unrooted), 1)
        self.assertEqual(unrooted[0]["impact"], "soft")

    # -- MAJOR-B: truncated / mis-shaped pinned table rows --------------------

    def test_truncated_h_rule_rows_are_hard_findings(self):
        plans, worktree = _copy_corpus("r6-hrow-truncated")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        # The unmutated live shape must not trip the new gate: the
        # 4+-cell H-1/H-3/H-4/H-6 rows are intentionally ignored (those
        # rules live in invariant_rules.json) and the well-formed
        # H-2/H-5/H-7 rows parse as before.
        base = self._run(plans, worktree)
        self.assertEqual(
            [f for f in _findings(base, "formalism-malformed-source")
             if "row" in f["source"]], [])
        self.assertIsNotNone(_by_id(base, "rule:H-7"))
        self.assertIsNotNone(_by_id(base, "rule:H-2:superseded"))
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        row_idx: dict[str, int] = {}
        for prefix in ("| H-2", "| H-7"):
            line = next(l for l in rng_lines if l.startswith(prefix))
            cells = line.strip().strip("|").split("|")
            self.assertGreaterEqual(len(cells), 4, prefix)
            truncated = "|" + "|".join(cells[:3]) + "|"
            self.assertNotIn(truncated + "\n", text, prefix)
            text = text.replace(line + "\n", truncated + "\n", 1)
            row_idx[prefix] = rng_lines.index(line)
        hp.write_text(text, "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"].get("path") == HARDENING_MD
               and "row" in f["source"]]
        self.assertEqual(len(bad), 2)
        for prefix in ("| H-2", "| H-7"):
            f = next(x for x in bad
                     if x["source"]["row"] == row_idx[prefix])
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["level"], "error")
            self.assertEqual(f["component"], "parity-formalism")
            self.assertEqual(f["source"],
                             {"root": "plans", "path": HARDENING_MD,
                              "heading": HEADING_2_1,
                              "row": row_idx[prefix]})
        # The truncated H-7 rule, its code pins, and its enforced-by
        # edges vanish; the truncated H-2 superseded clause and its
        # supersedes edge vanish. The current rules (owned by
        # invariant_rules.json) survive.
        self.assertIsNone(_by_id(res, "rule:H-7"))
        self.assertIsNone(_by_id(res, "rule:H-2:superseded"))
        self.assertIsNotNone(_by_id(res, "rule:H-2"))
        self.assertEqual(
            [r for r in _rels(res, "supersedes")
             if r["target"] == "rule:H-2:superseded"], [])
        self.assertEqual(
            [r for r in _rels(res, "enforced-by")
             if r["source"] == "rule:H-7"], [])
        self.assertTrue(_hard_findings(res))

    def test_misshaped_category_row_is_hard_finding(self):
        plans, worktree = _copy_corpus("r6-category-shape")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        # The unmutated live shape must not trip the new gate: all seven
        # well-formed C rows parse and the header/separator rows stay
        # silently skipped.
        base = self._run(plans, worktree)
        self.assertEqual(
            [f for f in _findings(base, "formalism-malformed-source")
             if "row" in f["source"]], [])
        self.assertEqual(
            sorted(e["category_id"] for e in _by_kind(base, "category")),
            PIN_CATEGORY_IDS)
        fp = plans / FORMALISM_MD
        text = fp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_CATEGORY)
        rng_lines = rng.splitlines()
        c1 = next(l for l in rng_lines if l.startswith("| C1 |"))
        row_idx = rng_lines.index(c1)
        extra = c1 + " extra |"
        self.assertNotIn(extra + "\n", text)
        fp.write_text(text.replace(c1 + "\n", extra + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"].get("path") == FORMALISM_MD
               and f["source"].get("row") == row_idx]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": FORMALISM_MD,
                          "heading": HEADING_CATEGORY,
                          "row": row_idx})
        # The category entity and its defines edge vanish; the other six
        # categories and their edges are intact.
        self.assertIsNone(_by_id(res, "category:C1"))
        sec_id = next(
            e["id"] for e in _by_kind(res, "document-section")
            if e.get("path") == FORMALISM_MD
            and e.get("selector", {}).get("kind") == "heading"
            and e["selector"].get("text") == HEADING_CATEGORY)
        self.assertEqual(
            [r for r in _rels(res, "defines")
             if r["source"] == sec_id and r["target"] == "category:C1"],
            [])
        self.assertEqual(
            sorted(e["category_id"] for e in _by_kind(res, "category")),
            ["C2", "C3", "C4", "C5", "C6", "C7"])
        self.assertTrue(_hard_findings(res))

    # -- m-1: _scan_only present-but-empty ------------------------------------

    def test_scan_only_rejects_present_empty_request_path(self):
        plans, worktree = _copy_corpus("r6-empty-reqpath")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        n = doc["arms"][0]["requests"][0]["n"]
        doc["arms"][0]["requests"][0]["path"] = ""
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in _findings(res, "formalism-malformed-source")
               if f["source"].get("path") == EXPECTED_VERDICTS_JSON
               and f["source"].get("index") == 0
               and f["source"].get("request_n") == n
               and f["source"].get("key") == "path"]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        # The value is dropped: the field is omitted, not null and not
        # an empty string.
        req = _by_id(res, f"request:sight-1-msw-a:{n:03d}")
        self.assertNotIn("path", req)
        # The clean sibling request keeps its path.
        sib = doc["arms"][0]["requests"][1]
        sib_req = _by_id(res, f"request:sight-1-msw-a:{sib['n']:03d}")
        self.assertEqual(sib_req.get("path"), sib["path"])

    # -- m-2: rooted source_dir emit strips the trailing slash ----------------

    def test_rooted_source_dir_emit_has_no_trailing_slash(self):
        plans, worktree = _copy_corpus("r6-srcdir-slash")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        cap_rel = "smoke/redteam/report/xyz/wire"
        (worktree / cap_rel).mkdir(parents=True)
        (worktree / cap_rel / "req-001.json").write_text("{}", "utf-8")
        p = plans / EXPECTED_VERDICTS_JSON
        doc = json.loads(p.read_text("utf-8"))
        doc["arms"][5]["source_dir"] = cap_rel + "/"
        p.write_text(json.dumps(doc, indent=2) + "\n", "utf-8")
        res = self._run(plans, worktree)
        arm = _by_id(res, "fixture:sight-1-mxai-c07")
        self.assertEqual(arm.get("source_dir"),
                         {"root": "worktree", "path": cap_rel})
        self.assertEqual(
            [f for f in _findings(res, "formalism-source-dir-unrooted")
             if f["source"].get("index") == 5], [])

    # -- m-3: non-string catalog component input ------------------------------

    def test_non_string_catalog_component_input_is_hard(self):
        plans, worktree = _copy_corpus("r6-catalog-input")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        comp = next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")
        comp["inputs"].append(
            {"root": "plans",
             "path": "parity-formalism/tools/invariant_rules.json"})
        bad_index = len(comp["inputs"]) - 1
        res = self._run(plans, worktree, catalog=catalog)
        row = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
            and f["source"].get("index") == bad_index
        ]
        self.assertEqual(len(row), 1)
        self.assertEqual(row[0]["impact"], "hard")
        self.assertEqual(row[0]["component"], "formalism-linter")
        self.assertEqual(row[0]["source"]["root"], "worktree")
        self.assertEqual(row[0]["source"]["path"],
                         "smoke/eval_harness/catalog.json")
        # The malformed item is skipped; the rest of the catalog walk is
        # intact.
        self.assertIsNotNone(_by_id(res, ARTIFACT_RULES_JSON))
        self.assertTrue(_hard_findings(res))


class Round7RowAndCatalogGatesTest(unittest.TestCase):
    """Batch A round 7 (bead apex-ayl.137): coordinator-adjudicated fixes.

    - M-1: a truthy non-list ``formalism-linter.inputs`` container
      (a string char-iterates into fabricated ``artifact:plans:<char>``
      junk while the declared artifacts vanish with zero findings; an
      int escapes as a raw ``TypeError``) is ONE hard
      ``formalism-malformed-source`` at the catalog-file locator (no
      index for a container violation) and the input walk is skipped;
    - M-2: GFM-legal leading-blank (0-3 space) table rows are accepted
      by both row gates (``| H-``/``|H-`` and the C-row regexes); 4+
      leading spaces is a code block and stays prose;
    - M-3: a ``| H-`` row whose cell count is not 4 (truncated <4 OR
      extra >4) is a hard finding + skipped, matching the C-row
      four-column discipline; unknown-rid 4-cell rows stay
      intentionally ignored (live H-1/H-3/H-4/H-6 shape pin);
    - M-4a: absolute or ``..``-segment catalog strings at the two
      id-mint sites (the artifact input walk, the curated_sources
      section mint) are hard findings + drops, never raw emits or
      host reads.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero
    row-gate fires, zero catalog-file-locator findings.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _row_findings(res, path: str) -> list:
        return [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("path") == path and "row" in f["source"]
        ]

    @staticmethod
    def _declared_artifact_ids(catalog: dict, plans: Path,
                               worktree: Path) -> set:
        """The artifact ids the unmutated catalog walk mints: the seven
        formalism-linter input entries with the ``**`` glob expanded
        against the supplied worktree copy (20 artifacts on the live
        catalog)."""
        comp = next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")
        comp_root = comp.get("root", "plans")
        roots = {"worktree": worktree, "plans": plans}
        declared = set()
        for item in comp["inputs"]:
            if ":" in item:
                root_id, rel = item.split(":", 1)
            else:
                root_id, rel = comp_root, item
            if root_id not in roots:
                continue
            if rel.endswith("/**"):
                base = roots[root_id] / rel[:-3]
                for dirpath, dirnames, filenames in os.walk(
                        base, followlinks=False):
                    for name in sorted(filenames):
                        full = Path(dirpath) / name
                        declared.add(
                            f"artifact:{root_id}:"
                            + full.relative_to(roots[root_id]).as_posix())
            else:
                declared.add(f"artifact:{root_id}:{rel}")
        return declared

    # -- unmutated live shape pins (M-2f / M-3c / M-4a-e) -------------------

    def test_live_shape_no_row_gate_fires(self):
        # Read-only live run (DESIGN §5 invocation): the unmutated live
        # shape is 172 entities / 84 relationships / 10 findings (all
        # soft) and neither row gate fires (the live table rows are
        # column-0).
        plans, worktree = _plans_root(), _worktree_root()
        res = _discover(plans, worktree)
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)
        self.assertEqual(len(res.findings), 10)
        self.assertEqual(_hard_findings(res), [])
        self.assertEqual(self._row_findings(res, FORMALISM_MD), [])
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])

    def test_live_catalog_locator_zero_findings(self):
        # Read-only live run: zero findings rooted at the catalog file.
        plans, worktree = _plans_root(), _worktree_root()
        res = _discover(plans, worktree)
        self.assertEqual(
            [f for f in res.findings
             if f["source"].get("path") == "smoke/eval_harness/catalog.json"],
            [])
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)
        self.assertEqual(len(res.findings), 10)
        self.assertEqual(_hard_findings(res), [])

    # -- M-1: non-list inputs container gate --------------------------------

    def test_string_inputs_container_is_one_hard_no_junk(self):
        plans, worktree = _copy_corpus("r7-inputs-string")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        # Baseline pin: every catalog-declared artifact (the seven
        # input entries, the glob expanded: 20) is present.
        declared = self._declared_artifact_ids(catalog, plans, worktree)
        self.assertEqual(len(declared), 20)
        base_ids = {e["id"] for e in base.entities
                    if e.get("kind") == "artifact"}
        self.assertTrue(declared <= base_ids)
        # The real declared input string becomes the whole container:
        # pre-fix, its characters each mint an artifact and the real
        # declared walk never runs.
        real_input = comp["inputs"][2]
        self.assertIsInstance(real_input, str)
        comp["inputs"] = real_input
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        # The container violation carries no index.
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "formalism-linter.inputs"})
        # Zero char-junk entities: the skipped walk mints nothing new
        # (no fabricated ``artifact:plans:<char>`` ids at all).
        mut_ids = {e["id"] for e in res.entities
                   if e.get("kind") == "artifact"}
        self.assertEqual(mut_ids - base_ids, set())
        self.assertNotIn("artifact:plans:a", mut_ids)
        # The registrations independent of the input list survive the
        # skipped walk (design-pinned core + the H-7 code pin).
        pinned = {
            ARTIFACT_RULES_JSON, ARTIFACT_GENERATOR, ARTIFACT_LINTER,
            ARTIFACT_RULES_GENERATED, ARTIFACT_PROJECTION_TESTS,
            ARTIFACT_HARD_RULES, ARTIFACT_XWAVEC71,
        }
        self.assertTrue(pinned <= mut_ids)
        # No findings delta beyond that one.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_dict_inputs_container_is_one_hard_no_junk(self):
        plans, worktree = _copy_corpus("r7-inputs-dict")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        comp["inputs"] = {"only": "one"}
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertNotIn("index", bad[0]["source"])
        # Pre-fix the dict's keys char-mint junk; the fix mints
        # nothing new and keeps the independent registrations.
        base_ids = {e["id"] for e in base.entities
                    if e.get("kind") == "artifact"}
        mut_ids = {e["id"] for e in res.entities
                   if e.get("kind") == "artifact"}
        self.assertEqual(mut_ids - base_ids, set())
        self.assertNotIn("artifact:plans:only", mut_ids)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_int_inputs_container_is_one_hard_no_typeerror(self):
        plans, worktree = _copy_corpus("r7-inputs-int")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        comp["inputs"] = 42
        # Pre-fix this escapes as a raw
        # TypeError: 'int' object is not iterable.
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertNotIn("index", bad[0]["source"])
        self.assertTrue(_hard_findings(res))

    def test_none_and_empty_inputs_containers_are_clean(self):
        # ``None`` (absent) and ``[]`` (present-empty) containers are
        # not violations: no catalog-file finding, no error, and the
        # independently-registered (design-pinned) artifacts survive.
        for tag, value in (("none", None), ("empty", [])):
            with self.subTest(container=tag):
                plans, worktree = _copy_corpus(f"r7-inputs-{tag}")
                self.addCleanup(shutil.rmtree, plans.parent,
                                ignore_errors=True)
                catalog = contract.load_catalog()
                comp = self._linter_comp(catalog)
                comp["inputs"] = value
                res = self._run(plans, worktree, catalog=catalog)
                self.assertEqual(
                    [f for f in res.findings
                     if f["source"].get("path")
                     == "smoke/eval_harness/catalog.json"],
                    [])
                self.assertIsNotNone(_by_id(res, ARTIFACT_RULES_JSON))
                self.assertIsNotNone(
                    _by_id(res, ARTIFACT_PROJECTION_TESTS))

    # -- M-2: GFM-legal leading-blank (0-3 space) table rows ----------------

    def test_leading_space_category_row_still_parses(self):
        plans, worktree = _copy_corpus("r7-lead-c1")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        # Unmutated: zero row findings (the live rows are column-0).
        self.assertEqual(self._row_findings(base, FORMALISM_MD), [])
        fp = plans / FORMALISM_MD
        text = fp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_CATEGORY)
        rng_lines = rng.splitlines()
        c1 = next(l for l in rng_lines if l.startswith("| C1 |"))
        row_idx = rng_lines.index(c1)
        self.assertNotIn(" " + c1 + "\n", text)
        # One leading space: a well-formed GFM table row (0-3 spaces).
        fp.write_text(text.replace(c1 + "\n", " " + c1 + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        # No false fire on the well-formed row, no findings delta.
        self.assertEqual(self._row_findings(res, FORMALISM_MD), [])
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(f) for f in base.findings})
        # The entity and its defines edge survive.
        self.assertIsNotNone(_by_id(res, "category:C1"))
        self.assertEqual(
            sorted(e["category_id"] for e in _by_kind(res, "category")),
            PIN_CATEGORY_IDS)
        sec = next(e for e in _by_kind(res, "document-section")
                   if e.get("path") == FORMALISM_MD
                   and e.get("selector", {}).get("kind") == "heading"
                   and e["selector"].get("text") == HEADING_CATEGORY)
        self.assertEqual(
            len([r for r in _rels(res, "defines")
                 if r["source"] == sec["id"]
                 and r["target"] == "category:C1"]),
            1)

    def test_leading_space_truncated_category_row_is_hard(self):
        plans, worktree = _copy_corpus("r7-lead-c2")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        fp = plans / FORMALISM_MD
        text = fp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_CATEGORY)
        rng_lines = rng.splitlines()
        c2 = next(l for l in rng_lines if l.startswith("| C2 |"))
        row_idx = rng_lines.index(c2)
        # Leading space + only three cells: GFM row, wrong column count.
        fp.write_text(text.replace(c2 + "\n", " | C2 | a | b |\n", 1),
                      "utf-8")
        res = self._run(plans, worktree)
        bad = self._row_findings(res, FORMALISM_MD)
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": FORMALISM_MD,
                          "heading": HEADING_CATEGORY, "row": row_idx})
        # The category is dropped; the other six survive.
        self.assertIsNone(_by_id(res, "category:C2"))
        self.assertEqual(
            sorted(e["category_id"] for e in _by_kind(res, "category")),
            ["C1", "C3", "C4", "C5", "C6", "C7"])

    def test_leading_space_truncated_h_row_is_hard(self):
        plans, worktree = _copy_corpus("r7-lead-h7")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        row_idx = rng_lines.index(h7)
        cells = [c.strip() for c in h7.strip().strip("|").split("|")]
        self.assertEqual(len(cells), 4)
        # Leading space + truncated to three cells.
        truncated = " " + "| " + " | ".join(cells[:3]) + " |"
        self.assertNotIn(truncated + "\n", text)
        hp.write_text(text.replace(h7 + "\n", truncated + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        bad = self._row_findings(res, HARDENING_MD)
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": HARDENING_MD,
                          "heading": HEADING_2_1, "row": row_idx})
        self.assertIsNone(_by_id(res, "rule:H-7"))

    def test_no_space_pipe_h_row_truncated_is_hard(self):
        plans, worktree = _copy_corpus("r7-nospace-h7")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        row_idx = rng_lines.index(h7)
        cells = [c.strip() for c in h7.strip().strip("|").split("|")]
        # The no-space-after-pipe variant (the recorded workhorse
        # observation, folded into M-2), truncated to three cells.
        truncated = "|H-7|" + "|".join(cells[1:3]) + "|"
        self.assertNotIn(truncated + "\n", text)
        hp.write_text(text.replace(h7 + "\n", truncated + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        bad = self._row_findings(res, HARDENING_MD)
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": HARDENING_MD,
                          "heading": HEADING_2_1, "row": row_idx})
        self.assertIsNone(_by_id(res, "rule:H-7"))

    def test_four_space_indented_h_row_stays_prose(self):
        plans, worktree = _copy_corpus("r7-4space-h7")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        row_idx = rng_lines.index(h7)
        # Four leading spaces is NOT a GFM table row (it is a code
        # block): prose, no row finding, no rule entity minted.
        hp.write_text(text.replace(h7 + "\n", "    " + h7 + "\n", 1),
                      "utf-8")
        res = self._run(plans, worktree)
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])
        self.assertIsNone(_by_id(res, "rule:H-7"))
        self.assertEqual(
            [r for r in _rels(res, "enforced-by")
             if r["source"] == "rule:H-7"],
            [])
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(f) for f in base.findings})

    # -- M-3: H-row cell count must be exactly 4 -----------------------------

    def test_h_row_with_extra_cell_is_hard(self):
        plans, worktree = _copy_corpus("r7-hrow-extra")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        row_idx = rng_lines.index(h7)
        # Append a fifth cell to the live four-cell row: the extra
        # cell must no longer be silently ignored.
        self.assertNotIn(h7 + " extra-cell |\n", text)
        hp.write_text(text.replace(h7 + "\n", h7 + " extra-cell |\n", 1),
                      "utf-8")
        res = self._run(plans, worktree)
        bad = [f for f in self._row_findings(res, HARDENING_MD)
               if f["source"]["row"] == row_idx]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": HARDENING_MD,
                          "heading": HEADING_2_1, "row": row_idx})
        # Zero other findings.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})
        # rule H-7, its pins, and its enforced-by edges are gone.
        self.assertIsNone(_by_id(res, "rule:H-7"))
        self.assertEqual(
            [r for r in _rels(res, "enforced-by")
             if r["source"] == "rule:H-7"],
            [])

    def test_unknown_h_row_four_cells_stays_ignored(self):
        plans, worktree = _copy_corpus("r7-hrow-unknown")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        # A planted well-formed four-cell row for an unknown rid: the
        # intentional-ignore pin (the live H-1/H-3/H-4/H-6 rows are
        # owned by invariant_rules.json) must not weaken.
        planted = "| H-9 | all | planted invariant text | EV-2 |"
        self.assertNotIn(planted + "\n", text)
        hp.write_text(text.replace(h7 + "\n", h7 + "\n" + planted + "\n", 1),
                      "utf-8")
        res = self._run(plans, worktree)
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])
        self.assertIsNone(_by_id(res, "rule:H-9"))
        # The shape is otherwise untouched (the copy pins the same
        # 172/84 entity/relationship shape as the live corpus).
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(f) for f in base.findings})
        self.assertEqual(_hard_findings(res), [])
        self.assertIsNotNone(_by_id(res, "rule:H-7"))

    # -- M-4a: absolute/..-segment catalog strings at the id-mint sites ------

    def test_absolute_rooted_catalog_input_is_dropped(self):
        plans, worktree = _copy_corpus("r7-abs-input")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        comp["inputs"].append("plans:/abs/outside.txt")
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
            and f["source"].get("index") == len(comp["inputs"]) - 1
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"]["root"], "worktree")
        self.assertEqual(bad[0]["source"]["path"],
                         "smoke/eval_harness/catalog.json")
        # No artifact is minted from the rejected spec, and the raw
        # absolute path never reaches the serialized output.
        artifact_ids = {e["id"] for e in res.entities
                        if e.get("kind") == "artifact"}
        self.assertNotIn("artifact:plans:/abs/outside.txt", artifact_ids)
        self.assertNotIn("/abs/outside.txt", _serialize(res))
        # One finding per violation, never more; the rest of the walk
        # is intact (every unmutated artifact still present).
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})
        self.assertTrue(
            all(e["id"] in artifact_ids
                for e in base.entities if e.get("kind") == "artifact"))

    def test_dotdot_segment_catalog_input_is_dropped(self):
        plans, worktree = _copy_corpus("r7-dotdot-input")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        comp["inputs"].append("plans:a/../outside.txt")
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
            and f["source"].get("index") == len(comp["inputs"]) - 1
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        artifact_ids = {e["id"] for e in res.entities
                        if e.get("kind") == "artifact"}
        self.assertNotIn("artifact:plans:a/../outside.txt", artifact_ids)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_bare_absolute_catalog_input_is_dropped(self):
        plans, worktree = _copy_corpus("r7-bare-abs-input")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        # A bare absolute item without a root: prefix.
        comp["inputs"].append("/abs/f")
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
            and f["source"].get("index") == len(comp["inputs"]) - 1
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        artifact_ids = {e["id"] for e in res.entities
                        if e.get("kind") == "artifact"}
        self.assertNotIn("artifact:plans:/abs/f", artifact_ids)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_absolute_curated_source_is_dropped(self):
        plans, worktree = _copy_corpus("r7-abs-curated")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        bad_index = len(curated)
        curated.append(
            {"root": "plans", "path": "/abs/curated.md", "selectors": []})
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "curated_sources"
            and f["source"].get("index") == bad_index
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"]["root"], "worktree")
        self.assertEqual(bad[0]["source"]["path"],
                         "smoke/eval_harness/catalog.json")
        # No section entity is minted and the raw path is never
        # echoed; one finding per violation, never more.
        entity_ids = {e["id"] for e in res.entities}
        self.assertFalse(any(eid.startswith("section:plans:/abs/curated.md")
                             for eid in entity_ids))
        self.assertNotIn("/abs/curated.md", _serialize(res))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})


class Round8FixesTest(unittest.TestCase):
    """Batch A round 8 (bead apex-ayl.137): coordinator-adjudicated fixes.

    - M-2: the H-row gate gains the C-row's cell-padding parity — a
      GFM-legal ``|  H-7 | …`` row (2-3 spaces after the opening pipe)
      is still a row: ``rule:H-7`` + pins + ``enforced-by`` edges
      survive with zero findings; 4+ leading spaces stay prose
      (round-7 pin);
    - MAJOR-1: every silent ``continue`` in the ``curated_sources``
      walk — non-dict entry, non-string root, root the context does not
      supply, non-string path, present-but-non-list selectors,
      non-dict selector — is a hard ``formalism-malformed-source`` at
      the catalog-file locator (entry index; selector index for the
      selector violation) with the entry dropped;
    - MAJOR-2: a present-but-empty EV cell on the H-7 row is a hard
      finding at the non-echoing row locator; ``code_pins`` is OMITTED
      (no pins minted, no artifact entity registered from the row)
      while ``rule:H-7`` is otherwise retained;
    - MINOR-1: no artifact id is minted from a shape-violating spec —
      a rooted spec naming a root the context does not supply (no bare-
      branch fall-through), an empty/whitespace-only path part, or a
      leading ``.`` segment / empty segment (``a//b``).

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero hard,
    zero row-gate fires, zero catalog-file-locator findings.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _row_findings(res, path: str) -> list:
        return [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("path") == path and "row" in f["source"]
        ]

    @staticmethod
    def _catalog_findings(res, key: str) -> list:
        return [
            f for f in res.findings
            if f["source"].get("path") == "smoke/eval_harness/catalog.json"
            and f["source"].get("key") == key
        ]

    # -- live unmutated shape pin (read-only) ----------------------------------

    def test_live_unmutated_shape_pin(self):
        # Read-only live run (DESIGN §5 invocation): the round-8 gates
        # fire nowhere on the unmutated corpus — 172 entities / 84
        # relationships / 10 findings (all soft), zero hard, zero
        # row-gate fires, zero catalog-file-locator findings, H-7 pins
        # intact with both pinned artifacts present.
        plans, worktree = _plans_root(), _worktree_root()
        res = _discover(plans, worktree)
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)
        self.assertEqual(len(res.findings), 10)
        self.assertEqual(_hard_findings(res), [])
        self.assertEqual(self._row_findings(res, FORMALISM_MD), [])
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])
        self.assertEqual(self._catalog_findings(res, "curated_sources"), [])
        self.assertEqual(
            self._catalog_findings(res, "formalism-linter.inputs"), [])
        h7 = _by_id(res, "rule:H-7")
        self.assertIsNotNone(h7)
        self.assertEqual(len(h7["code_pins"]), 2)
        self.assertIsNotNone(_by_id(res, ARTIFACT_SDD_71))
        self.assertIsNotNone(_by_id(res, ARTIFACT_XWAVEC71))

    # -- M-2: H-row cell-padding parity with the C-row gate -------------------

    def _h7_padded_pipe_probe(self, tag: str, spaces: int) -> None:
        plans, worktree = _copy_corpus(tag)
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        self.assertTrue(h7.startswith("| H-7"))
        # 2 (or 3) spaces after the opening pipe: a well-formed GFM
        # table row. Pre-fix the H-row gate admits 0-1 spaces only, so
        # the row is prose and rule:H-7 + pins + edges vanish with zero
        # findings.
        padded = "|" + " " * spaces + h7[2:]
        self.assertNotIn(padded + "\n", text)
        hp.write_text(text.replace(h7 + "\n", padded + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        # No false fire on the well-formed row, no findings delta.
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(f) for f in base.findings})
        # rule:H-7, its pins, and its enforced-by edges survive.
        h7e = _by_id(res, "rule:H-7")
        self.assertIsNotNone(h7e)
        self.assertEqual(len(h7e["code_pins"]), 2)
        self.assertEqual(
            len([r for r in _rels(res, "enforced-by")
                 if r["source"] == "rule:H-7"]),
            3)
        self.assertIsNotNone(_by_id(res, ARTIFACT_SDD_71))
        self.assertIsNotNone(_by_id(res, ARTIFACT_XWAVEC71))
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)

    def test_two_space_after_pipe_h_row_still_parses(self):
        self._h7_padded_pipe_probe("r8-h7-2space", 2)

    def test_three_space_after_pipe_h_row_still_parses(self):
        self._h7_padded_pipe_probe("r8-h7-3space", 3)

    # -- MAJOR-1: curated_sources shape gate ----------------------------------

    def test_non_dict_curated_source_is_hard_and_dropped(self):
        plans, worktree = _copy_corpus("r8-curated-list")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        # The entry becomes a two-item list (root, path). Pre-fix it
        # silently vanishes: the FORMALISM.md section, all seven
        # C-categories, and their defines edges disappear with zero
        # findings.
        catalog["curated_sources"][0] = [
            "plans", "parity-formalism/FORMALISM.md"]
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources", "index": 0})
        # The finding is the ONLY new finding.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})
        # No section: entity from the dropped source is minted.
        self.assertFalse(
            any(e["id"].startswith(
                "section:plans:parity-formalism/FORMALISM.md")
                for e in res.entities))
        # The seven C-categories + defines edges are gone ONLY because
        # the source was dropped: the entity and relationship deltas
        # are exactly the section plus the seven categories.
        base_ids = {e["id"] for e in base.entities}
        mut_ids = {e["id"] for e in res.entities}
        sec = next(e for e in base.entities
                   if e["kind"] == "document-section"
                   and e.get("path") == FORMALISM_MD
                   and e.get("selector", {}).get("text") == HEADING_CATEGORY)
        expected_gone = (
            {sec["id"]} | {f"category:{c}" for c in PIN_CATEGORY_IDS})
        self.assertEqual(base_ids - mut_ids, expected_gone)
        self.assertEqual(mut_ids - base_ids, set())
        base_rels = {r["id"] for r in base.relationships}
        mut_rels = {r["id"] for r in res.relationships}
        gone = [r for r in base.relationships
                if r["id"] not in mut_rels]
        self.assertEqual(
            sorted((r["kind"], r["target"]) for r in gone),
            sorted(("defines", f"category:{c}")
                   for c in PIN_CATEGORY_IDS))
        self.assertEqual(mut_rels - base_rels, set())
        self.assertEqual(_hard_findings(res), bad)

    def test_non_dict_selector_is_hard_and_entry_dropped(self):
        plans, worktree = _copy_corpus("r8-curated-sel-int")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        catalog["curated_sources"][0]["selectors"] = [42]
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        # The entry locator plus the selector index.
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources", "index": 0,
                          "selector": 0})
        self.assertEqual(_hard_findings(res), bad)
        # The entry is dropped: no section: entity from the source, no
        # categories.
        self.assertFalse(
            any(e["id"].startswith(
                "section:plans:parity-formalism/FORMALISM.md")
                for e in res.entities))
        self.assertIsNone(_by_id(res, "category:C1"))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_non_string_curated_source_path_is_hard(self):
        plans, worktree = _copy_corpus("r8-curated-path-int")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        catalog["curated_sources"][0]["path"] = 42
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources", "index": 0})
        self.assertEqual(_hard_findings(res), bad)
        self.assertFalse(
            any(e["id"].startswith(
                "section:plans:parity-formalism/FORMALISM.md")
                for e in res.entities))
        self.assertIsNone(_by_id(res, "category:C3"))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_unknown_root_curated_source_is_hard(self):
        plans, worktree = _copy_corpus("r8-curated-unkroot")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        # The catalog names a root the context does not supply
        # (schema-violating): pre-fix a silent continue.
        catalog["curated_sources"][0] = {"root": "no-such-root",
                                         "path": "x.md"}
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources", "index": 0})
        self.assertEqual(_hard_findings(res), bad)
        # The entry is dropped before any document access: no section
        # entity from the unknown root, no missing-doc finding.
        self.assertFalse(
            any("no-such-root" in e["id"] for e in res.entities))
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    def test_non_list_selectors_is_hard_and_entry_dropped(self):
        plans, worktree = _copy_corpus("r8-curated-sel-str")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        # Present-but-non-list: a string char-iterates pre-fix (every
        # character a silent non-dict-selector skip).
        catalog["curated_sources"][0]["selectors"] = "h"
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        # A container violation: the entry locator, no selector index.
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources", "index": 0})
        self.assertEqual(_hard_findings(res), bad)
        self.assertFalse(
            any(e["id"].startswith(
                "section:plans:parity-formalism/FORMALISM.md")
                for e in res.entities))
        self.assertIsNone(_by_id(res, "category:C5"))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})

    # -- MAJOR-2: present-but-empty H-7 EV cell --------------------------------

    def test_h7_empty_ev_cell_is_hard_pins_omitted(self):
        plans, worktree = _copy_corpus("r8-h7-empty-ev")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        hp = plans / HARDENING_MD
        text = hp.read_text("utf-8")
        rng = formalism.extract_heading_range(text, HEADING_2_1)
        rng_lines = rng.splitlines()
        h7 = next(l for l in rng_lines if l.startswith("| H-7"))
        row_idx = rng_lines.index(h7)
        cells = [c.strip() for c in h7.strip().strip("|").split("|")]
        self.assertEqual(len(cells), 4)
        # The EV cell (4th) is present-but-empty while every other
        # cell stays structurally valid, so no other gate fires.
        # Pre-fix the row parses with zero findings, code_pins becomes
        # [], and the pinned sdd-71 artifact entity silently vanishes
        # (172 -> 171).
        emptied = "| " + " | ".join(cells[:3]) + " |  |"
        self.assertNotIn(emptied + "\n", text)
        hp.write_text(text.replace(h7 + "\n", emptied + "\n", 1), "utf-8")
        res = self._run(plans, worktree)
        bad = self._row_findings(res, HARDENING_MD)
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "plans", "path": HARDENING_MD,
                          "heading": HEADING_2_1, "row": row_idx})
        self.assertEqual(_hard_findings(res), bad)
        # Zero other findings.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})
        # rule:H-7 is retained, code_pins OMITTED (no pins minted).
        h7e = _by_id(res, "rule:H-7")
        self.assertIsNotNone(h7e)
        self.assertNotIn("code_pins", h7e)
        self.assertEqual(h7e["rule_id"], "H-7")
        self.assertEqual(h7e["enforcement_layers"],
                         [{"layer": "A1", "state": PIN_H7_LAYERS["A1"]},
                          {"layer": "A2", "state": PIN_H7_LAYERS["A2"]},
                          {"layer": "A3", "state": PIN_H7_LAYERS["A3"]}])
        # No artifact entity is registered from the row: the sdd-71 pin
        # artifact (not catalog-declared) is absent; the catalog-
        # declared xwavec71 artifact survives.
        self.assertIsNone(_by_id(res, ARTIFACT_SDD_71))
        self.assertIsNotNone(_by_id(res, ARTIFACT_XWAVEC71))
        self.assertEqual(len(res.entities), 171)

    def test_h7_unmutated_code_pins_and_artifact_intact(self):
        plans, worktree = _copy_corpus("r8-h7-baseline")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        res = self._run(plans, worktree)
        self.assertEqual(self._row_findings(res, HARDENING_MD), [])
        h7e = _by_id(res, "rule:H-7")
        self.assertIsNotNone(h7e)
        self.assertIn("code_pins", h7e)
        self.assertEqual(len(h7e["code_pins"]), 2)
        self.assertIsNotNone(_by_id(res, ARTIFACT_SDD_71))
        self.assertIsNotNone(_by_id(res, ARTIFACT_XWAVEC71))
        # The entity/relationship shape is copy-invariant (the live 10-
        # finding count is NOT: on a /tmp copy the absolute source_dir
        # tokens rooted at the live worktree stop converting, adding
        # soft source-dir-unrooted findings — the live-only shape pin
        # test_live_unmutated_shape_pin owns that count).
        self.assertEqual(len(res.entities), 172)
        self.assertEqual(len(res.relationships), 84)
        self.assertEqual(_hard_findings(res), [])

    # -- MINOR-1: no id minted from a shape-violating spec --------------------

    def _input_shape_probe(self, tag: str, spec: str, bad_id: str,
                           echo_safe: bool = True) -> None:
        plans, worktree = _copy_corpus(tag)
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        comp = self._linter_comp(catalog)
        comp["inputs"].append(spec)
        res = self._run(plans, worktree, catalog=catalog)
        bad = [
            f for f in _findings(res, "formalism-malformed-source")
            if f["source"].get("key") == "formalism-linter.inputs"
            and f["source"].get("index") == len(comp["inputs"]) - 1
        ]
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "formalism-linter.inputs",
                          "index": len(comp["inputs"]) - 1})
        self.assertEqual(_hard_findings(res), bad)
        # No artifact id is minted from the rejected spec, and (where
        # the raw string cannot occur elsewhere in the live output) the
        # raw value never reaches the serialized output.
        artifact_ids = {e["id"] for e in res.entities
                        if e.get("kind") == "artifact"}
        self.assertNotIn(bad_id, artifact_ids)
        if echo_safe:
            self.assertNotIn(spec, _serialize(res))
        # One finding per violation, never more; the rest of the walk
        # is intact (every unmutated artifact still present).
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(f) for f in base.findings},
            {self._sig(bad[0])})
        self.assertTrue(
            all(e["id"] in artifact_ids
                for e in base.entities if e.get("kind") == "artifact"))

    def test_unknown_root_catalog_input_is_hard_no_mint(self):
        # A rooted spec naming a root the context does not supply:
        # pre-fix it falls through to the bare branch and mints
        # artifact:plans:agents-home:parity/x.json (the raw value,
        # colon and all, inside the id).
        self._input_shape_probe(
            "r8-input-unknown-root", "agents-home:parity/x.json",
            "artifact:plans:agents-home:parity/x.json")

    def test_empty_path_catalog_input_is_hard_no_mint(self):
        # worktree: — a rooted spec with an empty path part: pre-fix it
        # mints artifact:worktree: (empty path). The raw string is not
        # echo-asserted: "worktree:" is a substring of every healthy
        # artifact:worktree:<rel> id.
        self._input_shape_probe("r8-input-empty", "worktree:",
                                "artifact:worktree:", echo_safe=False)

    def test_dotleading_path_catalog_input_is_hard_no_mint(self):
        # plans:./x — a dot-leading segment in the path part: pre-fix it
        # mints artifact:plans:./x.
        self._input_shape_probe("r8-input-dotlead", "plans:./x",
                                "artifact:plans:./x")

    def test_empty_segment_catalog_input_is_hard_no_mint(self):
        # plans:a//b — an empty segment in the path part: pre-fix it
        # mints artifact:plans:a//b.
        self._input_shape_probe("r8-input-empty-seg", "plans:a//b",
                                "artifact:plans:a//b")


class Round9FixesTest(unittest.TestCase):
    """Batch A round 9: coordinator-adjudicated formalism fixes.

    - M-1: a missing or unknown curated selector kind (anything not
      heading/question/citations) is ONE hard ``formalism-malformed-source``
      at the selector locator and skips that selector only — the entry
      stays alive. ``citations`` (live-pinned on EXEMPLARS.md,
      OBSERVATIONS.md, and intel/02) is a silent continue; heading/
      question with a non-string or empty text/marker is the same single
      hard finding at the selector locator.
    - M-2: the curated walk sensitive-scans the declared ``rel`` (parity
      with the artifact-channel scan): a schema-legal filename carrying
      a raw key / canary is ONE hard ``formalism-sensitive-output`` at the
      catalog-file entry locator, the entry is dropped, and the raw name
      rides along only as a hash.
    - M-3: a present-but-empty/whitespace-only curated ``path`` is the
      existing hard + drop at the entry locator, not a cascade of
      escape/missing findings.
    - M-4: a shape-violating curated ``path`` (dot-leading segment,
      empty segment) is ONE hard ``formalism-malformed-source`` at the
      entry locator, no ids minted; live ``**``-glob and ``a..b`` shapes
      keep flowing.
    - M-5: a walked top-level (no-slash) sensitive file emits its
      finding at the non-empty non-echoing root-directory locator
      ``{"root": <root>, "path": "."}``, never an empty-path locator.
    - M-6: the tolerant locator-key helper backs ``finding()`` and both
      final sort keys, so a locator ``finding()`` accepts can never make
      a sort raise out of ``discover()``.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero hard.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _catalog_findings(res, key: str) -> list:
        return [
            f for f in res.findings
            if f["source"].get("path") == "smoke/eval_harness/catalog.json"
            and f["source"].get("key") == key
        ]

    # -- M-6: tolerant locator-key helper --------------------------------------

    def test_locator_key_tolerant_malformed_shape_falls_back(self):
        # M-6: any locator that finding() tolerates (key falls back to
        # b"{}") must produce the same fallback here — a sort keyed by
        # this helper can never raise what finding() swallowed.
        self.assertEqual(
            formalism._locator_key_tolerant({"root": "r", "path": ""}),
            b"{}")
        self.assertEqual(
            formalism._locator_key_tolerant({"root": "", "path": "x"}),
            b"{}")
        self.assertEqual(formalism._locator_key_tolerant(None), b"{}")

    def test_locator_key_tolerant_valid_locator_roundtrips(self):
        # A well-shaped locator round-trips to exactly the bytes
        # canonical_locator_key produces.
        loc = {"root": "plans", "path": "parity-formalism/FORMALISM.md",
               "key": "curated_sources", "index": 0}
        self.assertEqual(
            formalism._locator_key_tolerant(loc),
            contract.canonical_locator_key(loc))

    # -- M-5 + M-6: top-level sensitive file end-to-end tripwire --------------

    def test_toplevel_sensitive_walked_file_root_dir_locator(self):
        # M-5/M-6 end-to-end tripwire: the artifact walk hits a
        # TOP-LEVEL (no-slash) file whose name matches the sensitive
        # pattern. Pre-fix the finding's locator
        # {"root": "plans", "path": ""} carries an empty path: finding()
        # swallows it (fallback key b"{}"), but the unguarded final
        # findings sort raises ContractError(code="locator-shape") OUT
        # of discover() on schema-valid input. Post-fix: no raise, and
        # exactly one finding at the non-empty non-echoing root-dir
        # locator {"root": "plans", "path": "."}.
        plans, worktree = _copy_corpus("r9-toplevel-sensitive")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        rel = "sk-externalkey12345678901.md"
        (plans / rel).write_text("x\n", "utf-8")
        catalog = contract.load_catalog()
        self._linter_comp(catalog)["inputs"].append(rel)
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = _findings(res, "formalism-sensitive-output")
        self.assertEqual(len(sensitive), 1)
        f = sensitive[0]
        self.assertEqual(f["impact"], "hard")
        self.assertEqual(f["level"], "error")
        self.assertEqual(f["component"], "formalism-linter")
        self.assertEqual(f["source"], {"root": "plans", "path": "."})
        self.assertEqual(
            f["detail"],
            [{"pattern": h["pattern"], "offset": h["offset"]}
             for h in contract.sensitive_matches(rel)])
        self.assertEqual(
            f["rel_sha256"],
            hashlib.sha256(rel.encode("utf-8")).hexdigest())
        # The raw name is never echoed and no artifact is minted.
        self.assertNotIn(rel, _serialize(res))
        self.assertNotIn(
            f"artifact:plans:{rel}", {e["id"] for e in res.entities})
        # The finding is the ONLY new finding.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(sensitive[0])})

    # -- M-1: curated selector-kind gate ----------------------------------------

    def _sel_locator(self, index: int, selector: int) -> dict:
        return {"root": "worktree",
                "path": "smoke/eval_harness/catalog.json",
                "key": "curated_sources", "index": index,
                "selector": selector}

    def test_unknown_selector_kind_is_hard_selector_skipped_entry_alive(
            self):
        # M-1: a selector with an unknown kind is ONE hard finding at
        # the selector locator and skips that selector only — the
        # following VALID heading selector in the same entry still
        # resolves (the entry stays alive). "1. The objects" is a real
        # FORMALISM.md heading not curated by any live entry (a
        # re-curated §2 heading would double-mint the C-categories).
        plans, worktree = _copy_corpus("r9-sel-unknown-kind")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        bad_index = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{"kind": "anchor", "text": "x"},
                                      {"kind": "heading",
                                       "text": "1. The objects"}]})
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         self._sel_locator(bad_index, 0))
        # The entry stays alive: the following valid heading resolved
        # (section id hashes the selector text, per the id namespace).
        sec_id = (
            f"section:plans:{FORMALISM_MD}:"
            f"{hashlib.sha256(b'1. The objects').hexdigest()}")
        self.assertIsNotNone(_by_id(res, sec_id))
        # The finding is the ONLY new finding.
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(bad[0])})

    def test_missing_selector_kind_is_hard_selector_skipped(self):
        # M-1: a selector dict with no kind at all is the same single
        # hard finding at the selector locator.
        plans, worktree = _copy_corpus("r9-sel-missing-kind")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        bad_index = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{},
                                      {"kind": "heading",
                                       "text": "1. The objects"}]})
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         self._sel_locator(bad_index, 0))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(bad[0])})

    def test_empty_or_nonstring_selector_text_is_hard_skipped(self):
        # M-1: heading with text "" and heading with non-string text
        # (42) are each ONE hard finding at their selector locator and
        # the selector is skipped — the document is never read, so no
        # selector-missing cascade.
        plans, worktree = _copy_corpus("r9-sel-empty-text")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i_empty = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{"kind": "heading", "text": ""}]})
        i_int = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{"kind": "heading", "text": 42}]})
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 2)
        for f in bad:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(
            sorted(json.dumps(f["source"], sort_keys=True) for f in bad),
            sorted(json.dumps(d, sort_keys=True) for d in (
                self._sel_locator(i_empty, 0),
                self._sel_locator(i_int, 0))))
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertEqual(_hard_findings(res), bad)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(f) for f in bad})

    def _entry_locator(self, index: int) -> dict:
        return {"root": "worktree",
                "path": "smoke/eval_harness/catalog.json",
                "key": "curated_sources", "index": index}

    # -- M-2: sensitive scan on the curated rel --------------------------------

    def test_curated_path_with_raw_key_is_sensitive_entry_dropped(self):
        # M-2: a schema-legal filename containing a raw key currently
        # raw-emits into section ids with zero findings; post-fix it is
        # exactly ONE hard formalism-sensitive-output at the
        # catalog-file entry locator (parity with the artifact-channel
        # scan), the raw name rides along only as rel_sha256, and no
        # section entity is minted for the dropped entry.
        plans, worktree = _copy_corpus("r9-curated-sensitive")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        rel = "plans/sk-externalkey12345678901.md"
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        bad_index = len(curated)
        curated.append({"root": "plans", "path": rel,
                        "selectors": [{"kind": "heading", "text": "x"}]})
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("key") == "curated_sources"
        ]
        self.assertEqual(len(sensitive), 1)
        f = sensitive[0]
        self.assertEqual(f["impact"], "hard")
        self.assertEqual(f["level"], "error")
        self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(f["source"], self._entry_locator(bad_index))
        self.assertEqual(
            f["detail"],
            [{"pattern": h["pattern"], "offset": h["offset"]}
             for h in contract.sensitive_matches(rel)])
        self.assertEqual(
            f["rel_sha256"],
            hashlib.sha256(rel.encode("utf-8")).hexdigest())
        # The entry is dropped: no section entity, raw name never
        # echoed, and the finding is the ONLY new finding.
        self.assertFalse(
            any(e["id"].startswith(f"section:plans:{rel}")
                for e in res.entities))
        self.assertNotIn("sk-externalkey12345678901", _serialize(res))
        self.assertEqual(
            {self._sig(g) for g in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(f)})

    # -- M-3: present-but-empty curated path -----------------------------------

    def test_empty_curated_path_is_hard_at_entry_locator(self):
        # M-3: a present-but-empty (or whitespace-only) curated path is
        # the existing hard + drop at the entry locator — pre-fix ""
        # passed the isinstance gate, passed the escape gate, and
        # cascaded into per-selector missing findings (with empty-path
        # locators) instead of one hard finding at the entry.
        plans, worktree = _copy_corpus("r9-curated-empty-path")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i_empty = len(curated)
        curated.append({"root": "plans", "path": "",
                        "selectors": [{"kind": "heading",
                                       "text": HEADING_CATEGORY}]})
        i_space = len(curated)
        curated.append({"root": "plans", "path": "   ",
                        "selectors": [{"kind": "heading",
                                       "text": HEADING_CATEGORY}]})
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        # Exactly the two expected entries, no cascade.
        self.assertEqual(len(bad), 2)
        for f in bad:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(
            sorted(json.dumps(f["source"], sort_keys=True) for f in bad),
            sorted(json.dumps(d, sort_keys=True) for d in (
                self._entry_locator(i_empty),
                self._entry_locator(i_space))))
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertEqual(_hard_findings(res), bad)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(f) for f in bad})

    # -- M-4: shape gate in the curated walk ------------------------------------

    def test_shape_violating_curated_path_is_hard_no_mint(self):
        # M-4: dot-leading (./x.md) and empty-segment (a//b.md) curated
        # paths are each ONE hard malformed-source at the entry locator
        # and no ids minted — the same helper the two input-walk
        # branches use; live ** -glob and a..b shapes keep flowing.
        plans, worktree = _copy_corpus("r9-curated-shape")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i_dot = len(curated)
        curated.append({"root": "plans", "path": "./x.md",
                        "selectors": [{"kind": "heading", "text": "x"}]})
        i_seg = len(curated)
        curated.append({"root": "plans", "path": "a//b.md",
                        "selectors": [{"kind": "heading", "text": "x"}]})
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 2)
        for f in bad:
            self.assertEqual(f["impact"], "hard")
            self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(
            sorted(json.dumps(f["source"], sort_keys=True) for f in bad),
            sorted(json.dumps(d, sort_keys=True) for d in (
                self._entry_locator(i_dot),
                self._entry_locator(i_seg))))
        # No cascade, no minted ids for the dropped entries.
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertFalse(
            any(e["id"].startswith("section:plans:./x.md")
                or e["id"].startswith("section:plans:a//b.md")
                for e in res.entities))
        self.assertEqual(_hard_findings(res), bad)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(f) for f in bad})

    def test_citations_selector_kind_is_silent_continue(self):
        # M-1 regression guard for the live constraint: the live
        # catalog pins kind "citations" on EXEMPLARS.md, OBSERVATIONS.md,
        # and intel/02 — their sections resolve through the
        # citation-driven path, never the heading/question walk. An
        # extra citations selector must produce ZERO findings and zero
        # entity delta (never an unknown-kind hard fire).
        plans, worktree = _copy_corpus("r9-sel-citations")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        catalog["curated_sources"].append(
            {"root": "plans", "path": EXEMPLARS_MD,
             "selectors": [{"kind": "citations"}]})
        res = self._run(plans, worktree, catalog=catalog)
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(g) for g in base.findings})
        self.assertEqual(
            {e["id"] for e in res.entities},
            {e["id"] for e in base.entities})


class Round10FixesTest(unittest.TestCase):
    """Batch A round 10: coordinator-adjudicated formalism fixes.

    - F-1 (MAJOR-1): the curated selector ``text_`` is sensitive-scanned
      before any locator is built or document read: a raw key / canary
      in a heading text or question marker is ONE hard
      ``formalism-sensitive-output`` at the non-echoing catalog-file
      selector locator, the raw value rides along only as
      ``value_sha256``, that selector is skipped, and the document is
      never read (no selector-missing / -duplicate cascade, no section
      entity, no echo on ANY path).
    - F-2 (MAJOR-2): the ``components`` walk gains explicit container
      discipline: None is a clean empty walk (today's behavior); a
      non-list container is ONE hard ``formalism-malformed-source`` at
      ``{key: components}`` and the walk returns — an int escapes as a
      raw TypeError and a string char-iterates into a silent input-walk
      skip; a non-dict ROW is ONE hard at ``{key: components,
      index: j}`` and only that row is skipped — a valid linter row
      after a bad row must still be found.
    - F-3 (MAJOR-3): the ``curated_sources`` container is guarded at the
      top of the curated walk: None is clean; a non-list container is
      ONE hard at the CONTAINER locator (no "index" key) and the walk
      returns — an int escapes as a raw TypeError and a string
      char-iterates into per-char garbage findings.
    - F-4 (MINOR-2): a whitespace-only selector text is gated at the
      isinstance/empty gate (hard + skip, document never read),
      symmetric with the curated ``rel.strip()`` discipline — not
      allowed through to a loud selector-missing at the echo-bearing
      locator after reading the document.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero hard.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _catalog_findings(res, key: str) -> list:
        return [
            f for f in res.findings
            if f["source"].get("path") == "smoke/eval_harness/catalog.json"
            and f["source"].get("key") == key
        ]

    def _sel_locator(self, index: int, selector: int) -> dict:
        return {"root": "worktree",
                "path": "smoke/eval_harness/catalog.json",
                "key": "curated_sources", "index": index,
                "selector": selector}

    # -- F-1: sensitive scan of the selector text before doc read -------------

    def test_raw_key_heading_text_is_sensitive_before_doc_read(self):
        # F-1a: a heading text carrying a conforming raw key (lowercase
        # after sk-) on a rel that EXISTS on disk but lacks the heading:
        # exactly ONE hard formalism-sensitive-output at the catalog-file
        # selector locator, the document never read — no
        # formalism-selector-missing with the raw key echoed, no section
        # entity, raw key nowhere in the serialization.
        plans, worktree = _copy_corpus("r10-sel-raw-key")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        raw_key = "sk-externalkey12345678901"
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{"kind": "heading",
                                       "text": raw_key}]})
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("key") == "curated_sources"
        ]
        self.assertEqual(len(sensitive), 1)
        f = sensitive[0]
        self.assertEqual(f["impact"], "hard")
        self.assertEqual(f["level"], "error")
        self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(f["source"], self._sel_locator(i, 0))
        self.assertEqual(
            f["detail"],
            [{"pattern": h["pattern"], "offset": h["offset"]}
             for h in contract.sensitive_matches(raw_key)])
        self.assertEqual(f["detail"][0]["pattern"], "raw-key")
        self.assertEqual(
            f["value_sha256"],
            hashlib.sha256(raw_key.encode("utf-8")).hexdigest())
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertFalse(
            any(e["id"] ==
                f"section:plans:{FORMALISM_MD}:"
                f"{hashlib.sha256(raw_key.encode('utf-8')).hexdigest()}"
                for e in res.entities))
        self.assertNotIn(raw_key, _serialize(res))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(sensitive[0])})

    def test_canary_question_marker_is_sensitive_before_doc_read(self):
        # F-1b: a question marker embedding the pinned canary, absent in
        # the doc: the same single hard shape as F-1a, pattern
        # "canary", no echo — the document is never read.
        plans, worktree = _copy_corpus("r10-sel-canary")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        marker = f'Q: "{contract.CANARY_TOKEN} probe?"'
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i = len(curated)
        curated.append({"root": "plans", "path": QA_MD,
                        "selectors": [{"kind": "question",
                                       "marker": marker}]})
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("key") == "curated_sources"
        ]
        self.assertEqual(len(sensitive), 1)
        f = sensitive[0]
        self.assertEqual(f["impact"], "hard")
        self.assertEqual(f["level"], "error")
        self.assertEqual(f["component"], "parity-formalism")
        self.assertEqual(f["source"], self._sel_locator(i, 0))
        self.assertEqual(
            f["detail"],
            [{"pattern": h["pattern"], "offset": h["offset"]}
             for h in contract.sensitive_matches(marker)])
        self.assertEqual(f["detail"][0]["pattern"], "canary")
        self.assertEqual(
            f["value_sha256"],
            hashlib.sha256(marker.encode("utf-8")).hexdigest())
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertNotIn(contract.CANARY_TOKEN, _serialize(res))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(sensitive[0])})

    def test_raw_key_heading_duplicate_in_doc_is_sensitive_only(self):
        # F-1c: a raw-key heading text present TWICE in a planted doc
        # (the duplicate path): the single hard sensitive at the
        # catalog-file selector locator — the document is never read, so
        # no formalism-selector-duplicate with the raw key in source.
        plans, worktree = _copy_corpus("r10-sel-dup")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        raw_key = "sk-externalkey12345678901"
        rel = "parity-formalism/r10-dup.md"
        (plans / rel).write_text(
            f"# {raw_key}\n\nbody one\n\n# {raw_key}\n\nbody two\n", "utf-8")
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i = len(curated)
        curated.append({"root": "plans", "path": rel,
                        "selectors": [{"kind": "heading",
                                       "text": raw_key}]})
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("key") == "curated_sources"
        ]
        self.assertEqual(len(sensitive), 1)
        self.assertEqual(sensitive[0]["impact"], "hard")
        self.assertEqual(sensitive[0]["source"], self._sel_locator(i, 0))
        self.assertEqual(sensitive[0]["detail"][0]["pattern"], "raw-key")
        self.assertEqual(_findings(res, "formalism-selector-duplicate"),
                         [])
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertFalse(
            any(e["id"].startswith(f"section:plans:{rel}")
                for e in res.entities))
        self.assertNotIn(raw_key, _serialize(res))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(sensitive[0])})

    def test_raw_key_heading_present_once_is_sensitive_not_minted(self):
        # F-1d: a raw-key heading text present EXACTLY ONCE in a planted
        # doc (the success path): the single hard sensitive at the
        # catalog-file selector locator, no section entity minted, no
        # echo in the serialization.
        plans, worktree = _copy_corpus("r10-sel-once")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        raw_key = "sk-externalkey12345678901"
        rel = "parity-formalism/r10-once.md"
        (plans / rel).write_text(f"# {raw_key}\n\nbody\n", "utf-8")
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i = len(curated)
        curated.append({"root": "plans", "path": rel,
                        "selectors": [{"kind": "heading",
                                       "text": raw_key}]})
        res = self._run(plans, worktree, catalog=catalog)
        sensitive = [
            f for f in _findings(res, "formalism-sensitive-output")
            if f["source"].get("key") == "curated_sources"
        ]
        self.assertEqual(len(sensitive), 1)
        self.assertEqual(sensitive[0]["impact"], "hard")
        self.assertEqual(sensitive[0]["source"], self._sel_locator(i, 0))
        self.assertEqual(sensitive[0]["detail"][0]["pattern"], "raw-key")
        self.assertFalse(
            any(e["id"].startswith(f"section:plans:{rel}")
                for e in res.entities))
        self.assertNotIn(raw_key, _serialize(res))
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(sensitive[0])})

    def test_clean_heading_text_still_mints_section(self):
        # F-1e control: a normal clean heading text behaves exactly as
        # before — the section entity is minted and no sensitive
        # finding fires (guard against over-firing). "1. The objects" is
        # a real FORMALISM.md heading not curated by any live entry.
        plans, worktree = _copy_corpus("r10-sel-clean")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["curated_sources"].append(
            {"root": "plans", "path": FORMALISM_MD,
             "selectors": [{"kind": "heading",
                            "text": "1. The objects"}]})
        res = self._run(plans, worktree, catalog=catalog)
        self.assertEqual(
            [f for f in _findings(res, "formalism-sensitive-output")
             if f["source"].get("key") == "curated_sources"], [])
        sec_id = (
            f"section:plans:{FORMALISM_MD}:"
            f"{hashlib.sha256(b'1. The objects').hexdigest()}")
        self.assertIsNotNone(_by_id(res, sec_id))

    # -- F-2: components container discipline ---------------------------------

    def test_components_int_container_is_one_hard_no_raise(self):
        # F-2a: an int components container — pre-fix it escapes as a
        # raw TypeError out of discover(); post-fix exactly ONE hard
        # formalism-malformed-source at {key: components} and the walk
        # is skipped (no soft formalism-missing-fact).
        plans, worktree = _copy_corpus("r10-comp-int")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["components"] = 42
        res = self._run(plans, worktree, catalog=catalog)  # must not raise
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components"})
        # The walk is skipped: no soft missing-fact at the
        # parse_artifacts linter-absent locator (the source_files
        # channel's soft findings are a pre-existing /tmp-copy
        # artifact and carry a "key" locator field).
        self.assertEqual(
            [f for f in _findings(res, "formalism-missing-fact")
             if f["source"] == {"root": "plans", "path": RULES_JSON}],
            [])

    def test_components_string_container_is_one_hard_not_per_char(self):
        # F-2b: a string components container — pre-fix it char-iterates
        # (silent input-walk skip); post-fix exactly ONE hard at
        # {key: components}, NOT three per-char findings.
        plans, worktree = _copy_corpus("r10-comp-str")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["components"] = "abc"
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components"})
        # The walk is skipped: no soft missing-fact at the
        # parse_artifacts linter-absent locator.
        self.assertEqual(
            [f for f in _findings(res, "formalism-missing-fact")
             if f["source"] == {"root": "plans", "path": RULES_JSON}],
            [])

    def test_components_non_dict_row_hard_linter_row_still_found(self):
        # F-2c: a non-dict row BEFORE the real formalism-linter row —
        # exactly ONE hard at {key: components, index: 0} and the
        # linter's declared artifacts are still parsed (control: the
        # same run with only the linter row produces the identical
        # artifact set).
        plans, worktree = _copy_corpus("r10-comp-row")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        linter_row = self._linter_comp(contract.load_catalog())
        control_catalog = contract.load_catalog()
        control_catalog["components"] = [linter_row]
        control = self._run(plans, worktree, catalog=control_catalog)
        catalog = contract.load_catalog()
        catalog["components"] = [42, linter_row]
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components", "index": 0})
        artifacts_bad = {e["id"] for e in res.entities
                         if e["id"].startswith("artifact:")}
        artifacts_control = {e["id"] for e in control.entities
                             if e["id"].startswith("artifact:")}
        self.assertEqual(artifacts_bad, artifacts_control)
        self.assertTrue(artifacts_control)

    def test_components_none_is_clean_unchanged(self):
        # F-2d: a None components container keeps today's behavior — a
        # clean empty walk with NO container-level finding: the finding
        # and entity sets are byte-identical to a run with the
        # components key absent entirely.
        plans, worktree = _copy_corpus("r10-comp-none")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["components"] = None
        res = self._run(plans, worktree, catalog=catalog)
        self.assertEqual(self._catalog_findings(res, "components"), [])
        absent_catalog = contract.load_catalog()
        absent_catalog.pop("components")
        absent = self._run(plans, worktree, catalog=absent_catalog)
        self.assertEqual(sorted(self._sig(f) for f in res.findings),
                         sorted(self._sig(f) for f in absent.findings))
        self.assertEqual(
            {e["id"] for e in res.entities},
            {e["id"] for e in absent.entities})

    # -- F-3: curated_sources container discipline -----------------------------

    def test_curated_int_container_is_one_hard_no_index(self):
        # F-3a: an int curated_sources container — pre-fix it escapes
        # as a raw TypeError out of discover(); post-fix exactly ONE
        # hard formalism-malformed-source at the CONTAINER locator
        # (no "index" key) and the walk returns.
        plans, worktree = _copy_corpus("r10-curated-int")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["curated_sources"] = 42
        res = self._run(plans, worktree, catalog=catalog)  # must not raise
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources"})
        self.assertNotIn("index", bad[0]["source"])

    def test_curated_string_container_is_one_hard_not_per_char(self):
        # F-3b: a string curated_sources container — pre-fix it
        # char-iterates into per-char garbage findings; post-fix
        # exactly ONE hard at the container locator, NOT three.
        plans, worktree = _copy_corpus("r10-curated-str")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["curated_sources"] = "abc"
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "curated_sources"})
        self.assertNotIn("index", bad[0]["source"])

    def test_curated_none_is_clean_unchanged(self):
        # F-3c: a None curated_sources container keeps today's behavior
        # — a clean empty walk with NO container-level finding: the
        # finding and entity sets are byte-identical to a run with the
        # curated_sources key absent entirely.
        plans, worktree = _copy_corpus("r10-curated-none")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        catalog = contract.load_catalog()
        catalog["curated_sources"] = None
        res = self._run(plans, worktree, catalog=catalog)
        self.assertEqual(self._catalog_findings(res, "curated_sources"), [])
        absent_catalog = contract.load_catalog()
        absent_catalog.pop("curated_sources")
        absent = self._run(plans, worktree, catalog=absent_catalog)
        self.assertEqual(sorted(self._sig(f) for f in res.findings),
                         sorted(self._sig(f) for f in absent.findings))
        self.assertEqual(
            {e["id"] for e in res.entities},
            {e["id"] for e in absent.entities})

    # -- F-4: whitespace-only selector text ------------------------------------

    def test_whitespace_only_selector_text_is_hard_skipped(self):
        # F-4: a whitespace-only heading text is gated at the
        # isinstance/empty gate — exactly ONE hard
        # formalism-malformed-source at the selector locator, selector
        # skipped, document never read (no loud
        # formalism-selector-missing at the echo-bearing locator after
        # reading the document); symmetric with the curated
        # rel.strip() discipline (round 9, M-3).
        plans, worktree = _copy_corpus("r10-sel-ws")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        curated = catalog["curated_sources"]
        i = len(curated)
        curated.append({"root": "plans", "path": FORMALISM_MD,
                        "selectors": [{"kind": "heading",
                                       "text": "   "}]}
                       )
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "curated_sources")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "parity-formalism")
        self.assertEqual(bad[0]["source"], self._sel_locator(i, 0))
        self.assertEqual(_findings(res, "formalism-selector-missing"), [])
        self.assertEqual(_hard_findings(res), bad)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(bad[0])})


class Round11FixesTest(unittest.TestCase):
    """Batch A round 11: coordinator-adjudicated formalism fix.

    - K-1 (GLM M-1): the round-10 F-2 ``components`` walk broke out of
      the loop on the linter-row match, so rows AFTER the linter row
      were never type-checked — a malformed trailing row vanished
      silently in standalone ``formalism.discover()`` (0 findings, all
      21 artifacts intact) while the same bytes raise
      ``malformed-catalog-input`` in the inventory adapter — asymmetric.
      The container is now validated in full BEFORE the linter search:
      ONE hard ``formalism-malformed-source`` per non-dict row at
      ``{key: components, index: j}`` (component ``formalism-linter``),
      then the linter row is located in a second pass that inspects
      dict rows only. The round-10 F-2c bad-row-BEFORE-linter behavior
      is unchanged.

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero hard.
    """

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _catalog_findings(res, key: str) -> list:
        return [
            f for f in res.findings
            if f["source"].get("path") == "smoke/eval_harness/catalog.json"
            and f["source"].get("key") == key
        ]

    # -- K-1: whole-container validation before the linter search -----------

    def test_trailing_malformed_row_after_linter_is_one_hard(self):
        # K-1a (GLM's exact probe): live catalog + a trailing None row
        # AFTER the linter row — pre-fix the linter-row break left the
        # trailing row uninspected: 0 findings, all 21 artifacts intact,
        # the malformed row vanished silently. Post-fix exactly ONE hard
        # at the trailing row index; the artifact set is byte-identical
        # to the unmutated run (21 artifacts); the finding diff vs
        # baseline is exactly that one finding.
        plans, worktree = _copy_corpus("r11-comp-trailing")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = contract.load_catalog()
        i = len(catalog["components"])  # trailing index after the append
        catalog["components"].append(None)
        res = self._run(plans, worktree, catalog=catalog)  # must not raise
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components", "index": i})
        # All 21 declared linter artifacts still minted, byte-identical
        # to the unmutated run.
        artifacts_base = {e["id"] for e in base.entities
                          if e["id"].startswith("artifact:")}
        self.assertEqual(len(artifacts_base), 21)
        self.assertEqual(
            {e["id"] for e in res.entities
             if e["id"].startswith("artifact:")},
            artifacts_base)
        self.assertEqual(
            {e["id"] for e in res.entities},
            {e["id"] for e in base.entities})
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(bad[0])})

    def test_int_row_after_linter_is_one_hard_artifacts_intact(self):
        # K-1b: a 42 row directly AFTER the linter row — exactly ONE
        # hard at {key: components, index: 1} and the linter artifact
        # set identical to the linter-only control.
        plans, worktree = _copy_corpus("r11-comp-int-after")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        linter_row = self._linter_comp(contract.load_catalog())
        control_catalog = contract.load_catalog()
        control_catalog["components"] = [linter_row]
        control = self._run(plans, worktree, catalog=control_catalog)
        catalog = contract.load_catalog()
        catalog["components"] = [linter_row, 42]
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components", "index": 1})
        artifacts_bad = {e["id"] for e in res.entities
                         if e["id"].startswith("artifact:")}
        artifacts_control = {e["id"] for e in control.entities
                             if e["id"].startswith("artifact:")}
        self.assertEqual(artifacts_bad, artifacts_control)
        self.assertTrue(artifacts_control)

    def test_int_row_before_linter_unchanged_regression(self):
        # K-1c (regression): a 42 row BEFORE the linter row — the
        # round-10 F-2c behavior must be unchanged: exactly ONE hard at
        # {key: components, index: 0} and the linter artifacts still
        # minted (identical to the linter-only control).
        plans, worktree = _copy_corpus("r11-comp-int-before")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        linter_row = self._linter_comp(contract.load_catalog())
        control_catalog = contract.load_catalog()
        control_catalog["components"] = [linter_row]
        control = self._run(plans, worktree, catalog=control_catalog)
        catalog = contract.load_catalog()
        catalog["components"] = [42, linter_row]
        res = self._run(plans, worktree, catalog=catalog)
        bad = self._catalog_findings(res, "components")
        self.assertEqual(len(bad), 1)
        self.assertEqual(bad[0]["impact"], "hard")
        self.assertEqual(bad[0]["level"], "error")
        self.assertEqual(bad[0]["component"], "formalism-linter")
        self.assertEqual(bad[0]["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components", "index": 0})
        artifacts_bad = {e["id"] for e in res.entities
                         if e["id"].startswith("artifact:")}
        artifacts_control = {e["id"] for e in control.entities
                             if e["id"].startswith("artifact:")}
        self.assertEqual(artifacts_bad, artifacts_control)
        self.assertTrue(artifacts_control)


class Round12FixesTest(unittest.TestCase):
    """Batch A round 12: coordinator-adjudicated formalism fix F-1.

    The linter row's own ``root`` field flowed unvalidated into the bare
    (no-colon) input channel: a schema-legal root the context does not
    supply (``agents-home``, against {worktree, plans}) made the ``/**``
    glob branch raise a raw ``KeyError`` from
    ``Path(self.roots[root_id])``; an unhashable root (a list) made the
    ``_register_artifact`` ``in self.roots`` guard raise a raw
    ``TypeError``; and a hashable-but-unknown root silently dropped every
    non-glob declared artifact behind that same tolerance (DESIGN.md §3:
    malformed data never silently disappears).

    The fix validates the row root once, inside the ``comp is not None``
    block, BEFORE the input walk: ``isinstance(comp_root, str) and
    comp_root in self.roots``. On failure exactly ONE hard
    ``formalism-malformed-source`` at the linter row's catalog locator
    ``{root: worktree, path: smoke/eval_harness/catalog.json, key:
    components, index: 1}`` (component ``formalism-linter``), and every
    bare (no-colon) item is skipped with ``continue`` — no additional
    findings, no raw escape, and the bad root value is never echoed.
    Rooted (colon-bearing) items keep their own spec_root validation and
    process exactly as before; the ``comp.get("root", "plans")`` default,
    the ``_register_artifact`` missing-root tolerance (the design-pinned
    core registrations depend on it), and the K-1 two-pass container
    validation are untouched (round 8, MINOR-1 one-hard-and-drop rule).

    Every probe mutates a fresh /tmp copy; the real corpus is never
    written. The unmutated live shape is pinned throughout: 172
    entities / 84 relationships / 10 findings (all soft), zero hard.
    """

    # Live catalog: components[1] is the formalism-linter row.
    LINTER_ROW_INDEX = 1

    def _run(self, plans: Path, worktree: Path,
             catalog: dict | None = None):
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if c.get("id") == "formalism-linter")

    @staticmethod
    def _components_findings(res) -> list:
        return [
            f for f in res.findings
            if f["source"].get("path") == "smoke/eval_harness/catalog.json"
            and f["source"].get("key") == "components"
        ]

    def _row_hard(self, res, base) -> dict:
        """Pin the single row-level hard and the exact finding delta."""
        bad = self._components_findings(res)
        self.assertEqual(len(bad), 1)
        b = bad[0]
        self.assertEqual(b["code"], "formalism-malformed-source")
        self.assertEqual(b["component"], "formalism-linter")
        self.assertEqual(b["level"], "error")
        self.assertEqual(b["impact"], "hard")
        self.assertEqual(b["source"],
                         {"root": "worktree",
                          "path": "smoke/eval_harness/catalog.json",
                          "key": "components",
                          "index": self.LINTER_ROW_INDEX})
        # findings = baseline + exactly this one hard, nothing else
        self.assertEqual(len(res.findings), len(base.findings) + 1)
        self.assertEqual(
            {self._sig(f) for f in res.findings}
            - {self._sig(g) for g in base.findings},
            {self._sig(b)})
        return b

    # -- F-1: the linter row root must be a supplied context root -----------

    def test_unknown_string_root_bare_glob_is_one_hard(self):
        # F-1a (coordinator probe): schema-legal enum root "agents-home"
        # is not among the supplied {worktree, plans}; the appended bare
        # glob reaches the ``/**`` branch — pre-fix a raw
        # KeyError('agents-home') escaped discover(). Post-fix: ONE hard
        # at the linter row index, no artifact minted from the glob, no
        # escape; the bare-minted artifacts are all redundantly
        # registered (design-pinned core registrations + code pins), so
        # the whole entity set stays byte-identical to the unmutated
        # run (K-1a discipline) and only the finding changes.
        plans, worktree = _copy_corpus("r12-root-str-glob")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        comp["root"] = "agents-home"
        comp["inputs"].append("parity-formalism/tools/fixtures/**")
        try:
            res = self._run(plans, worktree, catalog=catalog)
        except Exception as exc:
            self.fail(f"discover() escaped with a raw "
                      f"{type(exc).__name__} (F-1)")
        self.assertIsInstance(res, contract.AdapterResult)
        self._row_hard(res, base)
        ids = {e["id"] for e in res.entities}
        self.assertFalse(
            any("parity-formalism/tools/fixtures" in aid for aid in ids),
            "artifacts minted from the bare glob on a bad-root row")
        self.assertEqual(ids, {e["id"] for e in base.entities})

    def test_int_root_bare_glob_is_one_hard(self):
        # F-1b (coordinator probe): root=42 — hashable but unknown: the
        # bare literals vanish behind the _register_artifact tolerance,
        # then the appended bare glob raises a raw KeyError(42).
        # Post-fix: the same single row-level hard, no escape, and the
        # whole entity set stays byte-identical to the unmutated run.
        plans, worktree = _copy_corpus("r12-root-int-glob")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        comp["root"] = 42
        comp["inputs"].append("parity-formalism/tools/fixtures/**")
        try:
            res = self._run(plans, worktree, catalog=catalog)
        except Exception as exc:
            self.fail(f"discover() escaped with a raw "
                      f"{type(exc).__name__} (F-1)")
        self.assertIsInstance(res, contract.AdapterResult)
        self._row_hard(res, base)
        ids = {e["id"] for e in res.entities}
        self.assertFalse(
            any("parity-formalism/tools/fixtures" in aid for aid in ids))
        self.assertEqual(ids, {e["id"] for e in base.entities})

    def test_list_root_bare_literal_is_one_hard(self):
        # F-1c (coordinator probe): root=["a"] — unhashable: the
        # _register_artifact ``in self.roots`` guard raises a raw
        # TypeError on the row's first bare item. Post-fix: ONE hard, no
        # TypeError, and the appended bare literal mints no artifact.
        plans, worktree = _copy_corpus("r12-root-list-lit")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        comp["root"] = ["a"]
        comp["inputs"].append("parity-formalism/EXEMPLARS.md")
        try:
            res = self._run(plans, worktree, catalog=catalog)
        except Exception as exc:
            self.fail(f"discover() escaped with a raw "
                      f"{type(exc).__name__} (F-1)")
        self.assertIsInstance(res, contract.AdapterResult)
        self._row_hard(res, base)
        ids = {e["id"] for e in res.entities}
        self.assertNotIn("artifact:plans:parity-formalism/EXEMPLARS.md",
                         ids)
        self.assertEqual(ids, {e["id"] for e in base.entities})

    def test_rooted_item_on_bad_root_row_still_registers(self):
        # F-1d: a rooted (colon-bearing) item on the same bad-root row
        # validates its own spec_root and still registers while every
        # bare item on the row is skipped — the entity set stays
        # byte-identical to the unmutated run (the bare-minted
        # artifacts are redundantly registered by the design-pinned
        # core registrations and code pins) and the finding delta is
        # exactly the one row-level hard.
        plans, worktree = _copy_corpus("r12-root-bare-skip")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        comp["root"] = "agents-home"
        comp["inputs"].append("worktree:" + HARD_RULES_JSON)
        res = self._run(plans, worktree, catalog=catalog)  # must not raise
        self._row_hard(res, base)
        ids = {e["id"] for e in res.entities}
        self.assertIn(ARTIFACT_HARD_RULES, ids)
        # The bare items on the row are skipped, but every artifact they
        # would have minted is redundantly registered elsewhere
        # (design-pinned core registrations + code pins), so the entity
        # set stays byte-identical to the unmutated run and the skip is
        # pinned by the single row-level hard alone.
        self.assertEqual(ids, {e["id"] for e in base.entities})

    def test_unmutated_control_identical_to_clean_baseline(self):
        # F-1e (control): the unmutated shape through the same harness
        # is identical to the clean baseline — zero new findings, zero
        # hard. Guards the harness itself for the F-1 probes.
        plans, worktree = _copy_corpus("r12-control")
        self.addCleanup(shutil.rmtree, plans.parent, ignore_errors=True)
        base = self._run(plans, worktree)
        res = self._run(plans, worktree,
                        catalog=copy.deepcopy(contract.load_catalog()))
        self.assertEqual(len(res.findings), len(base.findings))
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(f) for f in base.findings})
        self.assertEqual({e["id"] for e in res.entities},
                         {e["id"] for e in base.entities})
        self.assertEqual(len(res.relationships), len(base.relationships))
        self.assertEqual([f for f in res.findings
                          if f["impact"] == "hard"], [])


# --- no-write proof -------------------------------------------------------------


class NoWriteProofTest(unittest.TestCase):
    """Discovery must create or modify no plans-corpus file, including any
    __pycache__ entry, and must not leave bytecode under the harness itself."""

    def test_real_discover_never_writes_corpus(self):
        plans, worktree = _plans_root(), _worktree_root()
        before = _snapshot_corpus(plans)
        res = _discover(plans, worktree)
        after = _snapshot_corpus(plans)
        self.assertEqual(before, after,
                         "discovery created or modified a plans-corpus "
                         "entry (byte or link)")
        self.assertEqual(_hard_findings(res), [])

    def test_no_pycache_under_eval_harness(self):
        self.assertEqual(os.environ.get("PYTHONDONTWRITEBYTECODE"), "1",
                         "tests must run with PYTHONDONTWRITEBYTECODE=1")
        import smoke.eval_harness
        root = Path(smoke.eval_harness.__file__).resolve().parent
        pycache = [p for p in root.rglob("__pycache__")]
        self.assertEqual(pycache, [])
        self.assertEqual(list(root.rglob("*.pyc")), [])


class Round14TwinFixTest(unittest.TestCase):
    """Formalism-side twin of the round-14 GLM M-2 fix: the shared shape
    predicate rejected only a DOT-LEADING ``.`` segment, so an
    interior/trailing ``.`` segment (``a/./b``, ``x/.``) survived the
    catalog-spec channels. The inventory mirror proves the same
    predicate value drives hard findings at every consumer (round-9 M-4
    curated walk, declared-input walk); this class pins the predicate
    itself plus the adjudicated non-violations."""

    def test_interior_dot_is_violation(self):
        self.assertTrue(
            formalism._catalog_spec_shape_violation("docs/./owner.md")
        )

    def test_trailing_dot_is_violation(self):
        self.assertTrue(
            formalism._catalog_spec_shape_violation("parity-formalism/.")
        )

    def test_leading_and_bare_dot_still_violations(self):
        """Guard: the round-8 shapes keep violating."""
        for spec in (".", "./x.md", "a//b.md", "", "   "):
            self.assertTrue(
                formalism._catalog_spec_shape_violation(spec), spec
            )

    def test_substring_dotfile_and_glob_shapes_still_flow(self):
        """Over-correction guards: '.'-substring names, dotfile globs,
        '**' globs and trailing-dot names are legal (round-8 MINOR-1
        narrow-scope discipline) and must keep flowing."""
        for spec in ("a..b", "a/..b/c", "trailing.dot.", "**", "a..b/**",
                     ".hidden/x.md", "parity-formalism/**"):
            self.assertFalse(
                formalism._catalog_spec_shape_violation(spec), spec
            )

    def test_clean_paths_still_flow(self):
        """Guard: ordinary declared paths are unaffected."""
        for spec in ("docs/owner.md", "parity-formalism/FORMALISM.md",
                     "smoke/eval_harness/catalog.json"):
            self.assertFalse(
                formalism._catalog_spec_shape_violation(spec), spec
            )


class Round15FixesTest(unittest.TestCase):
    """Batch A round 15: coordinator-verified formalism fixes R-1/R-2/R-3.

    R-1 (GLM F-1): the shared shape predicate rejected only an
    empty/whitespace-only BODY, but the schema path-token character
    class (``^[A-Za-z0-9._*%-]+(?:/[A-Za-z0-9._*%-]+)*$``) forbids
    whitespace ANYWHERE: a padded or internal-whitespace spec survived
    the gate and its consumers silently missed (inventory-side
    designation flip / exclude no-op) or hard-failed on this side
    (curated walk, linter input walk, artifact collection). The gate
    now rejects any whitespace character in the spec.

    R-2 (QC M-1): the catalog channels had NO NUL rejection. A NUL in a
    curated_sources path passes every shape gate and then escapes
    ``read()`` as a raw ``ValueError: embedded null byte``; a NUL in a
    linter bare or rooted input rides verbatim into a minted artifact
    id (zero findings); a NUL in a rooted glob input is a silent drop
    (zero findings). Mirrors the inventory ``_reject_nul``-at-every-
    channel discipline: ONE hard ``formalism-malformed-source`` at the
    catalog locator, the item is dropped, and the raw value is never
    echoed (locators carry only root/path/key/index constants).

    R-3 (QC M-2): the linter search breaks on first match, so a SECOND
    well-formed row with id == ``formalism-linter`` was silently
    ignored (its inputs never walked, zero findings) — the K-1 "every
    row accounted for" principle, with no mirror of the load-time
    duplicate-id rejection. First-match-wins stays for collection;
    every ADDITIONAL linter-id row is ONE hard finding at its own row
    index.

    Every probe mutates only a deep copy of the live catalog and runs
    read-only against the live plans corpus and worktree (the live
    catalog itself is never written); no /tmp corpus copies are needed.
    """

    @staticmethod
    def _run_live(catalog: dict | None = None):
        plans, worktree = _plans_root(), _worktree_root()
        ctx = contract.AdapterContext(
            roots={"worktree": worktree, "plans": plans},
            catalog=catalog if catalog is not None
            else contract.load_catalog())
        return formalism.discover(ctx, ())

    @staticmethod
    def _sig(f: dict) -> tuple:
        return (
            f["component"], f["code"], f["level"], f["impact"],
            json.dumps(f["source"], sort_keys=True), f["occurrence"],
        )

    @staticmethod
    def _linter_comp(catalog: dict) -> dict:
        return next(c for c in catalog["components"]
                    if isinstance(c, dict)
                    and c.get("id") == formalism.COMPONENT_LINTER)

    def _assert_one_hard_delta(self, res, base, source: dict,
                               component: str) -> dict:
        """Findings = baseline + exactly ONE hard at ``source``."""
        self.assertEqual(len(res.findings), len(base.findings) + 1)
        new = ({self._sig(f) for f in res.findings}
               - {self._sig(g) for g in base.findings})
        self.assertEqual(len(new), 1, "finding delta is not one record")
        b = next(f for f in res.findings if self._sig(f) in new)
        self.assertEqual(b["code"], "formalism-malformed-source")
        self.assertEqual(b["component"], component)
        self.assertEqual(b["level"], "error")
        self.assertEqual(b["impact"], "hard")
        self.assertEqual(b["source"], source)
        return b

    @classmethod
    def _source_values(cls, value):
        if isinstance(value, dict):
            for k, v in value.items():
                yield k
                yield from cls._source_values(v)
        elif isinstance(value, (list, tuple)):
            for v in value:
                yield from cls._source_values(v)
        else:
            yield value

    def _assert_no_nul_echo(self, res) -> None:
        """No NUL may ride into an id, a finding source, or a target."""
        self.assertFalse(
            any("\x00" in e.get("id", "") for e in res.entities),
            "an entity id carries a NUL byte (raw emit)")
        for f in res.findings:
            self.assertFalse(
                any("\x00" in v
                    for v in self._source_values(f["source"])
                    if isinstance(v, str)),
                f"a finding source echoes a NUL: {sorted(f['source'])}")
        for r in res.relationships:
            self.assertNotIn("\x00", r.get("source", ""))
            self.assertNotIn("\x00", r.get("target", ""))

    # -- R-1 (GLM F-1): whitespace anywhere is a shape violation -----------

    def test_leading_whitespace_spec_is_violation(self):
        for spec in (" x/y", "\tx/y"):
            self.assertTrue(
                formalism._catalog_spec_shape_violation(spec), spec)

    def test_trailing_whitespace_spec_is_violation(self):
        for spec in ("x/y ", "x/y\t"):
            self.assertTrue(
                formalism._catalog_spec_shape_violation(spec), spec)

    def test_internal_whitespace_spec_is_violation(self):
        for spec in ("x y/z", "x\ty"):
            self.assertTrue(
                formalism._catalog_spec_shape_violation(spec), spec)

    def test_clean_spec_shapes_still_flow(self):
        """Guard (green pre- and post-fix): schema-legal shapes keep
        flowing — the widened gate must not over-reject."""
        for spec in ("x/y", "a..b", ".hidden/x", "**", "trailing.dot."):
            self.assertFalse(
                formalism._catalog_spec_shape_violation(spec), spec)

    def test_padded_linter_input_is_one_hard_not_raw_minted(self):
        # R-1 channel integration: a leading-padded bare spec passed
        # the pre-fix gate (only the empty body was rejected) and
        # minted ``artifact:plans: parity-formal/F.md`` — id with a
        # leading space — verbatim with ZERO findings; the
        # inventory-side designation flip / exclude silently no-opped
        # on the same value. Post-fix: ONE hard at the item locator,
        # item dropped, nothing minted.
        plans, worktree = _plans_root(), _worktree_root()
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        idx = len(comp["inputs"])
        comp["inputs"].append(" parity-formal/F.md")
        res = self._run_live(catalog=catalog)  # must not raise
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": f"{formalism.COMPONENT_LINTER}.inputs",
             "index": idx},
            formalism.COMPONENT_LINTER)
        self.assertEqual(len(res.entities), len(base.entities))
        self.assertFalse(
            any(e["id"].startswith("artifact:plans: ")
                for e in res.entities),
            "a padded spec raw-minted an artifact id")

    # -- R-2 (QC M-1): NUL gates on the catalog channels --------------------

    def _linter_input_probe(self, item: str):
        """Baseline + probe run with one item appended to the live
        linter row's inputs (deep-copied catalog; live roots read-only).
        Returns ``(base, res, index)``."""
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        comp = self._linter_comp(catalog)
        idx = len(comp["inputs"])
        comp["inputs"].append(item)
        try:
            res = self._run_live(catalog=catalog)
        except Exception as exc:
            self.fail(f"discover() escaped with a raw "
                      f"{type(exc).__name__} (QC M-1)")
        return base, res, idx

    def test_bare_nul_input_is_one_hard_no_raw_emit(self):
        # A bare spec with a NUL passed every shape gate and minted
        # ``artifact:plans:parity-formal/F\x00RM.md`` verbatim with
        # ZERO findings (verified round 15). Post-fix: ONE hard at the
        # item locator, item dropped, no NUL rides into an id.
        base, res, idx = self._linter_input_probe("parity-formal/F\x00RM.md")
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": f"{formalism.COMPONENT_LINTER}.inputs",
             "index": idx},
            formalism.COMPONENT_LINTER)
        self.assertEqual(len(res.entities), len(base.entities))
        self._assert_no_nul_echo(res)

    def test_rooted_nul_input_is_one_hard_no_raw_emit(self):
        # A rooted (colon-bearing) spec with a NUL minted the artifact
        # id verbatim with ZERO findings (verified round 15); the same
        # item locator covers the rooted variant.
        base, res, idx = self._linter_input_probe(
            "plans:parity\x00-formalism/EXEMPLARS.md")
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": f"{formalism.COMPONENT_LINTER}.inputs",
             "index": idx},
            formalism.COMPONENT_LINTER)
        self.assertEqual(len(res.entities), len(base.entities))
        self._assert_no_nul_echo(res)

    def test_glob_nul_input_is_one_hard_not_silently_dropped(self):
        # A rooted glob with a NUL was a SILENT DROP pre-fix (the base
        # directory never exists, the walk returns quietly, zero
        # findings); DESIGN.md §8 requires the drop to be accounted
        # for: ONE hard at the item locator, nothing else moves.
        base, res, idx = self._linter_input_probe("plans:par\x00alism/**")
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": f"{formalism.COMPONENT_LINTER}.inputs",
             "index": idx},
            formalism.COMPONENT_LINTER)
        self.assertEqual(len(res.entities), len(base.entities))
        self._assert_no_nul_echo(res)

    def test_curated_nul_path_is_one_hard_no_value_error(self):
        # A NUL in a curated_sources path passes every shape gate and
        # then escapes ``read()`` as a raw ``ValueError: embedded null
        # byte`` (verified round 15). Post-fix: ONE hard at the entry
        # locator, the entry is dropped, no escape.
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        catalog["curated_sources"][0]["path"] = \
            "parity-formal\x00/FORMALISM.md"
        try:
            res = self._run_live(catalog=catalog)
        except ValueError as exc:
            self.fail(f"discover() escaped with a raw ValueError "
                      f"({exc!r}) (QC M-1)")
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": "curated_sources", "index": 0},
            formalism.COMPONENT_CORPUS)
        self._assert_no_nul_echo(res)

    # -- R-3 (QC M-2): every additional linter-id row is accounted for -----

    def test_duplicate_linter_row_is_one_hard_first_row_wins(self):
        # A full copy of the linter row appended as the 13th component
        # row was SILENTLY IGNORED pre-fix (break-on-first-match: the
        # duplicate's inputs never walked, zero findings) — the K-1
        # "every row accounted for" principle. Post-fix: ONE hard at
        # the duplicate's row index; collection keeps first-match-wins.
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        idx = len(catalog["components"])
        catalog["components"].append(
            copy.deepcopy(self._linter_comp(catalog)))
        res = self._run_live(catalog=catalog)
        self._assert_one_hard_delta(
            res, base,
            {"root": "worktree",
             "path": "smoke/eval_harness/catalog.json",
             "key": "components", "index": idx},
            formalism.COMPONENT_LINTER)

    def test_duplicate_linter_row_keeps_first_row_entities(self):
        # First-match-wins: the FIRST row's artifacts and sections
        # stay exactly the baseline run (the duplicate's inputs would
        # all re-register redundantly, so the entity and relationship
        # sets are unchanged) and the finding delta is exactly the one
        # row-level hard.
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        catalog["components"].append(
            copy.deepcopy(self._linter_comp(catalog)))
        res = self._run_live(catalog=catalog)
        self.assertEqual(len(res.entities), len(base.entities))
        self.assertEqual({e["id"] for e in res.entities},
                         {e["id"] for e in base.entities})
        self.assertEqual(len(res.relationships), len(base.relationships))

    def test_two_additional_linter_rows_are_two_hards(self):
        # "For EVERY additional dict row": two appended copies of the
        # linter row are each ONE hard at their own index, still
        # first-match-wins for collection.
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        first_idx = len(catalog["components"])
        catalog["components"].append(
            copy.deepcopy(self._linter_comp(catalog)))
        catalog["components"].append(
            copy.deepcopy(self._linter_comp(catalog)))
        res = self._run_live(catalog=catalog)
        self.assertEqual(len(res.findings), len(base.findings) + 2)
        hards = [f for f in res.findings if f["impact"] == "hard"]
        self.assertEqual(len(hards), 2)
        for f, j in zip(hards, (first_idx, first_idx + 1)):
            self.assertEqual(f["code"], "formalism-malformed-source")
            self.assertEqual(f["component"], formalism.COMPONENT_LINTER)
            self.assertEqual(f["level"], "error")
            self.assertEqual(f["source"],
                             {"root": "worktree",
                              "path": "smoke/eval_harness/catalog.json",
                              "key": "components", "index": j})
        self.assertEqual(len(res.entities), len(base.entities))

    def test_duplicate_non_linter_row_adds_no_findings(self):
        """Guard (green pre- and post-fix): the R-3 gate is scoped to
        the linter row id — an appended duplicate of a NON-linter row
        (a corpus row the formalism adapter never walks) changes
        nothing: zero new findings, zero hard."""
        base = self._run_live()
        catalog = copy.deepcopy(contract.load_catalog())
        row = next(c for c in catalog["components"]
                   if isinstance(c, dict)
                   and c.get("id") == "parity-formalism")
        catalog["components"].append(copy.deepcopy(row))
        res = self._run_live(catalog=catalog)
        self.assertEqual(len(res.findings), len(base.findings))
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(g) for g in base.findings})
        self.assertEqual([f for f in res.findings
                          if f["impact"] == "hard"], [])
        self.assertEqual(len(res.entities), len(base.entities))

    def test_unmutated_live_shaped_run_adds_zero_findings(self):
        """Guard (green pre- and post-fix): a deep-copied UNMUTATED
        live catalog through the same harness adds ZERO findings over
        the loaded-catalog run and stays hard-free — the new gates
        must be silent on the live shape."""
        base = self._run_live()
        res = self._run_live(catalog=copy.deepcopy(contract.load_catalog()))
        self.assertEqual(len(res.findings), len(base.findings))
        self.assertEqual(
            {self._sig(f) for f in res.findings},
            {self._sig(g) for g in base.findings})
        self.assertEqual([f for f in res.findings
                          if f["impact"] == "hard"], [])
        self.assertEqual(len(res.entities), len(base.entities))
        self.assertEqual(len(res.relationships), len(base.relationships))


class Round16SeatReviewFixesTest(unittest.TestCase):
    """Batch A round 16: three-seat review fix R-4 (Seat C M-4).

    A whole-value ``source_dir`` that is a single-segment absolute path
    (``/etc``, ``/``) matched no ``_ABS_TOKEN_RE`` token (that pattern
    requires two or more segments) and was not a clean relative path, so
    it fell through to the descriptive-annotation branch and was copied
    verbatim into the emitted arm record with ZERO findings — the one
    shape that published an absolute machine path while staying silent.
    A leading ``/`` is now the same finding-plus-hash path as the other
    traversal shapes.

    Hermetic: the gate is driven directly on a ``_State`` whose only root
    is an empty temporary directory, so no live corpus is read.
    """

    LOC = {"root": "plans", "path": "parity-formalism/tools/x.json",
           "key": "source_dir"}

    def _state(self) -> formalism._State:
        root = Path(tempfile.mkdtemp(prefix="formalism-r16-"))
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        return formalism._State(contract.AdapterContext(
            roots={"plans": root}, catalog={}))

    def test_single_segment_absolute_source_dir_is_finding_plus_hash(self):
        for value in ("/etc", "/", "/tmp", "/Home", "/a b"):
            with self.subTest(value=value):
                st = self._state()
                out = st._source_dir(value, self.LOC)
                self.assertIsNone(out, value)
                hits = [f for f in st.findings
                        if f["code"] == "formalism-source-dir-unrooted"]
                self.assertEqual(len(hits), 1, value)
                self.assertEqual(hits[0]["level"], "warning")
                self.assertEqual(hits[0]["impact"], "soft")
                self.assertEqual(
                    hits[0]["detail"],
                    [{"sha256": hashlib.sha256(
                        value.encode("utf-8")).hexdigest(),
                      "bytes": len(value.encode("utf-8"))}])
                self.assertEqual(hits[0]["source"], self.LOC)
                # The raw value is never echoed into any finding detail
                # (checked as a complete JSON string, not a substring).
                blob = contract.canonical_json_bytes(
                    [dict(f) for f in st.findings]).decode("utf-8")
                self.assertNotIn(json.dumps(value), blob)

    def test_multi_segment_absolute_and_prose_paths_are_unchanged(self):
        # Guard (green pre- and post-fix): the absolute-token branch, the
        # descriptive-annotation prose of the live corpus, and the
        # rooted-conversion branch all keep their round-15 behaviour.
        st = self._state()
        out = st._source_dir("/opt/app/config", self.LOC)
        self.assertEqual(
            out,
            [f"@external/config#"
             f"{hashlib.sha256(b'/opt/app/config').hexdigest()}"], out)
        self.assertEqual(
            [f["code"] for f in st.findings],
            ["formalism-source-dir-unrooted"])
        prose = ("SYNTHESIZED (HARDENING-SPEC 2.2a clause-isolating pin; "
                 "EV-13/EV-9-stamped)")
        st2 = self._state()
        self.assertEqual(st2._source_dir(prose, self.LOC), prose)
        self.assertEqual(st2.findings, [])
        rooted = "wt/grok-build-responses/smoke/redteam/report/x/wire"
        st3 = self._state()
        (Path(st3.roots["plans"]) / rooted).mkdir(parents=True)
        self.assertEqual(st3._source_dir(rooted + "/", self.LOC),
                         {"root": "plans", "path": rooted})
        self.assertEqual(st3.findings, [])


if __name__ == "__main__":
    unittest.main()
