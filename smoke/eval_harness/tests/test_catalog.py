"""Contract tests for the strict registry, schemas, and catalog (Task 2).

Normative source: plans/eval-harness/DESIGN.md §§2–5. Every expectation
below is pinned verbatim from that design and first-hand repository
verification of the owner documents; nothing here reads the plans
repository at test time (hermetic).

Run from the worktree root:
    python3.14 -m unittest -v smoke.eval_harness.tests.test_catalog
"""
import copy
import dataclasses
import json
import os
import sys
import unittest
from pathlib import Path

from smoke.eval_harness import contract, paths

try:
    import jsonschema
except ImportError:  # pragma: no cover - dual-validation is optional
    jsonschema = None

CATALOG_PATH = os.path.join(os.path.dirname(contract.__file__), "catalog.json")
CATALOG_SCHEMA_NAME = "catalog.schema.json"
INDEX_SCHEMA_NAME = "evidence-index.schema.json"

# --- DESIGN.md §2: the six roots, verbatim ---------------------------------
EXPECTED_ROOTS = [
    {"id": "worktree", "required_for": ["build", "health"]},
    {"id": "plans", "required_for": ["build", "health"]},
    {"id": "grok-home", "required_for": []},
    {"id": "codex-home", "required_for": []},
    {"id": "agents-home", "required_for": []},
    {"id": "logscale", "required_for": []},
]

# --- DESIGN.md §2: the three inventory scopes, verbatim ---------------------
EXPECTED_SCOPES = [
    {
        "id": "plans-corpus",
        "root": "plans",
        "includes": ["parity-formalism", "provenance", "xwire"],
        "selection": "recursive-all-regular-and-symlink",
    },
    {
        "id": "plans-declared-inputs",
        "root": "plans",
        "selection": "catalog-static-sources-outside-plans-corpus",
    },
    {
        "id": "worktree-declared-inputs",
        "root": "worktree",
        "selection": "catalog-static-sources-plus-indexer-source",
        "includes": [
            "catalog-declared",
            "smoke/eval_harness/catalog.json",
            "smoke/eval_harness/README.md",
            "smoke/eval_harness/*.py",
            "smoke/eval_harness/adapters/*.py",
            "smoke/eval_harness/schemas/*.json",
            "smoke/eval_harness/tests/**",
        ],
        "excludes": [
            "smoke/eval_harness/site/**",
            "smoke/eval_harness/report/**",
            "**/__pycache__/**",
            "**/*.pyc",
        ],
    },
]

EXPECTED_COMPONENT_IDS = [
    "parity-formalism",
    "formalism-linter",
    "provenance",
    "xwire",
    "redteam",
    "wstream",
    "xwfix",
    "wiretap",
    "lifecycle",
    "dogfood",
    "parity-repro",
    "session-triage",
]

# --- DESIGN.md §2: the redteam row, verbatim + its §4 selftest entry --------
EXPECTED_REDTEAM_ROW = {
    "id": "redteam",
    "kind": "runner",
    "root": "worktree",
    "entrypoints": [
        {
            "root": "worktree",
            "path": "smoke/redteam/run.py",
            "role": "source",
            "availability": "required",
        },
        {
            "root": "worktree",
            "path": "smoke/redteam/run.py",
            "role": "selftest",
            "availability": "health-only",
            "argv": ["python3.14", "smoke/redteam/run.py", "--selftest"],
        },
    ],
    "adapter": "redteam",
    "inputs": ["smoke/redteam/case.schema.json", "smoke/redteam/cases"],
    "outputs": ["campaign.json", "report.json", "*/verdict.json", "*/wire"],
    "lifecycle": {"registry": "smoke/runs.jsonl", "suite": "redteam"},
    "install": None,
    "owner_docs": [
        {
            "root": "worktree",
            "path": "AGENTS.md",
            "heading": "9. The redteam / smoke suite (`smoke/redteam/`)",
        }
    ],
    "content_policy": "metadata-only",
}

# --- DESIGN.md §2: parity-formalism declared inputs, verbatim ---------------
# Inputs are relative to the row's declared root `plans`, i.e. they carry
# the `parity-formalism/` prefix (review B-1: they must not be relative to
# the component's own directory).
EXPECTED_PARITY_FORMALISM_INPUTS = [
    "parity-formalism/EXEMPLARS.md",
    "parity-formalism/FORMALISM.md",
    "parity-formalism/HARDENING-SPEC.md",
    "parity-formalism/OBSERVATIONS.md",
    "parity-formalism/Q-A.md",
    "parity-formalism/intel/02-qwen-codexfam-reasoning-parity.md",
    "parity-formalism/parity-hardening-report.md",
    "parity-formalism/tools/expected_verdicts_ev12.json",
    "parity-formalism/tools/fixtures",
]

# --- DESIGN.md §3.4: exact curated selectors --------------------------------
# The HARDENING-SPEC.md selector allowlist is exhaustive (DESIGN.md §3 item
# 4); the §2 umbrella heading is deliberately NOT in it (review M-A1).
FORMALISM_CATEGORY_HEADING = '2. Category decomposition (the "math" of a parity diff)'
HARDENING_HEADINGS = [
    "1. Evidence base (wire-verified as of 2026-09-23)",
    "2.1 Hard invariants (I^h; default artifact set A1 + A2 + A3 — a row may override, see H-7)",
    "2.2 Soft invariants (I^s — violation = cache/continuity/$ loss, not 4xx; A1 only, flag-level)",
    "2.2a Strict-row enc re-scope — exact clauses (T8 OQ-T8-1/2 adjudication, 2026-09-23; bead apex-ayl.126.8.5)",
    "2.3 Open questions — NOT enforced (wire evidence owed first)",
]
QA_MARKERS = [
    'Q: "Back to nuts and bolts — what about the beta headers for context management, '
    "like clear_thinking or tool-level TTL, there's a bunch of advanced concepts?\"",
    "Q: \"And across codex and claude we can start systematically — how are "
    'encrypted_content and thinking signatures etc?"',
    "Q: \"For qwen, 'thinking' comes back only in the summary field, right? And how's "
    "qwen parity, codex vs grok-build?\"",
    "Q: \"I want a formalism. We have APIs per provider, invariants that 400 (solved with "
    "'projectors'), content differences, and nondeterminism that grows with turn count. "
    'This should be math. What are we even describing? Get on the same page first."',
    "Q: \"Document what we observe, and give us a Q-A.md of this back-and-forth (legible, "
    'not verbatim) so other sessions can brainstorm with us."',
]

# Owner-document anchors verified first-hand against the live documents
# (DESIGN.md §2: exact normalized heading text, no substring matching).
EXPECTED_OWNER_DOCS = {
    "parity-formalism": [
        {"root": "plans", "path": "parity-formalism/FORMALISM.md",
         "heading": "Wire-parity formalism — same-page doc (v0, 2026-09-21)"}
    ],
    "formalism-linter": [
        {"root": "plans", "path": "parity-formalism/HARDENING-SPEC.md",
         "heading": "3.1 A1 — linter: `grok/plans/parity-formalism/tools/invariant_lint.py`"}
    ],
    "provenance": [
        {"root": "plans", "path": "provenance/donors.md",
         "heading": "Donor registry (apex-v2-grok-build campaign)"}
    ],
    "xwire": [
        {"root": "worktree", "path": "AGENTS.md",
         "heading": "6. Wire invariants (model-visible context discipline)"}
    ],
    "redteam": EXPECTED_REDTEAM_ROW["owner_docs"],
    "wstream": [
        {"root": "worktree", "path": "smoke/wstream/README.md",
         "heading": "WSTREAM — wire-streaming matrix (model × api_backend × wire)"}
    ],
    "xwfix": [
        {"root": "worktree", "path": "smoke/xwfix/README.md",
         "heading": "XW-FIXTURES — cross-wire golden corpus (apex-ayl.70)"}
    ],
    "wiretap": [
        {"root": "plans", "path": "HT-1-redteam-harness-spec.md",
         "heading": "2. A1 — wiretap2 (wire capture, definitive evidence)"}
    ],
    "lifecycle": [
        {"root": "plans", "path": "smoke-gate-unification-design-20260919.md",
         "heading": "6.2 New instrumentation (not in this doc's inventory)"}
    ],
    "dogfood": [
        {"root": "plans", "path": "HT-1-redteam-harness-spec.md",
         "heading": "3. A5 — grok-dogfood v2 (dogfood = instrumented by default)"}
    ],
    "parity-repro": [
        {"root": "plans", "path": "codex-parity-smoke/PLAN.md",
         "heading": "codex-parity-smoke — PLAN (mission: why is sol expensive on "
                    "grok-build, and how do we make sol/terra/luna on the custom "
                    "harness as cheap+efficient as first-party codex?)"}
    ],
    "session-triage": [
        {"root": "plans", "path": "session-triage-skill-plan-20260915.md",
         "heading": "Session-Triage Script + Skill Implementation Plan (v2)"}
    ],
}

# formalism-linter declared inputs (DESIGN.md §2: generator, rules, linter,
# the H-7 code-pin report, and the three worktree targets).
LINTER_PLANS_INPUTS = [
    "parity-formalism/tools/generate_outbound_lint_rules.py",
    "parity-formalism/tools/invariant_rules.json",
    "parity-formalism/tools/invariant_lint.py",
    "xwavec71/wavec71-green-report-qwen-20260918.md",
]
LINTER_WORKTREE_INPUTS = [
    "worktree:crates/codegen/xai-grok-sampling-types/src/conversation/rules_generated.rs",
    "worktree:crates/codegen/xai-grok-sampling-types/src/conversation/projection_tests.rs",
    "worktree:crates/codegen/xai-grok-sampling-types/fixtures/outbound_lint/**",
]

SELFTEST_ARGV_TEMPLATES = [
    ["python3.14", "smoke/redteam/run.py", "--selftest"],
    ["python3.14",
     "<plans-root>/parity-formalism/tools/generate_outbound_lint_rules.py",
     "--check", "--worktree", "."],
    ["python3.14",
     "<plans-root>/parity-formalism/tools/invariant_lint.py",
     "<plans-root>/parity-formalism/tools/fixtures/at-strict-xwire-01",
     "--soft", "--worktree", "."],
]

COMPONENT_REQUIRED_KEYS = [
    "id", "kind", "root", "entrypoints", "adapter", "inputs", "outputs",
    "lifecycle", "install", "owner_docs", "content_policy",
]
TOP_LEVEL_REQUIRED_KEYS = [
    "schema_version", "roots", "inventory_scopes", "components", "curated_sources",
]


def load_catalog():
    with open(CATALOG_PATH, "rb") as fh:
        return json.loads(fh.read().decode("utf-8"))


def component_row(catalog, comp_id):
    for row in catalog["components"]:
        if row["id"] == comp_id:
            return row
    raise AssertionError(f"component row {comp_id} missing")


class CatalogAcceptanceTest(unittest.TestCase):
    """The complete hand-maintained catalog is accepted and pinned exactly."""

    def test_complete_catalog_accepted(self):
        self.assertIsNone(contract.validate_catalog(load_catalog()))

    def test_exact_six_roots(self):
        self.assertEqual(load_catalog()["roots"], EXPECTED_ROOTS)

    def test_exact_three_scopes(self):
        self.assertEqual(load_catalog()["inventory_scopes"], EXPECTED_SCOPES)

    def test_exact_twelve_component_ids(self):
        catalog = load_catalog()
        self.assertEqual([r["id"] for r in catalog["components"]],
                         EXPECTED_COMPONENT_IDS)

    def test_redteam_row_verbatim(self):
        self.assertEqual(component_row(load_catalog(), "redteam"),
                         EXPECTED_REDTEAM_ROW)

    def test_parity_formalism_inputs_verbatim_root_relative(self):
        # B-1 pin: the nine inputs are relative to the declared root
        # `plans` (each carries the `parity-formalism/` prefix), not to the
        # component's own directory.
        row = component_row(load_catalog(), "parity-formalism")
        self.assertEqual(row["root"], "plans")
        self.assertEqual(row["inputs"], EXPECTED_PARITY_FORMALISM_INPUTS)

    def test_formalism_linter_declares_generator_rules_linter_report_targets(self):
        row = component_row(load_catalog(), "formalism-linter")
        for inp in LINTER_PLANS_INPUTS + LINTER_WORKTREE_INPUTS:
            self.assertIn(inp, row["inputs"])
        self.assertEqual(row["root"], "plans")
        entry_paths = [e["path"] for e in row["entrypoints"]]
        self.assertIn("parity-formalism/tools/generate_outbound_lint_rules.py",
                      entry_paths)
        self.assertIn("parity-formalism/tools/invariant_lint.py", entry_paths)
        self.assertIn("parity-formalism/tools/invariant_rules.json", entry_paths)

    def test_all_twelve_owner_document_headings(self):
        catalog = load_catalog()
        for comp_id in EXPECTED_COMPONENT_IDS:
            self.assertEqual(component_row(catalog, comp_id)["owner_docs"],
                             EXPECTED_OWNER_DOCS[comp_id], comp_id)

    def test_curated_selectors_named(self):
        catalog = load_catalog()
        by_path = {
            (s["root"], s["path"]): s["selectors"]
            for s in catalog["curated_sources"]
        }
        headings = by_path[("plans", "parity-formalism/FORMALISM.md")]
        self.assertEqual(headings,
                         [{"kind": "heading", "text": FORMALISM_CATEGORY_HEADING}])
        hard = by_path[("plans", "parity-formalism/HARDENING-SPEC.md")]
        self.assertEqual(sorted(h["text"] for h in hard if h["kind"] == "heading"),
                         sorted(HARDENING_HEADINGS))
        questions = by_path[("plans", "parity-formalism/Q-A.md")]
        self.assertEqual(sorted(q["marker"] for q in questions
                                if q["kind"] == "question"),
                         sorted(QA_MARKERS))
        for cited_path in ("parity-formalism/EXEMPLARS.md",
                           "parity-formalism/OBSERVATIONS.md",
                           "parity-formalism/intel/02-qwen-codexfam-reasoning-parity.md"):
            self.assertEqual(by_path[("plans", cited_path)],
                             [{"kind": "citations"}], cited_path)

    def test_fixed_selftest_argv_templates(self):
        catalog = load_catalog()
        templates = sorted(
            e["argv"]
            for row in catalog["components"]
            for e in row["entrypoints"]
            if e.get("role") == "selftest"
        )
        self.assertEqual(templates, sorted(SELFTEST_ARGV_TEMPLATES))

    def test_outputs_stay_run_root_relative(self):
        for row in load_catalog()["components"]:
            for glob in row["outputs"]:
                self.assertFalse(glob.startswith("/"), (row["id"], glob))
                self.assertFalse(":" in glob, (row["id"], glob))
                self.assertNotIn("\\", glob, (row["id"], glob))


class SchemaStrictnessTest(unittest.TestCase):
    """Every required key/enum is enforced; malformed structure fails."""

    def setUp(self):
        self.catalog = load_catalog()

    def assert_rejected(self, value, label):
        with self.assertRaises(contract.ContractError, msg=label):
            contract.validate_catalog(value)

    def test_required_top_level_keys_enforced(self):
        for key in TOP_LEVEL_REQUIRED_KEYS:
            mutated = copy.deepcopy(self.catalog)
            del mutated[key]
            self.assert_rejected(mutated, f"missing top-level {key}")

    def test_required_component_keys_enforced(self):
        for key in COMPONENT_REQUIRED_KEYS:
            mutated = copy.deepcopy(self.catalog)
            del mutated["components"][0][key]
            self.assert_rejected(mutated, f"missing component key {key}")

    def test_unknown_keys_fail(self):
        cases = []
        m = copy.deepcopy(self.catalog); m["extra_top"] = 1; cases.append(("top-level", m))
        m = copy.deepcopy(self.catalog); m["roots"][0]["extra"] = 1; cases.append(("root", m))
        m = copy.deepcopy(self.catalog); m["inventory_scopes"][0]["extra"] = 1
        cases.append(("scope", m))
        m = copy.deepcopy(self.catalog); m["components"][0]["extra"] = 1
        cases.append(("component", m))
        m = copy.deepcopy(self.catalog)
        m["components"][4]["entrypoints"][0]["extra"] = 1
        cases.append(("entrypoint", m))
        m = copy.deepcopy(self.catalog)
        m["components"][4]["owner_docs"][0]["extra"] = 1
        cases.append(("owner_doc", m))
        m = copy.deepcopy(self.catalog)
        m["components"][0]["lifecycle"]["extra"] = 1
        cases.append(("lifecycle", m))
        m = copy.deepcopy(self.catalog)
        m["curated_sources"][0]["extra"] = 1
        cases.append(("curated_source", m))
        m = copy.deepcopy(self.catalog)
        m["curated_sources"][0]["selectors"][0]["extra"] = 1
        cases.append(("selector", m))
        for label, mutated in cases:
            self.assert_rejected(mutated, f"unknown key in {label}")

    def test_duplicate_component_ids_fail(self):
        mutated = copy.deepcopy(self.catalog)
        shadow = copy.deepcopy(mutated["components"][0])
        shadow["id"] = "parity-formalism"
        shadow["kind"] = "tool"  # distinct object: beats whole-object uniqueItems
        mutated["components"][-1] = shadow
        self.assert_rejected(mutated, "duplicate component id")

    def test_duplicate_root_ids_fail(self):
        mutated = copy.deepcopy(self.catalog)
        shadow = copy.deepcopy(mutated["roots"][0])
        shadow["id"] = "plans"
        shadow["required_for"] = []  # distinct object
        mutated["roots"][-1] = shadow
        self.assert_rejected(mutated, "duplicate root id")

    def test_duplicate_scope_ids_fail(self):
        mutated = copy.deepcopy(self.catalog)
        shadow = copy.deepcopy(mutated["inventory_scopes"][0])
        shadow["id"] = "worktree-declared-inputs"
        shadow["selection"] = "catalog-static-sources-plus-indexer-source"
        # Replace the middle scope so the real one at index 2 remains.
        mutated["inventory_scopes"][1] = shadow
        self.assert_rejected(mutated, "duplicate scope id")

    def test_component_count_and_set_enforced(self):
        mutated = copy.deepcopy(self.catalog)
        mutated["components"] = mutated["components"][:-1]
        self.assert_rejected(mutated, "11 components")
        mutated = copy.deepcopy(self.catalog)
        shadow = copy.deepcopy(mutated["components"][0])
        shadow["id"] = "redteam"
        shadow["kind"] = "tool"
        mutated["components"].append(shadow)
        self.assert_rejected(mutated, "13 components")

    def test_enums_enforced(self):
        def reject_with(mutator, label):
            mutated = copy.deepcopy(self.catalog)
            mutator(mutated)
            self.assert_rejected(mutated, label)

        def bad_role(c): c["components"][4]["entrypoints"][0]["role"] = "run"
        def bad_availability(c): c["components"][4]["entrypoints"][0]["availability"] = "always"
        def bad_content_policy(c): c["components"][0]["content_policy"] = "full-text"
        def bad_adapter(c): c["components"][0]["adapter"] = "nope"
        def bad_selection(c): c["inventory_scopes"][0]["selection"] = "everything"
        def bad_root_id(c): c["roots"][0]["id"] = "Worktree"
        def bad_required_for(c): c["roots"][0]["required_for"] = ["deploy"]
        def bad_kind(c): c["components"][0]["kind"] = "Runner!"
        for fn, label in [
            (bad_role, "role enum"), (bad_availability, "availability enum"),
            (bad_content_policy, "content_policy enum"), (bad_adapter, "adapter enum"),
            (bad_selection, "selection enum"), (bad_root_id, "root id enum"),
            (bad_required_for, "required_for enum"), (bad_kind, "kind slug"),
        ]:
            reject_with(fn, label)

    def test_lifecycle_shape_enforced(self):
        def registry_without_suite(c):
            c["components"][5]["lifecycle"] = {"registry": "smoke/runs.jsonl"}
        def null_registry_without_reason(c):
            c["components"][5]["lifecycle"] = {"registry": None}
        def null_registry_with_suite(c):
            c["components"][5]["lifecycle"] = {"registry": None, "suite": "x"}
        def string_registry_with_reason(c):
            c["components"][5]["lifecycle"] = {"registry": "smoke/runs.jsonl",
                                              "suite": "wstream", "reason": "x"}
        for fn, label in [
            (registry_without_suite, "registry without suite"),
            (null_registry_without_reason, "null registry without reason"),
            (null_registry_with_suite, "null registry with suite"),
            (string_registry_with_reason, "string registry with reason"),
        ]:
            mutated = copy.deepcopy(self.catalog)
            fn(mutated)
            self.assert_rejected(mutated, label)

    def test_absolute_backslash_dotdot_paths_fail(self):
        def into_entrypoint(c): c["components"][4]["entrypoints"][0]["path"] = bad
        def into_owner(c): c["components"][4]["owner_docs"][0]["path"] = bad
        def into_input(c): c["components"][4]["inputs"][0] = bad
        def into_output(c): c["components"][4]["outputs"][0] = bad
        def into_scope_include(c): c["inventory_scopes"][2]["includes"][1] = bad
        def into_scope_exclude(c): c["inventory_scopes"][2]["excludes"][0] = bad
        def into_curated(c): c["curated_sources"][0]["path"] = bad
        for bad in ("/abs/x", "a\\b", "a/../b", "a/b/..", ".", "a/./b"):
            for fn, label in [
                (into_entrypoint, "entrypoint.path"), (into_owner, "owner_docs.path"),
                (into_input, "inputs"), (into_output, "outputs"),
                (into_scope_include, "scope includes"), (into_scope_exclude, "scope excludes"),
                (into_curated, "curated_sources path"),
            ]:
                mutated = copy.deepcopy(self.catalog)
                fn(mutated)
                self.assert_rejected(mutated, f"{label} = {bad!r}")

    def test_invalid_output_globs_fail(self):
        for bad in ("/abs/*.json", "worktree:run/*.json", "a/../b/*.json", "*\\x"):
            mutated = copy.deepcopy(self.catalog)
            mutated["components"][4]["outputs"][0] = bad
            self.assert_rejected(mutated, f"outputs glob {bad!r}")

    def test_cross_root_paths_fail(self):
        mutated = copy.deepcopy(self.catalog)
        mutated["components"][1]["inputs"][0] = "nosuchroot:a/b"
        self.assert_rejected(mutated, "undeclared root in inputs")
        mutated = copy.deepcopy(self.catalog)
        mutated["components"][4]["entrypoints"][0]["root"] = "nope"
        self.assert_rejected(mutated, "undeclared root in entrypoints")

    def test_selftest_argv_required_and_placeholder_rooted(self):
        mutated = copy.deepcopy(self.catalog)
        for row in mutated["components"]:
            for ep in row["entrypoints"]:
                if ep.get("role") == "selftest":
                    del ep["argv"]
        self.assert_rejected(mutated, "selftest without argv")
        mutated = copy.deepcopy(self.catalog)
        for row in mutated["components"]:
            for ep in row["entrypoints"]:
                if ep.get("role") == "selftest":
                    for i, elem in enumerate(ep["argv"]):
                        if elem.startswith("<"):
                            ep["argv"][i] = "<nosuch-root>/x"
        self.assert_rejected(mutated, "selftest placeholder with undeclared root")

    def test_selftest_argv_mid_element_placeholder_fails(self):
        # A <root-id-root> occurrence is only legal as a whole element or at
        # the start of one; embedded mid-element it slips no validator.
        mutated = copy.deepcopy(self.catalog)
        placed = False
        for row in mutated["components"]:
            for ep in row["entrypoints"]:
                if ep.get("role") == "selftest":
                    ep["argv"].append("--out=<plans-root>")
                    placed = True
                    break
            if placed:
                break
        self.assertTrue(placed)
        self.assert_rejected(mutated, "mid-element placeholder in selftest argv")


class ContractHelperTest(unittest.TestCase):
    """canonical_json_bytes, locator keys, sensitive_matches, the frozen
    adapter dataclasses, evidence-index id checks, module aliases."""

    def test_canonical_json_bytes_stable(self):
        value = {"b": [3, 2, 1], "a": {"z": "é", "y": None}, "c": True}
        first = contract.canonical_json_bytes(value)
        second = contract.canonical_json_bytes(copy.deepcopy(value))
        self.assertEqual(first, second)
        expected = (
            '{\n  "a": {\n    "y": null,\n    "z": "é"\n  },\n'
            '  "b": [\n    3,\n    2,\n    1\n  ],\n  "c": true\n}\n'
        ).encode("utf-8")
        self.assertEqual(first, expected)
        self.assertEqual(first[-1:], b"\n")
        self.assertEqual(first.count(b"\n\n"), 0)

    def test_canonical_json_utf8_not_escaped(self):
        out = contract.canonical_json_bytes({"x": "×—é"})
        self.assertIn("×—é".encode("utf-8"), out)
        self.assertNotIn(b"\\u", out)

    def test_canonical_json_bytes_rejects_non_finite_values(self):
        # Canonical records must be strict JSON: NaN/Infinity are not valid
        # JSON values, so canonicalization must raise, not emit them.
        for bad in ({"x": float("nan")}, [float("inf")], {"y": float("-inf")}):
            with self.assertRaises(contract.ContractError, msg=repr(bad)):
                contract.canonical_json_bytes(bad)

    def test_absent_locator_canonicalizes_as_empty_object(self):
        self.assertEqual(contract.canonical_locator_key(None), b"{}")

    def test_present_locator_canonical_compact_sorted(self):
        locator = {"path": "a/b.md", "root": "plans", "heading": "h"}
        self.assertEqual(
            contract.canonical_locator_key(locator),
            b'{"heading":"h","path":"a/b.md","root":"plans"}',
        )
        unsorted = {"root": "plans", "heading": "h", "path": "a/b.md"}
        self.assertEqual(contract.canonical_locator_key(unsorted),
                         contract.canonical_locator_key(locator))

    def test_present_malformed_locators_rejected(self):
        for bad in (
            "not-a-mapping",           # non-mapping
            ["root", "plans"],         # non-mapping (list)
            {},                        # empty mapping
            {"path": "a/b.md"},        # missing root
            {"root": "plans"},         # missing path
            {"root": "", "path": "a/b.md"},   # empty root
            {"root": "plans", "path": ""},    # empty path
        ):
            with self.assertRaises(contract.ContractError, msg=repr(bad)):
                contract.canonical_locator_key(bad)

    def test_locator_non_serializable_value_rejected(self):
        # A locator value json.dumps cannot serialize must surface as
        # ContractError, never a leaked TypeError.
        with self.assertRaises(contract.ContractError):
            contract.canonical_locator_key(
                {"root": "plans", "path": "a/b", "weird": object()}
            )

    def test_sensitive_matches_raw_key_and_canary(self):
        payload = "prefix sk-abcdef0123456789ABCDEF more canary-key-DO-NOT-LEAK-0123456789 end"
        matches = contract.sensitive_matches(payload)
        self.assertEqual(
            matches,
            [
                {"pattern": "raw-key", "offset": 7},
                {"pattern": "canary", "offset": 38},
            ],
        )
        dumped = repr(matches)
        self.assertNotIn("sk-abcdef0123456789ABCDEF", dumped)
        self.assertNotIn("canary-key-DO-NOT-LEAK-0123456789", dumped)

    def test_sensitive_matches_bytes_offsets(self):
        data = b"x" * 3 + b"sk-" + b"a" * 16
        matches = contract.sensitive_matches(data)
        self.assertEqual(matches, [{"pattern": "raw-key", "offset": 3}])

    def test_sensitive_matches_multibyte_prefix_char_offsets(self):
        # "é" encodes as two UTF-8 bytes, so the byte offset of the match is
        # 7 while the reported character offset must be 6.
        payload = "héllo sk-" + "a" * 16
        self.assertEqual(
            contract.sensitive_matches(payload),
            [{"pattern": "raw-key", "offset": 6}],
        )

    def test_sensitive_matches_below_threshold_is_clean(self):
        self.assertEqual(contract.sensitive_matches("sk-" + "a" * 15), [])
        self.assertEqual(contract.sensitive_matches(b"nothing to see"), [])

    def test_contract_error_is_the_sole_validation_exception(self):
        with self.assertRaises(contract.ContractError):
            paths.RootedPath.parse("bad token")
        with self.assertRaises(contract.ContractError):
            contract.validate_catalog({"nope": 1})

    def test_adapter_context_and_result_are_frozen(self):
        ctx = contract.AdapterContext(roots={"worktree": Path(".")}, catalog={})
        with self.assertRaises(dataclasses.FrozenInstanceError):
            ctx.roots = {}
        with self.assertRaises(dataclasses.FrozenInstanceError):
            ctx.catalog = {}
        result = contract.AdapterResult()
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.entities = ({"id": "x"},)
        with self.assertRaises(dataclasses.FrozenInstanceError):
            result.findings = ({"id": "y"},)

    def test_evidence_index_duplicate_entity_ids_rejected(self):
        doc = _evidence_index_fixture()
        doc["entities"].append({"id": "rule:H-1", "kind": "rule"})
        with self.assertRaises(contract.ContractError):
            contract.validate_evidence_index(doc)

    def test_evidence_index_cross_collection_id_collision_rejected(self):
        doc = _evidence_index_fixture()
        doc["runs"].append({"id": "rule:H-1", "component": "redteam"})
        with self.assertRaises(contract.ContractError):
            contract.validate_evidence_index(doc)

    def test_evidence_index_duplicate_inventory_triple_rejected(self):
        # (scope, root, path) must be unique. The schema's uniqueItems covers
        # whole-object duplicates only, so a record that differs in another
        # field is a stdlib-only semantic check (same documented gap as the
        # duplicate-id fixtures).
        doc = _evidence_index_fixture()
        dup = copy.deepcopy(doc["inventory"][0])
        dup["sha256"] = "1" + "0" * 63  # distinct object: not a whole dup
        doc["inventory"].append(dup)
        with self.assertRaises(contract.ContractError):
            contract.validate_evidence_index(doc)

    def test_canonical_imports_create_no_top_level_sibling_aliases(self):
        for name in ("contract", "paths", "adapters", "smoke.eval_harness.contract",
                     "smoke.eval_harness.paths"):
            if name.split(".")[0] in ("contract", "paths", "adapters"):
                self.assertNotIn(name, sys.modules, name)
        self.assertIn("smoke.eval_harness.contract", sys.modules)
        self.assertIn("smoke.eval_harness.paths", sys.modules)


# --- Dual validation --------------------------------------------------------
# The two JSON Schemas are normative; contract.py implements the same used
# subset without jsonschema. Wherever a constraint is expressible in the
# schema, both validators must decide identically. Cross-item key uniqueness
# (duplicate component/root/scope ids with distinct row bodies) is a design
# rule JSON Schema cannot express; those fixtures are asserted against the
# standard-library validator only, and the schema's (documented, accepting)
# decision is recorded below so the gap stays visible.

def _catalog_schema():
    return contract.load_schema(CATALOG_SCHEMA_NAME)


def _index_schema():
    return contract.load_schema(INDEX_SCHEMA_NAME)


def _stdlib_decision(value, schema, validator):
    try:
        validator(value)
        return True
    except contract.ContractError:
        return False


def _jsonschema_decision(value, schema):
    try:
        jsonschema.validate(value, schema)
        return True
    except jsonschema.ValidationError:
        return False


def _hex(n=64):
    return "0" * (n - 1) + "1"


def _evidence_index_fixture():
    return {
        "schema_version": 1,
        "snapshot": {
            "catalog_sha256": _hex(),
            "content_sha256": _hex(64).replace("0" * 63 + "1", "1" + "0" * 63),
            "scopes": [
                {"id": "plans-corpus", "root": "plans",
                 "inventory_sha256": _hex(), "entries": 3},
                {"id": "plans-declared-inputs", "root": "plans",
                 "inventory_sha256": _hex(), "entries": 2},
                {"id": "worktree-declared-inputs", "root": "worktree",
                 "inventory_sha256": _hex(), "entries": 9},
            ],
        },
        "inventory": [
            {
                "scope": "plans-corpus", "root": "plans",
                "path": "parity-formalism/FORMALISM.md",
                "file_type": "regular", "role": "curated-document",
                "media_type": "text/markdown", "bytes": 1234,
                "sha256": _hex(), "git_state": "tracked-modified",
                "generation_state": "source-authored",
                "content_policy": "curated-text",
            },
            {
                "scope": "plans-corpus", "root": "plans",
                "path": "provenance/ledger.md",
                "file_type": "symlink", "role": "other",
                "media_type": "inode/symlink", "bytes": 12,
                "sha256": _hex(), "git_state": "untracked",
                "generation_state": "unknown",
                "content_policy": "metadata-only",
                "symlink_target": "../x/ledger.md",
            },
        ],
        "components": [{"id": "redteam"}],
        "entities": [
            {"id": "component:redteam", "kind": "component"},
            {"id": "rule:H-1", "kind": "rule", "class": "AzStrict"},
        ],
        "relationships": [
            {"id": "checks:rule:H-1", "kind": "checks",
             "source": "rule:H-1", "target": "component:redteam"},
        ],
        "runs": [],
        "findings": [
            {"id": "finding:redteam:owner-doc-heading-missing",
             "component": "redteam", "code": "owner-doc-heading-missing",
             "level": "warning", "impact": "soft"},
        ],
    }


@unittest.skipIf(jsonschema is None, "jsonschema not importable")
class DualValidatorTest(unittest.TestCase):
    """Standard-library validator and jsonschema agree on every schema-level
    valid/invalid fixture for both normative schemas."""

    def _catalog_fixtures(self):
        good = load_catalog()
        fixtures = [("complete catalog", good, True)]

        def mutate(fn, label, expected=True):
            m = copy.deepcopy(good)
            fn(m)
            fixtures.append((label, m, expected))

        mutate(lambda c: c.update({"extra_top": 1}), "unknown top-level key", False)
        mutate(lambda c: c.update(schema_version=2), "schema_version 2", False)
        def drop_roots(c): c.pop("roots")
        mutate(drop_roots, "missing roots", False)
        mutate(lambda c: c["roots"][0].update(id="Worktree"), "bad root id", False)
        mutate(lambda c: c["roots"][0].update(required_for=["deploy"]),
               "bad required_for", False)
        mutate(lambda c: c["roots"][0].update(extra=1), "unknown root key", False)
        mutate(lambda c: c["inventory_scopes"][0].update(selection="everything"),
               "bad selection", False)
        def bad_include(c): c["inventory_scopes"][2]["includes"][1] = "/abs.json"
        mutate(bad_include, "absolute scope include", False)
        def bad_exclude(c): c["inventory_scopes"][2]["excludes"][0] = "a\\b"
        mutate(bad_exclude, "backslash scope exclude", False)
        def bad_component(c): c["components"][0].update(adapter="nope")
        mutate(bad_component, "bad adapter", False)
        def drop_component_key(c): c["components"][0].pop("install")
        mutate(drop_component_key, "missing component key", False)
        def extra_component_key(c): c["components"][0]["extra"] = 1
        mutate(extra_component_key, "unknown component key", False)
        def bad_role(c): c["components"][4]["entrypoints"][0]["role"] = "run"
        mutate(bad_role, "bad entrypoint role", False)
        def selftest_no_argv(c):
            for ep in c["components"][1]["entrypoints"]:
                if ep["role"] == "selftest":
                    ep.pop("argv")
        mutate(selftest_no_argv, "selftest without argv", False)
        def non_null_install(c): c["components"][0]["install"] = {"path": "x"}
        mutate(non_null_install, "non-null install", False)
        def bad_owner_heading(c): c["components"][4]["owner_docs"][0]["heading"] = ""
        mutate(bad_owner_heading, "empty owner heading", False)
        def abs_output(c): c["components"][4]["outputs"][0] = "/abs/*.json"
        mutate(abs_output, "absolute output glob", False)
        def rooted_output(c): c["components"][4]["outputs"][0] = "worktree:x/*.json"
        mutate(rooted_output, "rooted output glob", False)
        def dotdot_input(c): c["components"][1]["inputs"][0] = "a/../b"
        mutate(dotdot_input, "dotdot input", False)
        def unknown_rooted_input(c): c["components"][1]["inputs"][0] = "nosuchroot:a/b"
        mutate(unknown_rooted_input, "undeclared root in input", False)
        def bad_lifecycle(c): c["components"][5]["lifecycle"] = {"registry": None}
        mutate(bad_lifecycle, "null registry without reason", False)
        def lifecycle_both(c):
            c["components"][5]["lifecycle"] = {"registry": "smoke/runs.jsonl",
                                               "suite": "wstream", "reason": "x"}
        mutate(lifecycle_both, "registry with reason", False)
        def bad_selector(c): c["curated_sources"][0]["selectors"][0]["kind"] = "anchor"
        mutate(bad_selector, "bad selector kind", False)
        mutate(lambda c: c["curated_sources"].append(
            {"root": "plans", "path": "parity-formalism/Q-A.md",
             "selectors": [{"kind": "question", "marker": "no q prefix"}]}),
            "question marker without Q prefix", False)
        return fixtures

    def test_catalog_dual_decisions_identical(self):
        # Every schema-level fixture must decide identically under both
        # validators. Cross-item key-uniqueness fixtures (duplicate ids with
        # distinct bodies) are asserted in SchemaStrictnessTest against the
        # standard-library validator only: JSON Schema 2020-12 cannot express
        # "unique key across array items", so those cases are, by construction,
        # outside the normative schema's expressible subset.
        schema = _catalog_schema()
        for label, value, expected in self._catalog_fixtures():
            stdlib = _stdlib_decision(value, schema, contract.validate_catalog)
            self.assertEqual(stdlib, expected, f"stdlib decision on {label!r}")
            self.assertEqual(_jsonschema_decision(value, schema), stdlib, label)

    def _index_fixtures(self):
        good = _evidence_index_fixture()
        fixtures = [("complete index", good, True)]

        def mutate(fn, label, expected=False):
            m = copy.deepcopy(good)
            fn(m)
            fixtures.append((label, m, expected))

        mutate(lambda d: d.pop("findings"), "missing findings")
        mutate(lambda d: d.update(extra=1), "unknown top-level key")
        mutate(lambda d: d.update(schema_version=2), "schema_version 2")
        mutate(lambda d: d["snapshot"].update(catalog_sha256="Z" * 64),
               "bad sha256 charset")
        mutate(lambda d: d["snapshot"]["scopes"][0].update(entries=-1),
               "negative entries")
        mutate(lambda d: d["inventory"][0].update(role="bogus"),
               "bad inventory role")
        mutate(lambda d: d["inventory"][0].update(media_type="text/x-unknown"),
               "bad media type")
        mutate(lambda d: d["inventory"][0].update(git_state="deleted"),
               "bad git state")
        mutate(lambda d: d["inventory"][1].pop("symlink_target"),
               "symlink record without symlink_target")
        mutate(lambda d: d["inventory"][0].update(symlink_target="x"),
               "regular record with symlink_target")
        mutate(lambda d: d["inventory"][1].update(media_type="text/plain"),
               "symlink record with non-inode media type")
        mutate(lambda d: d["inventory"][0].update(media_type="inode/symlink"),
               "regular record with inode/symlink media type")
        mutate(lambda d: d["inventory"][1].update(symlink_target="/abs/target"),
               "absolute symlink target")
        mutate(lambda d: d["inventory"][0].update(path="../escape.md"),
               "escaping inventory path")
        mutate(lambda d: d["inventory"][0].update(
            path="provenance/fixtures/xreplay76/mint/report/20260918T005237Z/"
                 "home/sessions/%2FUsers%2Fpalanisd/summary.json"),
            "percent-escaped path segment validates", expected=True)
        mutate(lambda d: d["inventory"][0].update(path="a/%2Fb/../c"),
               "percent segment does not loosen dotdot rejection")
        mutate(lambda d: d["inventory"][0].update(scope="worktree-declared-inputs"),
               "worktree scope with plans root (pairing mismatch)")
        mutate(lambda d: d["entities"][0].update(kind="mystery"),
               "bad entity kind")
        mutate(lambda d: d["findings"][0].update(impact="fatal"),
               "bad finding impact")
        mutate(lambda d: d["components"][0].update(id="bogus"),
               "bad component id in index")
        return fixtures

    def test_evidence_index_dual_decisions_identical(self):
        schema = _index_schema()
        for label, value, expected in self._index_fixtures():
            stdlib = _stdlib_decision(value, schema, contract.validate_evidence_index)
            self.assertEqual(stdlib, expected, f"stdlib decision on {label!r}")
            self.assertEqual(_jsonschema_decision(value, schema), stdlib, label)
