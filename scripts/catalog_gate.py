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

D3 fail-closed (supersedes the flag-only era), extended by
CATALOG-REQUIRED-CURATION-1 (apex-kb6, operator ruling 2026-09-19): the
overlay is the SINGLE curation home — EVERY legal field is REQUIRED on
EVERY overlay entry (the 15-key full-row contract: 14 non-null keys +
the null-allowed cache_ttl). The gate FAILS (exit 2) with explicit
`missing:` + `invalid:` lines when:
    * any baking model (every generated model + every `bake`-listed
      model) has no overlay entry at all;
    * any overlay entry (baking or between-build) lacks a required
      field;
    * a role pin references a model that is not in the merged row set.
  A new model with no overlay entry is a build failure, never a silent
  bless — the operator cures the overlay, the gate enforces completeness.
  The merged file is machine output: the operator curates the overlay,
  never the merged file.
  CATALOG-CCLASS-SEED-1 (apex-byc, operator ruling 2026-09-19): the gate
  also FAILS (exit 2) with an explicit per-entry list when an overlay
  entry carries a FORBIDDEN_OVERLAY_FIELDS value (context_window /
  max_completion_tokens) for a model that IS in the generated catalog
  (on the proxy) — the generated capture is the truth for the baked
  caps, and config.toml rows remain the top runtime override. Overlay-
  only models (not in the generated catalog) are permitted: the overlay
  is the sole source of caps for those rows.

Merge rules (runtime precedence UNCHANGED: config row > live fetch >
baked catalog > fallback — this gate only decides the baked layer):
  * Every merged row carries id/model + the runtime cap mapping
    (max_input_tokens -> context_window, > 0; max_output_tokens ->
    max_completion_tokens, 0 < v <= u32::MAX) + the curated overlay
    fields. Overlay wins on any collision (O/H class is curated
    authority) — EXCEPT C-class caps: a generated cap always beats an
    overlay cap (CATALOG-CCLASS-SEED-1); overlay context_window /
    max_completion_tokens ride only for overlay-only models that have no
    generated row (the off-proxy seed carriers). Raw C-class fields
    (mode/costs/providers/hint menus) stay in catalog_generated.json
    only.
  * Overlay entries that are neither generated nor bake-listed are
    accepted (between-build additions) with a WARNING; they stay
    config-side until the proxy serves the model or it is bake-listed.

Exit codes: 0 = merged + validated (warnings may be present);
2 = fail-closed (incomplete curation / dangling pin / forbidden C-class
    overlay caps / schema validation failure — artifact NOT written);
    3 = input unreadable / bad shape;
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

# D3 + CATALOG-REQUIRED-CURATION-1 (apex-kb6): the FULL-ROW contract —
# every overlay entry must carry ALL of these (14 non-null keys + the
# null-allowed cache_ttl), and every baking model needs an entry that
# does. The flag-only era is over: never a silent bless.
REQUIRED_FIELDS = [
    "api_backend",
    "model_family",
    "name",
    "reasoning_efforts",
    "supports_reasoning_effort",
    "reasoning_effort",
    "cache_ttl",
    "multi_agent_v2",
    "supports_backend_search",
    "strict_responses_input",
    "extra_headers",
    "auto_compact_threshold_percent",
    "compaction_at_tokens",
    "compactions_remaining",
    "system_prompt_label",
]
# The ReasoningEffort enum (config.schema.json) — the canonical effort
# values a menu entry / reasoning_effort may carry.
EFFORT_VALUES = ["none", "minimal", "low", "medium", "high", "xhigh",
                 "max", "ultra"]
# The api_backend values the gate wire-cross-checks (the schema enum);
# unknown values fall through to the schema validator.
API_BACKENDS = ["responses", "messages", "chat_completions"]
# Documented cache TTL tiers (the schema's cache_ttl description: "5m"
# or "1h"); null = no TTL override.
CACHE_TTL_TIERS = ["5m", "1h"]
# The upstream default_models.json role pins (canonical artifact order).
ROLE_PIN_KEYS = ("default", "web_search", "image_description", "session_summary")
# The gate bakes IN PLACE into the file the seed path already reads (D1).
DEFAULT_OUT_NAME = "default_models.json"

# Credential / identity fields must never be baked (the runtime parse
# struct does not even model them). The ConfigModelOverride schema allows
# these on live config rows — the overlay is a stricter surface, so the
# gate enforces it here.
CREDENTIAL_FIELDS = ("api_key", "env_key", "auth_provider", "mtls_cert_dir")

# CATALOG-CCLASS-SEED-1 (apex-byc, operator ruling 2026-09-19): C-class
# caps — the generated catalog is the proxy's truth for the baked
# context_window / max_completion_tokens, and config.toml rows remain the
# top runtime override. Overlay entries for models that ARE in the
# generated catalog (on the proxy) must not carry these fields (a curated
# cap would silently beat the generated truth — the 071 sol 353000 leak).
# Overlay-only models (not in the generated catalog — e.g. grok-4.5,
# bake-listed; gemma-4-31b, between-build) are permitted: the overlay is
# the sole source of caps for those rows.
FORBIDDEN_OVERLAY_FIELDS = ["context_window", "max_completion_tokens"]


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


def _is_bool(value):
    return isinstance(value, bool)


def _is_uint(value, lo, hi):
    return isinstance(value, int) and not isinstance(value, bool) and lo <= value <= hi


def _slug_family(slug):
    """Python mirror of catalog_wire.rs `catalog_family` (case-insensitive
    slug prefixes): xai / openai / anthropic / other."""
    mid = slug.lower()
    if mid.startswith("grok"):
        return "xai"
    if mid.startswith("gpt-") or (
            len(mid) >= 2 and mid[0] == "o" and mid[1] in "0123456789"):
        return "openai"
    if mid.startswith("claude"):
        return "anthropic"
    return "other"


def _openai_responses_slug(slug):
    """Python mirror of catalog_wire.rs `openai_responses_slug`: the
    o-series, gpt-4o, gpt-4.1*, and gpt-5+ slugs are Responses-wire."""
    mid = slug.lower()
    if len(mid) >= 2 and mid[0] == "o" and mid[1] in "0123456789":
        return True
    rest = mid[4:] if mid.startswith("gpt-") else mid
    return rest == "4o" or rest.startswith("4.1") or (rest and rest[0] in "56789")


def validate_entry_fields(mid, entry):
    """kb6 full-row contract for one overlay entry (dict). Returns problem
    strings (empty = valid). Absent keys are the caller's `missing`
    report; here PRESENT values are checked for nullability / type / menu
    consistency. The api_backend wire cross-check is separate
    (find_wire_mismatches)."""
    problems = []

    for key in ("api_backend", "model_family", "name", "system_prompt_label"):
        if key in entry and not (isinstance(entry[key], str) and entry[key]):
            problems.append(f"{key} must be a non-empty string (got {entry[key]!r})")

    for key in ("supports_reasoning_effort", "multi_agent_v2",
                "supports_backend_search", "strict_responses_input"):
        if key in entry and not _is_bool(entry[key]):
            problems.append(f"{key} must be a bool (got {entry[key]!r})")

    if "auto_compact_threshold_percent" in entry and not _is_uint(
            entry["auto_compact_threshold_percent"], 0, 100):
        problems.append(
            f"auto_compact_threshold_percent must be an int in 0..=100 "
            f"(got {entry['auto_compact_threshold_percent']!r})")

    if "compaction_at_tokens" in entry:
        v = entry["compaction_at_tokens"]
        if not (_is_bool(v) or _is_uint(v, 1, U32_MAX)):
            problems.append(
                f"compaction_at_tokens must be a bool or a positive int (got {v!r})")

    if "compactions_remaining" in entry:
        v = entry["compactions_remaining"]
        if not (_is_bool(v) or _is_uint(v, 0, 255)):
            problems.append(
                f"compactions_remaining must be a bool or an int in 0..=255 (got {v!r})")

    if "extra_headers" in entry:
        h = entry["extra_headers"]
        if not isinstance(h, dict) or not all(
                isinstance(k, str) and isinstance(v, str) for k, v in h.items()):
            problems.append(
                f"extra_headers must be an object of string -> string (got {h!r})")

    # cache_ttl: the one null-allowed key — null (no TTL override) or a
    # documented tier.
    if "cache_ttl" in entry:
        v = entry["cache_ttl"]
        if v is not None and not (isinstance(v, str) and v in CACHE_TTL_TIERS):
            problems.append(
                f"cache_ttl must be null or one of {CACHE_TTL_TIERS} (got {v!r})")

    # reasoning_efforts: array of bare canonical strings or full objects.
    menu_raw = entry.get("reasoning_efforts")
    if "reasoning_efforts" in entry and not isinstance(menu_raw, list):
        problems.append(f"reasoning_efforts must be an array (got {menu_raw!r})")
    for i, item in enumerate(menu_raw if isinstance(menu_raw, list) else []):
        if isinstance(item, str):
            if item not in EFFORT_VALUES:
                problems.append(
                    f"reasoning_efforts[{i}] '{item}' is not a canonical "
                    f"effort ({', '.join(EFFORT_VALUES)})")
        elif isinstance(item, dict):
            v = item.get("value")
            if not (isinstance(v, str) and v in EFFORT_VALUES):
                problems.append(
                    f"reasoning_efforts[{i}].value must be a canonical "
                    f"effort (got {v!r})")
        else:
            problems.append(
                f"reasoning_efforts[{i}] must be a string or an object "
                f"(got {type(item).__name__})")

    # Menu consistency: at most one default marker; non-empty menu =>
    # exactly one marker and reasoning_effort == the marker's value;
    # empty menu => reasoning_effort is null;
    # supports_reasoning_effort == (menu non-empty).
    menu = menu_raw if isinstance(menu_raw, list) else []
    marker_values = [item.get("value") for item in menu
                     if isinstance(item, dict) and item.get("default") is True]
    if menu and len(marker_values) != 1:
        problems.append(
            f"reasoning_efforts: a non-empty menu needs exactly one "
            f"default marker (got {len(marker_values)})")
    elif not menu and marker_values:
        problems.append("reasoning_efforts: an empty menu carries no "
                        "default marker")
    if ("supports_reasoning_effort" in entry
            and _is_bool(entry["supports_reasoning_effort"])
            and entry["supports_reasoning_effort"] != bool(menu)):
        problems.append("supports_reasoning_effort must equal "
                        "(menu non-empty)")
    if "reasoning_effort" in entry:
        v = entry["reasoning_effort"]
        if not menu:
            if v is not None:
                problems.append(
                    f"reasoning_effort must be null for an empty menu (got {v!r})")
        elif not marker_values:
            if v is not None and not (isinstance(v, str) and v in EFFORT_VALUES):
                problems.append(f"reasoning_effort '{v}' is not a canonical effort")
        elif v != marker_values[0]:
            problems.append(
                f"reasoning_effort must equal the default marker's value "
                f"'{marker_values[0]}' (got {v!r})")
    return problems


def _wire_problems(mid, entry):
    """kb6 071 ruling per entry: the curated api_backend must equal the
    EFFECTIVE wire per slug inference (Python mirror of
    catalog_wire.rs). Entries with an absent / unknown api_backend are
    skipped — the schema enum catches those. 'Other' slugs (qwen/glm/
    google) are exempt: their curation is evidence-based operator
    judgment (the 46/17/15 census), never left to inference."""
    backend = entry.get("api_backend")
    if not isinstance(backend, str) or backend not in API_BACKENDS:
        return []
    family = _slug_family(mid)
    if family == "xai":
        if backend != "responses":
            return [f"api_backend must be 'responses' for a grok* slug "
                    f"(got '{backend}')"]
        return []
    if family == "openai":
        expected = ("responses" if _openai_responses_slug(mid)
                    else "chat_completions")
        if backend != expected:
            return [f"api_backend must be '{expected}' for this openai "
                    f"slug (got '{backend}')"]
        return []
    if family == "anthropic" and backend not in ("chat_completions", "messages"):
        return [f"api_backend must be 'chat_completions' or 'messages' "
                f"for a claude* slug (got '{backend}')"]
    return []


def find_wire_mismatches(ov_models):
    """The api_backend wire cross-check over every overlay entry. Returns
    [(model_id, problems)] in sorted order (see _wire_problems)."""
    mismatches = []
    for mid in sorted(ov_models):
        entry = ov_models[mid]
        if not isinstance(entry, dict):
            continue  # rejected upstream (exit 3)
        problems = _wire_problems(mid, entry)
        if problems:
            mismatches.append((mid, problems))
    return mismatches


def collect_missing(gen_models, ov_models, bake_list):
    """D3 + kb6 fail-closed coverage and validation. Returns [(model_id,
    problems)] in sorted order; problems == REQUIRED_FIELDS means 'no
    overlay entry at all', otherwise a mix of absent key names (in
    REQUIRED_FIELDS order) and human-readable invalid strings
    (nullability / type / menu consistency / wire cross-check). Enforced
    on EVERY overlay entry (baking or between-build) and on every baking
    model — the curation is exhaustive by construction."""
    missing = []

    def problems_for(mid, entry):
        problems = [f for f in REQUIRED_FIELDS if f not in entry]
        problems += validate_entry_fields(mid, entry)
        problems += _wire_problems(mid, entry)
        return problems

    row_ids = set(gen_models) | set(bake_list)
    for mid in sorted(row_ids):
        entry = ov_models.get(mid)
        if not isinstance(entry, dict):
            missing.append((mid, list(REQUIRED_FIELDS)))
            continue
        problems = problems_for(mid, entry)
        if problems:
            missing.append((mid, problems))
    for mid, entry in sorted(ov_models.items()):
        if mid in row_ids or not isinstance(entry, dict):
            continue
        problems = problems_for(mid, entry)
        if problems:
            missing.append((mid, problems))
    return missing


def find_forbidden_caps(gen_models, ov_models):
    """CATALOG-CCLASS-SEED-1: rejection is scoped by generated-catalog
    membership — an overlay entry whose model IS in the generated catalog
    (on the proxy) may not carry a C-class cap (the generated capture is
    the truth); overlay-only models are permitted (the overlay is their
    sole cap source). Returns [(model_id, fields)] in sorted order."""
    gen_ids = set(gen_models)
    hits = []
    for mid in sorted(ov_models):
        entry = ov_models[mid]
        if not isinstance(entry, dict):
            continue  # non-table entries are rejected upstream (exit 3)
        fields = [f for f in FORBIDDEN_OVERLAY_FIELDS if f in entry]
        if fields and mid in gen_ids:
            hits.append((mid, fields))
    return hits


def merge_rows(generated, overlay_models, bake_list=()):
    """D1: merged rows = id/model + the runtime cap mapping + the curated
    overlay fields (overlay wins on collision; a generated C-class cap
    always beats an overlay cap; raw C-class fields stay in
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
        gen_has_cw = isinstance(cw, (int, float)) and cw > 0
        if gen_has_cw:
            fields["context_window"] = int(cw)
        mo = src.get("max_output_tokens")
        gen_has_mct = isinstance(mo, (int, float)) and 0 < mo <= U32_MAX
        if gen_has_mct:
            fields["max_completion_tokens"] = int(mo)
        entry = overlay_models[mid]
        for k in sorted(entry):
            if k in ("id", "model"):
                continue  # the generated id is authoritative
            # CATALOG-CCLASS-SEED-1: a generated cap is the proxy's truth
            # — an overlay cap never beats it. Overlay caps ride only when
            # the generated side has none (overlay-only / off-proxy seed
            # carriers).
            if k == "context_window" and gen_has_cw:
                continue
            if k == "max_completion_tokens" and gen_has_mct:
                continue
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

    # CATALOG-CCLASS-SEED-1 (apex-byc): overlay entries for models that
    # ARE in the generated catalog may not carry C-class caps — the
    # generated capture is the truth, config.toml rows are the runtime
    # override. Fail closed (exit 2), explicit per-entry list, artifact
    # NOT written. Overlay-only models are exempt (sole cap source).
    forbidden = find_forbidden_caps(gen_models, ov_models)
    if forbidden:
        print(
            f"FAIL-CLOSED: {len(forbidden)} overlay entr(y/ies) carry "
            f"forbidden C-class field(s) {FORBIDDEN_OVERLAY_FIELDS} for a "
            "model that is in the generated catalog — the generated "
            "capture is the truth for the baked caps; config.toml rows "
            "remain the runtime override (overlay-only models are "
            "permitted):",
            file=sys.stderr,
        )
        for mid, fields in forbidden:
            print(f"  forbidden: {mid}: {', '.join(fields)}", file=sys.stderr)
        sys.exit(2)

    # D3 + kb6 fail-closed: every baking model needs a complete, valid
    # overlay entry (the 15-key full-row contract).
    missing = collect_missing(gen_models, ov_models, bake_list)
    if missing:
        print(
            f"FAIL-CLOSED: {len(missing)} model(s) lack complete and "
            f"valid overlay curation — cure catalog_overlay.json "
            f"(REQUIRED_FIELDS = "
            + ", ".join(REQUIRED_FIELDS) + "):",
            file=sys.stderr,
        )
        for mid, problems in missing:
            absent = [p for p in problems if p in REQUIRED_FIELDS]
            if problems == list(REQUIRED_FIELDS):
                detail = "no overlay entry"
            elif absent:
                detail = ", ".join(absent)
            else:
                detail = "all required fields present"
            print(f"  missing: {mid}: {detail}", file=sys.stderr)
            for problem in problems:
                if problem not in REQUIRED_FIELDS:
                    print(f"  invalid: {mid}: {problem}", file=sys.stderr)
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
