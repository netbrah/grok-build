//! Provider-specific request patches for the Responses API.
//!
//! This module ports the Codex provider adapter from open-grok and the
//! content-type normalization from the codex fork (netbrah/codex) into
//! grok-build. It applies provider-aware patches to the serialized Responses
//! request body before it is sent, gated on `model_family` metadata — never
//! on model slugs or URLs.
//!
//! ## What it does
//!
//! For `model_family = "codex"` (GPT-5.6 Sol/Terra/Luna on the llm-proxy):
//! - Maps `Max` and `Ultra` reasoning efforts to the wire value `"max"`.
//! - Injects the multi-agent v2 proactive/explicit developer policy item
//!   (Ultra → proactive delegation, Max → explicit-request-only).
//! - Grants `external_web_access: true` on hosted `web_search` tools.
//!
//! For non-OpenAI providers (`model_family = "glm"`, etc.):
//! - Normalizes `input_text`/`output_text` content part types to `"text"`,
//!   so Responses→ChatCompletions shims (vLLM/SGLang) accept the request.
//!
//! Patches are additive and idempotent: they only rewrite fields they own.

use serde_json::Value;
use xai_grok_sampling_types::ReasoningEffort;

/// Compatibility policy for Responses API event decoding.
///
/// This is derived once from catalog provider-family metadata. It is not
/// inferred from the model slug, endpoint URL, API backend, or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponsesWireDialect {
    /// xAI Responses frames may omit `sequence_number`, but unknown semantic
    /// events remain fatal until they are deliberately supported.
    Xai,
    /// Codex Responses permits sparse lifecycle envelopes and ignores future
    /// top-level event types as liveness-only frames.
    Codex,
    /// No compatibility normalization beyond recognized auxiliary frames.
    Strict,
}

pub(crate) fn responses_wire_dialect_for_model_family(
    model_family: Option<&str>,
) -> ResponsesWireDialect {
    match model_family {
        Some(family) if family.eq_ignore_ascii_case("codex") => ResponsesWireDialect::Codex,
        Some(family) if family.eq_ignore_ascii_case("xai") => ResponsesWireDialect::Xai,
        // Existing uncatalogued/default configurations are xAI-native.
        None => ResponsesWireDialect::Xai,
        Some(_) => ResponsesWireDialect::Strict,
    }
}

/// Developer policy tags for multi-agent v2 mode.
const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
const PROACTIVE_MULTI_AGENT_MODE_TEXT: &str = "proactive";
const EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT: &str = "explicit_request_only";

/// Apply provider-specific patches to a serialized Responses API request body.
///
/// `model_family` selects the provider dialect. `reasoning_effort` is the
/// local (pre-wire) effort, needed for Max/Ultra mapping and v2 policy.
/// `multi_agent_v2` enables the v2 developer policy injection.
pub fn patch_responses_request(
    request_body: &mut Value,
    model_family: Option<&str>,
    reasoning_effort: Option<ReasoningEffort>,
    multi_agent_v2: bool,
) {
    let family = model_family.unwrap_or_default();

    if family.eq_ignore_ascii_case("codex") {
        patch_codex_responses_request(request_body, reasoning_effort, multi_agent_v2);
    }

    // Content-type normalization for non-OpenAI providers whose Responses
    // shim expects "text" instead of "input_text"/"output_text".
    if !is_openai_family(family) {
        normalize_content_types(request_body);
    }
}

/// Whether this provider family is OpenAI-native (no content-type normalization needed).
fn is_openai_family(family: &str) -> bool {
    family.eq_ignore_ascii_case("xai")
        || family.eq_ignore_ascii_case("codex")
        || family.eq_ignore_ascii_case("openai")
        || family.is_empty()
}

/// Codex Responses dialect patches.
fn patch_codex_responses_request(
    request_body: &mut Value,
    local_effort: Option<ReasoningEffort>,
    multi_agent_v2: bool,
) {
    // Grant live sources on hosted web_search (Codex dialect behavior).
    if let Some(tools) = request_body.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools.iter_mut() {
            if tool.get("type").and_then(Value::as_str) == Some("web_search") {
                if let Some(obj) = tool.as_object_mut() {
                    if !obj.contains_key("external_web_access") {
                        obj.insert("external_web_access".into(), true.into());
                    }
                }
            }
        }
    }

    // Map Max/Ultra → "max" on the wire. The Responses API has no Ultra variant;
    // Ultra enables proactive delegation via the v2 policy item below.
    if matches!(
        local_effort,
        Some(ReasoningEffort::Max | ReasoningEffort::Ultra)
    ) {
        ensure_reasoning_object(request_body);
        request_body["reasoning"]["effort"] = Value::String("max".to_owned());
    }

    if !multi_agent_v2 {
        return;
    }

    let mode_text = if local_effort == Some(ReasoningEffort::Ultra) {
        PROACTIVE_MULTI_AGENT_MODE_TEXT
    } else {
        EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT
    };
    let rendered = format!("{MULTI_AGENT_MODE_OPEN_TAG}{mode_text}{MULTI_AGENT_MODE_CLOSE_TAG}");

    let Some(input) = request_body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };

    // Remove any stale multi_agent_mode item, then insert the fresh one
    // just before the last user message (matching codex-rs placement).
    input.retain(|item| !is_multi_agent_mode_item(item));
    let mode_item = serde_json::json!({
        "type": "message",
        "role": "developer",
        "content": [{ "type": "input_text", "text": rendered }],
    });
    let insert_at = input
        .last()
        .filter(|item| item.get("role").and_then(Value::as_str) == Some("user"))
        .map_or(input.len(), |_| input.len() - 1);
    input.insert(insert_at, mode_item);
}

/// Strip `encrypted_content` from replayed `reasoning` input items.
///
/// The LLM proxy load-balances the Responses API across Azure deployments
/// with different API keys, and `encrypted_content` is only decryptable by
/// the deployment that produced it:
/// - `reasoning` items carry it as OPTIONAL on replay (it only preserves
///   provider-side reasoning continuity). Replayed across deployments it
///   400s with `invalid_encrypted_content`, so strip it: multi-call
///   (tool-loop) turns keep working.
/// - `compaction` / `context_compaction` carrier items carry it as
///   REQUIRED. They are left untouched on purpose: with a proxy that
///   session-pins /responses to one deployment the ciphertext round-trips,
///   and stripping it would make carrier replay unrecoverably impossible
///   (`missing_required_parameter`).
fn strip_encrypted_content(input: &mut [Value]) {
    for item in input.iter_mut() {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) == Some("reasoning") {
            obj.remove("encrypted_content");
        }
    }
}

/// Strip `encrypted_content` from `reasoning` input items of a final
/// Responses request body (see [`strip_encrypted_content`]).
///
/// Transport seam: call this after dialect patching AND after the raw Codex
/// compaction-carrier splice, so typed reasoning items never reach the
/// proxy carrying deployment-bound ciphertext.
pub fn strip_encrypted_content_input(body: &mut Value) {
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        strip_encrypted_content(input);
    }
}

/// Project replayed `reasoning` input items for targets that enforce the
/// strict OpenAI/Azure Responses input schema (REPLAY-1).
///
/// Lenient backends (the vLLM Responses shim) emit `reasoning` output items
/// carrying a `content` array of `reasoning_text` parts; this harness stores
/// the item verbatim and replays it on later turns. Strict targets (Azure
/// OpenAI) model `reasoning.content` on input as an array with `maxItems: 0`
/// and 400 with `array_above_max_length` ("Invalid 'input[N].content': array
/// too long...") the moment a non-empty one is replayed — deterministic,
/// `is_retryable=false`. The projection is lossless: the same reasoning text
/// rides in `summary`, which the strict schema accepts, and the full item
/// remains in the local chat history.
///
/// Schema audit (REPLAY-1): of the input item types this harness replays
/// (message, reasoning, function_call, function_call_output,
/// web_search_call, custom_tool_call, code_interpreter_call, compaction
/// carrier), only `reasoning` carries a schema-forbidden non-empty
/// `content` on strict targets; the audit addendum below covers the second,
/// independently observed `reasoning` violation (item `id`).
///
/// Schema audit addendum (REPLAY-1, observed 2026-09-14 after the content
/// strip cleared): with `store: false` — this transport's standing setting
/// (the carrier-splice seam above depends on it) — strict targets also
/// reject replayed `reasoning` items that carry their prior-response `id`,
/// because no item state is persisted under any id. Verbatim 400 (req-006,
/// smoke/redteam/report/20260914T054941Z/rt-m1/wire/resp-006.jsonl, same
/// class on the R2 path in 20260914T055323Z/rt-r2/wire/resp-013.jsonl):
/// "Item with id 'rs_a87ac633e3b4401c9bd568c33c6a37e6' not found. Items are
/// not persisted when `store` is set to false. Try again with `store` set
/// to true, or remove this item from your input." (`invalid_request_error`,
/// param `input`). The projection therefore removes `id` as well —
/// lossless for the same reason as `content`: the item is self-contained via
/// `summary`. `store: true` is rejected as an alternative because it would
/// change the persistence/load-balancing contract the carrier splice
/// depends on.
///
/// Intentional divergences from the codex reference (pinned 2026-09-14,
/// coordinator ruling KEEP; grok/plans/replay1-codex-determination.md):
/// (1) id-strip — this projection removes `id` for `strict_responses_input`
/// targets because Azure/store=false rejects unknown ids (wire-pinned
/// above); codex keeps prefixed ids because its daily target (OpenAI)
/// tolerates them — a target-aware refinement required by the llm-proxy
/// multi-provider scenario. (2) Lenient rows stay verbatim — this harness
/// projects ONLY for strict rows, so vLLM-bound requests retain the full
/// vLLM-coined shape (content + id) at maximum fidelity, wire-proven
/// accepted (M1 req-005, 200); codex drops `content` unconditionally via
/// serde for ALL targets, which we deliberately do not — vLLM accepts its
/// own output verbatim and nothing is lost.
///
/// Transport seam: call on every /responses send path after dialect patching
/// and the raw carrier splice, gated by the per-model
/// `strict_responses_input` config. Lenient targets must stay
/// byte-identical (vLLM-dialect regression pin).
pub fn project_strict_responses_input(body: &mut Value, strict_dialect: bool) {
    if !strict_dialect {
        return;
    }
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input.iter_mut() {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            if obj.get("type").and_then(Value::as_str) == Some("reasoning") {
                obj.remove("content");
                obj.remove("id");
            }
        }
    }
}

/// Ensure a `reasoning` object exists on the request body.
fn ensure_reasoning_object(request_body: &mut Value) {
    if request_body.get("reasoning").is_none() {
        request_body["reasoning"] = serde_json::json!({});
    }
}

/// Whether a developer input item carries the multi_agent_mode marker.
fn is_multi_agent_mode_item(item: &Value) -> bool {
    let role = item.get("role").and_then(Value::as_str);
    if role != Some("developer") {
        return false;
    }
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return false;
    };
    content.iter().any(|part| {
        part.get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains(MULTI_AGENT_MODE_OPEN_TAG))
    })
}

/// Rewrite `input_text`/`output_text` content part types to `"text"`.
///
/// Ported from codex fork `content_type_compat.rs`. Providers such as vLLM
/// and SGLang expose a `/v1/responses` endpoint backed by a
/// Responses→ChatCompletions shim that passes content part types through
/// unchanged, causing pydantic validation errors. This rewrites them so the
/// shim accepts the request.
pub fn normalize_content_types(value: &mut Value) {
    let Some(input) = value.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for item in input.iter_mut() {
        let Some(content) = item.get_mut("content").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for part in content.iter_mut() {
            if let Some(type_str) = part.get("type").and_then(|t| t.as_str()) {
                if type_str == "input_text" || type_str == "output_text" {
                    if let Some(obj) = part.as_object_mut() {
                        obj.insert(
                            "type".to_string(),
                            serde_json::Value::String("text".to_string()),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_wire_dialect_comes_from_provider_family_metadata() {
        assert_eq!(
            responses_wire_dialect_for_model_family(Some("codex")),
            ResponsesWireDialect::Codex
        );
        assert_eq!(
            responses_wire_dialect_for_model_family(Some("xai")),
            ResponsesWireDialect::Xai
        );
        assert_eq!(
            responses_wire_dialect_for_model_family(None),
            ResponsesWireDialect::Xai
        );
        assert_eq!(
            responses_wire_dialect_for_model_family(Some("glm")),
            ResponsesWireDialect::Strict
        );
    }

    #[test]
    fn codex_ultra_maps_to_max_wire_effort() {
        let mut body = serde_json::json!({"reasoning": {"effort": "medium"}});
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Ultra), false);
        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn codex_max_maps_to_max_wire_effort() {
        let mut body = serde_json::json!({"reasoning": {"effort": "medium"}});
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Max), false);
        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn codex_high_leaves_effort_unchanged() {
        let mut body = serde_json::json!({"reasoning": {"effort": "high"}});
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::High), false);
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn codex_ultra_v2_injects_proactive_developer_item() {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Ultra), true);
        let input = body["input"].as_array().unwrap();
        // developer item inserted before the user message
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "developer");
        assert!(
            input[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("proactive")
        );
    }

    #[test]
    fn codex_max_v2_injects_explicit_request_only_item() {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Max), true);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
        assert!(
            input[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("explicit_request_only")
        );
    }

    #[test]
    fn codex_v2_replaces_stale_mode_item() {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<multi_agent_mode>stale</multi_agent_mode>"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Ultra), true);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert!(
            input[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("proactive")
        );
    }

    #[test]
    fn codex_web_search_gets_external_web_access() {
        let mut body = serde_json::json!({
            "tools": [{"type": "web_search"}]
        });
        patch_codex_responses_request(&mut body, None, false);
        assert_eq!(body["tools"][0]["external_web_access"], true);
    }

    #[test]
    fn normalize_content_types_rewrites_input_text() {
        let mut body = serde_json::json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}]
            }]
        });
        normalize_content_types(&mut body);
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn normalize_content_types_rewrites_output_text() {
        let mut body = serde_json::json!({
            "input": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello"}]
            }]
        });
        normalize_content_types(&mut body);
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn normalize_content_types_leaves_text_unchanged() {
        let mut body = serde_json::json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "text", "text": "hi"}]
            }]
        });
        normalize_content_types(&mut body);
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn patch_responses_request_dispatches_codex() {
        let mut body = serde_json::json!({
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "tools": [{"type": "web_search"}]
        });
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Ultra), true);
        // codex gets web_search access + ultra→max + v2 policy
        assert_eq!(body["tools"][0]["external_web_access"], true);
        assert_eq!(body["reasoning"]["effort"], "max");
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        // codex is OpenAI-family, so no content-type normalization
        assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
    }

    #[test]
    fn strip_encrypted_content_input_strips_reasoning_but_keeps_carriers() {
        let mut body = serde_json::json!({
            "input": [
                {
                    "id": "rs_01",
                    "type": "reasoning",
                    "content": [],
                    "summary": [{"type": "summary_text", "text": "thinking..."}],
                    "encrypted_content": "gAAAAA-reasoning"
                },
                {
                    "id": "cmp_01",
                    "type": "compaction",
                    "summary": "opaque summary",
                    "encrypted_content": "gAAAAA-compaction"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}]
                }
            ]
        });
        strip_encrypted_content_input(&mut body);
        let input = body["input"].as_array().unwrap();
        // Reasoning ciphertext is optional on replay -> stripped.
        assert!(input[0].get("encrypted_content").is_none());
        assert_eq!(input[0]["id"], "rs_01");
        assert_eq!(input[0]["summary"][0]["text"], "thinking...");
        // Compaction carriers REQUIRE their ciphertext (server-side context)
        // -> kept intact for session-pinned proxies.
        assert_eq!(input[1]["encrypted_content"], "gAAAAA-compaction");
        assert_eq!(input[1]["id"], "cmp_01");
        // Other items untouched.
        assert_eq!(input[2]["content"][0]["type"], "input_text");
    }

    #[test]
    fn strip_encrypted_content_input_noop_without_input_array() {
        let mut body = serde_json::json!({"model": "gpt-5.6-sol"});
        strip_encrypted_content_input(&mut body);
        assert_eq!(body["model"], "gpt-5.6-sol");
    }

    #[test]
    fn patch_responses_request_dispatches_glm() {
        let mut body = serde_json::json!({
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        patch_responses_request(&mut body, Some("glm"), None, false);
        // glm gets content-type normalization but no codex patches
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn patch_responses_request_xai_no_op() {
        let mut body = serde_json::json!({
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        let original = body.clone();
        patch_responses_request(&mut body, Some("xai"), None, false);
        assert_eq!(body, original);
    }

    // REPLAY-1: the real shape of input[7] from the first rejected sol request
    // (smoke/redteam/report/20260914T035640Z/rt-m1/wire/req-006.json): a
    // vLLM-emitted reasoning item with a reasoning_text content array, stored
    // verbatim and replayed into the strict Azure input schema (maxItems 0).
    fn captured_reasoning_item() -> serde_json::Value {
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_288b9ed724204ccd8cffb5ae41ca4753",
            "summary": [
                {
                    "type": "summary_text",
                    "text": "The user is requesting that I respond exactly as follows: \"RT-M1-Q1\"."
                }
            ],
            "content": [
                {
                    "type": "reasoning_text",
                    "text": "The user is requesting that I respond exactly as follows: \"RT-M1-Q1\"."
                }
            ]
        })
    }

    #[test]
    fn strict_replay_projection_strips_reasoning_content() {
        let mut body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                captured_reasoning_item()
            ]
        });
        project_strict_responses_input(&mut body, true);
        let reasoning = body["input"][1].as_object().unwrap();
        // The strict schema forbids non-empty reasoning.content -> omitted.
        assert!(
            reasoning.get("content").is_none(),
            "reasoning.content must be omitted for strict targets"
        );
        // Second violation (REPLAY-1 addendum): with store=false the strict
        // target has no persisted state under any id -> replayed
        // reasoning.id must be omitted too; the text survives in summary.
        assert!(
            reasoning.get("id").is_none(),
            "reasoning.id must be omitted for strict targets (store=false id lookup)"
        );
        assert_eq!(
            reasoning["summary"][0]["text"],
            "The user is requesting that I respond exactly as follows: \"RT-M1-Q1\"."
        );
        // Sibling items untouched.
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn lenient_replay_projection_keeps_reasoning_content_unchanged() {
        // vLLM-dialect regression pin: lenient targets accept the shape today,
        // so the replay must stay byte-identical (no projection).
        let body = serde_json::json!({
            "model": "qwen3.8-27b",
            "input": [captured_reasoning_item()]
        });
        let mut projected = body.clone();
        project_strict_responses_input(&mut projected, false);
        assert_eq!(projected, body);
    }

    #[test]
    fn strict_replay_projection_leaves_other_item_types_untouched() {
        // Schema audit of the item types the harness replays: only reasoning
        // carries a schema-forbidden non-empty `content` on strict targets.
        // function_call / function_call_output / message / compaction carriers
        // must pass through unmodified.
        let body = serde_json::json!({
            "input": [
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]},
                {"type": "function_call", "call_id": "call_1", "name": "echo", "arguments": "{\"x\":1}", "status": "completed"},
                {"type": "function_call_output", "call_id": "call_1", "output": "{\"x\":1}"},
                {"id": "cmp_01", "type": "compaction", "encrypted_content": "gAAAAA-compaction"},
                captured_reasoning_item()
            ]
        });
        let mut projected = body.clone();
        project_strict_responses_input(&mut projected, true);
        let input = projected["input"].as_array().unwrap();
        assert_eq!(input[0], body["input"][0]);
        assert_eq!(input[1], body["input"][1]);
        assert_eq!(input[2], body["input"][2]);
        assert_eq!(input[3], body["input"][3]);
        assert!(input[4].get("content").is_none());
        assert!(input[4].get("id").is_none());
    }

    #[test]
    fn strict_replay_projection_strips_typed_serialized_reasoning_item() {
        // End-to-end through the real serde path: a typed rs::ReasoningItem with
        // content: Some(..) serializes WITH content (the red state), and the
        // projection removes it for strict targets only.
        use xai_grok_sampling_types::rs;
        let make_item = || rs::ReasoningItem {
            id: "rs_288b9ed724204ccd8cffb5ae41ca4753".to_owned(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thinking...".to_owned(),
            })],
            content: Some(vec![rs::ReasoningTextContent {
                text: "thinking...".to_owned(),
            }]),
            encrypted_content: None,
            status: None,
        };
        let serialized = |item: rs::ReasoningItem| {
            let mut body = serde_json::json!({
                "input": [rs::InputItem::Item(rs::Item::Reasoning(item))]
            });
            xai_grok_sampling_types::patch_reasoning_text_types(&mut body);
            body
        };
        let red_state = serialized(make_item());
        assert!(
            red_state["input"][0].get("content").is_some(),
            "pre-fix wire state: typed serialization emits reasoning.content"
        );
        let mut projected = serialized(make_item());
        project_strict_responses_input(&mut projected, true);
        assert!(projected["input"][0].get("content").is_none());
        assert!(projected["input"][0].get("id").is_none());
        let mut control = serialized(make_item());
        project_strict_responses_input(&mut control, false);
        assert_eq!(control, red_state);
    }

    /// W-1 Task 1 (T1) — non-disruption guard: every OpenAI-family value
    /// (`codex`, `xai`, `openai`, `""`) must produce exactly today's body:
    /// no content-type rewrite, no `compaction_trigger`, lenient reasoning
    /// replay retained (content kept, encrypted_content stripped). If this
    /// test fails, the SOL/OpenAI path was disrupted.
    #[test]
    fn openai_families_are_byte_identical() {
        for family in ["codex", "xai", "openai", ""] {
            let mut body = serde_json::json!({
                "model": "gpt-5.6-sol",
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "reasoning", "id": "rs_1", "content": [{"type": "reasoning_text", "text": "t"}], "encrypted_content": "gAAA"}
                ]
            });
            patch_responses_request(&mut body, Some(family), None, false);
            strip_encrypted_content_input(&mut body);
            let input = body["input"].as_array().unwrap();
            let user = input
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
                .unwrap();
            assert_eq!(
                user["content"][0]["type"], "input_text",
                "family {family}: content part was normalized — SOL/OpenAI path disrupted"
            );
            let reasoning = input
                .iter()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
                .unwrap();
            assert!(
                reasoning.get("content").is_some(),
                "family {family}: lenient reasoning.content replay must be retained"
            );
            assert!(
                reasoning.get("encrypted_content").is_none(),
                "family {family}: encrypted_content must be stripped on replay"
            );
            assert!(
                input.iter().all(|item| item.get("type").and_then(Value::as_str) != Some("compaction_trigger")),
                "family {family}: compaction_trigger must not appear"
            );
        }
    }

    /// W-1 T1 companion pin: the `<multi_agent_mode>` developer item stays
    /// with the genuine `codex` family (part of today's Sol body — Option
    /// A/B may remove it only from mis-hydrated families, never from real
    /// Codex deployments) and never appears for non-codex families.
    #[test]
    fn multi_agent_mode_item_follows_codex_family_only() {
        let make_body = || {
            serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
                ]
            })
        };
        let mut body = make_body();
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Max), true);
        assert!(
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(is_multi_agent_mode_item),
            "codex family lost its <multi_agent_mode> developer item"
        );
        for family in ["qwen", "glm", "xai", "openai", ""] {
            let mut body = make_body();
            patch_responses_request(&mut body, Some(family), Some(ReasoningEffort::Max), true);
            assert!(
                !body["input"].as_array().unwrap().iter().any(is_multi_agent_mode_item),
                "family {family} gained a <multi_agent_mode> developer item"
            );
        }
    }

    /// W-1 Task 1.3 — the vLLM side of the guard: non-OpenAI families get
    /// the shim normalization today and must keep getting it after the fix.
    #[test]
    fn vllm_families_normalize_content_types() {
        for family in ["qwen", "glm"] {
            let mut body = serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}
                ]
            });
            patch_responses_request(&mut body, Some(family), None, true);
            assert_eq!(body["input"][0]["content"][0]["type"], "text", "family {family} input_text");
            assert_eq!(body["input"][1]["content"][0]["type"], "text", "family {family} output_text");
        }
    }
}
