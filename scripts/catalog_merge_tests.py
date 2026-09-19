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
  D3: FAIL-CLOSED gate. REQUIRED_FIELDS = ["api_backend", "model_family"]
      must be present on EVERY overlay entry, and every baking model
      (the 76 generated + the overlay `bake` list) needs a complete
      overlay entry — otherwise the gate exits 2 with an explicit
      missing list (the flag-only era is over: never a silent bless).
      Role pins ride the overlay and are written to the merged file.

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


def load_gate():
    spec = importlib.util.spec_from_file_location("catalog_gate", os.path.join(SCRIPTS, "catalog_gate.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gate = load_gate()

PASS = 0


def check(name, cond, detail=""):
    global PASS
    if not cond:
        sys.exit(f"FAIL: {name} {detail}")
    PASS += 1
    print(f"  ok: {name}")


def run_gate(gen, ov, out):
    proc = subprocess.run(
        [sys.executable, os.path.join(SCRIPTS, "catalog_gate.py"),
         "--generated", gen, "--overlay", ov, "--out", out],
        capture_output=True, text=True)
    return proc


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


def test_b_fail_closed_coverage():
    """(b) D3: fail-closed coverage with an explicit missing list."""
    print("b) D3 fail-closed coverage")
    gen = {"m1": {"id": "m1"}, "m2": {"id": "m2"}}
    missing = gate.collect_missing(
        gen, {"m1": {"api_backend": "responses", "model_family": "codex"}}, [])
    check("missing entry reported for the whole model",
          any(mid == "m2" and fields == list(gate.REQUIRED_FIELDS)
              for mid, fields in missing), str(missing))
    missing = gate.collect_missing(gen, {
        "m1": {"model_family": "codex"},
        "m2": {"api_backend": "responses"}}, [])
    check("missing fields reported per model",
          ("m1", ["api_backend"]) in missing
          and ("m2", ["model_family"]) in missing, str(missing))
    missing = gate.collect_missing(
        gen,
        {"m1": {"api_backend": "responses", "model_family": "codex"},
         "m2": {"api_backend": "responses", "model_family": "codex"}},
        ["grok-x"])
    check("bake-list model without an entry is a coverage failure",
          any(mid == "grok-x" for mid, _ in missing), str(missing))
    missing = gate.collect_missing(gen, {
        "m1": {"api_backend": "responses", "model_family": "codex"},
        "m2": {"api_backend": "responses", "model_family": "codex"},
        "gemma-x": {"api_backend": "chat_completions"}}, [])
    check("required fields enforced on every overlay entry",
          ("gemma-x", ["model_family"]) in missing, str(missing))
    # end-to-end: exit 2, explicit list, no artifact
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 100}}}, open(genp, "w"))
        json.dump({"models": {"m1": {"model_family": "codex"}}}, open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 2 on incomplete overlay", proc.returncode == 2,
              f"rc={proc.returncode} err={proc.stderr}")
        check("gate names the failing model", "m1" in proc.stderr)
        check("gate names the missing field", "api_backend" in proc.stderr)
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
    """(d) committed artifacts: D1 shape, D3 exhaustiveness, seed
    migration invariants (operator curation, byte-for-value)."""
    print("d) committed artifacts")
    gen = json.load(open(os.path.join(MODELS_DIR, "catalog_generated.json")))
    ov = json.load(open(os.path.join(MODELS_DIR, "catalog_overlay.json")))
    merged = json.load(open(os.path.join(MODELS_DIR, "default_models.json")))
    check("generated: 76 models",
          gen["model_count"] == 76 and len(gen["models"]) == 76)
    # D1 upstream shape: four role pins + a models array.
    check("merged carries the four role pins",
          all(merged.get(p) == "grok-4.6" for p in
              ("default", "web_search", "image_description", "session_summary")))
    rows = merged.get("models")
    check("merged models is an array", isinstance(rows, list))
    by_id = {r["id"]: r for r in rows}
    check("77 merged rows", len(rows) == 77, f"got {len(rows)}")
    check("row set = generated + bake list",
          set(by_id) == set(gen["models"]) | set(ov.get("bake", [])))
    check("rows sorted by id", [r["id"] for r in rows] == sorted(by_id))
    check("gemma-4-31b stays config-side (overlay-only)",
          "gemma-4-31b" not in by_id)
    # D3: the committed overlay is exhaustive.
    check("overlay has no coverage gaps",
          gate.collect_missing(gen["models"], ov["models"], ov.get("bake", [])) == [])
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
          "overlay no longer carries them, C-class)",
          g46["context_window"] == 500000 and g46["max_completion_tokens"] == 500000)
    check("grok-4.6: seed menu survives (4 items, high default)",
          [m["value"] for m in g46["reasoning_efforts"]] == ["xhigh", "high", "medium", "low"]
          and g46["reasoning_efforts"][1]["default"] is True)
    g45 = by_id["grok-4.5"]
    check("grok-4.5: overlay-only seed row — overlay cw 500000 is the "
          "sole source (no generated truth; C-class carve-out)",
          g45["context_window"] == 500000 and g45["api_backend"] == "responses"
          and g45["model_family"] == "xai"
          and [m["value"] for m in g45["reasoning_efforts"]] == ["high", "medium", "low"])
    sol = by_id["gpt-5.6-sol"]
    check("sol: cw 922000 from generated (C-class: the proxy's truth; the "
          "071 overlay 353000 leak is gone)",
          sol["context_window"] == 922000)
    check("sol: generated mct 128000 survives",
          sol["max_completion_tokens"] == 128000)
    check("sol: overlay menu wins (5 items, no xhigh)",
          [m["value"] for m in sol["reasoning_efforts"]] == ["low", "medium", "high", "max", "ultra"])
    check("sol: curated wire pins",
          sol["strict_responses_input"] is True and sol["multi_agent_v2"] is True
          and sol["supports_backend_search"] is False
          and sol["extra_headers"].get("x-litellm-tags") == "East US 2")
    check("sol: seed single-effort survives (low)", sol["reasoning_effort"] == "low")
    # Curation spot-checks (the D3 ruling table).
    check("claude pinned messages + anthropic + 1h cache",
          by_id["claude-sonnet-5"]["api_backend"] == "messages"
          and by_id["claude-sonnet-5"]["model_family"] == "anthropic"
          and by_id["claude-sonnet-5"]["cache_ttl"] == "1h")
    check("frontier gpt-5.1-codex-max: responses + codex",
          by_id["gpt-5.1-codex-max"]["api_backend"] == "responses"
          and by_id["gpt-5.1-codex-max"]["model_family"] == "codex")
    check("new gemini preview: responses + google",
          by_id["gemini-3-flash-preview"]["api_backend"] == "responses"
          and by_id["gemini-3-flash-preview"]["model_family"] == "google")
    check("legacy gpt-4: chat_completions + codex",
          by_id["gpt-4"]["api_backend"] == "chat_completions"
          and by_id["gpt-4"]["model_family"] == "codex")
    check("embedding row: capless (no mct) + chat_completions",
          "max_completion_tokens" not in by_id["text-embedding-ada-002"]
          and by_id["text-embedding-ada-002"]["api_backend"] == "chat_completions")
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
    # Redaction: no key material in any artifact.
    for name, doc in [("generated", gen), ("overlay", ov), ("merged", merged)]:
        check(f"{name}: no key-like strings",
              not re.search(r"sk-[A-Za-z0-9]{20,}|xai-[a-z0-9]{24,}|"
                            r"ghp_[A-Za-z0-9]{16,}|AKIA[A-Z0-9]{12,}", json.dumps(doc)))


def test_e_schema_validation():
    """(e) the apex-hw0 model-row definition still catches broken rows."""
    print("e) schema validation")
    schema_path = os.path.join(ROOT, "crates", "codegen", "xai-grok-shell", "config.schema.json")
    full_schema = json.load(open(schema_path))
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
        # validation (the pin check runs first — it is orthogonal here).
        json.dump({"default": "m",
                   "models": {"m": {"api_backend": "warp_drive",
                                    "model_family": "codex"}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 2 on schema-broken row", proc.returncode == 2,
              f"rc={proc.returncode} err={proc.stderr}")
        check("gate writes no artifact on failure", not os.path.exists(outp))
        check("gate names the failing model", "m:" in proc.stderr)


def test_f_gate_output_contract():
    """(f) the gate bakes IN PLACE into default_models.json (D1)."""
    print("f) gate output contract")
    check("gate default out is default_models.json",
          getattr(gate, "DEFAULT_OUT_NAME", None) == "default_models.json")
    with tempfile.TemporaryDirectory() as td:
        genp, ovp, outp = (os.path.join(td, n) for n in ("g.json", "o.json", "out.json"))
        json.dump({"models": {"m1": {"id": "m1", "max_input_tokens": 1000,
                                     "max_output_tokens": 32}}}, open(genp, "w"))
        json.dump({"default": "m1",
                   "models": {"m1": {"api_backend": "responses", "model_family": "codex"}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("gate exits 0 on complete curation", proc.returncode == 0, proc.stderr)
        art = json.load(open(outp))
        check("artifact carries the pins + models array",
              art.get("default") == "m1" and isinstance(art.get("models"), list))
        check("row = id/model + mapped caps + curated fields",
              art["models"][0] == {"id": "m1", "model": "m1",
                                   "context_window": 1000,
                                   "max_completion_tokens": 32,
                                   "api_backend": "responses",
                                   "model_family": "codex"})


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
                   "models": {"m-gen": {"api_backend": "responses",
                                        "model_family": "codex",
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
                   "models": {"m-gen": {"api_backend": "responses",
                                        "model_family": "codex"},
                              "m-seed": {"api_backend": "responses",
                                         "model_family": "xai",
                                         "context_window": 500000}}},
                  open(ovp, "w"))
        proc = run_gate(genp, ovp, outp)
        check("overlay-only overlay context_window passes the gate",
              proc.returncode == 0, f"rc={proc.returncode} err={proc.stderr}")
        art = json.load(open(outp))
        seed = {r["id"]: r for r in art["models"]}["m-seed"]
        check("overlay-only row carries the overlay cap as sole source",
              seed["context_window"] == 500000)


if __name__ == "__main__":
    test_a_merge_precedence_and_row_shape()
    test_b_fail_closed_coverage()
    test_c_role_pins()
    test_d_committed_artifacts()
    test_e_schema_validation()
    test_f_gate_output_contract()
    test_g_forbidden_overlay_caps()
    print(f"ALL PASS ({PASS} checks)")
