"""Strict, privacy-preserving Responses replay measurements for check_wire."""
import hashlib
import json
from pathlib import Path
import re


class CaptureError(ValueError):
    """A safe diagnostic: never include capture values or parser exceptions."""


def validate_spec(spec):
    errors = []
    if set(spec) - {"id", "kind", "file", "where", "label", "replay"}:
        errors.append("reasoning_replay does not support selection or comparison knobs")
    if not isinstance(spec.get("where"), dict) or not spec["where"]:
        errors.append("reasoning_replay needs a nonempty where scope")
    if not isinstance(spec.get("file"), str) or not spec["file"]:
        errors.append("reasoning_replay needs a request file glob")
    config = spec.get("replay")
    if not isinstance(config, dict):
        return errors + ["reasoning_replay needs a replay object"]
    if set(config) - {"version", "segments", "require_tool_continuation"}:
        errors.append("unsupported replay option")
    if type(config.get("version")) is not int or config["version"] != 1:
        errors.append("unsupported replay version (expected 1)")
    if type(config.get("require_tool_continuation", False)) is not bool:
        errors.append("require_tool_continuation must be boolean")
    segments = config.get("segments")
    if not isinstance(segments, list) or not segments:
        return errors + ["replay needs ordered model segments"]
    previous = None
    for segment in segments:
        if not isinstance(segment, dict):
            errors.append("replay segment must be an object")
            continue
        if set(segment) - {"model", "min_requests", "fields", "require_content",
                           "require_empty_summary"}:
            errors.append("unsupported replay segment option")
        model = segment.get("model")
        if not isinstance(model, str) or not model.strip() or model == previous:
            errors.append("segments need nonempty models and observable model transitions")
        previous = model
        if type(segment.get("min_requests")) is not int or segment["min_requests"] < 2:
            errors.append("each replay segment needs min_requests >= 2")
        fields = segment.get("fields", ["encrypted_content"])
        if (not isinstance(fields, list) or not fields
                or any(f not in ("encrypted_content", "summary", "content") for f in fields)
                or "encrypted_content" not in fields or len(set(fields)) != len(fields)):
            errors.append("fields must uniquely include encrypted_content and optional summary/content")
        for flag in ("require_content", "require_empty_summary"):
            if type(segment.get(flag, False)) is not bool:
                errors.append("replay shape requirements must be boolean")
    return errors


def _fail(detail, n=None):
    suffix = "" if n is None else " (request_n=%d)" % n
    raise CaptureError(detail + suffix)


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            _fail("duplicate JSON key; capture is ambiguous")
        result[key] = value
    return result


def _json(text):
    try:
        return json.loads(text, object_pairs_hook=_unique_object,
                          parse_constant=lambda _: _fail("nonfinite JSON value"))
    except (ValueError, UnicodeError, RecursionError):
        _fail("unparseable JSON capture")


def _read(path):
    try:
        text = Path(path).read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        _fail("missing or unreadable capture")
    if not text.strip():
        _fail("empty capture")
    return text


def _number(path, prefix, extension):
    match = re.fullmatch(prefix + r"-(\d+)\." + extension, Path(path).name)
    if not match:
        _fail("unsupported capture filename")
    return int(match[1])


def _digest(value):
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


_RESPONSE_EVENTS = {
    "response.created", "response.queued", "response.in_progress", "response.completed",
    "response.failed", "response.incomplete", "response.error",
    "response.output_item.added", "response.output_item.done",
    "response.content_part.added", "response.content_part.done",
    "response.output_text.delta", "response.output_text.done", "response.output_text.annotation.added",
    "response.refusal.delta", "response.refusal.done",
    "response.reasoning_summary_part.added", "response.reasoning_summary_part.done",
    "response.reasoning_summary_text.delta", "response.reasoning_summary_text.done",
    "response.reasoning_text.delta", "response.reasoning_text.done",
    "response.function_call_arguments.delta", "response.function_call_arguments.done",
    "response.custom_tool_call_input.delta", "response.custom_tool_call_input.done",
}


def _completed_response(path, n, request):
    records = [_json(line) for line in _read(path).splitlines() if line.strip()]
    header = records[0]
    if (not isinstance(header, dict) or type(header.get("n")) is not int
            or header["n"] != n or type(header.get("status")) is not int):
        _fail("response/request pairing mismatch", n)
    if header.get("truncated") or header.get("error"):
        _fail("response capture is truncated or failed", n)
    if header["status"] != 200:
        _fail("response HTTP status is not 200", n)
    frames = []
    for index, record in enumerate(records[1:]):
        if not isinstance(record, dict):
            _fail("unsupported response record", n)
        if "n" in record and (type(record["n"]) is not int or record["n"] != n):
            _fail("response/request pairing mismatch", n)
        if (type(record.get("frame_index")) is not int or record["frame_index"] != index
                or not isinstance(record.get("frame"), str)
                or record.get("error") or record.get("truncated")):
            _fail("truncated or unsupported response frames", n)
        frames.append(record["frame"])
    if not frames:
        _fail("empty response frames", n)
    stream = request.get("stream")
    if type(stream) is not bool:
        _fail("unsupported Responses stream mode", n)
    if not stream:
        response = _json("".join(frames))
    else:
        response = None
        completed_seen = False
        done = False
        for frame in frames:
            for block in frame.strip().split("\n\n"):
                data, event = [], None
                for line in block.splitlines():
                    if line.startswith("data:"):
                        data.append(line[5:].lstrip())
                    elif line.startswith("event:"):
                        if event is not None:
                            _fail("ambiguous SSE event", n)
                        event = line[6:].strip()
                    elif line and not line.startswith(":"):
                        _fail("unsupported SSE field", n)
                if not data:
                    if event is not None:
                        _fail("SSE event missing data", n)
                    continue
                payload = "\n".join(data)
                if payload == "[DONE]":
                    if not completed_seen or done:
                        _fail("SSE done without a unique completed response", n)
                    done = True
                    continue
                parsed = _json(payload)
                if not isinstance(parsed, dict):
                    _fail("unsupported Responses event payload", n)
                name = parsed.get("type")
                if (not isinstance(name, str) or name not in _RESPONSE_EVENTS
                        or (event is not None and event != name)):
                    _fail("unsupported Responses event type", n)
                if name in ("response.failed", "response.incomplete", "response.error"):
                    _fail("response did not complete", n)
                if completed_seen or done:
                    _fail("events after completed response; capture is ambiguous", n)
                if name == "response.completed":
                    completed_seen = True
                    response = parsed.get("response")
                    if not isinstance(response, dict):
                        _fail("completed event missing a response object", n)
        if not completed_seen:
            _fail("missing completed response (possibly truncated)", n)
    if (not isinstance(response, dict) or response.get("object") != "response"
            or response.get("status") != "completed" or response.get("error")
            or response.get("incomplete_details") or not isinstance(response.get("output"), list)):
        _fail("unsupported or incomplete Responses body", n)
    if response.get("model") != request["model"]:
        _fail("response source model mismatch; aliases unsupported", n)
    if not isinstance(response.get("id"), str) or not response["id"]:
        _fail("completed response missing identity", n)
    return response


ITEM_TYPES = {"message", "reasoning", "function_call", "function_call_output",
              "custom_tool_call", "custom_tool_call_output"}


def _items(items, n, output=False):
    if not isinstance(items, list):
        _fail("unsupported Responses input/output (typed array required)", n)
    states, counts, calls = {}, {}, {}
    shape = {"summary_empty": 0, "content_nonempty": 0, "litellm_outer": 0,
             "sglang_v1_bare_marker": 0}
    ids = set()
    call_types = (("function_call", "custom_tool_call") if output
                  else ("function_call_output", "custom_tool_call_output"))
    for item in items:
        if (not isinstance(item, dict) or not isinstance(item.get("type"), str)
                or item["type"] not in ITEM_TYPES):
            _fail("unsupported Responses item type", n)
        kind = item["type"]
        counts[kind] = counts.get(kind, 0) + 1
        if kind in call_types:
            call_id = item.get("call_id")
            if not isinstance(call_id, str) or not call_id or call_id in calls:
                _fail("missing or duplicate tool call identity", n)
            if output:
                if "status" in item and item["status"] != "completed":
                    _fail("incomplete or unsupported tool call status", n)
            elif not isinstance(item.get("output"), str) or not item["output"]:
                _fail("unsupported or empty tool result payload", n)
            calls[call_id] = kind + "_output" if output else kind
        if kind != "reasoning":
            continue
        if output:
            if item.get("status") not in (None, "completed"):
                _fail("incomplete reasoning item in completed response", n)
            identity = item.get("id")
            if not isinstance(identity, str) or not identity or identity in ids:
                _fail("missing or duplicate completed reasoning identity", n)
            ids.add(identity)
        for field, part_type in (("summary", "summary_text"), ("content", "reasoning_text")):
            parts = item.get(field, [])
            if (not isinstance(parts, list)
                    or any(not isinstance(part, dict) or part.get("type") != part_type
                           or not isinstance(part.get("text"), str) for part in parts)):
                _fail("unsupported reasoning plaintext field shape", n)
        has_content = any(part["text"].strip() for part in item.get("content", []))
        empty_summary = item.get("summary") == []
        shape["content_nonempty"] += bool(has_content)
        shape["summary_empty"] += empty_summary
        value = item.get("encrypted_content")
        if value is not None and not isinstance(value, str):
            _fail("unsupported encrypted_content type", n)
        if not value or not value.strip():
            continue
        try:
            encoded = value.encode("utf-8")
        except UnicodeEncodeError:
            _fail("unsupported Unicode in encrypted_content", n)
        sha = hashlib.sha256(encoded).hexdigest()
        if sha in states:
            _fail("duplicate ciphertext item; lineage is ambiguous", n)
        states[sha] = {"sha256": sha, "source_n": n,
                       "fields": {field: _digest([field in item, item.get(field)])
                                  for field in ("encrypted_content", "summary", "content")},
                       "content_nonempty": bool(has_content), "summary_empty": empty_summary}
        shape["litellm_outer"] += value.startswith("litellm_enc:")
        shape["sglang_v1_bare_marker"] += value.startswith("sglang-reasoning-v1.")
    return states, counts, shape, calls


def _session(doc, n):
    headers = doc.get("headers")
    if not isinstance(headers, dict):
        _fail("missing request session identity", n)
    values = [value for key, value in headers.items() if key.lower() == "x-grok-session-id"]
    if len(values) != 1 or not isinstance(values[0], str) or not values[0]:
        _fail("missing or ambiguous request session identity", n)
    return values[0]


def _exchanges(spec, base, resolve_glob, wire_filter):
    paths = resolve_glob(spec["file"], base)
    if not paths:
        _fail("missing request captures")
    requests, by_path, responses = {}, {}, {}
    models = {segment["model"] for segment in spec["replay"]["segments"]}
    for path in paths:
        n = _number(path, "req", "json")
        doc = _json(_read(path))
        if (not isinstance(doc, dict) or type(doc.get("n")) is not int
                or doc["n"] != n or n in requests):
            _fail("request identity missing, duplicate or mismatched", n)
        if doc.get("truncated") or doc.get("error"):
            _fail("request capture is truncated or failed", n)
        requests[n] = doc
        by_path[path] = n
        body = doc.get("body")
        if (not isinstance(doc.get("method"), str) or not doc["method"]
                or not isinstance(doc.get("path"), str) or not doc["path"]):
            _fail("missing request method/path identity", n)
        if doc.get("method") == "POST" and not isinstance(body, dict):
            _fail("unsupported request body", n)
        if isinstance(body, dict) and (
                "model" in body or doc.get("path") in ("/v1/responses", "/responses")):
            if not isinstance(body.get("model"), str) or not body["model"]:
                _fail("missing or unsupported request model identity", n)
    for path in resolve_glob("resp-*.jsonl", base):
        n = _number(path, "resp", "jsonl")
        if n in responses or n not in requests:
            _fail("duplicate response or missing paired request", n)
        responses[n] = path
    header_scope = {key: value for key, value in spec["where"].items()
                    if key.removeprefix("$.").startswith("headers.")}
    scoped = sorted(wire_filter(paths, header_scope), key=by_path.get)
    selected = wire_filter(scoped, spec["where"])
    declared = [by_path[path] for path in selected
                if isinstance(requests[by_path[path]].get("body"), dict)
                and requests[by_path[path]]["body"].get("model") in models]
    if not declared:
        _fail("no declared model requests matched the scope")
    source_session = _session(requests[declared[0]], declared[0])
    exchanges = []
    for path in scoped:
        n = by_path[path]
        doc = requests[n]
        body = doc.get("body")
        if not isinstance(body, dict) or "model" not in body:
            if doc["method"] == "POST" and _session(doc, n) == source_session:
                _fail("source-session request lacks model identity", n)
            continue
        session = _session(doc, n)
        if session != source_session:
            continue
        if doc["method"] != "POST" or doc["path"] not in ("/v1/responses", "/responses"):
            _fail("unsupported protocol in selected source session", n)
        if body["model"] not in models:
            _fail("undeclared model in selected source session", n)
        if path not in selected:
            _fail("ordinary source-session request excluded by wire filter", n)
        if n not in responses:
            _fail("missing paired response capture", n)
        if body.get("store") is not False:
            _fail("Responses request must use store=false", n)
        include = body.get("include")
        if (not isinstance(include, list) or any(not isinstance(x, str) for x in include)
                or "reasoning.encrypted_content" not in include):
            _fail("encrypted reasoning include not requested", n)
        response = _completed_response(responses[n], n, body)
        exchanges.append({"n": n, "request": body, "response": response,
                          "session": session, "file": Path(path).name,
                          "response_file": Path(responses[n]).name})
    if not exchanges:
        _fail("no declared model requests matched the scope")
    return exchanges


def evaluate(spec, base, resolve_glob, wire_filter):
    """Return (ok, safe detail, citation) using the runner's glob/where semantics."""
    measurement = {"version": 1, "include_requests": 0, "minted_ciphertexts": 0,
                   "replayed_ciphertexts": 0, "tool_continuations": 0, "exchanges": [],
                   "backend_attribution": "unsupported; markers are not backend identity"}
    cite = {"measurement": measurement}
    try:
        errors = validate_spec(spec)
        if errors:
            _fail("; ".join(errors))
        exchanges = _exchanges(spec, base, resolve_glob, wire_filter)
        cite.update({"file": exchanges[0]["file"], "request_n": exchanges[0]["n"],
                     "files": [name for ex in exchanges
                               for name in (ex["file"], ex["response_file"])]})
        groups = []
        session = exchanges[0]["session"]
        response_ids = set()
        for ex in exchanges:
            if ex["session"] != session:
                _fail("request session identity changed; source lineage is ambiguous", ex["n"])
            identity = ex["response"]["id"]
            if identity in response_ids:
                _fail("duplicate completed response identity", ex["n"])
            response_ids.add(identity)
            if not groups or groups[-1][0]["request"]["model"] != ex["request"]["model"]:
                groups.append([])
            groups[-1].append(ex)
        segments = spec["replay"]["segments"]
        if [group[0]["request"]["model"] for group in groups] != [s["model"] for s in segments]:
            _fail("observed model segments differ from declared source/destination sequence")
        known = {}
        for segment_index, (group, config) in enumerate(zip(groups, segments)):
            if len(group) < config["min_requests"]:
                _fail("segment lacks source response and next-request evidence")
            required, previous_calls = {}, {}
            for index, ex in enumerate(group):
                n, model = ex["n"], ex["request"]["model"]
                inputs, in_counts, in_shape, tool_outputs = _items(ex["request"].get("input"), n)
                outputs, out_counts, out_shape, calls = _items(ex["response"]["output"], n, output=True)
                row = {"request_n": n, "response_n": n, "segment": segment_index,
                       "model": model, "include_requested": True, "input_types": in_counts,
                       "output_types": out_counts, "input_shape": in_shape, "output_shape": out_shape,
                       "minted_ciphertexts": len(outputs), "replayed_ciphertexts": len(inputs),
                       "lineage": []}
                measurement["exchanges"].append(row)
                measurement["include_requests"] += 1
                measurement["minted_ciphertexts"] += len(outputs)
                measurement["replayed_ciphertexts"] += len(inputs)
                missing = set(required) - set(inputs)
                if missing:
                    _fail("missing replay lineage: %d required item(s)" % len(missing), n)
                for sha, state in inputs.items():
                    source = known.get(sha)
                    if source is None:
                        _fail("unobserved or modified ciphertext source", n)
                    if source["model"] != model:
                        _fail("foreign source ciphertext reached destination", n)
                    for field in config.get("fields", ["encrypted_content"]):
                        if state["fields"][field] != source["fields"][field]:
                            _fail("%s field changed despite ciphertext equality" % field, n)
                    row["lineage"].append({"source_n": source["source_n"], "sha256": sha,
                                           "ciphertext_equal": True,
                                           "fields_equal": config.get("fields", ["encrypted_content"])})
                    required[sha] = source
                if index == 0 and not (set(outputs) - set(known)):
                    _fail("first segment response minted no new nonempty reasoning state", n)
                for sha, state in outputs.items():
                    if config.get("require_content") and not state["content_nonempty"]:
                        _fail("minted reasoning lacks nonempty plaintext content", n)
                    if config.get("require_empty_summary") and not state["summary_empty"]:
                        _fail("minted reasoning does not have an empty summary", n)
                    prior = known.get(sha)
                    if prior and (prior["model"] != model or prior["fields"] != state["fields"]):
                        _fail("ambiguous ciphertext mint provenance", n)
                    state["model"] = model
                    known[sha] = state
                    required[sha] = state
                if any(call_id in tool_outputs and tool_outputs[call_id] != kind
                       for call_id, kind in previous_calls.items()):
                    _fail("incompatible post-tool call/result types", n)
                if previous_calls and all(tool_outputs.get(call_id) == kind
                                          for call_id, kind in previous_calls.items()):
                    measurement["tool_continuations"] += 1
                    row["post_tool_continuation"] = True
                previous_calls = calls
        if spec["replay"].get("require_tool_continuation") and not measurement["tool_continuations"]:
            _fail("missing completed-call to next-request tool-output lineage")
        detail = ("replay v1: %d completed exchanges, %d emitted ciphertext items, "
                  "%d replayed with SHA equality, %d post-tool continuations; "
                  "client field preservation only, not semantic reuse"
                  % (len(exchanges), measurement["minted_ciphertexts"],
                     measurement["replayed_ciphertexts"], measurement["tool_continuations"]))
        return True, detail, cite
    except CaptureError as exc:
        return False, "replay v1 FAIL-CLOSED: " + str(exc), cite
