#!/usr/bin/env python3
"""CATALOG GATE (apex-071 CATALOG-BAKE-1): merge generated + overlay and
bake the result IN PLACE into `default_models.json`.

The build-time contract (operator-adjudicated 3-delta, 2026-09-19):

  catalog_generated.json  (scripts/catalog_generate.py output; proxy truth;
                           raw C-class fields live HERE ONLY — drift
                           baseline + the apex-ayl.55 cost cut)
  catalog_overlay.json    (hand-curated wire semantics — the single place
                           O/H-class per-model values live; also carries
                           the role pins and the `bake` list of
                           overlay-only models that must ride the merged
                           catalog — seed migration)
        -> merge (overlay wins on collision) + validate against the
           apex-hw0 schema (config.schema.json#/definitions/
           ConfigModelOverride — the model-row definition)
        -> default_models.json (upstream shape: the four role pins + a
           `models` array sorted by id; include_str!-ed; ZERO network
           inside `cargo build` — the build is validate-only)

D3 fail-closed (supersedes the flag-only era):
  REQUIRED_FIELDS = ["api_backend", "model_family"]
  The gate FAILS (exit 2) with an explicit missing list when:
    * any baking model (every generated model + every `bake`-listed
      model) has no overlay entry at all;
    * any overlay entry (baking or between-build) lacks a required
      field;
    * a role pin references a model that is not in the merged row set.
  A new model with no overlay entry is a build failure, never a silent
  bless — the operator cures the overlay, the gate enforces completeness.
  The merged file is machine output: the operator curates the overlay,
  never the merged file.

Merge rules (runtime precedence UNCHANGED: config row > live fetch >
baked catalog > fallback — this gate only decides the baked layer):
  * Every merged row carries id/model + the runtime cap mapping
    (max_input_tokens -> context_window, > 0; max_output_tokens ->
    max_completion_tokens, 0 < v <= u32::MAX) + the curated overlay
    fields. Overlay wins on any collision (O/H class is curated
    authority). Raw C-class fields (mode/costs/providers/hint menus)
    stay in catalog_generated.json only.
  * Overlay entries that are neither generated nor bake-listed are
    accepted (between-build additions) with a WARNING; they stay
    config-side until the proxy serves the model or it is bake-listed.

Exit codes: 0 = merged + validated (warnings may be present);
2 = fail-closed (incomplete curation / dangling pin / schema validation
    failure — artifact NOT written); 3 = input unreadable / bad shape;
4 = jsonschema module missing.

Usage (CWD-independent; defaults resolve next to this script):
  scripts/catalog_gate.py
  scripts/catalog_gate.py --generated X --overlay Y --schema Z --out W
"""
import argparse
import json
import os
import sys

U32_MAX = 2**32 - 1

# D3: the small explicit list — every overlay entry must carry ALL of
# these, and every baking model needs an entry that does.
REQUIRED_FIELDS = ["api_backend", "model_family"]
# The upstream default_models.json role pins (canonical artifact order).
ROLE_PIN_KEYS = ("default", "web_search", "image_description", "session_summary")
# The gate bakes IN PLACE into the file the seed path already reads (D1).
DEFAULT_OUT_NAME = "default_models.json"

# Credential / identity fields must never be baked (the runtime parse
# struct does not even model them). The ConfigModelOverride schema allows
# these on live config rows — the overlay is a stricter surface, so the
# gate enforces it here.
CREDENTIAL_FIELDS = ("api_key", "env_key", "auth_provider", "mtls_cert_dir")


def humanize_effort_id(value):
    """Mirror of the Rust `humanize_effort_id` (Bare arm): 'xhigh' -> 'Xhigh'."""
    return value[:1].upper() + value[1:] if value else value


def normalize_efforts(menu):
    """Accept full objects or bare canonical strings (the Rust untagged
    deserializer accepts both; the committed artifact must carry FULL
    objects because the schema requires id/value/label/default)."""
    out = []
    for item in menu or []:
        if isinstance(item, str):
            out.append({
                "id": item,
                "value": item,
                "label": humanize_effort_id(item),
                "default": False,
            })
        elif isinstance(item, dict):
            v = item.get("value")
            if not isinstance(v, str) or not v:
                raise ValueError(f"reasoning_efforts entry without string 'value': {item!r}")
            entry = {
                "id": item.get("id") or v,
                "value": v,
                "label": item.get("label") or humanize_effort_id(v),
                "default": bool(item.get("default", False)),
            }
            if item.get("description") is not None:
                entry["description"] = item["description"]
            out.append(entry)
        else:
            raise ValueError(f"reasoning_efforts entry is neither string nor table: {item!r}")
    return out


def load_json(path, what):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        print(f"3: cannot read {what} at {path}: {e!r}", file=sys.stderr)
        sys.exit(3)


def extract_role_pins(overlay):
    """The overlay's role pins in canonical order (only keys present)."""
    return {k: overlay[k] for k in ROLE_PIN_KEYS if k in overlay}


def check_role_pins(pins, model_ids):
    """(ok, problems): 'default' is mandatory; every pin must reference a
    merged-row model id (the crate asserts default in models at boot)."""
    problems = []
    default = pins.get("default")
    if not isinstance(default, str) or not default:
        problems.append("overlay must carry a non-empty 'default' role pin")
    for key, value in pins.items():
        if isinstance(value, str) and value and value not in model_ids:
            problems.append(
                f"role pin '{key}' references '{value}', which is not a merged-row model")
    return (not problems), problems


def collect_missing(gen_models, ov_models, bake_list):
    """D3 fail-closed coverage. Returns [(model_id, missing_fields)] in
    sorted order; missing_fields == REQUIRED_FIELDS means 'no overlay
    entry at all'. Enforced on EVERY overlay entry (baking or
    between-build) — the curation is exhaustive by construction."""
    missing = []
    row_ids = set(gen_models) | set(bake_list)
    for mid in sorted(row_ids):
        entry = ov_models.get(mid)
        if not isinstance(entry, dict):
            missing.append((mid, list(REQUIRED_FIELDS)))
            continue
        miss = [f for f in REQUIRED_FIELDS if f not in entry]
        if miss:
            missing.append((mid, miss))
    for mid, entry in sorted(ov_models.items()):
        if mid in row_ids or not isinstance(entry, dict):
            continue
        miss = [f for f in REQUIRED_FIELDS if f not in entry]
        if miss:
            missing.append((mid, miss))
    return missing


def merge_rows(generated, overlay_models, bake_list=()):
    """D1: merged rows = id/model + the runtime cap mapping + the curated
    overlay fields (overlay wins on collision; raw C-class fields stay in
    catalog_generated.json only). Returns (models, warns); coverage and
    pin integrity are enforced by the caller (fail-closed, exit 2)."""
    warns = []
    row_ids = set(generated) | set(bake_list)
    models = {}
    for mid in sorted(row_ids):
        src = generated.get(mid) or {}
        fields = {}
        # Runtime mapping (ModelEntryConfig names; the crate parse struct
        # reads these).
        cw = src.get("max_input_tokens")
        if isinstance(cw, (int, float)) and cw > 0:
            fields["context_window"] = int(cw)
        mo = src.get("max_output_tokens")
        if isinstance(mo, (int, float)) and 0 < mo <= U32_MAX:
            fields["max_completion_tokens"] = int(mo)
        entry = overlay_models[mid]
        for k in sorted(entry):
            if k in ("id", "model"):
                continue  # the generated id is authoritative
            if k == "reasoning_efforts":
                fields[k] = normalize_efforts(entry[k])
            else:
                fields[k] = entry[k]  # overlay wins on collision
        models[mid] = {"id": mid, "model": mid, **fields}
    for mid in sorted(overlay_models):
        if mid not in row_ids:
            warns.append(
                f"overlay entry '{mid}' is not in the generated catalog and "
                "not in the 'bake' list — config-side only (between-build "
                "addition)")
    return models, warns


def row_subschema(full_schema):
    """The model-row definition with the schema's /definitions inlined so
    $ref resolution works against the subschema (draft-07)."""
    row = full_schema["definitions"]["ConfigModelOverride"]
    sub = {"$schema": "http://json-schema.org/draft-07/schema#"}
    sub.update({k: v for k, v in row.items() if k != "description"})
    sub["definitions"] = full_schema["definitions"]
    return sub


FORMAT_CHECKER = None


def _get_format_checker():
    global FORMAT_CHECKER
    import jsonschema

    if FORMAT_CHECKER is None:
        fc = jsonschema.FormatChecker()

        def _range(name, lo, hi):
            def check(instance):
                # `format` runs independently of `type` in draft-07; these
                # schema fields are `integer | null`, so null must pass the
                # format check (Rust serde reads it as None).
                if instance is None:
                    return True
                return isinstance(instance, int) and not isinstance(instance, bool) and lo <= instance <= hi

            return check

        fc.checks("uint8")(_range("uint8", 0, 0xFF))
        fc.checks("uint32")(_range("uint32", 0, 0xFFFFFFFF))
        fc.checks("uint64")(_range("uint64", 0, 0xFFFFFFFFFFFFFFFF))
        FORMAT_CHECKER = fc
    return FORMAT_CHECKER


def make_row_validator(full_schema):
    import jsonschema
    from jsonschema import Draft7Validator

    return Draft7Validator(row_subschema(full_schema), format_checker=_get_format_checker())


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    models_dir = os.path.join(root, "crates", "codegen", "xai-grok-models")
    ap = argparse.ArgumentParser(description="CATALOG GATE (apex-071)")
    ap.add_argument("--generated", default=os.path.join(models_dir, "catalog_generated.json"))
    ap.add_argument("--overlay", default=os.path.join(models_dir, "catalog_overlay.json"))
    ap.add_argument(
        "--schema",
        default=os.path.join(root, "crates", "codegen", "xai-grok-shell", "config.schema.json"),
    )
    ap.add_argument("--out", default=os.path.join(models_dir, DEFAULT_OUT_NAME))
    args = ap.parse_args()

    try:
        import jsonschema
    except ImportError:
        print("4: python3 jsonschema module required (pip install jsonschema)", file=sys.stderr)
        sys.exit(4)

    generated = load_json(args.generated, "catalog_generated.json")
    overlay = load_json(args.overlay, "catalog_overlay.json")
    full_schema = load_json(args.schema, "config.schema.json")

    gen_models = generated.get("models")
    if not isinstance(gen_models, dict) or not gen_models:
        print("3: catalog_generated.json must carry a non-empty 'models' map", file=sys.stderr)
        sys.exit(3)
    ov_models = overlay.get("models")
    if not isinstance(ov_models, dict):
        print("3: catalog_overlay.json must carry a 'models' map ({} ok)", file=sys.stderr)
        sys.exit(3)
    bake_list = overlay.get("bake", [])
    if not isinstance(bake_list, list) or not all(isinstance(b, str) for b in bake_list):
        print("3: catalog_overlay.json 'bake' must be a list of model ids", file=sys.stderr)
        sys.exit(3)

    for mid, entry in ov_models.items():
        if not isinstance(entry, dict):
            print(f"3: overlay entry '{mid}' must be a table of fields", file=sys.stderr)
            sys.exit(3)
        creds = [k for k in entry if k in CREDENTIAL_FIELDS]
        if creds:
            print(
                f"3: overlay entry '{mid}' carries credential field(s) {creds} — "
                "the overlay must stay credential-free (config.toml rows are "
                "where credentials live)",
                file=sys.stderr,
            )
            sys.exit(3)

    # D3 fail-closed: every baking model needs a complete overlay entry.
    missing = collect_missing(gen_models, ov_models, bake_list)
    if missing:
        print(
            f"FAIL-CLOSED: {len(missing)} model(s) lack a complete overlay "
            f"entry — cure catalog_overlay.json (REQUIRED_FIELDS = "
            + ", ".join(REQUIRED_FIELDS) + "):",
            file=sys.stderr,
        )
        for mid, fields in missing:
            detail = ", ".join(fields) if fields else "no overlay entry"
            print(f"  missing: {mid}: {detail}", file=sys.stderr)
        sys.exit(2)

    # D3: role pins ride the overlay; every pin must reference a row.
    pins = extract_role_pins(overlay)
    row_ids = set(gen_models) | set(bake_list)
    ok, problems = check_role_pins(pins, row_ids)
    if not ok:
        print("FAIL-CLOSED: role pins incomplete or dangling:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        sys.exit(2)

    try:
        models, warns = merge_rows(gen_models, ov_models, bake_list)
    except ValueError as e:
        print(f"3: overlay malformed: {e}", file=sys.stderr)
        sys.exit(3)

    validator = make_row_validator(full_schema)
    errors = []
    for mid in sorted(models):
        for err in sorted(validator.iter_errors(models[mid]), key=lambda e: list(e.path)):
            errors.append(f"{mid}: /{'/'.join(str(p) for p in err.path)}: {err.message}")
    if errors:
        print(f"SCHEMA VALIDATION FAILED ({len(errors)} error(s)) — artifact NOT written:",
              file=sys.stderr)
        for line in errors[:40]:
            print(f"  {line}", file=sys.stderr)
        sys.exit(2)

    for w in warns:
        print(f"WARN: {w}", file=sys.stderr)

    # D1: the upstream shape — role pins + a models array sorted by id.
    artifact = {**pins, "models": [models[mid] for mid in sorted(models)]}
    with open(args.out, "w") as f:
        json.dump(artifact, f, indent=1)
        f.write("\n")
    print(
        f"OK merged={len(models)} overlay_entries={len(ov_models)} "
        f"schema_errors=0 warns={len(warns)} out={args.out} "
        f"size={os.path.getsize(args.out)}B",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
