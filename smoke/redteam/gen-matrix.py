#!/usr/bin/env python3
"""Generate cases/rt-m6.json: the FULL live-catalog sweep (item-11 shape).

Fetches /v1/models from the proxy (CODEX_LLM_PROXY_KEY), drops embedding
models (no chat turns), and SKIPS the live config's [models]
disabled_models (a disabled model's headless turn fails fast with NO
model request, so the row's wire pins could never match — the 2026-09-17
SWEEP-1 run's 44 zero-wire rows were exactly that list). One row + the
standard wire asserts per remaining model: the path pin follows the LIVE
CONFIG ROUTING AUTHORITY — the runner's own 3-way resolver
(run.api_backend_for_model: [model."<id>"] api_backend > [endpoints]
default_api_backend > "responses") — + body.model echo. When the live
config is unreadable, paths fall back to the name heuristic (claude* ->
/v1/messages; gemini*/gemma* -> /v1/chat/completions; else -> /v1/
responses) and cc pins ship PREDICTED (config-backed, not speculative).

An assert that starts failing = a config/wire change to adjudicate,
which is the point of the sign-off sweep.

Emits schema v1 (smoke/redteam/case.schema.json, the apex-ayl.21/.22
shared contract): schema_version/suite/tier/driver/bead/retry join the
id/title/rows/est_calls/watchdog_s/row_asserts fields. No
mcp_calls/tool_calls on this slice: wire-shape only (the non-empty gate
applies to mcp-tool/tool-call suites).

Schema note (SWEEP-1): the frozen case.schema.json REQUIRES a
top-level `assert` on every case and its wire_assert allows no
`row_model` key (additionalProperties: false — the row_asserts
description's "Each entry carries row_model" predates that constraint).
The emitted rows case therefore carries an EMPTY `assert: {}` (the
rows path never evaluates it — presentation of the frozen contract
only) and scopes each row_assert spec per row through its where
filter ($.body.model); run._row_model_for_spec derives the row
identity from that filter, so `--rows a,b` still yields wire.row-skip
recon records for unexecuted rows.

Usage:  python3 smoke/redteam/gen-matrix.py
        python3 -u smoke/redteam/run.py RT-M6 --budget 120

The generated file is a build artifact of this script: do not hand-edit,
regenerate after catalog changes.
"""
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.environ.get(
    "GROK_PROXY_MATRIX_OUT", os.path.join(HERE, "cases", "rt-m6.json"))
MODELS_URL = os.environ.get(
    "GROK_PROXY_MODELS_URL", "https://llm-proxy-api.ai.eng.netapp.com/v1/models")
LIVE_CONFIG = os.environ.get(
    "GROK_LIVE_CONFIG", os.path.expanduser("~/.grok/config.toml"))

# apex-ayl.21 full-row emit fields (schema v1, SDD §3.1 / sweep manifest §8):
# the row coverage IS the .21 bead's matrix bar; the .36 event consumes it.
SCHEMA_VERSION = 1
SUITE = "wire-shape"
TIER = "full"
DRIVER = "headless"
BEAD = "apex-ayl.21"
# env-flap retry block (R-D: 404/502/503 = environmental transient -> max 1
# attempt + recheck /v1/models; a 400 is a tool-path finding, never retried).
RETRY = {"on_status": [404, 502, 503], "max_attempts": 1,
         "backoff_s": 30, "recheck_models": True}
CC_PATH = "/v1/chat/completions"
CC_RATIFY_NOTE = ("PREDICTED, config-backed post-.20/.35: gemini*/gemma* route "
                  "to %s; ratifies via the T21-CC path_rec first observation "
                  "at .36 Wave S, then may re-pin as eq %r" % (CC_PATH, CC_PATH))
CC_AUTHORITY_NOTE = (
    "LIVE CONFIG AUTHORITY (SWEEPFIX-VERIFY): [model.\"%s\"] "
    "api_backend = chat_completions in the live config — the case "
    "follows the live routing authority; the stale 2-way pin "
    "(\"/v1/responses\") from the pre-.20/.35 generator was the "
    "2026-09-17 red")


def embedding_model(mid):
    low = mid.lower()
    return ("embedding" in low or "colbert" in low)


def expected_path(mid):
    # 3-way name heuristic (G2 fix; the old 2-way had no cc branch, so
    # gemini/gemma rows pinned /v1/responses -- a true pre-fix RED on the
    # cc wire). FALLBACK ONLY when the live config is unreadable; the
    # emitted path pins otherwise follow the live config routing
    # authority (_runner_path).
    if mid.startswith("claude"):
        return "/v1/messages"
    if mid.startswith("gemini") or mid.startswith("gemma"):
        return CC_PATH
    return "/v1/responses"  # incl. terra/luna no-api_backend rows (default)


def read_live_config(path=LIVE_CONFIG):
    """The live config text (read-only, S-7: drift is recorded, never
    edited). None if unreadable (the caller degrades to the heuristic)."""
    try:
        with open(path) as f:
            return f.read()
    except OSError as e:
        print("WARN: live config unreadable (%s): %s" % (path, e),
              file=sys.stderr)
        return None


def disabled_model_ids(cfg_text):
    """The live config's [models] disabled_models (SWEEP-1: the 44/44
    zero-wire M6 rows were exactly this list). tomllib parse with a
    regex fallback over the [models] section; empty set (degrades to
    the pre-filter behavior) if neither works."""
    if not cfg_text:
        return set()
    try:
        import tomllib
        doc = tomllib.loads(cfg_text)
        val = (doc.get("models") or {}).get("disabled_models") or []
        return {str(m) for m in val}
    except Exception:
        pass
    m = re.search(r"^\[models\](.*?)(?=^\[|\Z)", cfg_text, re.M | re.S)
    if not m:
        return set()
    a = re.search(r"disabled_models\s*=\s*\[(.*?)\]", m.group(1), re.S)
    if not a:
        return set()
    return {s.strip().strip('"') for s in a.group(1).split(",")
            if s.strip()}


def _has_explicit_backend(cfg_text, mid):
    """True when [model."<mid>"] carries its own api_backend line (the
    pin is then the live authority, not a prediction)."""
    if not cfg_text:
        return False
    esc = re.escape(mid)
    sec = re.search(r'^\[model\.(?:"%s"|%s)\](.*?)(?=^\[|\Z)'
                    % (esc, esc), cfg_text, re.M | re.S)
    return bool(sec and re.search(r"^api_backend\s*=", sec.group(1), re.M))


def _runner_path(mid, cfg_text):
    """The live routing authority: the runner's own 3-way resolver
    (run.wire_path_for_model). None if the runner module or the config
    is unavailable (the caller falls back to the name heuristic)."""
    if not cfg_text:
        return None
    try:
        if HERE not in sys.path:
            sys.path.insert(0, HERE)
        import run
        return run.wire_path_for_model(cfg_text, mid)
    except Exception:
        return None


def main():
    key = os.environ.get("CODEX_LLM_PROXY_KEY", "")
    if not key:
        print("FATAL: CODEX_LLM_PROXY_KEY not in env", file=sys.stderr)
        return 1
    cfg_text = read_live_config()
    disabled = disabled_model_ids(cfg_text)
    # HYG-3: the auth header arrives on curl's stdin via `--config -`, so
    # `ps` only ever sees: curl -s --config - <MODELS_URL> (no key in argv).
    out = subprocess.run(
        ["curl", "-s", "--config", "-", MODELS_URL],
        input='header = "Authorization: Bearer %s"\n' % key,
        capture_output=True, text=True, check=True).stdout
    data = json.loads(out).get("data", [])
    ids = sorted(mid for mid in (m["id"] for m in data)
                 if not embedding_model(mid) and mid not in disabled)
    skipped = len(data) - len(ids)
    disabled_in_catalog = sorted(mid for mid in
                                 (m["id"] for m in data)
                                 if not embedding_model(mid)
                                 and mid in disabled)

    rows, wire = [], []
    for mid in ids:
        rows.append({"model": mid, "prompt": "Reply with exactly: HT1-OK"})
        path = _runner_path(mid, cfg_text) or expected_path(mid)
        path_pin = {"path": "$.path", "eq": path}
        if path == CC_PATH:
            path_pin["label"] = (CC_AUTHORITY_NOTE % mid
                                 if _has_explicit_backend(cfg_text, mid)
                                 else CC_RATIFY_NOTE)
        for path in (path_pin, {"path": "$.body.model", "eq": mid}):
            wire.append({"kind": "field", "file": "req-*.json",
                         "where": {"method": "POST", "$.body.model": mid},
                         "nth": 0, **path})

    case = {
        "schema_version": SCHEMA_VERSION,
        "id": "RT-M6",
        "title": ("GENERATED by gen-matrix.py (do not hand-edit): full live-catalog "
                  "sweep (ROW WIRE, RT-M5 lineage), one fresh headless turn per "
                  "model; sign-off pass (release / post proxy-side change). "
                  "Rows = live catalog minus embedding models minus the live "
                  "config's [models] disabled_models (a disabled model's turn "
                  "exits fast with NO model request -> its wire pins can never "
                  "match: deterministic false reds). Path pins follow the live "
                  "config routing authority ([model.<id>] api_backend > "
                  "[endpoints] default_api_backend > responses; name-heuristic "
                  "fallback only when the config is unreadable). Slim "
                  "regression set = RT-M5."),
        "suite": SUITE,
        "tier": TIER,
        "driver": DRIVER,
        "bead": BEAD,
        "rows": rows,
        "watchdog_s": 300,
        "est_calls": len(rows),
        "retry": RETRY,
        # Frozen-schema top-level assert: the rows path never evaluates
        # it (see the Schema note above) — empty object, contract only.
        "assert": {},
        "row_asserts": {"wire": wire},
    }
    with open(OUT, "w") as f:
        json.dump(case, f, indent=1)
        f.write("\n")
    print("wrote %s: %d rows (%d embedding skipped, %d disabled "
          "skipped: %s)" % (OUT, len(rows),
                            len([m for m in (x["id"] for x in data)
                                 if embedding_model(m)]),
                            len(disabled_in_catalog),
                            ", ".join(disabled_in_catalog) or "-"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
