#!/usr/bin/env python3
"""CATALOG GATE (apex-071 CATALOG-BAKE-1 + apex-ayl.128 ZC-SUBSET-DRIFT-1):
merge generated + overlay and bake the result IN PLACE into
`default_models.json`, and write `catalog_drift_report.json` (the subset
drift classification + effort advisory + easy-add scaffolds) next to the
artifact.

The build-time contract (operator-adjudicated 3-delta, 2026-09-19,
inverted to SUBSET DRIFT by apex-ayl.128 — coordinator adjudication
2026-09-22):

  catalog_generated.json  (scripts/catalog_generate.py output; proxy truth;
                           raw C-class fields live HERE ONLY — drift
                           baseline + the apex-ayl.55 cost cut)
  catalog_overlay.json    (hand-curated wire semantics — the single place
                           O/H-class per-model values live; also carries
                           the role pins, the `bake` list of overlay-only
                           models that must ride the merged catalog (seed
                           migration), and the `skip` list: entries
                           {model, reason} = known + intentionally
                           excluded on-proxy models; a skip entry
                           silences the drift alert for its model;
                           TWIN rows (apex-ayl.136 CTXWIN-1M-1M): a
                           curated row whose `model` field names a
                           DIFFERENT on-proxy wire slug than its row
                           key — e.g. a 1M context-window variant of a
                           proxy model — bakes as id=<row key>,
                           model=<wire slug> and rides the base slug's
                           generated caps; overlay C-class caps are
                           forbidden on twins exactly as on base rows)
        -> merge (overlay wins on collision) + validate against the
           apex-hw0 schema (config.schema.json#/definitions/
           ConfigModelOverride — the model-row definition)
        -> default_models.json (upstream shape: the four role pins + a
           `models` array sorted by id; include_str!-ed; ZERO network
           inside `cargo build` — the build is validate-only)
        -> catalog_drift_report.json (the drift classification + the
           effort advisory + paste-ready scaffolds; a bake snapshot
           written on successful bakes and committed with every bake)

SUBSET DRIFT CONTRACT (v4, supersedes the D3 full-coverage contract):
every on-proxy model (every generated-catalog model) must be in
{curated, skipped, alerted}. An UNLISTED model (no overlay row, no skip
entry) is NO longer a hard fail — it classifies `new_unlisted` with a
LOUD stderr alert (F6, post dual review: every diagnostic — including
the alert — goes to stderr; stdout is empty on every run) + a
drift-report entry + a paste-ready scaffold (the easy-add loop). The
bake row set is CURATED rows ∪ the explicit `bake` list (F1, the
product ruling): unlisted (new_unlisted) and skip-listed models are
EXCLUDED from default_models.json — no bare riding. Off-proxy state is
classified too:
  * removed_but_curated — an overlay row whose model is not on the proxy
    (between-build additions land here until the proxy serves them; a
    model the proxy no longer serves is the operator's to reconcile);
  * skip_stale — a skip entry whose model is not on the proxy AND has no
    curated row (an off-proxy skip for a curated model is subsumed by
    removed_but_curated — the row wins; N-R2-3);
  * bake_listed_off_proxy — bake-listed seed carriers (deliberate
    off-proxy, NOT drift — e.g. grok-4.5).
A skip entry for a model that ALSO has a curated row is redundant: a
WARN is printed and the row wins — on-proxy the model appears ONLY in
curated[]; off-proxy it is subsumed by removed_but_curated[]
(F5/N-R2-3: the classification classes are disjoint; counts never
double-count).

STILL HARD FAIL (exit 2): the 15-key completeness of EVERY curated row
(every overlay entry, baking or between-build) and of every bake-listed
model (it must ride the merged catalog with a full row); the CLOSED
OVERLAY KEY SET (F2/M3: every row key must be in (ConfigModelOverride
schema properties − credential fields) ∪ {id, model, context_window,
max_completion_tokens} — a renamed credential key such as
env_key_typo fails the gate BEFORE the artifact is written);
credential fields in the overlay (exit 3 — input bad shape); C-class
caps on on-proxy curated rows; dangling role pins; the REQUIRED_FIELDS
schema cross-check (below); schema + wire cross-checks.

DERIVATION RULE (v4 adjudication, supersedes spec item 4): EFFORT_VALUES
derives at run time from config.schema.json#/definitions/
ReasoningEffort.enum — the schema is the single source of truth, the
hardcoded copy is dead (a schema missing the definition is bad shape,
exit 3). REQUIRED_FIELDS STAYS the single documented curation-contract
list in this file — curation completeness is an APEX-overlay concept;
the runtime schema is deliberately all-optional (embedding `required`
would change parse semantics for all consumers) — BUT the gate asserts
at run start that every REQUIRED_FIELDS member is in
ConfigModelOverride.properties: a schema rename/removal FAILS the gate
(exit 2) instead of passing silently.

EFFORT ADVISORY (v4 item 5, NOT a fail): per curated row, the live hint
menu (catalog-digest.json .models[id].group.supported_reasoning_efforts)
is diffed against the curated reasoning_efforts values; divergence lands
in the drift report as an advisory entry. Divergence is expected for
evidence-based curation (the google-exempt precedent) and is
runtime-safe (apply_supported_effort clamps an offered effort). Rows
without a live hint get no advisory. The digest is READ-ONLY and
discovered (order: --digest > $GROK_CATALOG_DIGEST > worktree
grok/plans/model/data/ > main checkout grok/plans/model/data/ >
parent-of-main-checkout (the upstream plans root, machine-local) >
~/.grok/). An explicit --digest that does not exist is an input error
(exit 3); an undiscoverable digest degrades gracefully — a loud stderr
note and status=skipped in the drift report; the bake proceeds.

Exit codes: 0 = merged + validated (warnings may be present); the drift
    report is written alongside the artifact (bake snapshot);
2 = fail-closed (incomplete curation / closed-key-set violation (F2) /
    dangling pin / forbidden C-class overlay caps / REQUIRED_FIELDS
    schema cross-check failure / schema validation failure — the bake
    artifact + drift report NOT written);
3 = input unreadable / bad shape (incl. bad `skip` shape, a schema
    missing definitions.ReasoningEffort, an explicit missing --digest,
    a corrupt digest — fail-closed on unreadable input);
4 = jsonschema module missing;
5 = the validated artifact could not be WRITTEN (unwritable --out /
    artifact write failure — caught OSError, clean exit, documented
    alongside 0/2/3/4 (F7/N4)).

Usage (CWD-independent; defaults resolve next to this script):
  scripts/catalog_gate.py
  scripts/catalog_gate.py --generated X --overlay Y --schema Z --out W
                          [--drift-report R] [--digest D]
"""
import argparse
import json
import os
import sys
from datetime import datetime, timezone

U32_MAX = 2**32 - 1

# D3 + CATALOG-REQUIRED-CURATION-1 (apex-kb6) + apex-ayl.128
# adjudication: the FULL-ROW curation contract — every overlay entry
# (baking or between-build) and every bake-listed model must carry ALL
# of these (14 non-null keys + the null-allowed cache_ttl). This list is
# the single documented copy of the curation contract (an APEX-overlay
# concept — the runtime schema is deliberately all-optional); the gate
# cross-checks it against ConfigModelOverride.properties at run start
# (check_required_fields_against_schema) so a schema rename/removal
# fails the gate instead of passing silently. SUBSET DRIFT (apex-ayl.128):
# completeness no longer demands an entry for EVERY on-proxy model — an
# unlisted on-proxy model classifies new_unlisted (alert + drift report
# + scaffold, no hard fail).
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
# NOTE (apex-ayl.128 adjudication): EFFORT_VALUES is NO LONGER a
# hardcoded copy here — it derives at run time from
# config.schema.json#/definitions/ReasoningEffort.enum via
# derive_effort_values() (the schema is the single source of truth; this
# kills the manual copy the ZC-RECON-MASTERSCHEMA-1 census flagged).
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
# v4: the drift report bakes next to the artifact (committed with every
# bake).
DRIFT_REPORT_NAME = "catalog_drift_report.json"

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


def derive_effort_values(full_schema):
    """apex-ayl.128 adjudication: the canonical effort values DERIVE at
    run time from config.schema.json#/definitions/ReasoningEffort.enum —
    the schema is the single source of truth (the hardcoded copy is
    dead). A schema missing the definition is bad shape (exit 3)."""
    node = ((full_schema.get("definitions") or {}).get("ReasoningEffort") or {})
    enum = node.get("enum")
    if (not isinstance(enum, list) or not enum
            or not all(isinstance(v, str) and v for v in enum)):
        print("3: config.schema.json: definitions.ReasoningEffort.enum is "
              "missing or malformed — cannot derive the effort values "
              "(the schema is the single source of truth)", file=sys.stderr)
        sys.exit(3)
    return list(enum)


def check_required_fields_against_schema(full_schema):
    """apex-ayl.128 adjudication (the validation clause): REQUIRED_FIELDS
    stays the documented curation contract (an APEX-overlay concept — the
    runtime schema is deliberately all-optional), so the gate asserts at
    run start that every member still exists in
    ConfigModelOverride.properties — a schema rename/removal fails the
    gate instead of passing silently. Returns the missing members
    (empty list = ok)."""
    cmo = ((full_schema.get("definitions") or {}).get("ConfigModelOverride") or {})
    props = cmo.get("properties")
    if not isinstance(props, dict):
        # The whole contract is unverifiable — report everything missing.
        return list(REQUIRED_FIELDS)
    return [f for f in REQUIRED_FIELDS if f not in props]


def allowed_overlay_keys(full_schema):
    """F2 (M3 closed key set, post dual review): the CLOSED set of legal
    overlay row keys — (ConfigModelOverride schema properties −
    CREDENTIAL_FIELDS) ∪ the generated C-class keys {id, model,
    context_window, max_completion_tokens}. A key outside this set (e.g.
    a RENAMED credential such as env_key_typo) is a hard fail (exit 2)
    before the artifact is written: the apex-ayl.130 Rust gate would
    reject it at build time; the bake layer closes the ride first. The
    C-class union covers the keys the generated catalog owns (id/model
    are authoritative in the merge; context_window /
    max_completion_tokens ride on overlay-only models — their sole cap
    source). None when the schema lacks the properties block (the
    REQUIRED_FIELDS cross-check has already failed the gate by then)."""
    cmo = ((full_schema.get("definitions") or {}).get("ConfigModelOverride") or {})
    props = cmo.get("properties")
    if not isinstance(props, dict):
        return None
    return ((set(props) - set(CREDENTIAL_FIELDS))
            | {"id", "model", "context_window", "max_completion_tokens"})


def parse_skip_entries(overlay):
    """v4: the top-level 'skip' list — entries {model, reason} = known +
    intentionally excluded on-proxy models; a skip entry silences the
    new_unlisted alert for its model. Absent key = empty list (backward
    compatible with pre-v4 overlays). Bad shape exits 3."""
    raw = overlay.get("skip", [])
    if not isinstance(raw, list):
        print(f"3: catalog_overlay.json 'skip' must be a list of "
              f"{{model, reason}} objects (got {type(raw).__name__})",
              file=sys.stderr)
        sys.exit(3)
    entries, seen = [], set()
    for i, item in enumerate(raw):
        if not isinstance(item, dict):
            print(f"3: catalog_overlay.json skip[{i}] must be an object "
                  f"(got {type(item).__name__})", file=sys.stderr)
            sys.exit(3)
        model = item.get("model")
        reason = item.get("reason")
        if not isinstance(model, str) or not model:
            print(f"3: catalog_overlay.json skip[{i}].model must be a "
                  f"non-empty string (got {model!r})", file=sys.stderr)
            sys.exit(3)
        if not isinstance(reason, str) or not reason:
            print(f"3: catalog_overlay.json skip[{i}].reason must be a "
                  f"non-empty string (the skip audit trail; got {reason!r})",
                  file=sys.stderr)
            sys.exit(3)
        if model in seen:
            print(f"3: catalog_overlay.json skip: duplicate entry for "
                  f"model '{model}'", file=sys.stderr)
            sys.exit(3)
        seen.add(model)
        entries.append({"model": model, "reason": reason})
    return entries


def effective_model(ov_models, mid):
    """apex-ayl.136 (CTXWIN-1M-1M): the WIRE SLUG a curated row serves —
    the row's `model` field when it is a non-empty string, else the row
    key (legacy rows carry id == model, so this is identity for them).
    A twin row (row key != `model`, e.g. a 1M context-window variant of
    a proxy model) names the base slug it rides."""
    entry = ov_models.get(mid)
    if isinstance(entry, dict):
        model = entry.get("model")
        if isinstance(model, str) and model:
            return model
    return mid


def bake_row_ids(gen_models, ov_models, bake_list):
    """apex-ayl.136 (CTXWIN-1M-1M): the merged catalog's row set — the v5
    F1 set (curated on-proxy rows ∪ the explicit bake list) PLUS the
    twin rows: curated rows whose `model` field names a DIFFERENT
    on-proxy wire slug (the base slug must be in the generated catalog;
    an off-proxy twin is a between-build addition, handled by the
    existing merge warn). Legacy inputs (no `model` fields) return the
    byte-identical v5 F1 set."""
    rows = (set(gen_models) & set(ov_models)) | set(bake_list)
    for mid, entry in ov_models.items():
        if not isinstance(entry, dict):
            continue  # non-table entries are rejected upstream (exit 3)
        slug = entry.get("model")
        if (isinstance(slug, str) and slug and slug != mid
                and slug in gen_models):
            rows.add(mid)
    return rows


def classify_drift(gen_models, ov_models, skip_entries, bake_list):
    """v4 subset drift contract: every on-proxy model is in {curated,
    skipped, alerted(new_unlisted)} — unlisted is NO hard fail. Off-proxy
    state: removed_but_curated (overlay row, model not on the proxy;
    bake-listed seed carriers are deliberate and classify
    bake_listed_off_proxy instead), skip_stale (skip entry, model not on
    the proxy AND not curated — an off-proxy skip for a curated model is
    subsumed by removed_but_curated, the row wins; N-R2-3). A skip entry
    for a model that is ALSO curated is redundant (a WARN; the row wins —
    a skip never removes a curated row; F5/N1: the classes are DISJOINT —
    such a model appears ONLY in its curated-side class, on- or
    off-proxy, so the counts never double-count it). Returns
    (classification, warns). apex-ayl.136: classification is per WIRE
    SLUG — a twin row (row key != `model`) counts its base slug; the
    row key is not a slug, so it can never false-positive as
    removed_but_curated (legacy inputs classify identically)."""
    on_proxy = set(gen_models)
    # apex-ayl.136: per-slug membership (see effective_model) — a twin
    # row counts its base slug, not its row key.
    ov_slugs = {effective_model(ov_models, mid) for mid in ov_models}
    bake_ids = set(bake_list)
    skip_model_set = {e["model"] for e in skip_entries}
    classification = {
        "curated": sorted(on_proxy & ov_slugs),
        # F5/N1: a skip entry for a curated model is redundant (WARNed
        # below) — the row wins, the model counts ONCE, in curated[];
        # the classes stay disjoint so the counts never double-count.
        "skipped": sorted((e for e in skip_entries
                           if e["model"] in on_proxy
                           and e["model"] not in ov_slugs),
                          key=lambda e: e["model"]),
        "new_unlisted": sorted(m for m in on_proxy
                               if m not in ov_slugs and m not in skip_model_set),
        "removed_but_curated": sorted(m for m in ov_slugs
                                      if m not in on_proxy and m not in bake_ids),
        # N-R2-3: off-proxy skip for a CURATED model is subsumed by
        # removed_but_curated (the row wins) — disjoint with F5/N1.
        "skip_stale": sorted((e for e in skip_entries
                              if e["model"] not in on_proxy
                              and e["model"] not in ov_slugs),
                             key=lambda e: e["model"]),
        "bake_listed_off_proxy": sorted(bake_ids - on_proxy),
    }
    warns = []
    for e in sorted(skip_entries, key=lambda e: e["model"]):
        mid = e["model"]
        if mid in ov_slugs:
            warns.append(
                f"skip entry for '{mid}' is redundant: the model is also "
                f"curated (the row wins) — remove the skip entry")
    return classification, warns


def validate_entry_fields(mid, entry, effort_values):
    """kb6 full-row contract for one overlay entry (dict). Returns problem
    strings (empty = valid). Absent keys are the caller's `missing`
    report; here PRESENT values are checked for nullability / type / menu
    consistency. The api_backend wire cross-check is separate
    (find_wire_mismatches). `effort_values` derives at run time from the
    schema (apex-ayl.128) — no hardcoded enum here."""
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
            if item not in effort_values:
                problems.append(
                    f"reasoning_efforts[{i}] '{item}' is not a canonical "
                    f"effort ({', '.join(effort_values)})")
        elif isinstance(item, dict):
            v = item.get("value")
            if not (isinstance(v, str) and v in effort_values):
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
            if v is not None and not (isinstance(v, str) and v in effort_values):
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


def collect_missing(ov_models, bake_list, effort_values):
    """v4 subset contract (inverts the D3 full-coverage contract):
    completeness (the 15-key contract + field validation + the wire
    cross-check) is enforced on EVERY overlay entry (baking or
    between-build) and on EVERY bake-listed model (it must ride the
    merged catalog with a full row). An on-proxy model WITHOUT an
    overlay entry is NO longer a coverage failure — classify_drift
    reports it as new_unlisted (loud alert + drift report + scaffold,
    no hard fail). Returns [(model_id, problems)] in sorted order;
    problems == REQUIRED_FIELDS means 'no overlay entry at all'
    (bake-listed models only), otherwise a mix of absent key names
    (REQUIRED_FIELDS order) and human-readable invalid strings."""
    missing = []

    def problems_for(mid, entry):
        problems = [f for f in REQUIRED_FIELDS if f not in entry]
        problems += validate_entry_fields(mid, entry, effort_values)
        # apex-ayl.136: wire-check by the row's WIRE SLUG (a twin row
        # rides its base slug's wire).
        problems += _wire_problems(effective_model(ov_models, mid), entry)
        return problems

    for mid, entry in sorted(ov_models.items()):
        if not isinstance(entry, dict):
            continue  # rejected upstream (exit 3)
        problems = problems_for(mid, entry)
        if problems:
            missing.append((mid, problems))
    for mid in sorted(set(bake_list)):
        entry = ov_models.get(mid)
        if not isinstance(entry, dict):
            missing.append((mid, list(REQUIRED_FIELDS)))
    return missing


def find_forbidden_caps(gen_models, ov_models):
    """CATALOG-CCLASS-SEED-1: rejection is scoped by generated-catalog
    membership — an overlay entry whose model IS in the generated catalog
    (on the proxy) may not carry a C-class cap (the generated capture is
    the truth); overlay-only models are permitted (the overlay is their
    sole cap source). apex-ayl.136: membership is by the row's WIRE
    SLUG (the `model` field) — a twin rides its base slug's on-proxy
    status. Returns [(model_id, fields)] in sorted order."""
    gen_ids = set(gen_models)
    hits = []
    for mid in sorted(ov_models):
        entry = ov_models[mid]
        if not isinstance(entry, dict):
            continue  # non-table entries are rejected upstream (exit 3)
        fields = [f for f in FORBIDDEN_OVERLAY_FIELDS if f in entry]
        if fields and effective_model(ov_models, mid) in gen_ids:
            hits.append((mid, fields))
    return hits


def merge_rows(generated, overlay_models, bake_list=()):
    """D1: merged rows = id/model + the runtime cap mapping + the curated
    overlay fields (overlay wins on collision; a generated C-class cap
    always beats an overlay cap; raw C-class fields stay in
    catalog_generated.json only). v4: the row set is UNCHANGED
    (generated + bake list); v5 F1 (the product ruling, post dual
    review): the bake row set is CURATED rows (overlay rows on the
    proxy) ∪ the explicit bake list — an unlisted (new_unlisted) model
    and a skip-listed model are EXCLUDED from default_models.json (no
    bare riding: the menu is the curated SUBSET catalog only); a
    removed_but_curated row is excluded unless it is force-baked via
    the bake list (the existing mechanism, unchanged). The curation gap
    is drift, tracked by the drift report, never a crash. Returns
    (models, warns);
    coverage and pin integrity are enforced by the caller (fail-closed,
    exit 2). apex-ayl.136 (CTXWIN-1M-1M) twin rows: a curated row whose
    `model` field names a DIFFERENT on-proxy wire slug bakes as
    id=<row key>, model=<wire slug> and inherits the base slug's
    generated caps; the row set is the v5 F1 set ∪ the twin rows;
    legacy (key == slug) inputs merge byte-identically."""
    warns = []
    row_ids = bake_row_ids(generated, overlay_models, bake_list)
    models = {}
    for mid in sorted(row_ids):
        # apex-ayl.136: a row serves its WIRE SLUG — the overlay `model`
        # field (twin rows) or the row key (legacy id == model). A twin
        # inherits the base slug's generated caps.
        slug = effective_model(overlay_models, mid)
        src = generated.get(slug) or {}
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
        entry = overlay_models.get(mid) or {}
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
        models[mid] = {"id": mid, "model": slug, **fields}
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


# ---------------------------------------------------------------------------
# v4 (apex-ayl.128): the drift report, the effort advisory, the digest,
# and the easy-add scaffolds.
# ---------------------------------------------------------------------------

def _main_checkout_root(root=None):
    """The primary checkout root for a linked worktree (the .git FILE
    points at <main>/.git/worktrees/<name>); the repo root for a plain
    checkout. None if undeterminable. `root` defaults to the checkout
    this script lives in (a test may pass a synthetic one)."""
    if root is None:
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    gitfile = os.path.join(root, ".git")
    if os.path.isfile(gitfile):
        try:
            with open(gitfile) as f:
                line = f.read().strip()
        except OSError:
            return None
        if line.startswith("gitdir:"):
            gitdir = line.split(":", 1)[1].strip()
            # gitdir = <main>/.git/worktrees/<name> -> main root =
            # dirname x3 (drops <name>, worktrees, .git)
            return os.path.dirname(os.path.dirname(os.path.dirname(gitdir)))
        return None
    if os.path.isdir(gitfile):
        return root
    return None


def _digest_candidates():
    """The documented discovery order for the read-only live-hint source
    (catalog-digest.json .models[id].group.
    supported_reasoning_efforts): $GROK_CATALOG_DIGEST, the worktree
    grok/plans/model/data/, the main checkout grok/plans/model/data/, the
    parent-of-main-checkout (the upstream plans root — machine-local
    recon data), and ~/.grok/."""
    cands = []
    env = os.environ.get("GROK_CATALOG_DIGEST")
    if env:
        cands.append(env)
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    cands.append(os.path.join(root, "grok", "plans", "model", "data",
                              "catalog-digest.json"))
    main_root = _main_checkout_root()
    if main_root:
        cands.append(os.path.join(main_root, "grok", "plans", "model",
                                  "data", "catalog-digest.json"))
        parent = os.path.dirname(main_root.rstrip(os.sep))
        if parent and parent != main_root:
            cands.append(os.path.join(parent, "grok", "plans", "model",
                                      "data", "catalog-digest.json"))
    cands.append(os.path.expanduser(os.path.join("~", ".grok",
                                                 "catalog-digest.json")))
    seen, out = set(), []
    for c in cands:
        c = os.path.abspath(c)
        if c not in seen:
            seen.add(c)
            out.append(c)
    return out


def discover_digest(explicit=None):
    """v4 item 5: resolve the READ-ONLY live-hint source. Returns
    (path_or_None, searched). An explicit --digest that does not exist is
    the caller's input error (exit 3); discovery misses degrade
    gracefully (the advisory is skipped with a loud note)."""
    if explicit:
        found = explicit if os.path.isfile(explicit) else None
        return found, [explicit]
    candidates = _digest_candidates()
    for c in candidates:
        if os.path.isfile(c):
            return c, candidates
    return None, candidates


def effort_advisories(ov_models, digest, effort_values):
    """v4 item 5 advisory (NOT a fail): per curated row, diff the live
    hint menu (digest .models[id].group.supported_reasoning_efforts)
    against the curated reasoning_efforts values. Divergence is expected
    for evidence-based curation (the google-exempt precedent) and is
    runtime-safe (apply_supported_effort clamps). Rows absent from the
    digest, or whose digest row carries no supported_reasoning_efforts,
    get no advisory; off-enum hint values are proxy noise, filtered.
    Returns advisory entries in sorted model order."""
    dmodels = digest.get("models") if isinstance(digest, dict) else None
    if not isinstance(dmodels, dict):
        dmodels = {}
    out = []
    for mid in sorted(ov_models):
        entry = ov_models[mid]
        if not isinstance(entry, dict):
            continue
        drow = dmodels.get(mid)
        if not isinstance(drow, dict):
            continue
        live = (drow.get("group") or {}).get("supported_reasoning_efforts")
        if not isinstance(live, list):
            continue
        live = sorted({v for v in live
                       if isinstance(v, str) and v in effort_values})
        menu = entry.get("reasoning_efforts")
        if not isinstance(menu, list):
            menu = []
        curated = set()
        for item in menu:
            if isinstance(item, str):
                curated.add(item)
            elif isinstance(item, dict) and isinstance(item.get("value"), str):
                curated.add(item["value"])
        live_only = sorted(set(live) - curated)
        curated_only = sorted(curated - set(live))
        if live_only or curated_only:
            out.append({
                "model": mid,
                "live_hint": live,
                "curated": sorted(curated),
                "live_only": live_only,
                "curated_only": curated_only,
            })
    return out


def digest_freshness_warns(gen_models, digest, generated_at=None,
                           captured_at=None):
    """F4 (M2/M5, post dual review): the digest freshness cross-checks —
    ADVISORY only (the caller prints them as stderr WARNs; the bake
    NEVER fails on staleness). Two checks:
      * stale-vs-capture — a digest model carrying a live
        supported_reasoning_efforts hint that is ABSENT from
        catalog_generated.json (the capture moved on without the
        digest; one SUMMARY line, not one per model — N-R2-2);
      * predates-capture — digest.captured_at older than
        generated.generated_at (the live hints predate the capture).
    Unparseable / absent / naive-vs-aware-mixed timestamps degrade
    gracefully (no WARN) — an ADVISORY check must never crash the bake
    (M-R2-1).
    Returns WARN strings (empty = fresh as far as is visible)."""
    warns = []
    gen_ids = set(gen_models)
    dmodels = digest.get("models") if isinstance(digest, dict) else None
    stale_models = []
    if isinstance(dmodels, dict):
        for mid in sorted(dmodels):
            drow = dmodels[mid]
            live = ((drow.get("group") or {})
                    .get("supported_reasoning_efforts")
                    if isinstance(drow, dict) else None)
            if (mid not in gen_ids and isinstance(live, list) and live):
                stale_models.append(mid)
    if stale_models:
        sample = ", ".join(stale_models[:3])
        more = (f" (+{len(stale_models) - 3} more)"
                if len(stale_models) > 3 else "")
        warns.append(
            f"digest stale vs capture: {len(stale_models)} digest model(s) "
            f"carry supported_reasoning_efforts but are absent from "
            f"catalog_generated.json ({sample}{more}) — re-capture "
            "(catalog_generate.py) or prune the digest")
    if generated_at and captured_at:
        try:
            g = datetime.fromisoformat(generated_at)
            c = datetime.fromisoformat(captured_at)
            # M-R2-1: the comparison INSIDE the try — a naive/aware mix
            # raises TypeError here and degrades to no-WARN instead of
            # crashing the bake (pre-fix: traceback + exit 1, no artifact).
            predates = c < g
        except (ValueError, TypeError):
            predates = None
        if predates:
            warns.append(
                f"digest predates capture: digest.captured_at "
                f"({captured_at}) is older than generated.generated_at "
                f"({generated_at}) — the live hints may be stale; "
                "re-capture the digest before trusting the advisories")
    return warns


def _scaffold_family_wire(mid):
    """Conventional scaffold pre-fills (the easy-add loop): grok* ->
    (xai, responses); gpt-*/o* -> (codex, the openai_responses_slug wire);
    claude* -> (anthropic, None — the wire is curated explicitly:
    chat_completions or the messages pin); Other slugs -> (None, None —
    evidence-based operator curation, the wire cross-check exemption)."""
    family = _slug_family(mid)
    if family == "xai":
        return "xai", "responses"
    if family == "openai":
        return "codex", ("responses" if _openai_responses_slug(mid)
                         else "chat_completions")
    if family == "anthropic":
        return "anthropic", None
    return None, None


def build_scaffold(mid, gen_row, digest, effort_values):
    """v4 item 6 easy-add loop: a paste-ready overlay row for an
    unlisted on-proxy model — pre-filled from the generated C-class
    evidence (mode/costs/providers/caps) + the live hint menu, with the
    ~5 TODO judgment fields NULLed for operator paste (the gate stays
    fail-closed until the operator fills them). Nulled fields +
    'reasoning_efforts.default' are listed in todo_fields."""
    gen_row = gen_row or {}
    live = None
    dmodels = digest.get("models") if isinstance(digest, dict) else None
    if isinstance(dmodels, dict):
        drow = dmodels.get(mid)
        if isinstance(drow, dict) and isinstance(drow.get("group"), dict):
            live = drow["group"].get("supported_reasoning_efforts")
    if not isinstance(live, list):
        live = gen_row.get("supported_reasoning_efforts")
    live = [v for v in (live or [])
            if isinstance(v, str) and v in effort_values]
    family, wire = _scaffold_family_wire(mid)
    menu = [{"id": v, "value": v, "label": humanize_effort_id(v),
             "default": False} for v in live]
    row = {
        "api_backend": wire,
        "model_family": family,
        "name": None,
        "reasoning_efforts": menu,
        "supports_reasoning_effort": bool(menu),
        "reasoning_effort": None,
        "cache_ttl": None,
        "multi_agent_v2": None,
        "supports_backend_search": None,
        "strict_responses_input": None,
        "extra_headers": {},
        "auto_compact_threshold_percent": 80,
        "compaction_at_tokens": True,
        "compactions_remaining": 1,
        "system_prompt_label": None,
    }
    todo = []
    if row["api_backend"] is None:
        todo.append("api_backend")
    if row["model_family"] is None:
        todo.append("model_family")
    todo.append("name")
    if menu:
        # Pick the default: mark one menu entry default:true and set
        # reasoning_effort to its value (the gate enforces the match).
        todo.append("reasoning_effort")
        todo.append("reasoning_efforts.default")
    todo.append("multi_agent_v2")
    todo.append("supports_backend_search")
    todo.append("strict_responses_input")
    todo.append("system_prompt_label")
    evidence = {k: gen_row[k] for k in
                ("mode", "providers", "max_input_tokens", "max_output_tokens",
                 "input_cost_per_token", "output_cost_per_token",
                 "supported_reasoning_efforts") if k in gen_row}
    return {
        "model": mid,
        "evidence": evidence,
        "overlay_row": row,
        "todo_fields": todo,
        "note": ("Paste into catalog_overlay.json under models.\"<id>\", "
                 "fill the nulled TODO fields, and (when the menu is "
                 "non-empty) mark one reasoning_efforts entry "
                 "default:true with reasoning_effort = its value. The "
                 "gate stays fail-closed until the row is complete."),
    }


def build_drift_report(generated_path, overlay_path, schema_path, digest_path,
                       classification, counts, advisories, scaffolds,
                       generated_at=None, digest_captured_at=None):
    """The catalog_drift_report.json document (v4): the subset drift
    classification + the effort advisory + the easy-add scaffolds. A
    bake snapshot — written on successful bakes only, committed with
    every bake."""
    # F4 (M5, post dual review): the inputs section carries the
    # freshness-relevant timestamps alongside the paths —
    # inputs.generated.generated_at (the catalog capture) and
    # inputs.digest.captured_at (the live-hint capture); None when the
    # input lacks the field or the digest is undiscoverable.
    inputs = {
        "generated": {"path": generated_path, "generated_at": generated_at},
        "overlay": overlay_path,
        "schema": schema_path,
        "digest": ({"path": digest_path,
                    "captured_at": digest_captured_at}
                   if digest_path is not None else None),
    }
    if digest_path:
        effort_advisory = {
            "status": "ok",
            "source": digest_path,
            "skip_reason": None,
            "advisories": advisories,
        }
    else:
        effort_advisory = {
            "status": "skipped",
            "source": None,
            "skip_reason": ("catalog-digest.json not found (searched the "
                            "documented candidate paths) — effort advisory "
                            "skipped; the bake proceeds without it"),
            "searched": _digest_candidates(),
            "advisories": [],
        }
    return {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "gate": "scripts/catalog_gate.py (apex-ayl.128 ZC-SUBSET-DRIFT-1)",
        "inputs": inputs,
        "counts": counts,
        "classification": classification,
        "effort_advisory": effort_advisory,
        "scaffolds": scaffolds,
    }


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    models_dir = os.path.join(root, "crates", "codegen", "xai-grok-models")
    ap = argparse.ArgumentParser(
        description="CATALOG GATE (apex-071 + apex-ayl.128 subset drift)")
    ap.add_argument("--generated", default=os.path.join(models_dir, "catalog_generated.json"))
    ap.add_argument("--overlay", default=os.path.join(models_dir, "catalog_overlay.json"))
    ap.add_argument(
        "--schema",
        default=os.path.join(root, "crates", "codegen", "xai-grok-shell", "config.schema.json"),
    )
    ap.add_argument("--out", default=os.path.join(models_dir, DEFAULT_OUT_NAME))
    ap.add_argument(
        "--drift-report", default=None,
        help="drift report path (default: catalog_drift_report.json next to --out)")
    ap.add_argument(
        "--digest", default=None,
        help="catalog-digest.json (read-only live-hint source; default: discover)")
    args = ap.parse_args()
    if args.drift_report is None:
        args.drift_report = os.path.join(
            os.path.dirname(os.path.abspath(args.out)), DRIFT_REPORT_NAME)

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
    skip_entries = parse_skip_entries(overlay)

    # v4 adjudication: the effort vocabulary DERIVES from the schema at
    # run time (the hardcoded copy is dead) — exit 3 if the definition is
    # gone (bad shape).
    effort_values = derive_effort_values(full_schema)

    # v4 adjudication (the validation clause): the curation contract must
    # still exist in the runtime schema — a rename/removal FAILS the gate
    # instead of passing silently.
    missing_props = check_required_fields_against_schema(full_schema)
    if missing_props:
        print("FAIL-CLOSED: REQUIRED_FIELDS member(s) absent from "
              "config.schema.json definitions.ConfigModelOverride.properties: "
              + ", ".join(missing_props)
              + " (the curation contract drifted from the runtime schema — "
                "artifact NOT written)", file=sys.stderr)
        sys.exit(2)

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

    # F2 (M3, post dual review): the CLOSED overlay key set — every row
    # key must be a ConfigModelOverride schema property (minus the
    # credential fields) or a generated C-class key. The name-based
    # credential check above misses a RENAMED credential (env_key_typo
    # & friends); the schema has no additionalProperties, so only this
    # check closes the ride at the bake layer. Hard fail (exit 2),
    # naming the key + the row; the artifact is NOT written.
    allowed = allowed_overlay_keys(full_schema)
    if allowed is not None:
        disallowed = []
        for mid in sorted(ov_models):
            entry = ov_models[mid]
            if not isinstance(entry, dict):
                continue  # rejected upstream (exit 3)
            extra = [k for k in entry if k not in allowed]
            if extra:
                disallowed.append((mid, sorted(extra)))
        if disallowed:
            print(
                f"FAIL-CLOSED: {len(disallowed)} overlay entr(y/ies) carry "
                "key(s) outside the closed set (ConfigModelOverride "
                "properties − credential fields ∪ the generated C-class "
                "keys id/model/context_window/max_completion_tokens) — "
                "an unknown or renamed key would otherwise ride the "
                "baked artifact (the apex-ayl.130 build gate would "
                "reject it at compile time):",
                file=sys.stderr,
            )
            for mid, extra in disallowed:
                print(f"  disallowed: {mid}: {', '.join(extra)}",
                      file=sys.stderr)
            sys.exit(2)

    # v4: the subset drift classification — a property of the input
    # state, printed even when a later fail-closed check stops the bake.
    classification, drift_warns = classify_drift(
        gen_models, ov_models, skip_entries, bake_list)
    for w in drift_warns:
        print(f"WARN: {w}", file=sys.stderr)
    # F3 (M4 skip-rot signal): a non-empty skip_stale classification
    # warns on stderr, naming EACH stale entry (the report keeps the
    # classification as-is — this is the operator-facing rot signal).
    for e in classification["skip_stale"]:
        print(
            f"WARN: skip_stale: skip entry '{e['model']}' is not on the "
            f"proxy (reason: {e['reason']}) — remove the stale entry, "
            "or keep it only if the model is expected to return",
            file=sys.stderr)
    if classification["new_unlisted"]:
        print(f"DRIFT ALERT (apex-ayl.128 ZC-SUBSET-DRIFT-1): "
              f"{len(classification['new_unlisted'])} on-proxy model(s) "
              "unlisted (neither curated nor skipped):", file=sys.stderr)
        for mid in classification["new_unlisted"]:
            print(f"  new_unlisted: {mid}", file=sys.stderr)
        print(f"  cure: add a curated row or a top-level skip entry "
              f'{{"model": ..., "reason": ...}} to {args.overlay}',
              file=sys.stderr)
        print(f"  scaffold: paste-ready entries in {args.drift_report} "
              "(scaffolds[])", file=sys.stderr)

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

    # v4 subset contract: completeness of EVERY overlay entry + EVERY
    # bake-listed model (an unlisted on-proxy model is drift, not failure).
    missing = collect_missing(ov_models, bake_list, effort_values)
    if missing:
        print(
            f"FAIL-CLOSED: {len(missing)} model(s) lack complete and "
            f"valid overlay curation (every overlay entry and every "
            f"bake-listed model must be complete — an UNLISTED on-proxy "
            f"model is drift, not failure) — cure catalog_overlay.json "
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
    # F1: a pin must reference a model that is ACTUALLY baked (curated
    # ∪ bake list ∪ twin rows, apex-ayl.136) — an unlisted / skipped /
    # removed target would dangle at boot the same way.
    row_ids = bake_row_ids(gen_models, ov_models, bake_list)
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

    # v4: the drift report + the artifact — a bake snapshot, written on
    # success only (committed with every bake).
    if args.digest:
        digest_path, searched = discover_digest(args.digest)
        if digest_path is None:
            print(f"3: --digest file not found at {args.digest} (an "
                  f"explicit input must exist)", file=sys.stderr)
            sys.exit(3)
    else:
        digest_path, searched = discover_digest()
    if digest_path:
        digest = load_json(digest_path, "catalog-digest.json")
        advisories = effort_advisories(ov_models, digest, effort_values)
    else:
        digest = None
        advisories = []
        print(
            "WARN: effort advisory skipped — catalog-digest.json not "
            f"found (searched: {', '.join(searched)}); the bake proceeds "
            "without it",
            file=sys.stderr,
        )
    # F4 (M2/M5, post dual review): the digest freshness cross-checks —
    # ADVISORY only (stderr WARN, exit 0): a digest model carrying a
    # live hint that is absent from the capture ("digest stale vs
    # capture"), or a digest captured before the generated capture
    # ("digest predates capture"). Staleness never fails the bake.
    if digest is not None:
        for w in digest_freshness_warns(
                gen_models, digest,
                generated.get("generated_at"),
                digest.get("captured_at") if isinstance(digest, dict)
                else None):
            print(f"WARN: {w}", file=sys.stderr)
    scaffolds = [
        build_scaffold(mid, gen_models.get(mid) or {}, digest, effort_values)
        for mid in classification["new_unlisted"]
    ]
    counts = {
        "on_proxy": len(gen_models),
        "overlay_rows": len(ov_models),
        "curated": len(classification["curated"]),
        "skipped": len(classification["skipped"]),
        "new_unlisted": len(classification["new_unlisted"]),
        "removed_but_curated": len(classification["removed_but_curated"]),
        "skip_stale": len(classification["skip_stale"]),
        "bake_listed_off_proxy": len(classification["bake_listed_off_proxy"]),
    }
    report = build_drift_report(
        args.generated, args.overlay, args.schema, digest_path,
        classification, counts, advisories, scaffolds,
        generated_at=generated.get("generated_at"),
        digest_captured_at=digest.get("captured_at")
        if isinstance(digest, dict) else None)
    try:
        with open(args.drift_report, "w") as f:
            json.dump(report, f, indent=1)
            f.write("\n")
    except OSError as e:
        print(f"WARN: cannot write drift report at {args.drift_report}: "
              f"{e!r}", file=sys.stderr)

    # D1: the upstream shape — role pins + a models array sorted by id.
    artifact = {**pins, "models": [models[mid] for mid in sorted(models)]}
    # F7 (N4, post dual review): the write is the last step — a failure
    # here is a clean, DOCUMENTED exit 5 (stderr line, no traceback),
    # not an uncaught OSError (exit 1, outside the 0/2/3/4 contract).
    try:
        with open(args.out, "w") as f:
            json.dump(artifact, f, indent=1)
            f.write("\n")
    except OSError as e:
        print(f"5: cannot write artifact at {args.out}: {e!r}",
              file=sys.stderr)
        sys.exit(5)
    print(
        f"OK merged={len(models)} overlay_entries={len(ov_models)} "
        f"schema_errors=0 warns={len(warns)} "
        f"drift=curated:{len(classification['curated'])}"
        f"/skipped:{len(classification['skipped'])}"
        f"/unlisted:{len(classification['new_unlisted'])}"
        f"/removed:{len(classification['removed_but_curated'])}"
        f"/skip_stale:{len(classification['skip_stale'])} "
        f"out={args.out} size={os.path.getsize(args.out)}B "
        f"drift_report={args.drift_report}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
