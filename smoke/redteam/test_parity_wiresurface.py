#!/usr/bin/env python3
"""Offline parity RED tests for the open WIRE-SURFACE param-surface gaps — stdlib only.

Characterizes the recorded RED state of the wire-fidelity param-surface gaps
in the grok-build-responses harness (apex-ayl.118 inventory,
grok/plans/redstack-wire-20260920.md, "Comprehensive inventory" tables,
msgw side and openai side): on the sealed redstack-20260920 /
redstack-20260920-fix2 / c95-84-20260920 campaign captures, 19 of the 20
gap pins show the wire surface the harness OWES is ABSENT from the newest
main call of its source case (the 20th, apex-ayl.84, is an inverted
PRESENCE pin — the wrong-wire aux request is PRESENT on /v1/responses) —
server tools in tools[], top_k, stop_sequences, metadata.user_id,
emittable thinking display knobs, per-tool control (disable_parallel_tool_use)
and per-tool cache_control, service_tier, context_management, the
anthropic-beta request header, TTL-split (5m/1h) usage fields,
prompt_cache_retention, the responses cost levers (service_tier /
max_tool_calls / parallel_tool_calls), reasoning.context / reasoning.mode,
and the body+header identity surface — while the control pins hold
(non-empty tools[], display == "summarized", the hardcoded
reasoning.summary == "concise" documented as a doc-control pin).

Fixtures (fixtures/parity/wiresurface/, see META.json for sources + sha256s):
  msgw/srvtool-01.json      n=5 of mgw-srvtool-01  — 31 client tools, no
                            server-tool element (MGW-F1)
  msgw/topk-01.json         n=5 of mgw-topk-01     — no body.top_k (MGW-F2a)
  msgw/stopseq-01.json      n=5 of mgw-stopseq-01  — no body.stop_sequences (MGW-F2b)
  msgw/userid-01.json       n=5 of mgw-userid-01   — no metadata.user_id (MGW-F2c)
  msgw/thinkknob-01.json    n=6 of mgw-thinkknob-01 (fix2 re-run) — display
                            "summarized", no block_binding (MGW-F3)
  msgw/toolctl-01.json      n=5 of mgw-toolctl-01  — no disable_parallel_tool_use (MGW-F5a)
  msgw/toolcache-01.json    n=5 of mgw-toolcache-01 — no cache_control key under
                            body.tools (MGW-F5b)
  msgw/svcseq-01.json       n=5 of mgw-svcseq-01   — no body.service_tier (MGW-F8)
  msgw/ctxmgt-01.json       n=5 of mgw-ctxmgt-01   — no body.context_management (MGW-F9)
  msgw/betahdr-01.json      n=5 of mgw-betahdr-01  — no anthropic-beta header (MGW-F11)
  msgw/ttlusage-01.json     session usage.json from a claude-messages case HOME
                            (home/sessions/**/usage.json, claude-sonnet-5) — no
                            TTL-split 5m/1h cache fields (MGW-F12)
  openai/cachectl-01.json   n=9 of ow-cachectl-01  — no prompt_cache_retention (OW-F1)
  openai/costctl-01.json    n=5 of ow-costctl-01   — no service_tier / max_tool_calls /
                            parallel_tool_calls (OW-F2a/F2b/F2c)
  openai/reasonsurf-01.json n=8 of ow-reasonsurf-01 — reasoning.summary "concise"
                            (doc-control, OW-F3); no reasoning.context/mode (OW-F4)
  openai/metadata-01.json   n=5 of ow-metadata-01 (fix2 re-run) — no body.user /
                            body.safety_identifier / x-grok-user-id header (OW-F9)
  openai/xresp-argtrunc-01.jsonl  c95-84-20260920/xresp-argtrunc-01 wire
                            resp-008.jsonl (glm-5.2; 1 response-header record
                            + 109 SSE 'data:' frame records) — the use_tool
                            function_call_arguments.done frame with the
                            proxy-truncated 202-char INVALID arguments (final
                            '}' dropped) AND the 85-char VALID
                            run_terminal_command echo .done in the same file
                            (apex-ayl.95)
  openai/auxmodel-01-req-004.json n=4 of c95-84-20260920/auxmodel-wire-01 —
                            POST /v1/responses body.model "probe-aux-01": the
                            synthetic aux entry hardcoded on the Responses
                            backend (defect evidence, apex-ayl.84)
  openai/auxmodel-01-req-005.json n=5 of the same case — POST /v1/messages
                            body.model claude-sonnet-5: the session's main
                            call, the wire the aux should inherit (control)

Fixture = the NEWEST POST to the case's target path (the session_title
auxiliary call is the OLDEST POST, n=4, and is excluded — same finding as
the redstack-20260920 adjudication of the THINKKNOB/METADATA control misses).
Request fixtures are the pretty-printed wire envelopes
{"n","method","path","ts","headers","body"} with body parsed; header values
are the masked set the wirecap records (e.g. authorization as
{len,masked,sha256_12}) — the header KEY SET is what the header gaps pin.
Source captures: smoke/redteam/report/{redstack-20260920,redstack-20260920-fix2,
c95-84-20260920}/<case>/wire/ (req-NNN.json envelopes; the sealed
resp-008.jsonl SSE capture for apex-ayl.95; <case>/home/sessions/**/usage.json
for MGW-F12). No new proxy calls were made for this suite.

Run it (OFFLINE: no proxy key, no binary, no live calls, no cargo):

  python3 smoke/redteam/test_parity_wiresurface.py          # exit 0 (fixtures)
  python3 smoke/redteam/test_parity_wiresurface.py --gate   # exit 1 today (RED)
  python3 smoke/redteam/test_parity_wiresurface.py --gate DIR

Bare run: asserts the CURRENT red state against the recorded fixtures
(every red pin must hold on the sealed capture) and prints the coverage
table. --gate asserts the GREEN (post-cut) expectation: for each gap, the
fixture under the gate dir must SHOW the owed surface (field/header present
or knob emittable). Default gate dir = the recorded fixtures themselves, so
today every green assert is unmet -> exit 1. Point --gate DIR at a
re-recorded post-cut fixture dir laid out like wiresurface/ (msgw/ + openai/
subdirs, same fixture file names) to run post-cut acceptance; exit 0 when
all 18 green asserts are met. 2 = usage error.

Test map (characterization pins on the recorded capture — all GREEN today;
the documented RED is the wire-surface drop, not a test failure):

  WireSurfaceParity.test_mgw_f1_server_tool_surface          GREEN (31 tools,
      control holds; no server-tool element)
  WireSurfaceParity.test_mgw_f2a_top_k                       GREEN (absent)
  WireSurfaceParity.test_mgw_f2b_stop_sequences              GREEN (absent)
  WireSurfaceParity.test_mgw_f2c_metadata_user_id            GREEN (absent)
  WireSurfaceParity.test_mgw_f3_thinking_display_knobs       GREEN (display
      all "summarized"; block_binding nowhere in body; no "updates" display)
  WireSurfaceParity.test_mgw_f5a_disable_parallel_tool_use   GREEN (absent on
      every tools[] element)
  WireSurfaceParity.test_mgw_f5b_tool_cache_control          GREEN (no
      cache_control key recursively inside body.tools — message/system-level
      cache_control breakpoints ARE present and are out of scope)
  WireSurfaceParity.test_mgw_f8_service_tier                 GREEN (absent)
  WireSurfaceParity.test_mgw_f9_context_management           GREEN (absent)
  WireSurfaceParity.test_mgw_f11_anthropic_beta_header       GREEN (no
      anthropic-beta key in the 21-key header set)
  WireSurfaceParity.test_mgw_f12_usage_ttl_split             GREEN (session +
      turns rows carry no ephemeral_5m_input_tokens / ephemeral_1h_input_tokens
      nor any 5m/1h key variant)
  WireSurfaceParity.test_ow_f1_prompt_cache_retention        GREEN (absent;
      prompt_cache_key IS present — a different field)
  WireSurfaceParity.test_ow_f2a_service_tier                 GREEN (absent)
  WireSurfaceParity.test_ow_f2b_max_tool_calls               GREEN (absent)
  WireSurfaceParity.test_ow_f2c_parallel_tool_calls          GREEN (absent)
  WireSurfaceParity.test_ow_f3_reasoning_summary_doc_control GREEN (summary
      "concise" — CHARACTERIZATION/DOC-CONTROL pin, not a hard RED; the
      hardcode is documented at responses.rs, and the pin re-ratchets if it
      moves)
  WireSurfaceParity.test_ow_f4_reasoning_context_mode        GREEN (reasoning
      present; no context/mode keys)
  WireSurfaceParity.test_ow_f9_body_identity                 GREEN (no
      body.user, no body.safety_identifier, no x-grok-user-id header — the
      identity surface is thinner than the .99.3 docs claim)
  WireSurfaceParity.test_95_xresp_argtrunc                   GREEN (use_tool
      .done arguments INVALID: len 202, tail ...responses"} — the proxy
      dropped the final '}')
  WireSurfaceParity.test_95_guard_echo_frame_valid           GREEN — GUARD
      pin, must pass BOTH pre- and post-fix: the run_terminal_command echo
      .done in the same file stays VALID (len 85; SDD RED 2-4 class)
  WireSurfaceParity.test_84_auxmodel_fallback_wire           GREEN —
      PRESENCE pin (inverted from the 18 absence rows): a /v1/responses
      request carries body.model probe-aux-01; the main-call control sits
      on /v1/messages with claude-sonnet-5
  WireSurfaceParity.test_meta_sha256_matches_fixtures        GREEN (META.json
      hashes re-verified against the fixture bytes)
  WireSurfaceParity.test_fixtures_carry_no_raw_keys          GREEN (raw-key
      sweep sk-[A0-9a-z]{16,} over every fixture: 0 hits)
  WireSurfaceParity.test_fixture_shape_pins                  GREEN (envelope
      shape, pinned n/method/path per fixture; usage fixture = claude-messages
      session + turn rows)
"""
import argparse
import hashlib
import json
import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE_DIR = os.path.join(HERE, "fixtures", "parity", "wiresurface")
# Worktree root per task spec: 4 levels up from this file's dir (dirname x4).
WORKTREE_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(HERE))))
DEFAULT_GATE_REL = "smoke/redteam/fixtures/parity/wiresurface"

RAW_KEY_RE = re.compile(rb"sk-[A0-9a-z]{16,}")

# ---------------------------------------------------------------------------
# fixture access + wire-surface helpers
# ---------------------------------------------------------------------------


def _load_json(path):
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _body(doc):
    if not isinstance(doc, dict) or not isinstance(doc.get("body"), dict):
        raise ValueError("fixture is not a request envelope with a dict body")
    return doc["body"]


def _headers(doc):
    hdrs = doc.get("headers")
    return hdrs if isinstance(hdrs, dict) else {}


def _tools(body):
    tools = body.get("tools")
    return tools if isinstance(tools, list) else []


def _is_server_tool(name):
    """Anthropic server-tool surface: named server tools (web_search_*,
    mcp_toolset, mcp_servers) and date-suffixed toolset slugs (web_search_20250305,
    *_2026*, ...)."""
    if not isinstance(name, str) or not name:
        return False
    if "web_search_" in name or "mcp_toolset" in name or "mcp_servers" in name:
        return True
    return re.search(r"_20\d{6}$", name) is not None


def _iter_keys(obj):
    """Yield every dict key in the tree (recursive through lists)."""
    if isinstance(obj, dict):
        for key, value in obj.items():
            yield key
            for sub in _iter_keys(value):
                yield sub
    elif isinstance(obj, list):
        for value in obj:
            for sub in _iter_keys(value):
                yield sub


def _display_values(obj):
    """Yield the value of every "display" key in the tree (recursive)."""
    if isinstance(obj, dict):
        for key, value in obj.items():
            if key == "display":
                yield value
            for sub in _display_values(value):
                yield sub
    elif isinstance(obj, list):
        for value in obj:
            for sub in _display_values(value):
                yield sub


def _usage_rows(doc):
    """Rows of a usage.json doc: the session object plus each turns[] entry
    (the usage seam's camelCase rows — same row set as the .101 gate)."""
    if not isinstance(doc, dict):
        return []
    rows = []
    if isinstance(doc.get("session"), dict):
        rows.append(("session", doc["session"]))
    for index, turn in enumerate(doc.get("turns") or []):
        if isinstance(turn, dict):
            rows.append(("turns[%d]" % index, turn))
    return rows


def _load_sse_jsonl(path):
    """Parse a resp-*.jsonl wire capture into its list of records.

    Line shapes (c95-84 wirecap): one response-header record
    {"headers","n","status","ts"} followed by frame records
    {"frame_index": N, "frame": "data: {json}"} — the JSON-encoded SSE
    chunk is a bare 'data:' line in this capture (no 'event:' lines)."""
    records = []
    with open(path, "r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    return records


def _sse_done_frames(records):
    """Yield (frame_index, payload) for every
    response.function_call_arguments.done frame in parsed resp-*.jsonl
    records (response-header records and [DONE] terminators skipped)."""
    for record in records or []:
        if not isinstance(record, dict):
            continue
        frame = record.get("frame")
        if not isinstance(frame, str):
            continue
        for chunk_line in frame.split("\n"):
            if not chunk_line.startswith("data:"):
                continue
            data = chunk_line[len("data:"):].strip()
            if data == "[DONE]":
                continue
            try:
                payload = json.loads(data)
            except ValueError:
                payload = None
            if isinstance(payload, dict) and payload.get("type") == "response.function_call_arguments.done":
                yield record.get("frame_index"), payload
            break


def _done_frame_by_name(records, name):
    """(payload, count) for the name-matched .done frames in parsed records."""
    hits = [payload for _fi, payload in _sse_done_frames(records) if payload.get("name") == name]
    return (hits[0] if hits else None), len(hits)


# ---------------------------------------------------------------------------
# per-gap red/green asserts — each returns (ok, detail)
# ---------------------------------------------------------------------------


def _make_key_pair(key):
    """Top-level body-key absence gaps (MGW-F2a/F2b/F8/F9, OW-F1/F2a/F2b/F2c):
    red = the owed key is ABSENT on the recorded wire; green (post-cut) = it is
    PRESENT (the param surface passes through)."""
    def red(doc):
        if key in _body(doc):
            return False, "body.%s PRESENT — red state broken (fixture drift?)" % key
        return True, "body.%s absent" % key

    def green(doc):
        if key not in _body(doc):
            return False, "body.%s still absent — param surface not yet cut" % key
        return True, "body.%s present on the wire" % key

    return red, green


def red_mgw_f1(doc):
    tools = _tools(_body(doc))
    if not tools:
        return False, "control violated: body.tools missing or empty"
    server = [t.get("name") for t in tools if isinstance(t, dict) and _is_server_tool(t.get("name"))]
    if server:
        return False, "server-tool element(s) already present: %s" % server
    return True, "body.tools non-empty (%d elements), no server-tool element" % len(tools)


def green_mgw_f1(doc):
    tools = _tools(_body(doc))
    server = [t.get("name") for t in tools if isinstance(t, dict) and _is_server_tool(t.get("name"))]
    if not server:
        return False, "no server-tool element in tools[] (%d elements) — server-tool surface still unemitted" % len(tools)
    return True, "server-tool element(s) on the wire: %s" % server


def red_mgw_f2c(doc):
    metadata = _body(doc).get("metadata")
    if isinstance(metadata, dict) and "user_id" in metadata:
        return False, "metadata.user_id PRESENT — red state broken (fixture drift?)"
    return True, "metadata.user_id absent (metadata %s)" % ("present" if isinstance(metadata, dict) else "absent")


def green_mgw_f2c(doc):
    metadata = _body(doc).get("metadata")
    if not isinstance(metadata, dict) or metadata.get("user_id") is None:
        return False, "metadata.user_id still absent/null — identity not yet plumbed to the wire"
    return True, "metadata.user_id present on the wire"


def red_mgw_f3(doc):
    body = _body(doc)
    serialized = json.dumps(body)
    displays = list(_display_values(body))
    non_summarized = [d for d in displays if d != "summarized"]
    if non_summarized:
        return False, "display value(s) other than 'summarized' already present: %s" % non_summarized
    if "block_binding" in serialized:
        return False, "block_binding already appears in the body — red state broken (fixture drift?)"
    return True, "every display value == 'summarized' (%s); block_binding nowhere; no 'updates' display" % (displays or "none")


def green_mgw_f3(doc):
    body = _body(doc)
    serialized = json.dumps(body)
    displays = list(_display_values(body))
    controllable = [d for d in displays if d != "summarized"]
    if controllable or "block_binding" in serialized:
        return True, "emittable thinking-knob surface on the wire (displays=%s, block_binding=%s)" % (displays, "block_binding" in serialized)
    return False, "knob surface still unemittable: displays=%s, no block_binding, no 'updates' display" % (displays or "none")


def red_mgw_f5a(doc):
    tools = _tools(_body(doc))
    hit = [t.get("name") for t in tools if isinstance(t, dict) and "disable_parallel_tool_use" in t]
    if hit:
        return False, "disable_parallel_tool_use already on tools[] element(s): %s" % hit
    return True, "no tools[] element carries disable_parallel_tool_use (%d elements)" % len(tools)


def green_mgw_f5a(doc):
    tools = _tools(_body(doc))
    hit = [t.get("name") for t in tools if isinstance(t, dict) and "disable_parallel_tool_use" in t]
    if not hit:
        return False, "no tools[] element carries disable_parallel_tool_use — per-tool control still unemitted"
    return True, "disable_parallel_tool_use on tools[] element(s): %s" % hit


def red_mgw_f5b(doc):
    tools = _body(doc).get("tools")
    if isinstance(tools, list) and "cache_control" in _iter_keys(tools):
        return False, "cache_control key already inside body.tools — red state broken (fixture drift?)"
    return True, "no cache_control key recursively inside body.tools"


def green_mgw_f5b(doc):
    tools = _body(doc).get("tools")
    if not isinstance(tools, list) or "cache_control" not in _iter_keys(tools):
        return False, "no cache_control key inside body.tools — per-tool cache breakpoints still unemitted"
    return True, "cache_control key inside body.tools"


def red_mgw_f11(doc):
    if "anthropic-beta" in _headers(doc):
        return False, "anthropic-beta header already present — red state broken (fixture drift?)"
    return True, "no anthropic-beta key in the request headers (%d header keys)" % len(_headers(doc))


def green_mgw_f11(doc):
    if "anthropic-beta" not in _headers(doc):
        return False, "no anthropic-beta key in the request headers — beta seam still unemitted"
    return True, "anthropic-beta header present"


def _ttl_split_keys(row):
    """TTL-split cache field names in a usage row: the canonical
    ephemeral_5m_input_tokens / ephemeral_1h_input_tokens and ANY 5m/1h
    key variant (both naming styles are pinned by the substring test)."""
    return [k for k in _iter_keys(row) if "5m" in k or "1h" in k]


def red_mgw_f12(doc):
    rows = _usage_rows(doc)
    if not rows:
        return False, "usage fixture has no session/turns rows (bad fixture)"
    hit = ["%s.%s" % (label, k) for label, row in rows for k in _ttl_split_keys(row)]
    if hit:
        return False, "TTL-split cache fields already present: %s" % hit
    return True, "no TTL-split cache fields in any of %d usage rows (session + turns; neither ephemeral_5m_input_tokens/ephemeral_1h_input_tokens nor any 5m/1h variant)" % len(rows)


def green_mgw_f12(doc):
    rows = _usage_rows(doc)
    hit = ["%s.%s" % (label, k) for label, row in rows for k in _ttl_split_keys(row)]
    if not hit:
        return False, "no TTL-split (5m/1h) cache fields in any usage row — TTL split still unrecorded"
    return True, "TTL-split fields in usage rows: %s" % hit


def red_ow_f3(doc):
    reasoning = _body(doc).get("reasoning")
    if not isinstance(reasoning, dict):
        return True, "no body.reasoning on the capture (pin vacuous on this request; hardcode documented)"
    if "summary" not in reasoning:
        return False, "body.reasoning present without a summary key — hardcode not emitted?"
    if reasoning["summary"] != "concise":
        return False, "reasoning.summary != 'concise' — documented hardcode moved: %r" % (reasoning["summary"],)
    return True, "reasoning.summary == 'concise' (documented hardcode — doc-control pin)"


def green_ow_f3(doc):
    reasoning = _body(doc).get("reasoning")
    if not isinstance(reasoning, dict) or "summary" not in reasoning:
        return False, "no body.reasoning.summary on the capture — operator value not yet emittable (re-record with a non-default summary to prove control)"
    if reasoning["summary"] == "concise":
        return False, "reasoning.summary still the hardcoded 'concise' — operator-set summary not yet plumbed"
    return True, "operator-set reasoning.summary on the wire: %r" % (reasoning["summary"],)


def red_ow_f4(doc):
    reasoning = _body(doc).get("reasoning")
    if not isinstance(reasoning, dict):
        return True, "no body.reasoning on the capture (pin vacuous on this request)"
    present = [k for k in ("context", "mode") if k in reasoning]
    if present:
        return False, "reasoning.%s already present — red state broken (fixture drift?)" % "/".join(present)
    return True, "body.reasoning present without context/mode keys (reasoning=%s)" % json.dumps(reasoning)


def green_ow_f4(doc):
    reasoning = _body(doc).get("reasoning")
    if not isinstance(reasoning, dict):
        return False, "no body.reasoning on the capture — context/mode surface not yet emittable"
    present = [k for k in ("context", "mode") if k in reasoning]
    if not present:
        return False, "no reasoning.context / reasoning.mode on the wire — fork Reasoning surface still absent"
    return True, "reasoning.%s on the wire" % "/".join(present)


def red_ow_f9(doc):
    body = _body(doc)
    present = []
    if "user" in body:
        present.append("body.user")
    if "safety_identifier" in body:
        present.append("body.safety_identifier")
    if "x-grok-user-id" in _headers(doc):
        present.append("x-grok-user-id header")
    if present:
        return False, "identity already on the wire — red state broken (fixture drift?): %s" % present
    return True, "identity surface empty: no body.user, no body.safety_identifier, no x-grok-user-id header"


def green_ow_f9(doc):
    body = _body(doc)
    present = []
    if "user" in body:
        present.append("body.user")
    if "safety_identifier" in body:
        present.append("body.safety_identifier")
    if "x-grok-user-id" in _headers(doc):
        present.append("x-grok-user-id header")
    if not present:
        return False, "identity surface still empty (no body.user, no body.safety_identifier, no x-grok-user-id) — identity still unemitted"
    return True, "identity on the wire: %s" % present


PROBE_AUX_SLUG = "probe-aux-01"
TRUNCATED_TAIL = 'grok-build-responses"}'
SDD_FIXED_TAIL = 'responses"}}'


def red_95(doc):
    """apex-ayl.95 RED: the use_tool function_call_arguments.done frame
    (glm-5.2, /v1/responses) carries the proxy-truncated arguments — INVALID
    JSON (the final '}' dropped), len 202, truncated tail present."""
    use, count = _done_frame_by_name(doc, "use_tool")
    if count != 1:
        return False, "expected exactly one use_tool function_call_arguments.done frame, found %d" % count
    args = use.get("arguments")
    if not isinstance(args, str):
        return False, "use_tool .done frame carries no string arguments"
    try:
        json.loads(args)
    except ValueError:
        pass
    else:
        return False, "use_tool arguments is VALID JSON — red state broken (proxy fix landed? re-mint the fixture)"
    if len(args) != 202:
        return False, "use_tool arguments len %d != 202 — SDD byte contract drifted" % len(args)
    if not args.endswith(TRUNCATED_TAIL):
        return False, "truncated tail not present: ...%r" % args[-30:]
    return True, "use_tool .done arguments INVALID (final '}' dropped): len 202, tail '...%s'" % TRUNCATED_TAIL


def green_95(doc):
    """apex-ayl.95 GREEN (post proxy-fix): the same frame's arguments is
    VALID JSON, len 203, tail 'responses"}}' — the SDD byte contract."""
    use, count = _done_frame_by_name(doc, "use_tool")
    if count != 1:
        return False, "expected exactly one use_tool function_call_arguments.done frame, found %d" % count
    args = use.get("arguments")
    if not isinstance(args, str):
        return False, "use_tool .done frame carries no string arguments"
    try:
        json.loads(args)
    except ValueError:
        return False, "use_tool arguments still INVALID JSON (len %d) — proxy still dropping the final '}' (pre-fix)" % len(args)
    if len(args) != 203:
        return False, "use_tool arguments len %d != 203 — SDD byte contract not met" % len(args)
    if not args.endswith(SDD_FIXED_TAIL):
        return False, "use_tool arguments tail ...%r != SDD contract %r" % (args[-20:], SDD_FIXED_TAIL)
    return True, "use_tool .done arguments VALID JSON, len 203, tail '%s' — SDD byte contract met" % SDD_FIXED_TAIL


def guard_95(doc):
    """apex-ayl.95 GUARD pin (SDD RED 2-4 class): must pass BOTH pre- and
    post-fix — the run_terminal_command echo .done frame in the same file
    stays VALID (len 85)."""
    echo, count = _done_frame_by_name(doc, "run_terminal_command")
    if count != 1:
        return False, "expected exactly one run_terminal_command echo .done frame, found %d" % count
    args = echo.get("arguments")
    if not isinstance(args, str):
        return False, "echo .done frame carries no string arguments"
    try:
        json.loads(args)
    except ValueError:
        return False, "echo (run_terminal_command) .done arguments became INVALID JSON (len %d) — guard pin broken" % len(args)
    if len(args) != 85:
        return False, "echo .done arguments len %d != 85" % len(args)
    return True, "echo (run_terminal_command) .done arguments still VALID (len 85)"


def red_84(doc):
    """apex-ayl.84 RED (PRESENCE — inverted from the 18 absence rows): a
    request on the /v1/responses path carries body.model probe-aux-01 (the
    synthetic aux entry hardcodes ApiBackend::Responses)."""
    path = doc.get("path") if isinstance(doc, dict) else None
    model = (doc.get("body") or {}).get("model") if isinstance(doc, dict) else None
    if path != "/v1/responses":
        return False, "fixture path %r is not /v1/responses — defect evidence lost (re-mint?)" % path
    if model != PROBE_AUX_SLUG:
        return False, "body.model %r is not %r on /v1/responses — defect evidence lost" % (model, PROBE_AUX_SLUG)
    return True, "PRESENCE (inverted pin): /v1/responses request carries body.model probe-aux-01 — the synthetic aux entry hardcodes ApiBackend::Responses"


def green_84(doc, envelopes=None):
    """apex-ayl.84 GREEN (post-cut): NO /v1/responses request carries the
    probe slug — the aux inherits the session wire, or the cut catalog-guards
    the slug so it never leaves the harness. dirscan: also checks every
    request envelope under the gate dir."""
    path = doc.get("path") if isinstance(doc, dict) else None
    model = (doc.get("body") or {}).get("model") if isinstance(doc, dict) else None
    if path == "/v1/responses" and model == PROBE_AUX_SLUG:
        return False, "/v1/responses request still carries body.model probe-aux-01 — aux still on the wrong wire (pre-cut)"
    if envelopes is not None:
        hit = [e for e in envelopes if isinstance(e, dict) and e.get("path") == "/v1/responses"
               and isinstance(e.get("body"), dict) and e.get("body").get("model") == PROBE_AUX_SLUG]
        if hit:
            return False, "probe-aux-01 still on the /v1/responses wire in %d request(s) — slug not yet cut" % len(hit)
    return True, "no /v1/responses request carries the probe slug (aux inherits the session wire, or the slug is catalog-guarded)"


def _key_pair_fns():
    """(red, green) assert pairs for the top-level body-key gaps, by gap id."""
    return {
        "MGW-F2a": _make_key_pair("top_k"),
        "MGW-F2b": _make_key_pair("stop_sequences"),
        "MGW-F8": _make_key_pair("service_tier"),
        "MGW-F9": _make_key_pair("context_management"),
        "OW-F1": _make_key_pair("prompt_cache_retention"),
        "OW-F2a": _make_key_pair("service_tier"),
        "OW-F2b": _make_key_pair("max_tool_calls"),
        "OW-F2c": _make_key_pair("parallel_tool_calls"),
    }


_KEY_PAIR_FNS = _key_pair_fns()


def _key_spec(gap, fixture, n, red_desc):
    red, green = _KEY_PAIR_FNS[gap]
    wire = "msgw" if fixture.startswith("msgw/") else "openai"
    return {"gap": gap, "wire": wire, "fixture": fixture, "n": n,
            "kind": "hard", "red": red, "green": green,
            "red_desc": red_desc, "green_desc": red_desc.replace("absent", "present")}


GAP_SPECS = [
    {"gap": "MGW-F1", "wire": "msgw", "fixture": "msgw/srvtool-01.json", "n": 5,
     "kind": "hard", "red": red_mgw_f1, "green": green_mgw_f1,
     "red_desc": "tools[] non-empty, no server-tool element",
     "green_desc": "a server-tool element in tools[]"},
    _key_spec("MGW-F2a", "msgw/topk-01.json", 5, "body.top_k absent"),
    _key_spec("MGW-F2b", "msgw/stopseq-01.json", 5, "body.stop_sequences absent"),
    {"gap": "MGW-F2c", "wire": "msgw", "fixture": "msgw/userid-01.json", "n": 5,
     "kind": "hard", "red": red_mgw_f2c, "green": green_mgw_f2c,
     "red_desc": "metadata.user_id absent (metadata may be)",
     "green_desc": "metadata.user_id present"},
    {"gap": "MGW-F3", "wire": "msgw", "fixture": "msgw/thinkknob-01.json", "n": 6,
     "kind": "hard", "red": red_mgw_f3, "green": green_mgw_f3,
     "red_desc": "displays all 'summarized'; no block_binding/updates",
     "green_desc": "an emittable knob: display!='summarized' or block_binding"},
    {"gap": "MGW-F5a", "wire": "msgw", "fixture": "msgw/toolctl-01.json", "n": 5,
     "kind": "hard", "red": red_mgw_f5a, "green": green_mgw_f5a,
     "red_desc": "no disable_parallel_tool_use in tools[]",
     "green_desc": "disable_parallel_tool_use on a tools[] element"},
    {"gap": "MGW-F5b", "wire": "msgw", "fixture": "msgw/toolcache-01.json", "n": 5,
     "kind": "hard", "red": red_mgw_f5b, "green": green_mgw_f5b,
     "red_desc": "no cache_control key under body.tools",
     "green_desc": "a cache_control key under body.tools"},
    _key_spec("MGW-F8", "msgw/svcseq-01.json", 5, "body.service_tier absent"),
    _key_spec("MGW-F9", "msgw/ctxmgt-01.json", 5, "body.context_management absent"),
    {"gap": "MGW-F11", "wire": "msgw", "fixture": "msgw/betahdr-01.json", "n": 5,
     "kind": "hard", "red": red_mgw_f11, "green": green_mgw_f11,
     "red_desc": "no anthropic-beta request header",
     "green_desc": "anthropic-beta request header present"},
    {"gap": "MGW-F12", "wire": "msgw", "fixture": "msgw/ttlusage-01.json", "n": None,
     "kind": "hard", "red": red_mgw_f12, "green": green_mgw_f12,
     "red_desc": "no 5m/1h TTL-split fields in usage rows",
     "green_desc": "TTL-split (5m/1h) fields in a usage row"},
    _key_spec("OW-F1", "openai/cachectl-01.json", 9, "body.prompt_cache_retention absent"),
    _key_spec("OW-F2a", "openai/costctl-01.json", 5, "body.service_tier absent"),
    _key_spec("OW-F2b", "openai/costctl-01.json", 5, "body.max_tool_calls absent"),
    _key_spec("OW-F2c", "openai/costctl-01.json", 5, "body.parallel_tool_calls absent"),
    {"gap": "OW-F3", "wire": "openai", "fixture": "openai/reasonsurf-01.json", "n": 8,
     "kind": "doc-control", "red": red_ow_f3, "green": green_ow_f3,
     "red_desc": "reasoning.summary == 'concise' (doc-control pin)",
     "green_desc": "operator-set summary != 'concise' on the wire"},
    {"gap": "OW-F4", "wire": "openai", "fixture": "openai/reasonsurf-01.json", "n": 8,
     "kind": "hard", "red": red_ow_f4, "green": green_ow_f4,
     "red_desc": "no reasoning.context/mode keys",
     "green_desc": "reasoning.context or reasoning.mode present"},
    {"gap": "OW-F9", "wire": "openai", "fixture": "openai/metadata-01.json", "n": 5,
     "kind": "hard", "red": red_ow_f9, "green": green_ow_f9,
     "red_desc": "no body.user/safety_identifier; no x-grok-user-id hdr",
     "green_desc": "an identity member on the wire (body or header)"},
    {"gap": "apex-ayl.95", "wire": "openai", "fixture": "openai/xresp-argtrunc-01.jsonl", "n": None,
     "kind": "hard", "guard": guard_95, "red": red_95, "green": green_95,
     "red_desc": "use_tool .done args INVALID len 202 (final '}' dropped); guard: echo .done valid len 85",
     "green_desc": "same frame's args VALID JSON len 203, tail responses}} (SDD byte contract)"},
    {"gap": "apex-ayl.84", "wire": "openai", "fixture": "openai/auxmodel-01-req-004.json", "n": 4,
     "kind": "presence", "dirscan": True, "red": red_84, "green": green_84,
     "red_desc": "PRESENCE (inverted pin): /v1/responses carries body.model probe-aux-01",
     "green_desc": "no /v1/responses request carries the probe slug (session-wire aux or guarded)"},
]

GAP_BY_ID = {spec["gap"]: spec for spec in GAP_SPECS}

# Unique fixture files (18): costctl-01 serves OW-F2a/b/c, reasonsurf-01 serves OW-F3/F4.
FIXTURE_FILES = sorted({spec["fixture"] for spec in GAP_SPECS})

# Control fixtures pinned by the shape tests but not a gap row of their own
# (no META.json row — the +2 META rows cover the two gap fixtures).
CONTROL_FIXTURES = [
    "openai/auxmodel-01-req-005.json",
]

# Envelope pins: fixture rel -> (n, path) for request captures; the usage
# fixture is pinned separately in test_fixture_shape_pins.
ENVELOPE_PINS = {
    "msgw/srvtool-01.json": (5, "/v1/messages"),
    "msgw/topk-01.json": (5, "/v1/messages"),
    "msgw/stopseq-01.json": (5, "/v1/messages"),
    "msgw/userid-01.json": (5, "/v1/messages"),
    "msgw/thinkknob-01.json": (6, "/v1/messages"),
    "msgw/toolctl-01.json": (5, "/v1/messages"),
    "msgw/toolcache-01.json": (5, "/v1/messages"),
    "msgw/svcseq-01.json": (5, "/v1/messages"),
    "msgw/ctxmgt-01.json": (5, "/v1/messages"),
    "msgw/betahdr-01.json": (5, "/v1/messages"),
    "openai/cachectl-01.json": (9, "/v1/responses"),
    "openai/costctl-01.json": (5, "/v1/responses"),
    "openai/reasonsurf-01.json": (8, "/v1/responses"),
    "openai/metadata-01.json": (5, "/v1/responses"),
    "openai/auxmodel-01-req-004.json": (4, "/v1/responses"),
    "openai/auxmodel-01-req-005.json": (5, "/v1/messages"),
}

# body.model pins for the c95-84 auxmodel fixtures (defect slug + session-wire
# control model).
MODEL_PINS = {
    "openai/auxmodel-01-req-004.json": "probe-aux-01",
    "openai/auxmodel-01-req-005.json": "claude-sonnet-5",
}

# Per-gap outcomes recorded as the suite/gate runs; consumed by the coverage
# table (bare mode: red results; gate mode: green results).
RED_RESULTS = {}
GREEN_RESULTS = {}


def print_coverage_table(results, mode):
    """gap | wire | fixture | red assert | green assert | today result."""
    headers = ["gap", "wire", "fixture", "red assert (bare = today)",
               "green assert (--gate = post-cut)", "today result"]
    rows = []
    for spec in GAP_SPECS:
        ok, _detail = results.get(spec["gap"], (False, "not run"))
        if mode == "bare":
            if not ok:
                result = "FAIL — pin broken (drift)"
            elif spec["kind"] == "doc-control":
                result = "PASS — doc-control pin holds"
            elif spec["kind"] == "presence":
                result = "PASS — gap live (RED, presence pin)"
            else:
                result = "PASS — gap live (RED)"
        else:
            result = "PASS — green met" if ok else "FAIL — green unmet (pre-cut)"
        rows.append([spec["gap"], spec["wire"], spec["fixture"],
                     spec["red_desc"], spec["green_desc"], result])
    widths = [max(len(str(r[i])) for r in rows + [headers]) for i in range(len(headers))]
    print("coverage table (%d gaps, %d fixtures)" % (len(GAP_SPECS), len(FIXTURE_FILES)))
    print(" | ".join(str(h).ljust(w) for h, w in zip(headers, widths)))
    print("-+-".join("-" * w for w in widths))
    for row in rows:
        print(" | ".join(str(c).ljust(w) for c, w in zip(row, widths)))


# ---------------------------------------------------------------------------
# fixture suite (bare run)
# ---------------------------------------------------------------------------


class WireSurfaceParity(unittest.TestCase):
    """Characterization pins on fixtures/parity/wiresurface (must pass today)."""

    def setUp(self):
        self.meta = _load_json(os.path.join(FIXTURE_DIR, "META.json"))
        self._docs = {}

    def _doc(self, fixture_rel):
        if fixture_rel not in self._docs:
            loader = _load_sse_jsonl if fixture_rel.endswith(".jsonl") else _load_json
            self._docs[fixture_rel] = loader(os.path.join(FIXTURE_DIR, fixture_rel))
        return self._docs[fixture_rel]

    def _red(self, gap):
        spec = GAP_BY_ID[gap]
        ok, detail = spec["red"](self._doc(spec["fixture"]))
        RED_RESULTS[gap] = (ok, detail)
        self.assertTrue(ok, "%s: %s" % (gap, detail))

    def test_mgw_f1_server_tool_surface(self):
        """MGW-F1: tools[] non-empty (control) with NO server-tool element —
        the server-tool surface (web_search_*, mcp_toolset, mcp_servers,
        date-suffixed toolsets) is unemitted."""
        self._red("MGW-F1")

    def test_mgw_f2a_top_k(self):
        """MGW-F2a: body.top_k absent on the recorded main call."""
        self._red("MGW-F2a")

    def test_mgw_f2b_stop_sequences(self):
        """MGW-F2b: body.stop_sequences absent on the recorded main call."""
        self._red("MGW-F2b")

    def test_mgw_f2c_metadata_user_id(self):
        """MGW-F2c: metadata.user_id absent (metadata itself may be present)."""
        self._red("MGW-F2c")

    def test_mgw_f3_thinking_display_knobs(self):
        """MGW-F3: every thinking display == 'summarized', block_binding
        nowhere in the body, no 'updates' display — the display knobs are
        unemittable (declared config keys ignored by the lenient row parser)."""
        self._red("MGW-F3")

    def test_mgw_f5a_disable_parallel_tool_use(self):
        """MGW-F5a: no tools[] element carries disable_parallel_tool_use."""
        self._red("MGW-F5a")

    def test_mgw_f5b_tool_cache_control(self):
        """MGW-F5b: no cache_control key recursively inside body.tools
        (message/system-level cache_control breakpoints are present and out
        of scope — the per-tool surface is what is owed)."""
        self._red("MGW-F5b")

    def test_mgw_f8_service_tier(self):
        """MGW-F8: body.service_tier absent on the recorded main call."""
        self._red("MGW-F8")

    def test_mgw_f9_context_management(self):
        """MGW-F9: body.context_management absent on the recorded main call."""
        self._red("MGW-F9")

    def test_mgw_f11_anthropic_beta_header(self):
        """MGW-F11: no anthropic-beta key in the request headers (0/22
        receipts wire finding — the beta seam is unemitted)."""
        self._red("MGW-F11")

    def test_mgw_f12_usage_ttl_split(self):
        """MGW-F12: the claude-messages session usage.json carries no
        TTL-split cache fields — neither the canonical
        ephemeral_5m_input_tokens/ephemeral_1h_input_tokens names nor any
        5m/1h key variant — in the session or any turns row."""
        self._red("MGW-F12")

    def test_ow_f1_prompt_cache_retention(self):
        """OW-F1: body.prompt_cache_retention absent (prompt_cache_key IS
        present — a different field, out of scope)."""
        self._red("OW-F1")

    def test_ow_f2a_service_tier(self):
        """OW-F2a: body.service_tier absent (hardcoded None)."""
        self._red("OW-F2a")

    def test_ow_f2b_max_tool_calls(self):
        """OW-F2b: body.max_tool_calls absent (hardcoded None)."""
        self._red("OW-F2b")

    def test_ow_f2c_parallel_tool_calls(self):
        """OW-F2c: body.parallel_tool_calls absent (hardcoded None)."""
        self._red("OW-F2c")

    def test_ow_f3_reasoning_summary_doc_control(self):
        """OW-F3: reasoning.summary == 'concise' — CHARACTERIZATION/DOC-CONTROL
        pin (the documented hardcode at responses.rs), not a hard RED; it
        re-ratchets if the hardcode moves or the cut adds gating."""
        self._red("OW-F3")

    def test_ow_f4_reasoning_context_mode(self):
        """OW-F4: body.reasoning present, with no context/mode keys (the fork
        Reasoning type has no context/mode surface)."""
        self._red("OW-F4")

    def test_ow_f9_body_identity(self):
        """OW-F9: no body.user, no body.safety_identifier, no x-grok-user-id
        header — wire finding: the identity surface is thinner than the
        docs claim (neither body nor header identity)."""
        self._red("OW-F9")

    def test_95_xresp_argtrunc(self):
        """apex-ayl.95: the use_tool function_call_arguments.done frame on
        /v1/responses (glm-5.2) carries proxy-truncated 202-char INVALID
        arguments (the final '}' dropped) — the RED state of XRESP-ARGTRUNC-1.
        Green (post proxy-fix): the same frame's arguments is VALID JSON,
        len 203, tail 'responses"}}' (the SDD byte contract)."""
        self._red("apex-ayl.95")

    def test_95_guard_echo_frame_valid(self):
        """apex-ayl.95 GUARD pin (SDD RED 2-4 class — must pass BOTH pre- and
        post-fix): the run_terminal_command echo .done frame in the same file
        stays VALID (len 85)."""
        ok, detail = guard_95(self._doc("openai/xresp-argtrunc-01.jsonl"))
        self.assertTrue(ok, detail)

    def test_84_auxmodel_fallback_wire(self):
        """apex-ayl.84 PRESENCE pin (inverted from the 18 absence rows): the
        synthetic aux entry hardcodes ApiBackend::Responses, so a request on
        the /v1/responses path carries body.model probe-aux-01 — while the
        session's main call rides /v1/messages (claude-sonnet-5), the wire
        the aux should inherit. Green (post-cut): no /v1/responses request
        carries the probe slug (session-wire aux or catalog-guarded slug)."""
        self._red("apex-ayl.84")
        main = self._doc("openai/auxmodel-01-req-005.json")
        self.assertEqual(main.get("path"), "/v1/messages", "control: main call rides the session wire")
        self.assertEqual((main.get("body") or {}).get("model"), "claude-sonnet-5", "control: main call model")

    def test_meta_sha256_matches_fixtures(self):
        """META.json lists all 20 gap rows (the +2 c95-84 rows; the control
        fixture has no row by design); every fixture hash re-verifies against
        the fixture bytes on disk."""
        entries = self.meta["fixtures"]
        self.assertEqual(len(entries), len(GAP_SPECS), "META.json gap-row count")
        self.assertEqual(sorted(e["gap"] for e in entries), sorted(GAP_BY_ID), "META.json gap set")
        for entry in entries:
            path = os.path.join(FIXTURE_DIR, entry["fixture"])
            self.assertTrue(os.path.isfile(path), "missing fixture: %s" % entry["fixture"])
            self.assertEqual(sha256_file(path), entry["sha256"], "sha256 mismatch: %s" % entry["fixture"])

    def test_fixtures_carry_no_raw_keys(self):
        """Raw-key sweep over every fixture byte (gap fixtures + control
        fixtures): 0 hits (masked headers only)."""
        for fixture_rel in sorted(set(FIXTURE_FILES) | set(CONTROL_FIXTURES)):
            with open(os.path.join(FIXTURE_DIR, fixture_rel), "rb") as fh:
                blob = fh.read()
            self.assertIsNone(RAW_KEY_RE.search(blob), "raw key pattern in fixture %s" % fixture_rel)

    def test_fixture_shape_pins(self):
        """Envelope shape + pinned n/method/path per request fixture; the
        usage fixture is a claude-messages session + turn-row doc."""
        for fixture_rel, (n, path) in sorted(ENVELOPE_PINS.items()):
            doc = self._doc(fixture_rel)
            for key in ("n", "method", "path", "ts", "headers", "body"):
                self.assertIn(key, doc, "%s: missing envelope key %s" % (fixture_rel, key))
            self.assertEqual(doc["n"], n, "%s: pinned n" % fixture_rel)
            self.assertEqual(doc["method"], "POST", "%s: pinned method" % fixture_rel)
            self.assertEqual(doc["path"], path, "%s: pinned path" % fixture_rel)
            self.assertIsInstance(doc["headers"], dict, "%s: headers" % fixture_rel)
            self.assertIsInstance(doc["body"], dict, "%s: body" % fixture_rel)
        usage = self._doc("msgw/ttlusage-01.json")
        self.assertIsInstance(usage.get("session"), dict, "usage fixture: session row")
        self.assertTrue(usage.get("turns"), "usage fixture: turns rows")
        self.assertEqual(usage["session"].get("primaryModelId"), "claude-sonnet-5", "usage fixture: model row")
        for fixture_rel, model in sorted(MODEL_PINS.items()):
            doc = self._doc(fixture_rel)
            self.assertEqual((doc.get("body") or {}).get("model"), model, "%s: pinned model" % fixture_rel)
        sse = self._doc("openai/xresp-argtrunc-01.jsonl")
        self.assertIsInstance(sse, list, "xresp fixture: records list")
        self.assertTrue(sse, "xresp fixture: non-empty")
        self.assertIn("headers", sse[0], "xresp fixture: leading response-header record")
        frame_records = [r for r in sse if isinstance(r, dict) and isinstance(r.get("frame"), str)]
        self.assertEqual(len(frame_records), 109, "xresp fixture: frame record census")
        done_names = sorted(p.get("name") for _fi, p in _sse_done_frames(sse) if p.get("name"))
        self.assertEqual(done_names, ["run_terminal_command", "use_tool"], "xresp fixture: named .done frames")


# ---------------------------------------------------------------------------
# post-cut gate
# ---------------------------------------------------------------------------


def _collect_envelopes(capture_dir):
    """All request envelopes ({"n","method","path",...,"body" dict}) under
    capture_dir — for dir-scoped green asserts (apex-ayl.84). META.json and
    non-envelope files (e.g. the resp-*.jsonl SSE capture) are skipped."""
    envelopes = []
    for dirpath, _dirnames, filenames in os.walk(capture_dir):
        for name in sorted(filenames):
            if name == "META.json" or not name.endswith(".json"):
                continue
            try:
                doc = _load_json(os.path.join(dirpath, name))
            except (ValueError, OSError):
                continue
            if isinstance(doc, dict) and "path" in doc and "method" in doc and isinstance(doc.get("body"), dict):
                envelopes.append(doc)
    return envelopes


def gate(capture_dir):
    """Post-cut acceptance gate over a re-recorded wiresurface fixture dir.

    For each of the 20 gap rows, load <dir>/<fixture relpath> (the
    wiresurface layout: msgw/ + openai/ subdirs, same file names as the
    recorded fixtures — a re-mint keeps the names) and run the GREEN assert:
    the capture must SHOW the owed wire surface (field/header present or
    knob emittable — for the inverted apex-ayl.84 presence row, the owed
    state is the probe slug being GONE from /v1/responses). dirscan specs
    (apex-ayl.84) also scan every request envelope under the gate dir; specs
    with a guard (apex-ayl.95) must keep the guard pin passing — a guard
    failure is a finding even when the green assert is met. Any unmet green
    assert or missing fixture is a finding. Returns (passed, findings);
    findings are human-readable strings carrying gap id and file path.
    Exit 0 = GREEN (post-cut acceptance), 1 = RED (surface still dropped)."""
    envelopes = _collect_envelopes(capture_dir) if any(spec.get("dirscan") for spec in GAP_SPECS) else []
    findings = []
    for spec in GAP_SPECS:
        path = os.path.join(capture_dir, spec["fixture"])
        if not os.path.isfile(path):
            findings.append("%s: fixture missing in gate dir: %s" % (spec["gap"], path))
            GREEN_RESULTS[spec["gap"]] = (False, "fixture missing")
            continue
        try:
            loader = _load_sse_jsonl if spec["fixture"].endswith(".jsonl") else _load_json
            doc = loader(path)
        except (ValueError, OSError) as exc:
            findings.append("%s: fixture unreadable: %s (%s)" % (spec["gap"], path, exc))
            GREEN_RESULTS[spec["gap"]] = (False, "fixture unreadable")
            continue
        if spec.get("dirscan"):
            ok, detail = spec["green"](doc, envelopes)
        else:
            ok, detail = spec["green"](doc)
        GREEN_RESULTS[spec["gap"]] = (ok, detail)
        if not ok:
            findings.append("%s: %s (%s)" % (spec["gap"], detail, path))
        guard = spec.get("guard")
        if guard is not None:
            guard_ok, guard_detail = guard(doc)
            if not guard_ok:
                findings.append("%s GUARD: %s (%s)" % (spec["gap"], guard_detail, path))
    return (not findings, findings)


def _resolve_gate_dir(specified):
    """Resolve the --gate dir. An explicit DIR is tried as-is (absolute or
    CWD-relative), then worktree-root-relative; the default DIR (the recorded
    fixtures) is tried worktree-root-relative, then CWD-relative. Returns the
    first existing dir or None."""
    if specified:
        if os.path.isabs(specified):
            candidates = [specified]
        else:
            candidates = [os.path.join(os.getcwd(), specified), os.path.join(WORKTREE_ROOT, specified)]
    else:
        candidates = [os.path.join(WORKTREE_ROOT, DEFAULT_GATE_REL), os.path.join(os.getcwd(), DEFAULT_GATE_REL)]
    for candidate in candidates:
        if os.path.isdir(candidate):
            return candidate
    return None


def run_bare():
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(sys.modules[__name__])
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    print()
    print_coverage_table(RED_RESULTS, "bare")
    if not result.wasSuccessful():
        print("FIXTURE SUITE RED: %d failure(s) — the recorded state no longer holds (fixture drift or a cut landed without a re-mint)"
              % (len(result.failures) + len(result.errors)))
        return 1
    print("FIXTURE SUITE GREEN: all %d pins hold on the recorded fixtures — the %d-gap RED state is characterized"
          % (result.testsRun, len(GAP_SPECS)))
    return 0


def run_gate(capture_dir):
    passed, findings = gate(capture_dir)
    for finding in findings:
        print(finding)
    print()
    print_coverage_table(GREEN_RESULTS, "gate")
    if passed:
        print("GATE GREEN: all %d green asserts met in %s (post-cut acceptance)" % (len(GAP_SPECS), capture_dir))
        return 0
    print("GATE RED: %d green assert(s) unmet in %s (param surface still dropped pre-cut)" % (len(findings), capture_dir))
    return 1


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    parser = argparse.ArgumentParser(
        description="wire-surface param-surface gap offline fixture tests + post-cut gate",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gate", nargs="?", const=DEFAULT_GATE_REL, default=None, metavar="DIR",
                        help="run the GREEN asserts over a fixture dir instead of the "
                             "recorded-fixture characterization suite; default DIR = %s "
                             "(the recorded fixtures — RED today); point DIR at a "
                             "re-recorded post-cut wiresurface-layout dir for acceptance" % DEFAULT_GATE_REL)
    ns, _rest = parser.parse_known_args(argv)
    if ns.gate is None:
        return run_bare()
    resolved = _resolve_gate_dir(ns.gate)
    if resolved is None:
        print("usage error: gate dir not found: %s (tried worktree root %s and CWD %s)"
              % (ns.gate, WORKTREE_ROOT, os.getcwd()))
        return 2
    return run_gate(resolved)


if __name__ == "__main__":
    sys.exit(main())
