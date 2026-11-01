#!/usr/bin/env python3
"""Offline Responses lineage tests; all captures are synthetic tempfiles."""
import copy
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
import run

FLASH = "qwen3.8-flash-next"
ASTRA = "gpt-6-astra"


def reasoning(label, content=True):
    item = {"type": "reasoning", "id": "rs_" + label, "summary": [],
            "encrypted_content": "litellm_enc:synthetic;sglang-reasoning-v1." + label}
    if content:
        item["content"] = [{"type": "reasoning_text", "text": "synthetic private " + label}]
    return item


def segment(model, **extra):
    return {"model": model, "min_requests": 2, **extra}


def pin(*segments, **extra):
    return {"id": "replay", "kind": "reasoning_replay", "file": "req-*.json",
            "where": {"method": "POST", "path": "/v1/responses"},
            "replay": {"version": 1, "segments": list(segments or [segment(FLASH)]), **extra}}


class ReasoningReplayTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="flash-replay-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def exchange(self, n, model, inputs, outputs, stream=True, session="synthetic-session"):
        req = {"n": n, "method": "POST", "path": "/v1/responses",
               "headers": {"x-grok-session-id": session},
               "body": {"model": model, "store": False, "stream": stream,
                        "include": ["reasoning.encrypted_content"], "input": inputs}}
        response = {"id": "resp_synthetic_" + str(n), "object": "response",
                    "model": model, "status": "completed", "output": outputs}
        records = [{"n": n, "status": 200, "headers": {}}]
        if stream:
            events = [{"type": "response.output_item.done", "item": item}
                      for item in outputs]
            events.append({"type": "response.completed", "response": response})
            records.extend({"frame_index": i, "frame": "event: " + event["type"]
                            + "\ndata: " + json.dumps(event)}
                           for i, event in enumerate(events))
        else:
            records.append({"frame_index": 0, "frame": json.dumps(response)})
        self.write_json("req-%03d.json" % n, req)
        self.write_records(n, records)
        return req, records

    def write_json(self, name, value):
        (self.root / name).write_text(json.dumps(value), encoding="utf-8")

    def write_records(self, n, records):
        (self.root / ("resp-%03d.jsonl" % n)).write_text(
            "".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")

    def check(self, spec=None):
        try:
            return run.check_wire(spec or pin(), str(self.root))
        except ValueError as exc:
            self.fail("existing wire engine cannot evaluate parsed replay: %s" % exc)

    def native(self, replayed=None, stream=True):
        first = reasoning("flash_one")
        self.exchange(9, FLASH, [], [first], stream=stream)
        self.exchange(10, FLASH, [first] if replayed is None else replayed,
                      [reasoning("flash_two")], stream=stream)
        return first

    def test_native_recurrence_counts_only_completed_output(self):
        self.native()
        result = self.check()
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(result.cite["measurement"]["minted_ciphertexts"], 2)
        self.assertEqual(result.cite["measurement"]["replayed_ciphertexts"], 1)
        self.assertEqual(result.cite["measurement"]["include_requests"], 2)

    def test_include_is_not_replayed_item_state(self):
        self.native(replayed=[])
        result = self.check()
        self.assertFalse(result.ok)
        self.assertIn("missing replay lineage", result.detail)

    def test_modified_ciphertext_fails_without_disclosing_it(self):
        wrong = reasoning("modified")
        self.native(replayed=[wrong])
        result = self.check()
        self.assertFalse(result.ok)
        published = json.dumps(vars(result))
        self.assertNotIn(wrong["encrypted_content"], published)
        self.assertNotIn(wrong["content"][0]["text"], published)

    def cross(self, source=FLASH, destination=ASTRA):
        one, two = reasoning("source_one"), reasoning("source_two")
        native = reasoning("destination_one")
        self.exchange(1, source, [], [one])
        self.exchange(2, source, [one], [two])
        self.exchange(3, destination, [], [native])
        self.exchange(4, destination, [native], [reasoning("destination_two")])
        return one, native, pin(segment(source), segment(destination))

    def test_cross_boundary_keeps_destination_native_ciphertext(self):
        for source, destination in ((FLASH, ASTRA), (ASTRA, FLASH)):
            with self.subTest(source=source):
                _, _, spec = self.cross(source, destination)
                result = self.check(spec)
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(result.cite["measurement"]["replayed_ciphertexts"], 2)

    def test_wrong_source_ciphertext_fails_at_destination(self):
        source, native, spec = self.cross()
        self.exchange(4, ASTRA, [native, source], [reasoning("destination_two")])
        result = self.check(spec)
        self.assertFalse(result.ok)
        self.assertIn("foreign source ciphertext", result.detail)

    def test_destination_must_mint_before_its_next_request(self):
        _, native, spec = self.cross()
        self.exchange(3, ASTRA, [], [])
        self.assertFalse(self.check(spec).ok)
        self.exchange(3, ASTRA, [], [native])
        self.exchange(4, ASTRA, [], [reasoning("destination_two")])
        self.assertFalse(self.check(spec).ok)

    def test_switch_back_allows_proven_same_model_state(self):
        first, _, spec = self.cross()
        returned = reasoning("returned")
        self.exchange(5, FLASH, [first], [returned])
        self.exchange(6, FLASH, [first, returned], [reasoning("return_two")])
        spec["replay"]["segments"].append(segment(FLASH))
        result = self.check(spec)
        self.assertTrue(result.ok, result.detail)

    def test_plaintext_fields_are_checked_separately(self):
        spec = pin(segment(FLASH, fields=["encrypted_content", "summary", "content"],
                           require_content=True, require_empty_summary=True))
        first = self.native()
        self.assertTrue(self.check(spec).ok)
        changed = copy.deepcopy(first)
        changed.pop("content")
        self.native(replayed=[changed])
        self.assertTrue(self.check().ok, "cipher-only gate does not claim plaintext equality")
        result = self.check(spec)
        self.assertFalse(result.ok)
        self.assertIn("content field changed", result.detail)

    def test_numeric_order_not_mtime_or_lexical_order(self):
        self.native()
        (self.root / "req-009.json").rename(self.root / "req-9.json")
        (self.root / "resp-009.jsonl").rename(self.root / "resp-9.jsonl")
        os.utime(self.root / "req-9.json", (9000, 9000))
        os.utime(self.root / "req-010.json", (1, 1))
        self.assertTrue(self.check().ok)

    def test_nonstream_completed_body(self):
        self.native(stream=False)
        self.assertTrue(self.check().ok)

    def test_missing_pair_even_with_enough_complete_pairs(self):
        first = self.native()
        self.exchange(11, FLASH, [first, reasoning("flash_two")], [reasoning("third")])
        (self.root / "resp-011.jsonl").unlink()
        result = self.check()
        self.assertFalse(result.ok)
        self.assertIn("missing paired response", result.detail)

    def test_unparseable_extra_request_is_not_filtered_away(self):
        self.native()
        (self.root / "req-011.json").write_text('{"body":', encoding="utf-8")
        self.assertFalse(self.check().ok)

    def test_missing_request_is_not_hidden_by_orphan_response(self):
        self.native()
        (self.root / "req-010.json").unlink()
        self.assertFalse(self.check().ok)

    def test_no_capture_is_not_an_absence_pass(self):
        self.assertFalse(self.check().ok)
        self.assertFalse(run.check_wire(pin(), None).ok)

    def test_capture_corruption_fails_closed(self):
        for defect in ("empty_request", "empty_response", "bad_jsonl", "bad_utf8", "no_completed",
                       "wrong_n", "wrong_model", "wrong_session", "store_true", "no_include",
                       "boolean_n", "bad_model_type", "bad_item_type", "bad_cipher_type",
                       "frame_gap", "duplicate_completed", "duplicate_request_number"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as root:
                original_root, self.root = self.root, Path(root)
                try:
                    first = self.native()
                    req_path, resp_path = self.root / "req-010.json", self.root / "resp-009.jsonl"
                    req = json.loads(req_path.read_text())
                    records = [json.loads(ln) for ln in resp_path.read_text().splitlines()]
                    if defect == "empty_request":
                        req_path.write_text("")
                    elif defect == "empty_response":
                        resp_path.write_text("")
                    elif defect == "bad_jsonl":
                        resp_path.write_text(resp_path.read_text() + '{"frame":')
                    elif defect == "bad_utf8":
                        resp_path.write_bytes(b"\xff")
                    elif defect == "no_completed":
                        self.write_records(9, records[:-1])
                    elif defect == "wrong_n":
                        records[0]["n"] = 999
                        self.write_records(9, records)
                    elif defect == "wrong_model":
                        self.exchange(9, ASTRA, [], [first])
                        self.write_json("req-009.json", {**req, "n": 9})
                    elif defect == "wrong_session":
                        req["headers"]["x-grok-session-id"] = "other-session"
                        self.write_json(req_path.name, req)
                    elif defect in ("store_true", "no_include", "boolean_n", "bad_model_type"):
                        if defect == "store_true":
                            req["body"]["store"] = True
                        elif defect == "no_include":
                            req["body"].pop("include")
                        elif defect == "boolean_n":
                            req["n"] = True
                        else:
                            req["body"]["model"] = {}
                        self.write_json(req_path.name, req)
                    elif defect in ("bad_item_type", "bad_cipher_type"):
                        item = copy.deepcopy(first)
                        item["type" if defect == "bad_item_type" else "encrypted_content"] = {}
                        self.exchange(10, FLASH, [item], [reasoning("flash_two")])
                    elif defect == "frame_gap":
                        records[-1]["frame_index"] += 1
                        self.write_records(9, records)
                    elif defect == "duplicate_completed":
                        duplicate = {**records[-1], "frame_index": len(records) - 1}
                        self.write_records(9, records + [duplicate])
                    else:
                        (self.root / "req-9.json").write_bytes((self.root / "req-009.json").read_bytes())
                    self.assertFalse(self.check().ok)
                finally:
                    self.root = original_root

    def test_lifecycle_reasoning_without_completed_output_is_not_minted(self):
        first = self.native()
        _, records = self.exchange(9, FLASH, [], [])
        records.insert(1, {"frame_index": 0, "frame": "data: " + json.dumps(
            {"type": "response.output_item.done", "item": first})})
        records[-1]["frame_index"] = 1
        self.write_records(9, records)
        self.assertFalse(self.check().ok)

    def test_protocol_is_explicitly_unsupported_not_guessed(self):
        self.native()
        req = json.loads((self.root / "req-010.json").read_text())
        req["path"] = "/v1/messages"
        self.write_json("req-010.json", req)
        result = self.check()
        self.assertFalse(result.ok)
        self.assertIn("unsupported protocol", result.detail)

    def test_post_tool_requires_a_completed_call_and_matching_next_output(self):
        first = reasoning("tool_reason")
        call = {"type": "function_call", "call_id": "call_synthetic", "name": "run_terminal_command",
                "arguments": "{}", "id": "fc_synthetic"}
        output = {"type": "function_call_output", "call_id": "call_synthetic", "output": "synthetic"}
        self.exchange(1, FLASH, [], [first, call])
        self.exchange(2, FLASH, [first, output], [reasoning("tool_followup")])
        spec = pin(require_tool_continuation=True)
        result = self.check(spec)
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(result.cite["measurement"]["tool_continuations"], 1)
        output["call_id"] = "wrong_call"
        self.exchange(2, FLASH, [first, output], [reasoning("tool_followup")])
        self.assertFalse(self.check(spec).ok)

    def test_empty_summary_content_premise_is_not_inferred_from_cipher(self):
        first = reasoning("empty_content", content=False)
        self.native(replayed=[first])
        self.exchange(9, FLASH, [], [first])
        spec = pin(segment(FLASH, require_content=True, require_empty_summary=True))
        self.assertTrue(self.check().ok)
        self.assertFalse(self.check(spec).ok)
        first["content"] = [{"type": "reasoning_text", "text": "synthetic"}]
        first["summary"] = [{"type": "summary_text", "text": "synthetic summary"}]
        self.exchange(9, FLASH, [], [first])
        self.exchange(10, FLASH, [first], [reasoning("flash_two")])
        self.assertFalse(self.check(spec).ok)

    def test_plaintext_or_envelope_in_message_text_is_not_item_state(self):
        first = reasoning("flash_one")
        self.native(replayed=[{"type": "message", "role": "user", "content": json.dumps(first)}])
        self.assertFalse(self.check().ok)

    def test_outer_litellm_marker_does_not_identify_a_backend(self):
        first = reasoning("outer_only")
        first["encrypted_content"] = "litellm_enc:synthetic-unclassified"
        self.exchange(1, FLASH, [], [first])
        self.exchange(2, FLASH, [first], [reasoning("second")])
        result = self.check()
        self.assertTrue(result.ok, result.detail)
        report = result.cite["measurement"]
        self.assertIn("unsupported", report["backend_attribution"])
        self.assertEqual(report["exchanges"][0]["output_shape"]["sglang_v1_bare_marker"], 0)
        published = json.dumps(vars(result))
        self.assertNotIn(first["encrypted_content"], published)
        self.assertNotIn(first["content"][0]["text"], published)
        self.assertNotIn(first["id"], published)

    def test_all_paired_evidence_is_audited(self):
        self.native()
        result = self.check()
        self.assertEqual(run.audit_wire_evidence({}, [result], str(self.root)), [])
        (self.root / "resp-010.jsonl").unlink()
        self.assertEqual(len(run.audit_wire_evidence({}, [result], str(self.root))), 1)

    def test_undeclared_same_session_transition_is_not_silently_filtered(self):
        self.native()
        self.exchange(11, "unexpected-model", [], [reasoning("unexpected")])
        native = reasoning("astra_new")
        self.exchange(12, ASTRA, [], [native])
        self.exchange(13, ASTRA, [native], [reasoning("astra_next")])
        result = self.check(pin(segment(FLASH), segment(ASTRA)))
        self.assertFalse(result.ok)
        self.assertIn("undeclared model", result.detail)

    def test_unrelated_session_and_explicitly_excluded_auxiliary_are_distinct(self):
        self.native()
        self.exchange(11, "other-model", [], [], session="unrelated-session")
        self.assertTrue(self.check().ok)
        req, _ = self.exchange(11, "other-model", [], [])
        req["headers"]["x-grok-req-id"] = "xai-title-synthetic"
        self.write_json("req-011.json", req)
        spec = pin()
        spec["where"]["headers.x-grok-req-id"] = {"not_prefix": "xai-"}
        self.assertTrue(self.check(spec).ok)

    def test_explicit_truncation_and_incomplete_item_are_not_completed_state(self):
        for defect in ("request_truncated", "header_truncated", "item_incomplete"):
            with self.subTest(defect=defect):
                first = self.native()
                req_path = self.root / "req-009.json"
                if defect == "request_truncated":
                    req = json.loads(req_path.read_text())
                    req["truncated"] = True
                    self.write_json(req_path.name, req)
                elif defect == "header_truncated":
                    resp = self.root / "resp-009.jsonl"
                    records = [json.loads(ln) for ln in resp.read_text().splitlines()]
                    records[0]["truncated"] = True
                    self.write_records(9, records)
                else:
                    first["status"] = "incomplete"
                    self.exchange(9, FLASH, [], [first])
                self.assertFalse(self.check().ok)

    def insert_events_before_completion(self, n, events):
        path = self.root / ("resp-%03d.jsonl" % n)
        records = [json.loads(line) for line in path.read_text().splitlines()]
        records[-1:-1] = [{"frame": "data: " + json.dumps(event)} for event in events]
        for index, record in enumerate(records[1:]):
            record["frame_index"] = index
        self.write_records(n, records)

    def test_completed_body_cannot_be_repaired_by_a_later_terminal(self):
        for include_body in (True, False):
            with self.subTest(include_body=include_body):
                self.native()
                malformed = {"type": "response.completed"}
                if include_body:
                    malformed["response"] = None
                self.insert_events_before_completion(9, [malformed])
                self.assertFalse(self.check().ok)

    def test_cancelled_and_unknown_events_cannot_precede_completion(self):
        self.native()
        supported = ["response.created", "response.in_progress", "response.output_item.added",
                     "response.output_item.done", "response.content_part.added", "response.content_part.done",
                     "response.output_text.delta", "response.output_text.done",
                     "response.output_text.annotation.added", "response.refusal.delta", "response.refusal.done",
                     "response.reasoning_summary_part.added", "response.reasoning_summary_part.done",
                     "response.reasoning_summary_text.delta", "response.reasoning_summary_text.done",
                     "response.reasoning_text.delta", "response.reasoning_text.done",
                     "response.function_call_arguments.delta", "response.function_call_arguments.done",
                     "response.custom_tool_call_input.delta", "response.custom_tool_call_input.done"]
        self.insert_events_before_completion(9, [{"type": name} for name in supported])
        self.assertTrue(self.check().ok)
        for name in ("response.cancelled", "response.expired", "response.unknown_event"):
            with self.subTest(event=name):
                self.native()
                self.insert_events_before_completion(9, [{"type": name}])
                self.assertFalse(self.check().ok)

    def tool_items(self, kind):
        call = {"type": kind, "id": "tool_synthetic", "call_id": "call_synthetic", "name": "synthetic"}
        call["arguments" if kind == "function_call" else "input"] = "{}"
        result = {"type": kind + "_output", "call_id": call["call_id"], "output": "synthetic result"}
        return call, result

    def check_tool_items(self, call, result):
        first = reasoning("typed_tool")
        self.exchange(9, FLASH, [], [first, call])
        self.exchange(10, FLASH, [first, result], [reasoning("tool_next")])
        return self.check(pin(require_tool_continuation=True))

    def test_post_tool_rejects_explicitly_incomplete_calls(self):
        for kind in ("function_call", "custom_tool_call"):
            call, result = self.tool_items(kind)
            self.assertTrue(self.check_tool_items(call, result).ok)
            call["status"] = "completed"
            self.assertTrue(self.check_tool_items(call, result).ok)
            for status in ("in_progress", "incomplete"):
                with self.subTest(kind=kind, status=status):
                    call["status"] = status
                    self.assertFalse(self.check_tool_items(call, result).ok)

    def test_post_tool_rejects_incompatible_result_kinds(self):
        for kind in ("function_call", "custom_tool_call"):
            with self.subTest(kind=kind):
                call, result = self.tool_items(kind)
                result["type"] = ("custom_tool_call_output" if kind == "function_call"
                                  else "function_call_output")
                self.assertFalse(self.check_tool_items(call, result).ok)

    def test_post_tool_requires_a_payload_bearing_result(self):
        for kind in ("function_call", "custom_tool_call"):
            call, result = self.tool_items(kind)
            result.pop("output")
            with self.subTest(kind=kind, output="missing"):
                self.assertFalse(self.check_tool_items(call, result).ok)
            for payload in (None, "", [], {}):
                with self.subTest(kind=kind, payload_type=type(payload).__name__):
                    result["output"] = payload
                    self.assertFalse(self.check_tool_items(call, result).ok)

    def test_endpoint_filter_cannot_hide_an_ordinary_source_session_transition(self):
        self.native()
        native = reasoning("astra_after_messages")
        self.exchange(12, ASTRA, [], [native])
        self.exchange(13, ASTRA, [native], [reasoning("astra_followup")])
        request = {"n": 11, "method": "POST", "path": "/v1/messages",
                   "headers": {"x-grok-session-id": "synthetic-session",
                               "x-grok-req-id": "ordinary-turn-synthetic"},
                   "body": {"model": "unexpected-model", "stream": False, "max_tokens": 16,
                            "messages": [{"role": "user", "content": "synthetic"}]}}
        response = {"id": "msg_synthetic", "type": "message", "role": "assistant",
                    "model": "unexpected-model", "content": [{"type": "text", "text": "synthetic"}],
                    "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}}
        self.write_records(11, [{"n": 11, "status": 200, "headers": {}},
                                {"frame_index": 0, "frame": json.dumps(response)}])
        case_path = HERE / "cases" / "flash-to-astra-replay-v1.json"
        spec = json.loads(case_path.read_text())["assert"]["wire"][0]
        for model in ("unexpected-model", FLASH):
            request["body"]["model"] = model
            for exclusion in ("auxiliary", "unrelated", "ordinary"):
                with self.subTest(model=model, exclusion=exclusion):
                    request["headers"]["x-grok-session-id"] = (
                        "unrelated-session" if exclusion == "unrelated" else "synthetic-session")
                    request["headers"]["x-grok-req-id"] = (
                        "xai-title-synthetic" if exclusion == "auxiliary" else "ordinary-turn-synthetic")
                    self.write_json("req-011.json", request)
                    result = self.check(spec)
                    if exclusion == "ordinary":
                        self.assertFalse(result.ok)
                        self.assertIn("unsupported protocol", result.detail)
                    else:
                        self.assertTrue(result.ok, result.detail)
        (self.root / "req-011.json").write_text('{"headers":')
        self.assertFalse(self.check(spec).ok)

    def test_surrogate_ciphertext_returns_a_value_free_failed_assertion(self):
        first = reasoning("unicode")
        first["encrypted_content"] = "synthetic-valid-\U0001f642"
        self.exchange(9, FLASH, [], [first])
        self.exchange(10, FLASH, [first], [reasoning("unicode_next")])
        self.assertTrue(self.check().ok)
        for scalar in ("\ud800", "\udfff"):
            with self.subTest(scalar_class="isolated surrogate"):
                first["encrypted_content"] = "PRIVATE-SURROGATE-" + scalar + "-PAYLOAD"
                self.exchange(9, FLASH, [], [first])
                self.exchange(10, FLASH, [first], [reasoning("unicode_next")])
                result = self.check()
                self.assertFalse(result.ok)
                self.assertEqual(result.detail, "replay v1 FAIL-CLOSED: unsupported Unicode in "
                                 "encrypted_content (request_n=9)")
                published = json.dumps(vars(result), ensure_ascii=True)
                self.assertNotIn("PRIVATE-SURROGATE", published)
                self.assertNotIn(json.dumps(scalar)[1:-1], published)


class ReplaySchemaTest(unittest.TestCase):
    def case(self, spec=None):
        return {"schema_version": 1, "id": "FLASH-OFFLINE-TEST", "title": "Synthetic offline contract",
                "bead": "apex-ayl.175", "suite": "wire-shape", "tier": "full", "driver": "headless",
                "model": FLASH, "wire": "responses", "steps": [{"op": "recon_note", "note": "offline"}],
                "assert": {"wire": [spec or pin()]}}

    def test_schema_engines_accept_versioned_assertion(self):
        engines = ["hand"] + (["jsonschema"] if run._jsonschema_engine() else [])
        for engine in engines:
            with self.subTest(engine=engine):
                self.assertEqual(run.draft07_gate(self.case(), run._load_schema(), engine), [])

    def test_schema_engines_reject_invalid_replay(self):
        engines = ["hand"] + (["jsonschema"] if run._jsonschema_engine() else [])
        for defect in ("missing_replay", "version", "short_segment", "unknown_field", "empty_segments"):
            spec = pin()
            if defect == "missing_replay":
                spec.pop("replay")
            elif defect == "version":
                spec["replay"]["version"] = 2
            elif defect == "short_segment":
                spec["replay"]["segments"][0]["min_requests"] = 1
            elif defect == "unknown_field":
                spec["replay"]["segments"][0]["fields"] = ["ciphertext"]
            else:
                spec["replay"]["segments"] = []
            for engine in engines:
                with self.subTest(defect=defect, engine=engine):
                    self.assertTrue(run.draft07_gate(self.case(spec), run._load_schema(), engine))

    def test_case_contract_rejects_ambiguous_or_vacuous_specs(self):
        for defect in ("selection", "duplicate_fields", "missing_cipher_field", "adjacent_model"):
            spec = pin()
            if defect == "selection":
                spec["any"] = True
            elif defect == "duplicate_fields":
                spec["replay"]["segments"][0]["fields"] = ["encrypted_content", "encrypted_content"]
            elif defect == "missing_cipher_field":
                spec["replay"]["segments"][0]["fields"] = ["summary"]
            else:
                spec["replay"]["segments"].append(segment(FLASH))
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as root:
                case_path = Path(root) / "case.json"
                case_path.write_text(json.dumps(self.case(spec)))
                self.assertTrue(run.validate_case_file(str(case_path), engine="hand"))


if __name__ == "__main__":
    unittest.main()
