#!/usr/bin/env python3
"""CATALOG GATE unit tests (apex-071 CATALOG-BAKE-1), TDD red-first.

v2 — the operator-adjudicated 3-delta (2026-09-19) reshaped the contract:

  D1: the merged (generated + overlay) catalog bakes IN PLACE into
      `default_models.json` (upstream shape: four role pins + a `models`
      array sorted by id). The separate `default_catalog.json` artifact
      is gone. Raw C-class fields (mode/costs/providers/hint menus) stay
      in `catalog_generated.json` only; merged rows carry the runtime
      mapping (context_window / max_completion_tokens) plus the curated
      O/H fields (overlay wins on collision).
  D2: the bundled seed path consumes enriched crate rows (rust-side); the
      python side pins the row shape that implies: id/model + caps +
      curated fields on every merged row.
  D3: FAIL-CLOSED gate. REQUIRED_FIELDS must be present on EVERY
      overlay entry, and every baking model (the 76 generated + the
      overlay `bake` list) needs a complete overlay entry — otherwise
      the gate exits 2 with an explicit missing list (the flag-only era
      is over: never a silent bless). Role pins ride the overlay and
      are written to the merged file.
  v3 — CATALOG-REQUIRED-CURATION-1 (apex-kb6, operator ruling 2026-09-19):
      the overlay is the single curation home — EVERY legal field is
      REQUIRED (the 15-key full-row contract: 14 non-null keys + the
      null-allowed cache_ttl). The gate enforces nullability / type /
      menu consistency / the api_backend wire cross-check fail-closed.
      Tests h/i/j pin the contract; the b/e/f/g fixtures ride full
      15-key curation and test_b tracks gate.REQUIRED_FIELDS
      dynamically.
  v4 — ZC-SUBSET-DRIFT-1 (apex-ayl.128, adjudicated 2026-09-22): the
      full-coverage contract inverts to SUBSET DRIFT. Every on-proxy
      model is in {curated, skipped, alerted}; an unlisted model is NO
      longer a hard fail — it classifies new_unlisted (loud stderr
      alert (F6, post dual review) + catalog_drift_report.json entry +
      paste-ready scaffold).
      Top-level overlay `skip` list: entries {model, reason} = known +
      intentionally excluded; silences the alert. Off-proxy state is
      classified too: removed_but_curated (overlay row, model not on
      the proxy — bake-listed seed carriers excluded, they are
      deliberate and classify bake_listed_off_proxy) and skip_stale
      (skip entry, model not on the proxy). The merge row set is
      UNCHANGED (generated + bake list) at v4 — an unlisted on-proxy
      model rides the merged catalog with id/model + the cap mapping
      only; the drift report tracks the curation gap. (v5 F1 then
      excludes unlisted/skipped models from the bake — see below.)
      STILL HARD FAIL: the
      15-key completeness of every curated (overlay) row + every
      bake-listed model, credential fields, C-class caps on on-proxy
      curated rows, dangling role pins, schema + wire cross-checks.
      EFFORT_VALUES derives at run time from the schema's
      ReasoningEffort enum (the hardcoded copy is dead); REQUIRED_FIELDS
      stays the documented curation contract but is cross-checked
      against ConfigModelOverride.properties at run start (a schema
      rename/removal fails the gate). The effort advisory (digest live
      hint vs curated menu, per row) is a drift-report entry, never a
      fail. The drift report bakes next to the artifact and is written
      on successful bakes only. Tests b/d were re-pinned for v4; k-n
      pin the new contract.
  v5 — fix pass (post dual review, coordinator-adjudicated F1-F8):
      F1: bake row set = CURATED rows (overlay rows on the proxy) ∪
          the explicit `bake` list — unlisted (new_unlisted) and
          skip-listed models are EXCLUDED from default_models.json
          (no bare riding); removed_but_curated rows are excluded
          unless force-baked via the bake list. On the current overlay
          this is byte-identical to the pre-F1 output (pinned in
          test_d). F2: closed overlay key set — (ConfigModelOverride
          schema properties − credential fields) ∪ {id, model,
          context_window, max_completion_tokens}; any other key (e.g.
          a renamed credential) exits 2 before the artifact is
          written. F3: skip_stale entries warn on stderr (naming each
          stale entry). F4: the drift report inputs section surfaces
          generated.generated_at + digest.captured_at; digest
          freshness cross-checks (stale-vs-capture, predates-capture)
          are advisory stderr WARNs, never a fail. F5: a model both
          curated and skip-listed appears ONLY in curated[] (row
          wins; disjoint classes, no double count). F6: the DRIFT
          ALERT block moved to stderr — stdout is empty on every
          run. F7: unwritable --out / artifact write failure → clean
          exit 5 (documented alongside 0/2/3/4).
  v6 — ZC-SUBSET-CURATION-1 (apex-ayl.129, operator-approved 8-row
      zero-config menu, 2026-09-22): the committed overlay is the
      curated 8-row menu (gpt-5.6-sol/terra/luna, claude-opus-5/
      sonnet-5, grok-4.6, gemini-3.8-flash, gemini-3.1-pro-preview),
      `bake` is empty (grok-4.5 cut), and `skip` lists all 68
      on-proxy models outside the core with per-class reasons.
      test_d re-pinned: 8 merged rows, the F1 byte-identity invariant
      now cmps the temp re-bake against the freshly baked in-tree
      default_models.json (the old invariant pinned the 77-row .128
      pre-cut baseline against git HEAD — the intentional old→new
      invariant move), and the curation spot-checks ride the 8-row
      menu. The 2026-09-22 live recapture also moved grok-4.6's
      proxy-truth caps 500000 -> 524288 (re-pinned where asserted).
      Addendum (operator ruling 2026-09-22, mid-flight): all four
      role pins move grok-4.6 -> gpt-5.6-terra (the Vertex
      grok-4.6 deployment is broken; the grok-4.6 row stays in the
      menu for explicit /model use) — the role-pin assertion in
      test_d is re-pinned accordingly.
  v7 — CTXWIN-1M-1M twin rows (apex-ayl.136, 2026-09-23): a curated
      overlay row whose `model` field names a DIFFERENT on-proxy wire
      slug is a TWIN row (the 1M context-window variants): it bakes as
      id=<row key>, model=<wire slug> and inherits the base slug's
      generated caps; overlay C-class caps are forbidden on twins via
      the effective slug (exit 2); drift classification is per WIRE
      SLUG (a twin row key can never false-positive as
      removed_but_curated, and the base slug counts ONCE in curated);
      legacy (key == slug) inputs merge byte-identically. test_r pins
      the contract.

Run:  python3 scripts/catalog_merge_tests.py   (exit 0 = all pass)
"""
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPTS = os.path.dirname(os.path.abspath(__file__))
MODELS_DIR = os.path.join(ROOT, "crates", "codegen", "xai-grok-models")
SCHEMA_PATH = os.path.join(ROOT, "crates", "codegen", "xai-grok-shell",
                           "config.schema.json")

# The canonical ReasoningEffort enum (config.schema.json
# definitions.ReasoningEffort) — the pinned expectation. The gate
# DERIVES this at run time (v4, test_l); the literal here is the test's
# fallback so the module loads against a pre-v4 gate (RED phase).
CANONICAL_EFFORTS = ["none", "minimal", "low", "medium", "high", "xhigh",
                     "max", "ultra"]


def load_gate():
    spec = importlib.util.spec_from_file_location("catalog_gate", os.path.join(SCRIPTS, "catalog_gate.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gate = load_gate()

try:
    EFFORTS = gate.derive_effort_values(json.load(open(SCHEMA_PATH)))
except (AttributeError, KeyError, TypeError, ValueError):
    EFFORTS = list(CANONICAL_EFFORTS)

PASS = 0


def check(name, cond, detail=""):
    global PASS
    if not cond:
        sys.exit(f"FAIL: {name} {detail}")
    PASS += 1
    print(f"  ok: {name}")


def run_gate(gen, ov, out, schema=None, digest=None, drift=None):
    proc = subprocess.run(
        [sys.executable, os.path.join(SCRIPTS, "catalog_gate.py"),
         "--generated", gen, "--overlay", ov, "--out", out,
         "--drift-report", drift or out + ".drift.json"]
        + (["--schema", schema] if schema else [])
        + (["--digest", digest] if digest else []),
        capture_output=True, text=True)
    return proc


def menu_objs(*values, default=None):
    """Full menu objects (the schema requires id/value/label/default)."""
    return [
        {"id": v, "value": v, "label": v[:1].upper() + v[1:], "default": v == default}
        for v in values
    ]


def full_entry(mid, wire, family, menu, **overrides):
    """A complete 15-key curation entry under the kb6 required contract.
    menu: full objects (menu_objs) or bare canonical strings; an empty
    list is the explicit non-reasoning curation. reasoning_effort and
    supports_reasoning_effort derive from the default marker (null /
    False for an empty menu); overrides replace any key for the
    negative tests."""
    values = []
    normalized = []
    for item in menu or []:
        if isinstance(item, dict):
            normalized.append(item)
            if item.get("default"):
                values.append(item.get("value"))
        else:
            normalized.append({"id": item, "value": item,
                               "label": item[:1].upper() + item[1:],
                               "default": False})
    entry = {
        "api_backend": wire,
        "model_family": family,
        "name": mid,
        "reasoning_efforts": normalized,
        "supports_reasoning_effort": bool(normalized),
        "reasoning_effort": values[0] if values else None,
        "cache_ttl": None,
        "multi_agent_v2": False,
        "supports_backend_search": False,
        "strict_responses_input": False,
        "extra_headers": {},
        "auto_compact_threshold_percent": 80,
        "compaction_at_tokens": True,
        "compactions_remaining": 1,
        "system_prompt_label": mid,
    }
    entry.update(overrides)
    return entry


def test_a_merge_precedence_and_row_shape():
    """(a) overlay wins over generated; D1 row shape (no raw C fields)."""
    print("a) merge precedence + D1 row shape")
    generated = {
        "m-ov": {
            "id": "m-ov", "mode": "chat", "max_input_tokens": 100000,
            "max_output_tokens": 16384, "providers": ["azure"],
            "input_cost_per_token": 1e-06, "output_cost_per_token": 2e-06,
            "supported_reasoning_efforts": ["low", "high"],
            # O/H-class field present on the generated side: the overlay
            # must win when it disagrees.
            "api_backend": "chat_completions",
        },
        "m-gen": {"id": "m-gen", "mode": "chat", "max_input_tokens": 272000},
    }
    overlay = {
        "m-ov": {"api_backend": "responses", "model_family": "codex",
                 "strict_responses_input": True,
                 "reasoning_efforts": ["low", "high"]},
        "m-gen": {"api_backend": "chat_completions", "model_family": "codex"},
    }
    models, warns = gate.merge_rows(generated, overlay, [])
    row = models["m-ov"]
    check("overlay api_backend wins over generated", row["api_backend"] == "responses")
    check("overlay strict flag present", row["strict_responses_input"] is True)
    check("overlay menu normalized to full objects",
          row["reasoning_efforts"] == [
              {"id": "low", "value": "low", "label": "Low", "default": False},
              {"id": "high", "value": "high", "label": "High", "default": False}])
    check("runtime mapping: context_window from max_input", row["context_window"] == 100000)
    check("runtime mapping: max_completion_tokens from max_output",
          row["max_completion_tokens"] == 16384)
    # D1: raw C-class fields stay in catalog_generated.json only.
    for f in ("mode", "providers", "input_cost_per_token",
              "output_cost_per_token", "supported_reasoning_efforts",
              "max_input_tokens", "max_output_tokens"):
        check(f"merged row drops raw C field {f}", f not in row)
    # C-class (CATALOG-CCLASS-SEED-1, operator ruling + 2026-09-19
    # correction): for a model that IS in the generated catalog (on the
    # proxy) the generated caps are the truth — an overlay
    # context_window / max_completion_tokens never beats them (the 071
    # sol 353000 leak is gone). Overlay caps ride only for overlay-only
    # models (no generated truth): the overlay is the row's sole source.
    models, _ = gate.merge_rows(
        {"m": {"id": "m", "max_input_tokens": 900000, "max_output_tokens": 4096}},
        {"m": {"api_backend": "responses", "model_family": "codex",
               "context_window": 353000, "max_completion_tokens": 99999}}, [])
    check("generated context_window beats overlay (C-class: proxy truth)",
          models["m"]["context_window"] == 900000)
    check("generated max_completion_tokens beats overlay (C-class)",
          models["m"]["max_completion_tokens"] == 4096)
    # Overlay-only model (not in the generated catalog): the overlay caps
    # are the row's sole source and ride unchanged (grok-4.5 pattern).
    models, _ = gate.merge_rows(
        {}, {"m-seed": {"api_backend": "responses", "model_family": "xai",
                        "context_window": 500000}}, ["m-seed"])
    check("overlay-only caps ride when there is no generated truth",
          models["m-seed"]["context_window"] == 500000)
    # Between-build overlay entry (not generated, not bake-listed): warn only.
    _, warns = gate.merge_rows(
        {}, {"m-x": {"api_backend": "messages", "model_family": "anthropic"}}, [])
    check("between-build overlay addition warns", any("m-x" in w for w in warns))
    # F1 (product ruling, post dual review): an on-proxy model with NO
    # overlay entry and no skip entry (unlisted) is EXCLUDED from the
    # bake row set — no bare riding. The curation gap is drift (alert +
    # report + scaffold), not a crash and not an artifact row.
    models, _ = gate.merge_rows(
        {"m-bare": {"id": "m-bare", "max_input_tokens": 123456,
                    "max_output_tokens": 2048}}, {}, [])
    check("F1: unlisted on-proxy model is EXCLUDED from the bake row set (no bare riding)",
          models == {}, json.dumps(models))
    # The bake row set is CURATED rows (overlay rows on the proxy) ∪ the
    # explicit bake list: the curated on-proxy model rides, the unlisted
    # sibling does not.
    models, _ = gate.merge_rows(
        {"m1": {"id": "m1", "max_input_tokens": 100},
         "m2": {"id": "m2"}},
        {"m1": {"api_backend": "responses"}}, [])
    check("F1: row set = curated ∪ bake list (curated rides, unlisted excluded)",
          sorted(models) == ["m1"], json.dumps(sorted(models)))
    # A bake-listed model is force-baked even off-proxy (the existing
    # mechanism, unchanged).
    models, _ = gate.merge_rows(
        {"m1": {"id": "m1"}},
        {"m1": {"api_backend": "responses"},
         "m-seed": {"api_backend": "responses"}},
        ["m-seed"])
    check("F1: bake-listed model is force-baked (existing mechanism, unchanged)",
          sorted(models) == ["m-seed", "m1"], json.dumps(sorted(models)))


def test_b_subset_drift_coverage():
    """(b) v4 SUBSET DRIFT (inverts the D3 full-coverage contract): an
    on-proxy model WITHOUT an overlay entry is NO longer a coverage
    failure — it classifies new_unlisted (loud alert, no hard fail,
    scaffold in the drift report). Completeness stays enforced on EVERY
    overlay entry and on EVERY bake-listed model (it must ride the
    merged catalog with a full row). The expected lists track
    gate.REQUIRED_FIELDS dynamically."""
    print("b) v4 subset drift coverage")
    gen = {"m1": {"id": "m1"}, "m2": {"id": "m2"}}
    # end-to-end FIRST (clean contract failure pre-implementation): the
    # unlisted on-proxy model m2 must NOT fail the gate.
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": gen}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex",
                                               menu_objs("low", "high",
                                                         default="high"))}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("unlisted on-proxy model is NOT a hard fail (exit 0)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        check("F6: DRIFT ALERT names the unlisted model on stderr",
              "DRIFT ALERT" in proc.stderr and "m2" in proc.stderr,
              proc.stderr)
        check("F6: gate stdout is empty (all diagnostics on stderr)",
              proc.stdout == "", repr(proc.stdout))
        check("bake artifact written despite unlisted model",
              os.path.exists(outp))
        drift_p = outp + ".drift.json"
        check("drift report written on the bake", os.path.exists(drift_p))
        report = json.load(open(drift_p))
        cls = report["classification"]
        check("drift report: m2 classifies new_unlisted, m1 curated",
              cls["new_unlisted"] == ["m2"] and cls["curated"] == ["m1"],
              json.dumps(cls))
        check("drift report: scaffold emitted for the unlisted model",
              [s["model"] for s in report["scaffolds"]] == ["m2"],
              json.dumps(report["scaffolds"])[:300])
        row = {r["id"]: r for r in json.load(open(outp))["models"]}
        check("F1: unlisted row is EXCLUDED from the bake artifact (no bare riding)",
              "m2" not in row and sorted(row) == ["m1"], json.dumps(row))
    # unit: collect_missing no longer takes the generated catalog — the
    # coverage question is gone; completeness is per overlay entry + per
    # bake-listed model.
    missing = gate.collect_missing(
        {"m1": full_entry("m1", "responses", "codex",
                          menu_objs("low", "high", default="high"))},
        [], EFFORTS)
    check("unit: on-proxy model without an entry is not a coverage failure",
          missing == [], str(missing))
    missing = gate.collect_missing(
        {"m1": {"model_family": "codex"}, "m2": {"api_backend": "responses"}},
        [], EFFORTS)
    check("unit: incomplete overlay entries still fail (per model, per field)",
          ("m1", [f for f in gate.REQUIRED_FIELDS if f != "model_family"]) in missing
          and ("m2", [f for f in gate.REQUIRED_FIELDS if f != "api_backend"]) in missing,
          str(missing))
    missing = gate.collect_missing({}, ["grok-x"], EFFORTS)
    check("unit: bake-list model without an entry is still a coverage failure",
          any(mid == "grok-x" and fields == list(gate.REQUIRED_FIELDS)
              for mid, fields in missing), str(missing))
    # end-to-end: an INCOMPLETE curated row still fails closed.
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 100}}}, open(genp, "w"))
        json.dump({"models": {"m1": {"model_family": "codex"}}}, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 2 on an incomplete curated row", proc.returncode == 2,
              f"rc={proc.returncode} err={proc.stderr}")
        check("gate names the failing model", "m1" in proc.stderr)
        check("gate names a missing field",
              any("api_backend" in l for l in proc.stderr.splitlines()
                  if l.strip().startswith("missing:")), proc.stderr)
        check("gate writes no artifact on failure", not os.path.exists(outp))


def test_c_role_pins():
    """(c) role pins ride the overlay; dangling pins fail closed."""
    print("c) role pins")
    ok, bad = gate.check_role_pins(
        {"default": "m1", "web_search": "m1"}, {"m1"})
    check("pins present and referenced", ok and not bad)
    ok, bad = gate.check_role_pins({"default": "ghost"}, {"m1"})
    check("pin referencing a missing model fails",
          (not ok) and any("default" in b for b in bad), str(bad))
    ok, bad = gate.check_role_pins({}, {"m1"})
    check("overlay must carry the 'default' pin", not ok, str(bad))
    pins = gate.extract_role_pins(
        {"default": "m1", "web_search": "m1", "image_description": "m1"})
    check("pins keep the canonical order",
          list(pins) == ["default", "web_search", "image_description"])


def test_d_committed_artifacts():
    """(d) committed artifacts: D1 shape, v4 subset contract, seed
    migration invariants (operator curation, byte-for-value), and the
    drift report committed with the bake."""
    print("d) committed artifacts")
    gen = json.load(open(os.path.join(MODELS_DIR, "catalog_generated.json")))
    ov = json.load(open(os.path.join(MODELS_DIR, "catalog_overlay.json")))
    merged = json.load(open(os.path.join(MODELS_DIR, "default_models.json")))
    check("generated: 76 models",
          gen["model_count"] == 76 and len(gen["models"]) == 76)
    # D1 upstream shape: four role pins + a models array.
    check("merged carries the four role pins",
          all(merged.get(p) == "gpt-5.6-terra" for p in
              ("default", "web_search", "image_description", "session_summary")))
    rows = merged.get("models")
    check("merged models is an array", isinstance(rows, list))
    by_id = {r["id"]: r for r in rows}
    # apex-ayl.136 (CTXWIN-1M-1M): the menu is now 8 base rows + 4
    # 1M-twin rows (claude-opus-5-1m / claude-sonnet-5-1m /
    # gpt-5.6-sol-1m / gpt-5.6-terra-1m) — 12 merged rows.
    check("12 merged rows (the 8-row curated menu, apex-ayl.129, + the "
          "four 1M twin rows, apex-ayl.136)",
          len(rows) == 12, f"got {len(rows)}")
    check("row set = bake_row_ids (F1 product ruling ∪ twin rows: the "
          "curated menu rides; unlisted/skipped on-proxy models do not)",
          set(by_id) == gate.bake_row_ids(gen["models"], ov["models"],
                                          ov.get("bake", [])))
    check("twin rows ride: id = row key, model = base wire slug",
          by_id["claude-opus-5-1m"]["model"] == "claude-opus-5"
          and by_id["claude-sonnet-5-1m"]["model"] == "claude-sonnet-5"
          and by_id["gpt-5.6-sol-1m"]["model"] == "gpt-5.6-sol"
          and by_id["gpt-5.6-terra-1m"]["model"] == "gpt-5.6-terra")
    check("twin rows carry the base slug's generated caps (C-class: the "
          "proxy's truth — claude 1000000, gpt-5.6 922000)",
          by_id["claude-opus-5-1m"]["context_window"] == 1000000
          and by_id["claude-sonnet-5-1m"]["context_window"] == 1000000
          and by_id["gpt-5.6-sol-1m"]["context_window"] == 922000
          and by_id["gpt-5.6-terra-1m"]["context_window"] == 922000
          and all(by_id[t]["max_completion_tokens"] == 128000
                  for t in ("claude-opus-5-1m", "claude-sonnet-5-1m",
                            "gpt-5.6-sol-1m", "gpt-5.6-terra-1m")))
    check("rows sorted by id", [r["id"] for r in rows] == sorted(by_id))
    check("gemma-4-31b absent from the menu (its overlay row was cut "
          "with the 8-row curation)",
          "gemma-4-31b" not in by_id)
    # v4: the committed overlay carries the top-level skip key (a list of
    # {model, reason}; populated by apex-ayl.129 — every on-proxy model
    # outside the 8-row curated menu, per-class reasons).
    check("overlay carries a top-level 'skip' list",
          isinstance(ov.get("skip"), list), str(list(ov)))
    for e in ov["skip"]:
        check(f"skip entry {e.get('model')!r} has model + reason",
              isinstance(e, dict) and isinstance(e.get("model"), str) and e["model"]
              and isinstance(e.get("reason"), str) and e["reason"],
              json.dumps(e))
    check("v4: committed overlay passes the subset completeness contract",
          gate.collect_missing(ov["models"], ov.get("bake", []), EFFORTS) == [])
    check("no overlay entry for a generated (on-proxy) model carries a "
          "C-class cap (CATALOG-CCLASS-SEED-1)",
          gate.find_forbidden_caps(gen["models"], ov["models"]) == [])
    # Seed migration invariants (the pre-bake rows, byte-for-value).
    g46 = by_id["grok-4.6"]
    check("grok-4.6: overlay backend_search=false beats the seed's true",
          g46["supports_backend_search"] is False)
    check("grok-4.6: seed name/label survive the migration",
          g46["name"] == "Grok 4.6" and g46["system_prompt_label"] == "Grok 4.6")
    check("grok-4.6: caps (generated cw + generated max_output — the "
          "overlay no longer carries them, C-class; the 2026-09-22 "
          "recapture moved the proxy truth 500000 -> 524288)",
          g46["context_window"] == 524288 and g46["max_completion_tokens"] == 524288)
    # kb6 (A5' OPTION 1): the seed menu survives and gains ultra
    # immediately after the top tier (xhigh) — 5 items, high default.
    check("grok-4.6: seed menu survives (5 items incl. ultra, high default)",
          [m["value"] for m in g46["reasoning_efforts"]] == ["xhigh", "ultra", "high", "medium", "low"]
          and g46["reasoning_efforts"][2]["default"] is True)
    check("grok-4.5: cut from the zero-config menu (apex-ayl.129 — the "
          "overlay row and the bake-list entry are gone)",
          "grok-4.5" not in by_id
          and "grok-4.5" not in set(ov["models"]) | set(ov.get("bake", [])))
    sol = by_id["gpt-5.6-sol"]
    check("sol: cw 922000 from generated (C-class: the proxy's truth; the "
          "071 overlay 353000 leak is gone)",
          sol["context_window"] == 922000)
    check("sol: generated mct 128000 survives",
          sol["max_completion_tokens"] == 128000)
    check("sol: overlay menu wins (5 items, no xhigh)",
          [m["value"] for m in sol["reasoning_efforts"]] == ["low", "medium", "high", "max", "ultra"])
    check("sol: curated wire pins (ZC-EAST2-UNPIN-1 / apex-ayl.126.6: the "
          "x-litellm-tags 'East US 2' pin is gone — the row rides untagged)",
          sol["strict_responses_input"] is True and sol["multi_agent_v2"] is True
          and sol["supports_backend_search"] is False
          and sol["extra_headers"].get("x-litellm-tags") is None)
    check("sol: seed single-effort survives (low)", sol["reasoning_effort"] == "low")
    # Curation spot-checks (the D3 ruling table).
    check("claude pinned messages + anthropic + 1h cache",
          by_id["claude-sonnet-5"]["api_backend"] == "messages"
          and by_id["claude-sonnet-5"]["model_family"] == "anthropic"
          and by_id["claude-sonnet-5"]["cache_ttl"] == "1h")
    check("frontier gpt-5.6-terra: responses + codex",
          by_id["gpt-5.6-terra"]["api_backend"] == "responses"
          and by_id["gpt-5.6-terra"]["model_family"] == "codex")
    check("gemini 3.8-flash: responses + google",
          by_id["gemini-3.8-flash"]["api_backend"] == "responses"
          and by_id["gemini-3.8-flash"]["model_family"] == "google")
    check("xai frontier grok-4.6: responses + xai",
          by_id["grok-4.6"]["api_backend"] == "responses"
          and by_id["grok-4.6"]["model_family"] == "xai")
    check("menu rows carry generated C-class caps (the overlay is "
          "C-class-free; the embedding rows left the menu)",
          all(r.get("context_window") for r in rows)
          and all(r.get("max_completion_tokens") for r in rows)
          and "text-embedding-ada-002" not in by_id)
    check("no raw C fields ride any merged row",
          all(not (set(r) & {"mode", "providers", "input_cost_per_token",
                             "output_cost_per_token", "supported_reasoning_efforts"})
              for r in rows))
    # Determinism + committed artifact == merge of committed inputs.
    m1, w1 = gate.merge_rows(gen["models"], ov["models"], ov.get("bake", []))
    m2, w2 = gate.merge_rows(gen["models"], ov["models"], ov.get("bake", []))
    check("merge is deterministic", (m1, w1) == (m2, w2))
    check("committed artifact == merge of committed inputs",
          [by_id[r["id"]] for r in rows] == [m1[i] for i in sorted(m1)]
          and {k: v for k, v in merged.items() if k != "models"} == gate.extract_role_pins(ov))
    # F1 (CRITICAL INVARIANT — the product ruling): on the CURRENT
    # overlay the merge output must be deterministic and
    # BYTE-IDENTICAL to the freshly baked in-tree artifact (merged=12,
    # the 8-row curated zero-config menu + the four 1M twin rows,
    # apex-ayl.136; drift=curated:8/skipped:68/
    # unlisted:0/removed:0/skip_stale:0 — apex-ayl.129 curation cut,
    # 2026-09-22; the old invariant pinned the 77-row .128 pre-cut
    # baseline against git HEAD). Re-bake to a temp out and cmp
    # against the committed default_models.json.
    with tempfile.TemporaryDirectory() as td:
        base_out = os.path.join(td, "default_models.json")
        proc = run_gate(os.path.join(MODELS_DIR, "catalog_generated.json"),
                        os.path.join(MODELS_DIR, "catalog_overlay.json"),
                        base_out)
        check("F1 baseline invariant: in-place re-bake exits 0",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        last = proc.stderr.splitlines()[-1] if proc.stderr else ""
        check("F1 baseline invariant: the exact baseline OK line",
              last.startswith(
                  "OK merged=12 overlay_entries=12 schema_errors=0 warns=0 "
                  "drift=curated:8/skipped:68/unlisted:0/removed:0/"
                  "skip_stale:0 out="),
              last)
        with open(os.path.join(MODELS_DIR, "default_models.json"), "rb") as f:
            committed = f.read()
        with open(base_out, "rb") as f:
            baked = f.read()
        check("F1 baseline invariant: baked artifact BYTE-IDENTICAL to the "
              "committed default_models.json (cmp)",
              baked == committed,
              f"baked={len(baked)}B committed={len(committed)}B")
        check("F1 baseline invariant: merged row count is 12 (8 base + 4 "
              "twin rows, apex-ayl.136)",
              len(json.load(open(base_out))["models"]) == 12)
    # v4: the drift report bakes with every bake (committed alongside).
    drift_p = os.path.join(MODELS_DIR, "catalog_drift_report.json")
    check("drift report committed with the bake", os.path.isfile(drift_p))
    report = json.load(open(drift_p))
    check("drift report: schema_version + inputs + counts",
          report.get("schema_version") == 1 and "inputs" in report
          and "counts" in report, json.dumps(report)[:300])
    cls = report["classification"]
    check("drift report: classification recomputes from the committed inputs",
          # apex-ayl.136: per WIRE SLUG — a twin row counts its base
          # slug (effective_model), so the twin row keys never leak into
          # curated / removed_but_curated.
          cls["curated"] == sorted(
              set(gen["models"])
              & {gate.effective_model(ov["models"], m) for m in ov["models"]})
          and cls["removed_but_curated"] == sorted(
              {gate.effective_model(ov["models"], m) for m in ov["models"]}
              - set(gen["models"]) - set(ov.get("bake", [])))
          and cls["bake_listed_off_proxy"] == sorted(
              set(ov.get("bake", [])) - set(gen["models"])),
          json.dumps(cls)[:400])
    check("drift report: counts consistent with the classification",
          report["counts"]["curated"] == len(cls["curated"])
          and report["counts"]["new_unlisted"] == len(cls["new_unlisted"])
          and report["counts"]["removed_but_curated"]
          == len(cls["removed_but_curated"])
          and report["counts"]["on_proxy"] == len(gen["models"]))
    check("drift report: effort advisory source noted (path or skip reason)",
          report["effort_advisory"].get("status") in ("ok", "skipped")
          and (report["effort_advisory"].get("source") is not None
               or report["effort_advisory"].get("skip_reason")),
          json.dumps(report["effort_advisory"])[:300])
    check("drift report: zero unlisted at this cut -> no scaffolds",
          cls["new_unlisted"] == [] and report["scaffolds"] == [])
    # Redaction: no key material in any artifact.
    for name, doc in [("generated", gen), ("overlay", ov), ("merged", merged),
                      ("drift report", report)]:
        check(f"{name}: no key-like strings",
              not re.search(r"sk-[A-Za-z0-9]{20,}|xai-[a-z0-9]{24,}|"
                            r"ghp_[A-Za-z0-9]{16,}|AKIA[A-Z0-9]{12,}", json.dumps(doc)))


def test_e_schema_validation():
    """(e) the apex-hw0 model-row definition still catches broken rows."""
    print("e) schema validation")
    full_schema = json.load(open(SCHEMA_PATH))
    validator = gate.make_row_validator(full_schema)
    good = {"id": "m", "model": "m", "context_window": 1000,
            "api_backend": "responses", "model_family": "codex",
            "reasoning_efforts": [
                {"id": "high", "value": "high", "label": "High", "default": True}]}
    check("good row validates", list(validator.iter_errors(good)) == [])
    check("bad api_backend enum rejected",
          len(list(validator.iter_errors(dict(good, api_backend="telepathy")))) == 1)
    check("negative context_window rejected",
          list(validator.iter_errors(dict(good, context_window=-5))) != [])
    check("menu entry missing label/default rejected",
          list(validator.iter_errors(
              dict(good, reasoning_efforts=[{"id": "high", "value": "high"}]))) != [])
    check("max_completion_tokens above u32 rejected",
          list(validator.iter_errors(dict(good, max_completion_tokens=2**32))) != [])
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m": {"id": "m", "max_input_tokens": 100}}}, open(genp, "w"))
        # The 'default' pin is present so the run reaches schema
        # validation (the pin check runs first — it is orthogonal
        # here). The entry is a FULL kb6 curation so the gate's own
        # field checks pass; the wire cross-check skips the unknown
        # api_backend value (the schema enum is its home).
        json.dump({"default": "m",
                   "models": {"m": full_entry("m", "warp_drive", "codex", [])}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 2 on schema-broken row", proc.returncode == 2,
              f"rc={proc.returncode} err={proc.stderr}")
        check("gate writes no artifact on failure", not os.path.exists(outp))
        check("gate names the failing model", "m:" in proc.stderr)


def test_f_gate_output_contract():
    """(f) the gate bakes IN PLACE into default_models.json (D1) and the
    drift report next to the artifact (v4)."""
    print("f) gate output contract")
    check("gate default out is default_models.json",
          getattr(gate, "DEFAULT_OUT_NAME", None) == "default_models.json")
    check("gate default drift report name is catalog_drift_report.json",
          getattr(gate, "DRIFT_REPORT_NAME", None) == "catalog_drift_report.json")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 1000,
                                     "max_output_tokens": 32}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex",
                                               menu_objs("low", "high",
                                                         default="high"))}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 0 on complete curation", proc.returncode == 0, proc.stderr)
        art = json.load(open(outp))
        check("artifact carries the pins + models array",
              art.get("default") == "m1" and isinstance(art.get("models"), list))
        check("row = id/model + mapped caps + the full kb6 curation",
              art["models"][0] == {"id": "m1", "model": "m1",
                                   "context_window": 1000,
                                   "max_completion_tokens": 32,
                                   **full_entry("m1", "responses", "codex",
                                                menu_objs("low", "high",
                                                          default="high"))})
        drift_p = outp + ".drift.json"
        check("drift report written next to the artifact on success",
              os.path.exists(drift_p))
        report = json.load(open(drift_p))
        check("drift report: fully curated proxy -> zero drift classes",
              report["classification"]["new_unlisted"] == []
              and report["classification"]["removed_but_curated"] == []
              and report["classification"]["skip_stale"] == []
              and report["scaffolds"] == [],
              json.dumps(report["classification"]))


def test_g_forbidden_overlay_caps():
    """(g) CATALOG-CCLASS-SEED-1 (operator ruling, 2026-09-19 correction):
    rejection is SCOPED by generated-catalog membership — an overlay
    entry whose model IS in catalog_generated.json (on the proxy) may not
    carry context_window / max_completion_tokens (the generated capture is
    the truth; config.toml rows remain the runtime override); the gate
    FAILS with an explicit per-entry list and writes no artifact.
    Overlay-only models (not in the generated catalog) are PERMITTED —
    the overlay is the sole source of caps for those rows."""
    print("g) forbidden overlay caps (C-class, scoped by proxy membership)")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        # (a) proxy-model overlay entry with context_window -> FAIL.
        json.dump({"models": {"m-gen": {"id": "m-gen", "max_input_tokens": 922000}}},
                  open(genp, "w"))
        json.dump({"default": "m-gen",
                   "models": {"m-gen": {**full_entry("m-gen", "responses",
                                                     "codex", []),
                                        "context_window": 353000}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 2 on proxy-model overlay context_window",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("rejection names the model + field",
              "m-gen" in proc.stderr and "context_window" in proc.stderr,
              proc.stderr)
        check("gate writes no artifact on forbidden cap", not os.path.exists(outp))
        # (b) overlay-only model entry with context_window -> PASS.
        json.dump({"models": {"m-gen": {"id": "m-gen", "max_input_tokens": 922000}}},
                  open(genp, "w"))
        json.dump({"default": "m-gen", "bake": ["m-seed"],
                   "models": {"m-gen": full_entry("m-gen", "responses",
                                                  "codex", []),
                              "m-seed": {**full_entry("m-seed", "responses",
                                                      "xai", []),
                                         "context_window": 500000}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("overlay-only overlay context_window passes the gate",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        art = json.load(open(outp))
        seed = {r["id"]: r for r in art["models"]}["m-seed"]
    check("overlay-only row carries the overlay cap as sole source",
          seed["context_window"] == 500000)


def test_h_required_fields_and_nullability():
    """(h) kb6 full-row contract (CATALOG-REQUIRED-CURATION-1): every one
    of the 15 required keys — absent, or (for the 14 non-null keys)
    nulled — exits 2 and names the field; cache_ttl is the one
    null-allowed key (null -> exit 0, off-tier -> exit 2); type
    violations fail closed."""
    print("h) required fields + nullability (kb6 15-key contract)")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 1000,
                                     "max_output_tokens": 32}}}, open(genp, "w"))

        def run_overlay(entry):
            json.dump({"default": "m1", "models": {"m1": entry}}, open(ovp, "w"))
            return run_gate(genp, ovp, outp)

        def base():
            return full_entry("m1", "responses", "codex",
                              menu_objs("low", "high", default="high"))

        def named_line(proc, prefix, text):
            return any(l.strip().startswith(prefix) and text in l
                       for l in proc.stderr.splitlines())

        # (a) every required key removed individually -> exit 2, named.
        for key in gate.REQUIRED_FIELDS:
            entry = base()
            del entry[key]
            proc = run_overlay(entry)
            check(f"removing {key} exits 2", proc.returncode == 2,
                  f"rc={proc.returncode} err={proc.stderr}")
            check(f"removing {key} names the field",
                  named_line(proc, "missing:", key), proc.stderr)
            check(f"removing {key} writes no artifact", not os.path.exists(outp))
        # (b) every non-null key nulled -> exit 2 (cache_ttl is exempt).
        for key in gate.REQUIRED_FIELDS:
            if key == "cache_ttl":
                continue
            proc = run_overlay({**base(), key: None})
            check(f"nulling {key} exits 2", proc.returncode == 2,
                  f"rc={proc.returncode} err={proc.stderr}")
            check(f"nulling {key} names the field",
                  named_line(proc, "invalid:", key), proc.stderr)
        # (c) cache_ttl: the one null-allowed key.
        proc = run_overlay(base())
        check("cache_ttl null passes (the only null-allowed key)",
              proc.returncode == 0, proc.stderr)
        for bad in ("2h", ""):
            proc = run_overlay({**base(), "cache_ttl": bad})
            check(f"cache_ttl {bad!r} exits 2 (off-tier)",
                  proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
            check(f"cache_ttl {bad!r} names the field",
                  named_line(proc, "invalid:", "cache_ttl"), proc.stderr)
        # (d) type violations.
        proc = run_overlay({**base(), "auto_compact_threshold_percent": "80"})
        check("str auto_compact_threshold_percent exits 2",
              proc.returncode == 2, proc.stderr)
        check("percent violation names the field",
              named_line(proc, "invalid:", "auto_compact_threshold_percent"),
              proc.stderr)


def test_i_menu_effort_consistency():
    """(i) kb6 menu/effort consistency: a non-empty menu carries exactly
    one default marker and reasoning_effort == the marker's value; an
    empty menu carries no marker and a null reasoning_effort;
    supports_reasoning_effort == (menu non-empty); effort values stay in
    the ReasoningEffort enum (DERIVED from the schema at run time, v4)."""
    print("i) menu/effort consistency")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 1000,
                                     "max_output_tokens": 32}}}, open(genp, "w"))

        def run_overlay(entry):
            json.dump({"default": "m1", "models": {"m1": entry}}, open(ovp, "w"))
            return run_gate(genp, ovp, outp)

        proc = run_overlay(full_entry("m1", "responses", "codex", ["low", "medium"]))
        check("non-empty menu without a default marker exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("missing-marker problem named", "default marker" in proc.stderr,
              proc.stderr)
        proc = run_overlay(full_entry("m1", "responses", "codex",
                                      [{"id": "low", "value": "low",
                                        "label": "Low", "default": True},
                                       {"id": "high", "value": "high",
                                        "label": "High", "default": True}]))
        check("two default markers exit 2", proc.returncode == 2,
              f"rc={proc.returncode} err={proc.stderr}")
        proc = run_overlay(full_entry("m1", "responses", "codex",
                                      menu_objs("low", "high", default="high"),
                                      reasoning_effort="low"))
        check("reasoning_effort != the default marker exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("marker agreement named", "default marker" in proc.stderr, proc.stderr)
        proc = run_overlay(full_entry("m1", "responses", "codex", [],
                                      reasoning_effort="medium"))
        check("empty menu with a set reasoning_effort exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        proc = run_overlay(full_entry("m1", "responses", "codex", []))
        check("empty menu + null effort + supports=false passes",
              proc.returncode == 0, proc.stderr)
        proc = run_overlay(full_entry("m1", "responses", "codex",
                                      menu_objs("low", "high", default="high"),
                                      supports_reasoning_effort=False))
        check("non-empty menu with supports=false exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("supports agreement named", "supports_reasoning_effort" in proc.stderr,
              proc.stderr)
        proc = run_overlay(full_entry("m1", "responses", "codex",
                                      menu_objs("low", "turbo", default="low")))
        check("menu value outside the ReasoningEffort enum exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("off-enum value named", "turbo" in proc.stderr, proc.stderr)


def test_j_wire_cross_check():
    """(j) kb6 071 ruling: the curated api_backend must equal the
    EFFECTIVE wire per slug inference (the gate's Python mirror of
    catalog_wire.rs): grok* -> responses; openai slugs follow
    openai_responses_slug (o-series + gpt-4o + gpt-4.1* + gpt-5+ ->
    responses; legacy gpt-3.x/4.x -> chat_completions); claude* must be
    curated explicitly (chat_completions or the messages pin); Other
    slugs (qwen/glm/google) are exempt (evidence-based operator
    curation)."""
    print("j) api_backend wire cross-check (slug inference mirror)")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))

        def run_case(mid, wire, family):
            json.dump({"models": {mid: {"id": mid}}}, open(genp, "w"))
            json.dump({"default": mid,
                       "models": {mid: full_entry(mid, wire, family, [])}},
                      open(ovp, "w"))
            return run_gate(genp, ovp, outp)

        proc = run_case("gpt-5.1", "chat_completions", "codex")
        check("gpt-5.1 chat_completions exits 2 (gpt-5+ slug -> responses)",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("wire mismatch names the model", "gpt-5.1" in proc.stderr, proc.stderr)
        proc = run_case("grok-4.6", "chat_completions", "xai")
        check("grok-4.6 chat_completions exits 2 (grok* slug -> responses)",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        proc = run_case("gpt-4", "responses", "codex")
        check("gpt-4 responses exits 2 (legacy 4.x slug -> chat_completions)",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        proc = run_case("claude-sonnet-5", "messages", "anthropic")
        check("claude-sonnet-5 messages passes (the messages pin)",
              proc.returncode == 0, proc.stderr)
        proc = run_case("claude-sonnet-5", "responses", "anthropic")
        check("claude-sonnet-5 responses exits 2 (claude* -> cc or messages)",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        for mid, family in (("gemini-3.5-flash", "google"), ("glm-5.2", "glm"),
                            ("qwen3.8-27b", "qwen")):
            proc = run_case(mid, "responses", family)
            check(f"{mid} responses passes (Other-slug exemption)",
                  proc.returncode == 0, proc.stderr)


def test_k_skip_key_and_drift_classes():
    """(k) v4: the top-level overlay `skip` list ({model, reason})
    silences the new_unlisted alert; off-proxy state classifies
    removed_but_curated / skip_stale / bake_listed_off_proxy (bake-listed
    seed carriers are deliberate, not drift); a skip entry for a curated
    model is redundant (WARN, the row wins); bad skip shape exits 3. The
    bake row set is CURATED rows ∪ the explicit bake list (F1: skipped /
    unlisted / removed-but-curated models are excluded)."""
    print("k) v4 skip key + drift classes")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        gen = {"models": {"m1": {"id": "m1"}, "m2": {"id": "m2"},
                          "m3": {"id": "m3"}, "m4": {"id": "m4"}}}
        ov = {"default": "m1",
              "bake": ["m-seed"],
              "skip": [{"model": "m2", "reason": "test: intentionally excluded"},
                       {"model": "m-gone", "reason": "test: stale skip entry"},
                       {"model": "m4", "reason": "test: redundant (curated)"}],
              "models": {"m1": full_entry("m1", "responses", "codex", []),
                         "m4": full_entry("m4", "responses", "codex", []),
                         "m-ghost": full_entry("m-ghost", "responses", "codex", []),
                         "m-seed": full_entry("m-seed", "responses", "codex", [],
                                              context_window=500000)}}
        json.dump(gen, open(genp, "w"))
        json.dump(ov, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 0 with skip + drift present (no hard fail)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        report = json.load(open(outp + ".drift.json"))
        cls = report["classification"]
        check("skip silences the alert (m2 skipped, not unlisted)",
              [e["model"] for e in cls["skipped"]] == ["m2"]
              and "m2" not in cls["new_unlisted"], json.dumps(cls))
        check("F5: curated+skipped m4 appears ONLY in curated (row wins; classes disjoint)",
              "m4" in cls["curated"]
              and all(e["model"] != "m4" for e in cls["skipped"]),
              json.dumps(cls))
        check("m3 (neither curated nor skipped) classifies new_unlisted",
              cls["new_unlisted"] == ["m3"], json.dumps(cls))
        check("F6: DRIFT ALERT on stderr names only the unlisted model",
              "m3" in proc.stderr and "m2" not in proc.stderr
              and "DRIFT ALERT" in proc.stderr, proc.stderr)
        check("F6: gate stdout is empty on the skip/unlisted run too",
              proc.stdout == "", repr(proc.stdout))
        check("removed_but_curated: overlay row, model not on the proxy",
              cls["removed_but_curated"] == ["m-ghost"], json.dumps(cls))
        check("skip_stale: skip entry, model not on the proxy",
              [e["model"] for e in cls["skip_stale"]] == ["m-gone"],
              json.dumps(cls))
        check("bake-listed off-proxy seed carrier is NOT drift",
              cls["bake_listed_off_proxy"] == ["m-seed"]
              and "m-seed" not in cls["removed_but_curated"], json.dumps(cls))
        check("redundant skip (curated m4) warns on stderr",
              any("m4" in l and "redundant" in l for l in proc.stderr.splitlines()),
              proc.stderr)
        check("F3: skip_stale entry warns on stderr (names the stale model)",
              any("skip_stale" in l and "m-gone" in l
                  for l in proc.stderr.splitlines()),
              proc.stderr)
        check("between-build WARN for m-ghost still printed",
              any("m-ghost" in l and "WARN" in l for l in proc.stderr.splitlines()),
              proc.stderr)
        art = json.load(open(outp))
        by_id = {r["id"]: r for r in art["models"]}
        check("F1: bake row set = curated ∪ bake list (m2 skipped, m3 unlisted, m-ghost removed — all excluded)",
              set(by_id) == {"m1", "m4", "m-seed"}, str(sorted(by_id)))
        check("counts reflect the classification",
              report["counts"]["on_proxy"] == 4
              and report["counts"]["curated"] == 2
              # F5: m4 is curated AND skip-listed — it counts once
              # (curated); skipped is m2 only.
              and report["counts"]["skipped"] == 1
              and report["counts"]["new_unlisted"] == 1
              and report["counts"]["removed_but_curated"] == 1
              and report["counts"]["skip_stale"] == 1
              and report["counts"]["bake_listed_off_proxy"] == 1,
              json.dumps(report["counts"]))
    # bad skip shapes -> exit 3 (input bad shape)
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        base = {"default": "m1", "models": {"m1": full_entry("m1", "responses",
                                                             "codex", [])}}

        def run_skip(skip):
            json.dump({**base, "skip": skip}, open(ovp, "w"))
            return run_gate(genp, ovp, outp)

        for name, skip in [
            ("not a list", "nope"),
            ("entry not an object", ["nope"]),
            ("missing reason", [{"model": "m1"}]),
            ("empty model", [{"model": "", "reason": "r"}]),
            ("non-string reason", [{"model": "m1", "reason": 42}]),
            ("duplicate model", [{"model": "m1", "reason": "a"},
                                 {"model": "m1", "reason": "b"}]),
        ]:
            proc = run_skip(skip)
            check(f"bad skip ({name}) exits 3", proc.returncode == 3,
                  f"rc={proc.returncode} err={proc.stderr}")
            check(f"bad skip ({name}) writes no artifact", not os.path.exists(outp))
        # absent skip key is legal (backward compatible)
        json.dump(base, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("absent skip key is legal (backward compatible, exit 0)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")


def test_l_schema_derived_contract():
    """(l) v4 adjudicated derivation rule: EFFORT_VALUES derives at run
    time from config.schema.json definitions.ReasoningEffort.enum (the
    hardcoded copy is dead); REQUIRED_FIELDS stays the documented
    curation contract but the gate cross-checks it against
    ConfigModelOverride.properties at run start — a schema
    rename/removal FAILS the gate (exit 2) instead of passing silently.
    A schema missing the ReasoningEffort definition is bad shape
    (exit 3)."""
    print("l) v4 schema-derived EFFORT_VALUES + REQUIRED_FIELDS cross-check")
    schema = json.load(open(SCHEMA_PATH))
    # end-to-end FIRST: a schema that renamed away a REQUIRED_FIELDS
    # property must fail the gate (the validation clause).
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex", [])}},
                  open(ovp, "w"))
        renamed = json.loads(json.dumps(schema))
        del renamed["definitions"]["ConfigModelOverride"]["properties"][
            "system_prompt_label"]
        renamed["definitions"]["ConfigModelOverride"]["properties"][
            "system_prompt_lable"] = renamed["definitions"][
                "ConfigModelOverride"]["properties"].get(
                "system_prompt_label", {"type": ["string", "null"]})
        schp = os.path.join(td, "s.json")
        json.dump(renamed, open(schp, "w"))
        proc = run_gate(genp, ovp, outp, schema=schp)
        check("REQUIRED_FIELDS member renamed away in the schema exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("cross-check failure names the missing member",
              "system_prompt_label" in proc.stderr, proc.stderr)
        check("cross-check failure names the schema definition",
              "ConfigModelOverride" in proc.stderr, proc.stderr)
        check("cross-check failure writes no artifact", not os.path.exists(outp))
    # a schema missing the ReasoningEffort definition -> exit 3.
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex", [])}},
                  open(ovp, "w"))
        broken = json.loads(json.dumps(schema))
        del broken["definitions"]["ReasoningEffort"]
        schp = os.path.join(td, "s.json")
        json.dump(broken, open(schp, "w"))
        proc = run_gate(genp, ovp, outp, schema=schp)
        check("schema without ReasoningEffort definition exits 3",
              proc.returncode == 3, f"rc={proc.returncode} err={proc.stderr}")
        check("exit 3 names the missing definition",
              "ReasoningEffort" in proc.stderr, proc.stderr)
    # unit: derivation + cross-check
    check("EFFORT_VALUES derives the canonical 8-value enum",
          gate.derive_effort_values(schema) == CANONICAL_EFFORTS,
          str(gate.derive_effort_values(schema)))
    try:
        gate.derive_effort_values({"definitions": {}})
        check("derive_effort_values exits 3 on a missing enum", False,
              "no SystemExit raised")
    except SystemExit as e:
        check("derive_effort_values exits 3 on a missing enum", e.code == 3,
              f"code={e.code}")
    check("cross-check passes on the real schema",
          gate.check_required_fields_against_schema(schema) == [])
    check("cross-check reports the renamed member",
          gate.check_required_fields_against_schema(renamed)
          == ["system_prompt_label"],
          str(gate.check_required_fields_against_schema(renamed)))
    check("the hardcoded EFFORT_VALUES copy is gone from the gate",
          not hasattr(gate, "EFFORT_VALUES"),
          "gate.EFFORT_VALUES still exists")


def test_m_effort_advisory():
    """(m) v4 item 5: the live hint menu (catalog-digest.json
    .models[id].group.supported_reasoning_efforts) is diffed per row
    against the curated reasoning_efforts -> a drift-report ADVISORY
    entry (NOT a fail; the google-exempt precedent is evidence-based
    curation, runtime safety is apply_supported_effort). Rows without a
    live hint get no advisory. An explicit --digest that is missing is
    an input error (exit 3); an undiscoverable digest degrades
    gracefully (loud note + status=skipped in the report)."""
    print("m) v4 effort advisory (live hint vs curated menu)")
    # unit: the diff
    digest = {"models": {
        "m1": {"group": {"supported_reasoning_efforts":
                         ["low", "medium", "high"]}},
        "m2": {"group": {"supported_reasoning_efforts":
                         ["low", "high"]}},
        "m3": {"group": {"supported_reasoning_efforts": ["high"]}},
        "m5": {"group": {}},
    }}
    ov = {"m1": full_entry("m1", "responses", "codex",
                           menu_objs("low", "high", default="high")),
          "m2": full_entry("m2", "responses", "codex", []),
          "m3": full_entry("m3", "responses", "codex",
                           menu_objs("high", default="high"))}
    adv = gate.effort_advisories(ov, digest, EFFORTS)
    by_model = {a["model"]: a for a in adv}
    check("diverging rows advisored, agreeing / hint-less rows not",
          sorted(by_model) == ["m1", "m2"], json.dumps(adv))
    check("m1: live_only diff (medium offered, not curated)",
          by_model["m1"]["live_only"] == ["medium"]
          and by_model["m1"]["curated_only"] == [], json.dumps(by_model["m1"]))
    check("m2: curated-empty menu vs live hint -> live_only = the hint",
          by_model["m2"]["live_only"] == ["high", "low"]
          and by_model["m2"]["curated"] == [], json.dumps(by_model["m2"]))
    # unit: discovery degrades gracefully when nothing is found
    # unit: worktree .git-file -> main checkout root derivation (the
    # upstream-plans-root candidate hangs off it).
    with tempfile.TemporaryDirectory() as td:
        wt = os.path.join(td, "repo", "wt", "name")
        os.makedirs(os.path.join(td, "repo", "main", ".git", "worktrees", "name"))
        os.makedirs(os.path.join(wt, "scripts"))
        open(os.path.join(wt, ".git"), "w").write(
            "gitdir: " + os.path.join(td, "repo", "main", ".git",
                                      "worktrees", "name") + "\n")
        check("worktree .git file derives the main checkout root",
              gate._main_checkout_root(wt) == os.path.join(td, "repo", "main"),
              str(gate._main_checkout_root(wt)))
        os.remove(os.path.join(wt, ".git"))
        os.makedirs(os.path.join(wt, ".git"))
        check("plain checkout .git dir -> the root itself",
              gate._main_checkout_root(wt) == wt,
              str(gate._main_checkout_root(wt)))
    real_isfile = os.path.isfile
    try:
        def fake_isfile(p):
            # Block only the digest candidates — the gate's own .git
            # parsing (main-checkout derivation) must keep working.
            if str(p).endswith("catalog-digest.json"):
                return False
            return real_isfile(p)
        os.path.isfile = fake_isfile
        found, searched = gate.discover_digest()
    finally:
        os.path.isfile = real_isfile
    check("discover_digest -> (None, searched) when nothing is found",
          found is None and len(searched) >= 4, str(searched))
    check("searched order: worktree, main checkout, upstream plans root, ~/.grok",
          any("/wt/grok-build-responses/grok/" in c for c in searched)
          and any(c.startswith(os.path.expanduser("~/.grok/")) for c in searched[-1:]),
          str(searched))
    # unit: the report builder records the graceful skip
    cls = {"curated": ["m1"], "skipped": [], "new_unlisted": [],
           "removed_but_curated": [], "skip_stale": [],
           "bake_listed_off_proxy": []}
    report = gate.build_drift_report(
        "g.json", "o.json", None, None, cls,
        {"on_proxy": 1, "overlay_rows": 1, "curated": 1, "skipped": 0,
         "new_unlisted": 0, "removed_but_curated": 0, "skip_stale": 0,
         "bake_listed_off_proxy": 0},
        [], [])
    check("report: unavailable digest -> status skipped + reason + searched",
          report["effort_advisory"]["status"] == "skipped"
          and report["effort_advisory"]["source"] is None
          and report["effort_advisory"]["skip_reason"]
          and report["effort_advisory"]["searched"],
          json.dumps(report["effort_advisory"]))
    # end-to-end: explicit digest, advisory in the report, NOT a fail
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp, digp = (os.path.join(td, n) for n in
                                 ("g.json", "o.json", "out.json", "d.json"))
        json.dump({"models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex",
                                               menu_objs("low", default="low"))}},
                  open(ovp, "w"))
        json.dump({"models": {"m1": {"group": {
            "supported_reasoning_efforts": ["low", "high"]}}}}, open(digp, "w"))
        proc = run_gate(genp, ovp, outp, digest=digp)
        check("advisory divergence is NOT a fail (exit 0)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        report = json.load(open(outp + ".drift.json"))
        ea = report["effort_advisory"]
        check("report: advisory ok + source path + the m1 divergence",
              ea["status"] == "ok" and ea["source"] == digp
              and [a["model"] for a in ea["advisories"]] == ["m1"]
              and ea["advisories"][0]["live_only"] == ["high"],
              json.dumps(ea))
        check("F4: inputs.generated surfaces the capture timestamp (None when absent)",
              report["inputs"]["generated"] == {"path": genp,
                                                "generated_at": None},
              json.dumps(report["inputs"]))
        check("F4: inputs.digest surfaces the digest timestamp (None when absent)",
              report["inputs"]["digest"] == {"path": digp,
                                             "captured_at": None},
              json.dumps(report["inputs"]))
        # explicit digest that does not exist -> exit 3
        proc = run_gate(genp, ovp, outp, digest=os.path.join(td, "absent.json"))
        check("explicit missing digest exits 3", proc.returncode == 3,
              f"rc={proc.returncode} err={proc.stderr}")
        check("explicit missing digest is named",
              "absent.json" in proc.stderr, proc.stderr)
    # F4 (M2/M5, post dual review): the digest freshness cross-checks
    # are ADVISORY (exit 0, stderr WARN) — staleness never fails the
    # bake.
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp, digp = (os.path.join(td, n) for n in
                                 ("g.json", "o.json", "out.json", "d.json"))
        json.dump({"generated_at": "2026-01-02T00:00:00+00:00",
                   "models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex",
                                               menu_objs("low",
                                                         default="low"))}},
                  open(ovp, "w"))
        json.dump({"captured_at": "2026-01-01T00:00:00+00:00",
                   "models": {"m1": {"group": {
                       "supported_reasoning_efforts": ["low", "high"]}},
                              "m-ghost-d": {"group": {
                                  "supported_reasoning_efforts": ["low"]}},
                              "m-no-hint": {"group": {}}}},
                  open(digp, "w"))
        proc = run_gate(genp, ovp, outp, digest=digp)
        check("F4: digest freshness WARNs are advisory (the bake still exits 0)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        check("F4: digest older than the capture -> 'digest predates capture' stderr WARN",
              any("digest predates capture" in l
                  for l in proc.stderr.splitlines()),
              proc.stderr)
        check("F4: digest model absent from the capture -> 'digest stale vs capture' WARN naming it",
              any("digest stale vs capture" in l and "m-ghost-d" in l
                  for l in proc.stderr.splitlines()),
              proc.stderr)
        report = json.load(open(outp + ".drift.json"))
        check("F4: report inputs carry the surfaced timestamps",
              report["inputs"]["generated"] == {
                  "path": genp, "generated_at": "2026-01-02T00:00:00+00:00"}
              and report["inputs"]["digest"] == {
                  "path": digp, "captured_at": "2026-01-01T00:00:00+00:00"},
              json.dumps(report["inputs"]))
    # unit: the freshness cross-check is pure (warn strings, not prints).
    warns = gate.digest_freshness_warns(
        {"m1": {}},
        {"models": {"m1": {"group": {"supported_reasoning_efforts": ["low"]}},
                    "m-ghost-d": {"group": {"supported_reasoning_efforts": ["low"]}},
                    "m-no-hint": {"group": {}}}},
        "2026-01-02T00:00:00+00:00", "2026-01-01T00:00:00+00:00")
    check("F4 unit: stale-vs-capture names the absent hinted model only",
          any("digest stale vs capture" in w and "m-ghost-d" in w for w in warns)
          and not any("m-no-hint" in w for w in warns), str(warns))
    check("F4 unit: predates-capture fires when the digest is older",
          any("digest predates capture" in w for w in warns), str(warns))
    warns = gate.digest_freshness_warns(
        {"m1": {}}, {"models": {}}, "not-a-timestamp", "also-not")
    check("F4 unit: unparseable timestamps -> no predates WARN (graceful)",
          warns == [], str(warns))
    warns = gate.digest_freshness_warns(
        {"m1": {}}, {"models": {"m1": {"group": {
            "supported_reasoning_efforts": ["low"]}}}},
        "2026-01-02T00:00:00+00:00", "2026-01-03T00:00:00+00:00")
    check("F4 unit: fresh digest (newer than the capture) -> no WARNs",
          warns == [], str(warns))


def test_n_easy_add_scaffold():
    """(n) v4 item 6: the drift report emits a scaffold entry for every
    new_unlisted model — pre-filled from the generated C-class evidence
    (mode/costs/providers/caps + the live hint menu) with the ~5 TODO
    judgment fields nulled for operator paste (the gate stays
    fail-closed until the operator fills them). Family/wire pre-fills
    follow the curation conventions: grok* -> xai/responses; gpt-*/o*
    -> codex + the openai_responses_slug wire; claude* -> anthropic with
    the wire left to curation (cc or the messages pin); Other slugs ->
    both TODO."""
    print("n) v4 easy-add scaffolds")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp, digp = (os.path.join(td, n) for n in
                                 ("g.json", "o.json", "out.json", "d.json"))
        gen = {"models": {
            "m1": {"id": "m1", "max_input_tokens": 100},
            "gpt-5.9-turbo": {
                "id": "gpt-5.9-turbo", "mode": "chat", "providers": ["azure"],
                "max_input_tokens": 250000, "max_output_tokens": 32000,
                "input_cost_per_token": 1e-05, "output_cost_per_token": 3e-05,
                "supported_reasoning_efforts": ["low", "medium", "high"]},
            "qwen4-9b": {
                "id": "qwen4-9b", "mode": "chat", "providers": ["self_hosted"],
                "max_input_tokens": 32768},
            "grok-5.0": {
                "id": "grok-5.0", "mode": "chat", "providers": ["xai"],
                "max_input_tokens": 200000, "max_output_tokens": 64000},
        }}
        json.dump(gen, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex", [])}},
                  open(ovp, "w"))
        json.dump({"models": {
            "gpt-5.9-turbo": {"group": {
                "supported_reasoning_efforts": ["low", "medium", "high"]}},
            "grok-5.0": {"group": {
                "supported_reasoning_efforts": ["low", "high", "xhigh"]}},
        }}, open(digp, "w"))
        proc = run_gate(genp, ovp, outp, digest=digp)
        check("gate exits 0 with unlisted models (scaffolds, not fails)",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        report = json.load(open(outp + ".drift.json"))
        by_model = {s["model"]: s for s in report["scaffolds"]}
        check("a scaffold per new_unlisted model",
              sorted(by_model) == ["gpt-5.9-turbo", "grok-5.0", "qwen4-9b"],
              json.dumps(sorted(by_model)))
        g = by_model["gpt-5.9-turbo"]
        row = g["overlay_row"]
        check("scaffold: generated C-class evidence carried",
              g["evidence"]["max_input_tokens"] == 250000
              and g["evidence"]["max_output_tokens"] == 32000
              and g["evidence"]["providers"] == ["azure"]
              and g["evidence"]["mode"] == "chat"
              and g["evidence"]["input_cost_per_token"] == 1e-05,
              json.dumps(g["evidence"]))
        check("scaffold: family + wire pre-filled (codex + responses)",
              row["model_family"] == "codex"
              and row["api_backend"] == "responses", json.dumps(row))
        check("scaffold: menu pre-filled from the live hint (no default marker)",
              [m["value"] for m in row["reasoning_efforts"]] == ["low", "medium", "high"]
              and all(m["default"] is False for m in row["reasoning_efforts"])
              and row["supports_reasoning_effort"] is True,
              json.dumps(row["reasoning_efforts"]))
        check("scaffold: the ~5 TODO judgment fields are nulled + named",
              sorted(f for f, v in row.items()
                     if v is None and f != "cache_ttl")
              == ["multi_agent_v2", "name", "reasoning_effort",
                  "strict_responses_input", "supports_backend_search",
                  "system_prompt_label"]
              and set(g["todo_fields"])
              >= {"name", "system_prompt_label", "reasoning_effort",
                  "multi_agent_v2", "supports_backend_search",
                  "strict_responses_input"},
              json.dumps(g["todo_fields"]))
        check("scaffold: safe defaults pre-filled (compaction trio, ttl, headers)",
              row["auto_compact_threshold_percent"] == 80
              and row["compaction_at_tokens"] is True
              and row["compactions_remaining"] == 1
              and row["cache_ttl"] is None
              and row["extra_headers"] == {} and "cache_ttl" not in g["todo_fields"],
              json.dumps(row))
        q = by_model["qwen4-9b"]
        qrow = q["overlay_row"]
        check("scaffold: Other slug -> family + wire TODO (evidence-based)",
              qrow["model_family"] is None and qrow["api_backend"] is None
              and "model_family" in q["todo_fields"]
              and "api_backend" in q["todo_fields"],
              json.dumps(q["todo_fields"]))
        check("scaffold: no live hint -> empty menu, null effort, no effort TODO",
              qrow["reasoning_efforts"] == []
              and qrow["supports_reasoning_effort"] is False
              and qrow["reasoning_effort"] is None
              and "reasoning_effort" not in q["todo_fields"],
              json.dumps(q["todo_fields"]))
        gr = by_model["grok-5.0"]["overlay_row"]
    check("scaffold: grok* -> xai + responses pre-filled",
          gr["model_family"] == "xai" and gr["api_backend"] == "responses",
          json.dumps(gr))


def test_o_closed_keyset():
    """(o) F2/M3 (post dual review): the overlay row key set is CLOSED —
    every row key must be in (ConfigModelOverride schema properties −
    CREDENTIAL_FIELDS) ∪ the generated C-class keys {id, model,
    context_window, max_completion_tokens}. Any other key — e.g. a
    renamed credential (env_key_typo) — exits 2 BEFORE the artifact is
    written (the apex-ayl.130 Rust gate catches the same ride at build
    time; this closes it at the bake layer)."""
    print("o) F2 closed overlay key set (M3: the renamed-credential ride)")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1"}}}, open(genp, "w"))
        # (a) the renamed-credential ride (the M3 finding, reproduced).
        json.dump({"default": "m1",
                   "models": {"m1": {**full_entry("m1", "responses", "codex", []),
                                     "env_key_typo": "not-a-real-key"}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("F2: renamed-credential key in an overlay row exits 2",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("F2: rejection names the disallowed key AND the row",
              "env_key_typo" in proc.stderr and "m1" in proc.stderr,
              proc.stderr)
        check("F2: no artifact written on the closed-key-set failure",
              not os.path.exists(outp))
        # (b) an arbitrary unknown (non-credential) key: same hard fail.
        json.dump({"default": "m1",
                   "models": {"m1": {**full_entry("m1", "responses", "codex", []),
                                     "totally_unknown_field": 1}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("F2: arbitrary unknown key in an overlay row exits 2 (named)",
              proc.returncode == 2 and "totally_unknown_field" in proc.stderr,
              f"rc={proc.returncode} err={proc.stderr}")
        check("F2: no artifact written for the unknown key either",
              not os.path.exists(outp))
        # (c) no false positive on the legal C-class ride: an
        # overlay-only model's context_window (its sole cap source)
        # still passes the gate (test_g's pattern, re-pinned against
        # the closed set).
        json.dump({"default": "m1", "bake": ["m-seed"],
                   "models": {"m1": full_entry("m1", "responses", "codex", []),
                              "m-seed": {**full_entry("m-seed", "responses",
                                                      "xai", []),
                                         "context_window": 500000}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("F2: legal C-class overlay cap (overlay-only model) still passes",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
    schema = json.load(open(SCHEMA_PATH))
    allowed = gate.allowed_overlay_keys(schema)
    check("F2: allowed set = (schema props − credential fields) ∪ C-class keys",
          allowed == (set(schema["definitions"]["ConfigModelOverride"]["properties"])
                      - set(gate.CREDENTIAL_FIELDS))
          | {"id", "model", "context_window", "max_completion_tokens"},
          str(sorted(allowed)))
    check("F2: credential fields are NOT in the allowed set",
          not (set(gate.CREDENTIAL_FIELDS) & allowed),
          str(sorted(allowed)))
    check("F2: generated C-class keys ARE in the allowed set",
          {"id", "model", "context_window", "max_completion_tokens"} <= allowed)


def test_p_exit_code_contract():
    """(p) F7/N4 (post dual review): the artifact write is the LAST
    step — if the validated artifact cannot be WRITTEN (unwritable
    --out), the gate exits CLEANLY with the documented code 5 (a
    stderr line, no traceback) instead of an uncaught OSError (exit 1,
    outside the documented 0/2/3/4 contract)."""
    print("p) F7 exit-code contract (unwritable --out -> clean exit 5)")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp = (os.path.join(td, n) for n in ("g.json", "o.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 1000,
                                     "max_output_tokens": 32}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": full_entry("m1", "responses", "codex",
                                               menu_objs("low", "high",
                                                         default="high"))}},
                  open(ovp, "w"))
        # --out pointing at an EXISTING DIRECTORY: open(dir, "w") raises
        # IsADirectoryError (an OSError subclass) on every platform —
        # deterministic without touching permissions.
        rodir = os.path.join(td, "ro")
        os.makedirs(rodir)
        proc = run_gate(genp, ovp, rodir)
        check("F7: unwritable --out exits 5 (documented contract)",
              proc.returncode == 5, f"rc={proc.returncode} err={proc.stderr}")
        check("F7: no traceback on the artifact write failure",
              "Traceback" not in proc.stderr, proc.stderr)
        check("F7: the stderr line is the code-5 diagnostic naming the path",
              proc.stderr.splitlines()
              and proc.stderr.splitlines()[-1].startswith("5:")
              and rodir in proc.stderr,
              proc.stderr)
    check("F7: exit code 5 is documented in the module docstring (alongside 0/2/3/4)",
          "5 =" in (gate.__doc__ or ""),
          (gate.__doc__ or "<no docstring>")[:200])


def test_q_round2_microfixes():
    """Round-2 micro-fixes (coordinator adjudication after dual review r2):
    M-R2-1 naive/aware timestamp mix must not crash the advisory check;
    N-R2-2 per-model staleness WARNs aggregate into one summary line;
    N-R2-3 off-proxy curated+skip lists ONCE (row wins, F5 extension)."""
    gate = load_gate()
    # M-R2-1: mixed naive/aware timestamps — pre-fix the `c < g` comparison
    # sat outside the parse try and raised TypeError (advisory check crashed
    # the bake: traceback + exit 1 + no artifact).
    for gen_ts, cap_ts, label in (
            ("2026-09-19T05:23:58Z", "2026-09-18T00:00:00",
             "aware generated x naive captured"),
            ("2026-09-19T05:23:58", "2026-09-18T00:00:00+02:00",
             "naive generated x aware captured")):
        try:
            warns = gate.digest_freshness_warns(["a"], {"models": {}},
                                                generated_at=gen_ts,
                                                captured_at=cap_ts)
            check(f"M-R2-1: {label} -> no crash, returns list",
                  isinstance(warns, list), repr(warns))
        except TypeError as exc:
            check(f"M-R2-1: {label} -> no crash, returns list",
                  False, f"TypeError: {exc}")
    # N-R2-2: 3 stale digest models -> ONE summary WARN (not 3 lines).
    d = {"models": {f"m{i}": {"group": {"supported_reasoning_efforts": ["high"]}}
                    for i in range(3)}}
    warns = gate.digest_freshness_warns(["keepme"], d)
    stale = [w for w in warns if "stale vs capture" in w]
    check("N-R2-2: 3 stale digest models -> exactly 1 summary WARN",
          len(stale) == 1, repr(warns))
    check("N-R2-2: summary names the count and a sample model",
          bool(stale) and "3" in stale[0] and "m0" in stale[0], repr(stale))
    # N-R2-3: off-proxy curated + skip-listed -> removed_but_curated ONLY
    # (row wins — the F5 disjointness extended to the off-proxy pair).
    cls, warns = gate.classify_drift(
        gen_models=["live-1"],
        ov_models={"gone-1": {"model": "gone-1"}},
        skip_entries=[{"model": "gone-1", "reason": "gone"}],
        bake_list=[])
    check("N-R2-3: off-proxy curated+skip in removed_but_curated",
          cls["removed_but_curated"] == ["gone-1"], repr(cls))
    check("N-R2-3: ...and NOT in skip_stale (row wins)",
          cls["skip_stale"] == [], repr(cls["skip_stale"]))
    check("N-R2-3: redundant-skip WARN still fires off-proxy",
          any("redundant" in w for w in warns), repr(warns))
    # off-proxy skip for a NON-curated model still classifies skip_stale.
    cls2, _ = gate.classify_drift(
        gen_models=["live-1"],
        ov_models={},
        skip_entries=[{"model": "ghost", "reason": "x"}],
        bake_list=[])
    check("N-R2-3: off-proxy non-curated skip still skip_stale",
          cls2["skip_stale"] == [{"model": "ghost", "reason": "x"}],
          repr(cls2["skip_stale"]))


def test_r_twin_rows():
    """(r) v7 CTXWIN-1M-1M twin rows (apex-ayl.136): a curated row whose
    `model` field names a different on-proxy wire slug bakes as
    id=<row key>, model=<wire slug> riding the base slug's generated
    caps; overlay C-class caps on a twin fail closed via the effective
    slug; drift classification is per-slug (no false
    removed_but_curated); legacy rows merge byte-identically."""
    print("r) CTXWIN-1M-1M twin rows (apex-ayl.136)")
    gen = {
        "claude-opus-5": {"id": "claude-opus-5", "max_input_tokens": 1000000,
                          "max_output_tokens": 128000},
        "gpt-5.6-sol": {"id": "gpt-5.6-sol", "max_input_tokens": 922000,
                        "max_output_tokens": 128000},
    }
    ov = {
        "claude-opus-5": full_entry("claude-opus-5", "messages", "anthropic",
                                    menu_objs("low", "medium", "high",
                                              default="medium"),
                                    name="Claude Opus 5"),
        "claude-opus-5-1m": full_entry("claude-opus-5-1m", "messages",
                                       "anthropic",
                                       menu_objs("low", "medium", "high",
                                                 default="medium"),
                                       name="Claude Opus 5 (1M)",
                                       system_prompt_label="Claude Opus 5",
                                       model="claude-opus-5"),
        "gpt-5.6-sol": full_entry("gpt-5.6-sol", "responses", "codex",
                                  menu_objs("low", "medium", "high",
                                            default="medium"),
                                  name="GPT-5.6 Sol"),
        "gpt-5.6-sol-1m": full_entry("gpt-5.6-sol-1m", "responses", "codex",
                                     menu_objs("low", "medium", "high",
                                               default="medium"),
                                     name="GPT-5.6 Sol (1M)",
                                     system_prompt_label="GPT-5.6 Sol",
                                     model="gpt-5.6-sol"),
    }
    # (a) the twin bakes: id = row key, model = base wire slug, caps from
    # the BASE slug's generated row.
    models, warns = gate.merge_rows(gen, ov, [])
    check("twin bakes: id = row key, model = base wire slug",
          models["claude-opus-5-1m"]["id"] == "claude-opus-5-1m"
          and models["claude-opus-5-1m"]["model"] == "claude-opus-5",
          json.dumps(models["claude-opus-5-1m"]))
    check("claude twin inherits the base slug's generated caps",
          models["claude-opus-5-1m"]["context_window"] == 1000000
          and models["claude-opus-5-1m"]["max_completion_tokens"] == 128000)
    check("sol twin inherits the base slug's generated caps",
          models["gpt-5.6-sol-1m"]["model"] == "gpt-5.6-sol"
          and models["gpt-5.6-sol-1m"]["context_window"] == 922000
          and models["gpt-5.6-sol-1m"]["max_completion_tokens"] == 128000)
    # (d) legacy rows are byte-identical (id == model, no twin in sight).
    check("legacy rows merge byte-identical (id == model)",
          models["claude-opus-5"]["id"] == "claude-opus-5"
          and models["claude-opus-5"]["model"] == "claude-opus-5"
          and models["gpt-5.6-sol"]["model"] == "gpt-5.6-sol")
    check("row set = base rows ∪ twins",
          sorted(models) == ["claude-opus-5", "claude-opus-5-1m",
                             "gpt-5.6-sol", "gpt-5.6-sol-1m"],
          json.dumps(sorted(models)))
    # (b) drift classification is per WIRE SLUG: the twin row key never
    # false-positives as removed_but_curated, and the base slug counts
    # ONCE in curated — the twin leaves every drift class unchanged vs
    # the base-only state.
    cls_tw, _ = gate.classify_drift(gen_models=gen, ov_models=ov,
                                    skip_entries=[], bake_list=[])
    check("no removed_but_curated false positive for twin row keys",
          cls_tw["removed_but_curated"] == [], repr(cls_tw))
    check("curated counts each base slug ONCE (twins ride the slug)",
          cls_tw["curated"] == ["claude-opus-5", "gpt-5.6-sol"], repr(cls_tw))
    cls_base, _ = gate.classify_drift(
        gen_models=gen,
        ov_models={"claude-opus-5": ov["claude-opus-5"],
                   "gpt-5.6-sol": ov["gpt-5.6-sol"]},
        skip_entries=[], bake_list=[])
    check("twin rows leave every drift class unchanged vs base-only state",
          cls_tw == cls_base, repr((cls_tw, cls_base)))
    # wire cross-check rides the base slug: the clean twins pass; a twin
    # moved onto the wrong wire for its base slug is flagged.
    check("clean twins pass the wire cross-check via the base slug",
          gate.collect_missing(ov, [], EFFORTS) == [],
          repr(gate.collect_missing(ov, [], EFFORTS)))
    bad = dict(ov["gpt-5.6-sol-1m"])
    bad["api_backend"] = "messages"  # wrong wire for a gpt-5.6 slug
    ov_wire = dict(ov)
    ov_wire["gpt-5.6-sol-1m"] = bad
    missing = gate.collect_missing(ov_wire, [], EFFORTS)
    check("twin on the wrong wire is flagged via the base slug",
          len(missing) == 1 and missing[0][0] == "gpt-5.6-sol-1m"
          and any("api_backend" in p for p in missing[0][1]),
          repr(missing))
    # (c) end-to-end: the twin overlay bakes clean (exit 0, artifact
    # carries the twin row); a twin carrying overlay C-class caps fails
    # closed via the effective slug (exit 2, artifact NOT written).
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": gen}, open(genp, "w"))
        json.dump({"default": "gpt-5.6-sol", "models": ov}, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("twin overlay bakes clean (exit 0)", proc.returncode == 0,
              f"rc={proc.returncode} err={proc.stderr}")
        art = {m["id"]: m for m in json.load(open(outp))["models"]}
        check("artifact twin row: id = key, model = base slug, base caps",
              art["claude-opus-5-1m"]["model"] == "claude-opus-5"
              and art["claude-opus-5-1m"]["context_window"] == 1000000
              and art["gpt-5.6-sol-1m"]["model"] == "gpt-5.6-sol"
              and art["gpt-5.6-sol-1m"]["context_window"] == 922000,
              json.dumps(art.get("claude-opus-5-1m")))
        cap = dict(ov["claude-opus-5-1m"])
        cap["context_window"] = 1048576  # forbidden: the base is on-proxy
        ov_bad = dict(ov)
        ov_bad["claude-opus-5-1m"] = cap
        json.dump({"default": "gpt-5.6-sol", "models": ov_bad}, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("twin carrying overlay caps fails closed (exit 2)",
              proc.returncode == 2, f"rc={proc.returncode} err={proc.stderr}")
        check("fail names the twin row + the forbidden field",
              "claude-opus-5-1m" in proc.stderr
              and "context_window" in proc.stderr, proc.stderr)


if __name__ == "__main__":
    test_a_merge_precedence_and_row_shape()
    test_b_subset_drift_coverage()
    test_c_role_pins()
    test_d_committed_artifacts()
    test_e_schema_validation()
    test_f_gate_output_contract()
    test_g_forbidden_overlay_caps()
    test_h_required_fields_and_nullability()
    test_i_menu_effort_consistency()
    test_j_wire_cross_check()
    test_k_skip_key_and_drift_classes()
    test_l_schema_derived_contract()
    test_m_effort_advisory()
    test_n_easy_add_scaffold()
    test_o_closed_keyset()
    test_p_exit_code_contract()
    test_q_round2_microfixes()
    test_r_twin_rows()
    print(f"ALL PASS ({PASS} checks)")
