#!/usr/bin/env python3
"""CATALOG GENERATOR (apex-071 CATALOG-BAKE-1).

Promoted from the recon tool `/tmp/catalog-recon/catalog_capture.py`
(apex-6mz, 2026-09-18). The per-model `/v1/model/info` 76-loop is REMOVED:
the `?model=` parameter is a verified no-op on this deployment (a single
call returns ALL deployments regardless of the parameter), so the generator
makes EXACTLY THREE calls, all-or-nothing:

  1. GET /model_group/info           -> per-model-group metadata (the rich
                                        source: caps, costs, providers,
                                        supported_reasoning_efforts hint,
                                        mode) — one call for all groups
  2. GET /v1/models                  -> the authoritative model-id list
                                        (the endpoint the harness fetches)
  3. GET /v1/model/info?model=<id>   -> shape liveness check for the
                                        per-model info endpoint (single call;
                                        the parameter is a no-op, the
                                        deployment count is recorded)

Output: `catalog_generated.json` (committed artifact; the build is
validate-only, ZERO network inside `cargo build`).

Secret hygiene (unchanged from the recon tool): the key is read from an
env var and NEVER written to disk or logs; every string value in the
output is swept for key-like patterns (sk-/xai-/ghp_/AKIA/Bearer/long
bare hex) and ANY hit aborts before the artifact is written.

Usage (from the worktree root, or anywhere — paths are CWD-independent
unless overridden):
  CODEX_LLM_PROXY_KEY=<key> scripts/catalog_generate.py
  CODEX_LLM_PROXY_KEY=<key> scripts/catalog_generate.py \
      --out crates/codegen/xai-grok-models/catalog_generated.json

Exit codes: 0 = ok; 2 = secret-sweep hit (artifact NOT written);
3 = a call failed (artifact NOT written); 4 = env key missing / bad
shape.
"""
import argparse
import datetime
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_BASE = "https://llm-proxy-api.ai.eng.netapp.com"
DEFAULT_KEY_ENV = "CODEX_LLM_PROXY_KEY"
SCHEMA_VERSION = 1

KEY_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9]{20,}"),
    re.compile(r"xai-[a-z0-9]{24,}"),
    re.compile(r"ghp_[A-Za-z0-9]{16,}"),
    re.compile(r"AKIA[A-Z0-9]{12,}"),
    re.compile(r"Bearer\s+[A-Za-z0-9._\-]{20,}"),
    re.compile(r"\b[0-9a-f]{40,}\b"),  # long bare hex (key material, not a uuid-with-dashes)
]

# Per-model row fields, in emission order. Absent source fields are OMITTED
# (never null-filled) so the generated artifact stays a faithful projection
# of the proxy's truth and the merge step can distinguish "proxy reported
# nothing" from "value is null".
GROUP_FIELDS = [
    "mode",
    "max_input_tokens",
    "max_output_tokens",
    "input_cost_per_token",
    "output_cost_per_token",
    "supported_reasoning_efforts",
    "providers",
]


def get_json(base, path, key, timeout=30):
    """One GET, no retries (three calls total, all-or-nothing)."""
    url = base.rstrip("/") + path
    req = urllib.request.Request(
        url, headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=timeout) as r:
        body = r.read().decode()
    print(
        f"call: GET {path.split('?')[0]} -> {r.status} ({len(body)}B, {time.time()-t0:.1f}s)",
        file=sys.stderr,
    )
    return json.loads(body)


def norm_number(v):
    """Integral floats -> int (272000.0 -> 272000); non-integral pass through."""
    if isinstance(v, float) and v.is_integer():
        return int(v)
    return v


def sweep_value(path, v, hits):
    if isinstance(v, str):
        for p in KEY_PATTERNS:
            if p.search(v):
                hits.append(f"{path}: {p.pattern} matched {v[:8]}...")
    elif isinstance(v, dict):
        for k, x in v.items():
            sweep_value(f"{path}.{k}", x, hits)
    elif isinstance(v, list):
        for i, x in enumerate(v[:500]):
            sweep_value(f"{path}[{i}]", x, hits)


def row_for(id, group):
    """One per-model row: id + the group's projection (absent fields omitted)."""
    row = {"id": id}
    if isinstance(group, dict):
        for f in GROUP_FIELDS:
            v = group.get(f)
            if v is None:
                continue
            if f in ("max_input_tokens", "max_output_tokens"):
                v = norm_number(v)
                if not isinstance(v, (int, float)) or v <= 0:
                    # Zero/negative caps are not ceilings; omit so the merge
                    # step never maps them into a runtime window.
                    continue
            elif f in ("input_cost_per_token", "output_cost_per_token"):
                # Zero cost is meaningful (free on this proxy) — keep it.
                v = norm_number(v)
            row[f] = v
    return row


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("--base", default=DEFAULT_BASE)
    ap.add_argument("--key-env", default=DEFAULT_KEY_ENV)
    ap.add_argument(
        "--out",
        default=os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "crates", "codegen", "xai-grok-models", "catalog_generated.json",
        ),
    )
    args = ap.parse_args()

    key = os.environ.get(args.key_env)
    if not key:
        print(f"4: env {args.key_env} not set (the key is read from env, never logged)", file=sys.stderr)
        sys.exit(4)

    t0 = time.time()
    try:
        groups_raw = get_json(args.base, "/model_group/info", key)
        models_raw = get_json(args.base, "/v1/models", key)
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, json.JSONDecodeError) as e:
        print(f"3: capture call failed: {e!r} (artifact NOT written)", file=sys.stderr)
        sys.exit(3)

    ids = []
    for m in models_raw.get("data", []) if isinstance(models_raw, dict) else []:
        if isinstance(m, dict) and isinstance(m.get("id"), str) and m["id"]:
            if m["id"] not in ids:
                ids.append(m["id"])
    groups = {
        g.get("model_group"): g
        for g in groups_raw.get("data", []) if isinstance(g, dict)
    }
    missing_groups = [i for i in ids if i not in groups]

    # Third call: per-model info endpoint shape liveness. The ?model=
    # parameter is a verified no-op on this deployment (all deployments are
    # returned regardless), so ONE call is the contract; looping per model
    # is the anti-pattern this generator exists to eliminate.
    first_id = ids[0] if ids else "probe"
    try:
        model_info_raw = get_json(
            args.base, f"/v1/model/info?model={urllib.parse.quote(first_id)}", key
        )
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, json.JSONDecodeError) as e:
        print(f"3: /v1/model/info liveness call failed: {e!r} (artifact NOT written)", file=sys.stderr)
        sys.exit(3)
    deployments = (
        model_info_raw.get("data", []) if isinstance(model_info_raw, dict) else []
    )

    models = {mid: row_for(mid, groups.get(mid)) for mid in ids}

    artifact = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "origin": args.base,
        "endpoints": ["/model_group/info", "/v1/models", "/v1/model/info"],
        "capture_note": (
            "3 calls total (all-or-nothing). /v1/model/info ?model= parameter "
            "is a verified no-op on this deployment (all deployments returned "
            "regardless); single liveness call only, never a per-model loop."
        ),
        "model_count": len(ids),
        "deployment_count": len(deployments) if isinstance(deployments, list) else None,
        "groups_missing": missing_groups,
        "models": models,
    }

    hits = []
    sweep_value("artifact", artifact, hits)
    if hits:
        print(f"SECRET SWEEP FAILED ({len(hits)} hits) — artifact NOT written:", file=sys.stderr)
        for h in hits[:10]:
            print(f"  {h}", file=sys.stderr)
        sys.exit(2)

    with open(args.out, "w") as f:
        json.dump(artifact, f, indent=1, sort_keys=True)
        f.write("\n")
    print(
        f"OK models={len(ids)} deployments={len(deployments) if isinstance(deployments, list) else '?'} "
        f"groups_missing={len(missing_groups)} sweep=0 out={args.out} "
        f"size={os.path.getsize(args.out)}B elapsed={time.time()-t0:.0f}s",
        file=sys.stderr,
    )
    if missing_groups:
        print("note: no /model_group/info record for:", file=sys.stderr)
        for mid in missing_groups:
            print(f"  {mid}", file=sys.stderr)


if __name__ == "__main__":
    main()
