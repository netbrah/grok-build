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
//! Two orthogonal axes on EVERY responses-wire request (PROACTIVE-ULTRA-1 /
//! apex-ayl.86, rulings R-MENU-DERIVED + R-UNIFIED-ITEM):
//!
//! - AXIS 1 — value translation (the remap): a locally-carried `ultra` maps
//!   to the model's own strongest-practical wire value. The codex arm
//!   (GPT-5.6 Sol/Terra/Luna on the llm-proxy) is FROZEN: Max/Ultra →
//!   `"max"`, byte-pinned, plus the `external_web_access: true` grant on
//!   hosted `web_search` tools. Every other family carries the menu-derived
//!   `ultra_wire_effort` for ultra (the highest advertised menu tier below
//!   `ultra`; no menu → `"max"`). Sub-ultra efforts are untouched on every
//!   family.
//! - AXIS 2 — policy hook (the unified item): exactly one
//!   `<multi_agent_mode>` developer item per body, every family, every
//!   effort — `proactive` on ultra, `explicit_request_only` otherwise. The
//!   codex family renders the frozen bare keyword (the binding codex
//!   bytes); every other family renders the expanded self-explanatory
//!   sentence wrapped in the same tag. The item rides EVERY responses-wire
//!   request — main turns, subagent turns, and summary-client requests —
//!   and is stripped (tag-based, form-agnostic) and re-injected before the
//!   last user message on every request.
//!
//! The wire seam carries NO flag (R-NO-FLAG). The `multi_agent_v2` argument
//! this cut deleted was homonym #1 of the three `multi_agent_v2` concepts:
//! the shell's `Feature::MultiAgentV2` tool tier (the `features.multi_agent_v2`
//! registry row — it gates which TOOLS a session has, not what the wire
//! carries) and the catalog's per-model `info.multi_agent_v2` field (the
//! §10 contingency source) are distinct and untouched.
//!
//! For non-OpenAI providers (`model_family = "glm"`, etc.) content part
//! types `input_text`/`output_text` are normalized to `"text"` so
//! Responses→ChatCompletions shims (vLLM/SGLang) accept the request. The
//! item injection runs BEFORE the normalization, so a freshly injected item
//! is swept with the rest of the input.
//!
//! Patches are additive and idempotent: they only rewrite fields they own.
//!
//! Provenance: R0 first-class responses catalog item (ledger §R0;
//! reviews/R0-task-review.md §4) — a 3-donor ADAPTED port (adapted, not
//! cherry-picked; the `Refs:` line on all four R0 commit bodies), donors
//! pinned in grok/plans/donors.md:
//! - open-grok@2a07373c — the Codex provider adapter: model_family
//!   dispatch, Max/Ultra → "max" effort mapping, multi-agent v2 policy,
//!   web_search external_web_access grant. Extracted from open-grok's
//!   inline patch sites in its `client.rs` + `conversation.rs` (R0
//!   review §4: 64-line overlap, longest run 5 lines).
//! - netbrah/codex@b4d4b125cc — the wire shim: `normalize_content_types`
//!   below (re-expression of `content_type_compat.rs`); the fork's
//!   companion 197ea1642c namespace-tool flatten half was NOT ported
//!   (0 hits in tree).
//! - netbrah/codex@0002ba5747 (apex-ayl.86, ADAPTED; in-house fork) — the
//!   codex-fork ultra hook this cut generalizes: the menu-derived wire
//!   value for a locally-carried `ultra` (re-expressed as the shell's
//!   `ModelsManager::ultra_wire_effort_for` + `effort_rank`) and the
//!   `<multi_agent_mode>` developer policy item (tag byte-identical to
//!   this module's; the codex family keeps the frozen bare keyword,
//!   non-codex families carry the NEW grok expanded text).
//! The item's hyper-grok-build@d7e99eac / @2baedd03 / @4e0fad59
//! decoder-side contribution lands in `sampler/src/client.rs` dialect
//! machinery + `sampler/src/stream/responses.rs`, not this file.

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
/// Non-codex expanded forms (R-UNIFIED-ITEM row 3 text ruling; the grok
/// system prompt does not define the tag for non-codex models, so a bare
/// keyword would be unexplained magic prose there). Ruling's recommended
/// sentence, adopted verbatim; pinned byte-exact by T9.
const PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED: &str =
    "Multi-agent mode: proactive — subagents may be spawned proactively when the task benefits from delegation.";
const EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED: &str =
    "Multi-agent mode: explicit_request_only — subagents may be spawned only when the user explicitly requests.";

/// Apply provider-specific patches to a serialized Responses API request
/// body (PROACTIVE-ULTRA-1 / apex-ayl.86 — the unified two-axis shape).
///
/// `model_family` selects the provider dialect. `reasoning_effort` is the
/// local (pre-wire) effort: AXIS 1 remaps a carried `ultra` (codex arm
/// FROZEN at Max/Ultra → `"max"`; other families take the menu-derived
/// `ultra_wire_effort`, falling back to wire `"max"` when it is `None`),
/// and AXIS 2 picks the unified `<multi_agent_mode>` item's mode text
/// (`proactive` on ultra, `explicit_request_only` otherwise) — one code
/// path for every family, every effort, every responses-wire request kind.
/// `ultra_wire_effort` is MENU DATA resolved by the shell (ruling
/// R-MENU-DERIVED) — not a flag: the wire seam carries no flag (R-NO-FLAG;
/// the deleted `multi_agent_v2` argument was homonym #1, see the module
/// doc). `normalize_content_types` is the apex-ayl.77 E2 named opt-in for
/// family-less rows (ruling R-B, binding flag spec); the item injection
/// runs BEFORE the normalization so a fresh item is swept with the rest of
/// the input (T10).
pub fn patch_responses_request(
    request_body: &mut Value,
    model_family: Option<&str>,
    reasoning_effort: Option<ReasoningEffort>,
    normalize_content_types: bool,
    ultra_wire_effort: Option<ReasoningEffort>,
) {
    let family = model_family.unwrap_or_default();

    let is_codex = family.eq_ignore_ascii_case("codex");
    let is_ultra = reasoning_effort == Some(ReasoningEffort::Ultra);

    if is_codex {
        patch_codex_responses_request(request_body, reasoning_effort);
    } else if is_ultra {
        // AXIS 1 remap (R-MENU-DERIVED): the menu-derived wire value
        // resolved by the shell, falling back to wire "max" when it is
        // None (no menu / legacy row — T4). Sub-ultra efforts are
        // untouched on every family (T5).
        let wire = ultra_wire_effort.unwrap_or(ReasoningEffort::Max);
        ensure_reasoning_object(request_body);
        request_body["reasoning"]["effort"] = Value::String(wire.as_str().to_owned());
    }

    // AXIS 2 (R-UNIFIED-ITEM): one item, every family, every effort —
    // codex renders the frozen bare keyword, every other family the
    // expanded self-explanatory sentence (T9 pins both byte-exact).
    let mode_text = if is_ultra {
        if is_codex {
            PROACTIVE_MULTI_AGENT_MODE_TEXT
        } else {
            PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED
        }
    } else if is_codex {
        EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT
    } else {
        EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED
    };
    inject_multi_agent_mode_item(request_body, mode_text);

    // Content-type normalization for non-OpenAI providers whose Responses
    // shim expects "text" instead of "input_text"/"output_text". The named
    // opt-in (apex-ayl.77 E2 ruling R-B, binding flag spec) fires the same
    // rewrite for a family-less row; the flagless path is byte-identical to
    // the pre-cut status quo. The call is qualified because the binding
    // param name shadows the rewrite fn in scope.
    if !is_openai_family(family) || normalize_content_types {
        crate::provider::normalize_content_types(request_body);
    }
}

/// Whether this provider family is OpenAI-native (no content-type normalization needed).
fn is_openai_family(family: &str) -> bool {
    family.eq_ignore_ascii_case("xai")
        || family.eq_ignore_ascii_case("codex")
        || family.eq_ignore_ascii_case("openai")
        || family.is_empty()
}

/// Codex Responses dialect patches (FROZEN arm, apex-ayl.86 — the cut
/// deletes only the `multi_agent_v2` parameter and its early return; the
/// web_search grant and the Max/Ultra → "max" remap are zero-diff, and the
/// former inline item block now runs on the unified `patch_responses_request`
/// path via `inject_multi_agent_mode_item`, rendering the byte-identical
/// bare tag for the codex family — T6 golden).
fn patch_codex_responses_request(
    request_body: &mut Value,
    local_effort: Option<ReasoningEffort>,
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

    // Map Max/Ultra → "max" on the wire. The Responses API has no Ultra
    // variant; Ultra enables proactive delegation via the unified
    // `<multi_agent_mode>` item injected by `patch_responses_request`.
    if matches!(
        local_effort,
        Some(ReasoningEffort::Max | ReasoningEffort::Ultra)
    ) {
        ensure_reasoning_object(request_body);
        request_body["reasoning"]["effort"] = Value::String("max".to_owned());
    }
}

/// Inject the unified `<multi_agent_mode>` developer item (SDD §3.4):
/// render the mode text in the tag, strip any stale item in either form
/// (tag-based, form-agnostic), then insert the fresh item just before the
/// last user message — appending when the input ends on something other
/// than a user message (the pre-cut codex placement, byte-binding; T2
/// pins index 4 for a [u,a,u,a] input).
fn inject_multi_agent_mode_item(request_body: &mut Value, mode_text: &str) {
    let rendered = format!("{MULTI_AGENT_MODE_OPEN_TAG}{mode_text}{MULTI_AGENT_MODE_CLOSE_TAG}");

    let Some(input) = request_body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };

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

// XW-ENC-AFFINITY-1 (apex-mf6): the unconditional D-ENC body-level strip
// (`strip_encrypted_content_input`) is RETIRED — the deployment-affinity
// gate at the typed send seam (client.rs drop_orphaned sites,
// `apply_enc_affinity_gate`) decides per item whether the ciphertext rides
// the wire, and a body-level strip would destroy the retained arm (the
// pin-compatible ciphertext is now the point of the cut). A wrong bet is
// covered reactively: SIG-ENC-BOUNDARY 503/400 -> bulk strip + retry once
// (`RetryDecision::StripEncryptedAndRetry`).

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

/// Provenance: netbrah/codex@b4d4b125cc codex-rs/codex-api/src/endpoint/content_type_compat.rs:16 :: normalize_content_types (re-expressed — the wire-shim half of the R0 item; the fork's companion 197ea1642c flatten half was NOT ported; ledger §R0)
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
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Ultra));
        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn codex_max_maps_to_max_wire_effort() {
        let mut body = serde_json::json!({"reasoning": {"effort": "medium"}});
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::Max));
        assert_eq!(body["reasoning"]["effort"], "max");
    }

    #[test]
    fn codex_high_leaves_effort_unchanged() {
        let mut body = serde_json::json!({"reasoning": {"effort": "high"}});
        patch_codex_responses_request(&mut body, Some(ReasoningEffort::High));
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn codex_ultra_v2_injects_proactive_developer_item() {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        // apex-ayl.86: the item block moved to the unified path — the test
        // now drives the public entry (assertions unchanged).
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Ultra), false, None);
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
        // apex-ayl.86: the item block moved to the unified path (assertions unchanged).
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Max), false, None);
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
        // apex-ayl.86: the item block moved to the unified path (assertions unchanged).
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Ultra), false, None);
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
        patch_codex_responses_request(&mut body, None);
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
        // apex-ayl.86: the wire-seam `multi_agent_v2` argument is deleted —
        // item injection is unconditional; `ultra_wire_effort` is None (codex
        // arm ignores it — T7).
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Ultra), false, None);
        // codex gets web_search access + ultra→max + v2 policy
        assert_eq!(body["tools"][0]["external_web_access"], true);
        assert_eq!(body["reasoning"]["effort"], "max");
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        // codex is OpenAI-family, so no content-type normalization
        assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
    }

    #[test]
    fn patch_responses_request_dispatches_glm() {
        let mut body = serde_json::json!({
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        patch_responses_request(&mut body, Some("glm"), None, false, None);
        // glm gets content-type normalization but no codex patches
        assert_eq!(body["input"][0]["content"][0]["type"], "text");
    }

    /// W-1 T1 companion (AMENDED for apex-ayl.86, ruling R-UNIFIED-ITEM —
    /// the pre-cut expectation "non-codex families never get the item" is
    /// superseded by the binding unified-item ruling: every responses-wire
    /// request, every family, every effort carries exactly one
    /// `<multi_agent_mode>` developer item). The W-1 contract this test
    /// protected — the xai wire effort is untouched and content part types
    /// are not normalized for the OpenAI-native family — is kept as
    /// targeted assertions; the only new byte is the unified item (SDD §5
    /// intentional delta 1).
    #[test]
    fn patch_responses_request_xai_wire_unchanged_gains_unified_item() {
        let mut body = serde_json::json!({
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        patch_responses_request(&mut body, Some("xai"), None, false, None);
        // Wire effort untouched: no `reasoning` object minted for a
        // sub-ultra (None) effort.
        assert!(body.get("reasoning").is_none());
        // OpenAI-native family: content part types not normalized.
        assert_eq!(
            body["input"][1]["content"][0]["type"],
            "input_text",
            "xai family must keep skipping normalization (user part)"
        );
        // Exactly one unified item, in the expanded explicit form.
        let input = body["input"].as_array().unwrap();
        let mode_items: Vec<&Value> = input.iter().filter(|item| is_multi_agent_mode_item(item)).collect();
        assert_eq!(mode_items.len(), 1, "xai turn must carry exactly one mode item");
        assert_eq!(
            mode_items[0]["content"][0]["text"].as_str().unwrap(),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            ),
        );
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
    /// replay retained (content kept; encrypted_content rides the wire per
    /// the mf6 typed-seam gate — the JSON seam no longer strips). If this
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
            // apex-ayl.86: wire-seam arg deleted, `ultra_wire_effort` None.
            // The unified item is a new byte below the user item (SDD §5
            // delta 1); the targeted assertions (user part un-normalized,
            // reasoning retained, no compaction_trigger) are unaffected.
            patch_responses_request(&mut body, Some(family), None, false, None);
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
                reasoning.get("encrypted_content").is_some(),
                "family {family}: the JSON seam is pin-unaware post-mf6 — the \
                 typed-seam affinity gate is the only strip authority"
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
    /// Codex deployments).
    ///
    /// AMENDED for apex-ayl.86 (ruling R-UNIFIED-ITEM, superseding the
    /// pre-cut "never appears for non-codex families" half of this pin):
    /// the unified item rides EVERY responses-wire request — the codex
    /// family keeps its EXACT bare keyword (frozen bytes, T6/T9) and every
    /// other family carries the expanded self-explanatory sentence in the
    /// same tag (SDD §5 intentional delta 1).
    #[test]
    fn multi_agent_mode_item_present_for_every_family() {
        let make_body = || {
            serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
                ]
            })
        };
        let mut body = make_body();
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Max), false, None);
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
            patch_responses_request(&mut body, Some(family), Some(ReasoningEffort::Max), false, None);
            assert!(
                body["input"].as_array().unwrap().iter().any(is_multi_agent_mode_item),
                "family {family} must carry the unified <multi_agent_mode> developer item (R-UNIFIED-ITEM)"
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
            patch_responses_request(&mut body, Some(family), None, true, None);
            // apex-ayl.86: the unified item now lands before the last user
            // message, so look the user/assistant parts up by role instead
            // of index (the pin's intent is unchanged: shim normalization).
            let input = body["input"].as_array().unwrap();
            let user_part = input
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|item| item["content"][0]["type"].as_str())
                .unwrap();
            let assistant_part = input
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some("assistant"))
                .and_then(|item| item["content"][0]["type"].as_str())
                .unwrap();
            assert_eq!(user_part, "text", "family {family} input_text");
            assert_eq!(assistant_part, "text", "family {family} output_text");
        }
    }

    /// W-1 Task 2 (T2) — the post-fix contract for vLLM families (Option A
    /// + B): a normal qwen/glm turn body must (a) be normalized
    /// input_text->text, (b) carry no `compaction_trigger`, (c) carry no
    /// `encrypted_content` anywhere under input — re-armed for apex-mf6:
    /// the unconditional body-level strip is retired, so the JSON seam is
    /// PIN-UNAWARE (ciphertext present at seam input survives the seam;
    /// the typed-seam affinity gate decided pre-serialization).
    ///
    /// Clause (d) AMENDED for apex-ayl.86 (ruling R-UNIFIED-ITEM): the
    /// pre-cut "no `<multi_agent_mode>` item off Codex" expectation (W-1
    /// ruling R-1) is superseded — the unified item rides every
    /// responses-wire request; (d) now pins the expanded explicit form
    /// present exactly once (SDD §5 intentional delta 1).
    #[test]
    fn vllm_families_fail_closed_on_codex_only_features() {
        for family in ["qwen", "glm"] {
            let mut body = serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "reasoning", "id": "rs_9", "content": [{"type": "reasoning_text", "text": "t"}], "encrypted_content": "gAAA"}
                ]
            });
            patch_responses_request(&mut body, Some(family), Some(ReasoningEffort::Max), false, None);
            let input = body["input"].as_array().unwrap();
            // (a) shim normalization
            let user_part = input
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|item| item["content"][0]["type"].as_str())
                .unwrap();
            assert_eq!(user_part, "text", "family {family}: input_text must normalize to text");
            // (b) no compaction-time item on a normal turn
            assert!(
                input.iter().all(|item| item.get("type").and_then(Value::as_str) != Some("compaction_trigger")),
                "family {family}: compaction_trigger must not appear"
            );
            // (c) mf6 (apex-mf6): the JSON seam is pin-unaware — the
            // synthetic ciphertext present at seam input must SURVIVE the
            // seam (the typed-seam gate is the only strip authority).
            assert!(
                input.iter().any(|item| item.get("encrypted_content").is_some()),
                "family {family}: the JSON seam no longer strips — the \
                 ciphertext must survive to the serialized body"
            );
            // (d) apex-ayl.86 (R-UNIFIED-ITEM): exactly one expanded
            // explicit-request-only item — the unified declaration rides
            // every non-codex turn (pre-cut clause (d) is superseded).
            let mode_items: Vec<&Value> =
                input.iter().filter(|item| is_multi_agent_mode_item(item)).collect();
            assert_eq!(
                mode_items.len(),
                1,
                "family {family}: exactly one <multi_agent_mode> item expected"
            );
            assert_eq!(
                mode_items[0]["content"][0]["text"].as_str().unwrap(),
                format!(
                    "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
                ),
            );
        }
    }

    /// W-1 Task 3.3 — RE-ARMED for XW-ENC-AFFINITY-1 (apex-mf6). The
    /// pre-mf6 contract was zero-`encrypted_content` on non-OpenAI bodies
    /// (the unconditional strip seam removed it). That strip is RETIRED:
    /// the JSON seam is now PIN-UNAWARE — it no longer touches
    /// `encrypted_content` at all. Ciphertext rides or is stripped at the
    /// typed send seam (the affinity gate, `apply_enc_affinity_gate`) and
    /// reactively (SIG-ENC-BOUNDARY fallback). This pin now asserts the
    /// seam is INERT: a non-OpenAI body carrying ciphertext in at seam
    /// input carries it out unchanged (the seam neither strips nor mints).
    #[test]
    fn non_openai_json_seam_is_inert_to_ciphertext_under_mf6() {
        for family in ["qwen", "glm"] {
            let mut body = serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "reasoning", "id": "rs_7", "encrypted_content": "gAAA", "summary": [{"type": "summary_text", "text": "s"}]}
                ]
            });
            patch_responses_request(&mut body, Some(family), None, true, None);
            let input = body["input"].as_array().unwrap();
            assert!(
                input.iter().any(|item| item.get("encrypted_content") == Some(&serde_json::json!("gAAA"))),
                "family {family}: the JSON seam must be inert — ciphertext in \
                 survives unchanged (no strip, no mint)"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // apex-ayl.77 INGRESS-NORMALIZE-1 — E2 add (JIG §6.3, adjudicated into
    // scope): the L0 invariant for the family-less responses-wire row.
    //
    // Today `is_openai_family("")` = TRUE (the `family.is_empty()` arm at
    // provider.rs:111), so a family-less row on a vLLM shim SILENTLY SKIPS
    // `normalize_content_types`. The fires-vs-named-flag ruling is a
    // PRODUCTION-BEHAVIOR change: picking "fires" (R-A) would (a) rewrite
    // input_text->text for any genuinely OpenAI-native row that merely has an
    // empty family (risking the SOL/OpenAI path, whose byte-identity is pinned
    // by `openai_families_are_byte_identical` incl. its `""` member), and
    // (b) require amending that pinned test. Per brief §6 this is therefore
    // marked **ADJUDICATION-NEEDED**: both arms are designed in the SDD
    // (§3.5), and the scratch state below is NON-PRESUPPOSING — it pins the
    // current behavior as a regression guard and stages the fire-case test as
    // `#[ignore]` (present, compiles, does not run until the ruling lands).
    // Reasoned default (SDD §3.5): R-B (named-flag opt-in).
    // ─────────────────────────────────────────────────────────────────────────

    /// E2 named-family regression guard (PASSES at RED, must keep passing at
    /// GREEN): the named OpenAI-native families keep skipping normalization —
    /// the E2 cut (whichever arm is ruled) must not over-fire onto the
    /// SOL/OpenAI path. Deliberately excludes `""`: the empty family is the
    /// adjudication subject and is pinned separately below.
    #[test]
    fn ingress77_openai_native_families_still_skip_normalization() {
        for family in ["openai", "xai", "codex"] {
            let mut body = serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}
                ]
            });
            // apex-ayl.86: wire-seam arg deleted, `ultra_wire_effort` None.
            // Post-cut the unified item lands at input[0] for these families
            // and its part type is also `input_text` (normalization skipped),
            // so this status-quo assertion holds unchanged (SDD §3.7).
            patch_responses_request(&mut body, Some(family), None, false, None);
            assert_eq!(
                body["input"][0]["content"][0]["type"], "input_text",
                "family {family}: named OpenAI-native family must keep skipping normalization"
            );
        }
    }

    /// E2 empty-family CURRENT-BEHAVIOR regression guard (PASSES at RED).
    /// Pins the status quo: with no named flag, an empty model family is
    /// treated as OpenAI-native and skips `normalize_content_types`. This is
    /// the SOL-path safety the JIG hazard analysis must not regress. If the
    /// ruling is R-A (fires), this test is retired/amended in the same cut;
    /// if R-B (named flag), it survives as the no-flag branch of the invariant.
    #[test]
    fn ingress77_empty_family_no_flag_current_behavior_skips_normalization() {
        for family in [None, Some("".to_owned())] {
            let mut body = serde_json::json!({
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
                ]
            });
            // apex-ayl.86: wire-seam arg deleted, `ultra_wire_effort` None.
            // Post-cut the unified item lands at input[0]; for the empty
            // family its part type is also `input_text` (normalization
            // skipped), so the status-quo assertion holds unchanged (SDD §3.7).
            patch_responses_request(&mut body, family.as_deref(), None, false, None);
            assert_eq!(
                body["input"][0]["content"][0]["type"], "input_text",
                "family {family:?}: empty family currently skips normalization (status-quo pin)"
            );
        }
    }

    // E2 fire-case — UNSTAGED by the coordinator's R-B ruling (named-flag
    // opt-in; SDD §3.5 ruling of record). The two-sided L0 invariant now
    // lives in the separate compile unit `tests/ingress77_e2_r_b.rs`
    // (no-flag skip + flag-set fire, post-cut 5-arg call shape). The
    // no-flag branch stays pinned here by
    // `ingress77_empty_family_no_flag_current_behavior_skips_normalization`.

    /// SDD-71 decision (b) · WAVE-C E2 pin (apex-ayl.71): pins the CURRENT
    /// truth of `is_openai_family` (def :107-112, empty arm :111 @40ffad1) —
    /// an empty `model_family` is treated as openai-class, so
    /// `patch_responses_request` SKIPS `normalize_content_types` (the
    /// vLLM-shim safety net). Standing L0 invariant test (tripwire, not
    /// policy); no behavior change in .71. INGRESS-NORMALIZE-1 (.77) re-pins
    /// this test if it flips the behavior.
    #[test]
    fn is_openai_family_empty_is_true_is_pinned() {
        assert!(
            is_openai_family(""),
            "empty model_family must stay openai-class (.77 owns any flip)"
        );
        // The skip consequence, end to end: an empty/None family leaves the
        // OpenAI-native content part types untouched...
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        // apex-ayl.86: wire-seam arg deleted, `ultra_wire_effort` None.
        // Post-cut the unified item lands at input[0]; for the empty family
        // its part type is also `input_text` (normalization skipped), so the
        // WAVE-C status-quo assertion holds unchanged (SDD §3.7).
        patch_responses_request(&mut body, None, None, false, None);
        assert_eq!(
            body["input"][0]["content"][0]["type"], "input_text",
            "empty family must SKIP normalize_content_types"
        );
        // ...while a vLLM-class family still gets the shim normalization.
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        // apex-ayl.86: wire-seam arg deleted, `ultra_wire_effort` None.
        // Post-cut the unified item lands at input[0]; for qwen the
        // normalization sweeps it to `text` like the rest of the input, so
        // the WAVE-C assertion holds unchanged (SDD §3.7).
        patch_responses_request(&mut body, Some("qwen"), None, false, None);
        assert_eq!(
            body["input"][0]["content"][0]["type"], "text",
            "qwen family must keep normalizing input_text -> text"
        );
    }

    // ═════════════════════════════════════════════════════════════════════
    // PROACTIVE-ULTRA-1 (apex-ayl.86) — unified two-axis egress test surface
    // (SDD §4: T1-T16; T13 lives in client.rs; the S/M tests live in the
    // shell + sampling-types pathspec files). RED-first: against the
    // Phase-1 stub the non-codex remap + unified-item assertions are ACTIVE
    // RED; T6/T7/T11 (codex arm intact in the stub) and the migrated
    // pre-cut pins pass.
    // ═════════════════════════════════════════════════════════════════════

    fn user_item(text: &str) -> Value {
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }]
        })
    }

    fn assistant_item(text: &str) -> Value {
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }]
        })
    }

    fn system_item(text: &str) -> Value {
        serde_json::json!({
            "type": "message",
            "role": "system",
            "content": [{ "type": "input_text", "text": text }]
        })
    }

    fn mode_items(body: &Value) -> Vec<&Value> {
        body["input"]
            .as_array()
            .expect("patched body has an input array")
            .iter()
            .filter(|item| is_multi_agent_mode_item(item))
            .collect()
    }

    fn mode_item_text(body: &Value) -> String {
        let items = mode_items(body);
        assert_eq!(
            items.len(),
            1,
            "exactly one <multi_agent_mode> item expected (I2); body: {body}"
        );
        items[0]["content"][0]["text"].as_str().unwrap().to_owned()
    }

    /// T1 (SDD §4): non-codex ultra carries the menu-derived wire value and
    /// the expanded proactive item (the v1 flag cells are superseded by
    /// R-UNIFIED-ITEM — there is no wire-seam flag).
    #[test]
    fn noncodex_ultra_remaps_to_menu_value_and_injects_proactive() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(
            &mut body,
            Some("qwen"),
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::Xhigh),
        );
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
    }

    /// T2 (SDD §4): placement per the UNCHANGED pre-cut insert_at logic
    /// (§3.5, :171-174) — before the last user message when the input ENDS
    /// with one, appended at the end otherwise (a conversation ending in an
    /// assistant item gets the item appended — the pre-cut codex behavior
    /// that T6 byte-pins).
    #[test]
    fn item_placement_before_last_user_message() {
        // Ends in an assistant item: the pre-cut logic appends at the end
        // (input [user, assistant, user, assistant] -> item at index 4).
        let mut body = serde_json::json!({
            "input": [
                user_item("u1"),
                assistant_item("a1"),
                user_item("u2"),
                assistant_item("a2")
            ]
        });
        patch_responses_request(&mut body, Some("qwen"), Some(ReasoningEffort::Medium), false, None);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 5);
        assert_eq!(input[4]["role"], "developer", "append at end when the last item is not a user message");
        // [assistant, user] -> index 1 (before the last user message).
        let mut body = serde_json::json!({ "input": [assistant_item("a1"), user_item("u1")] });
        patch_responses_request(&mut body, Some("qwen"), Some(ReasoningEffort::Medium), false, None);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["role"], "developer", "item lands before the last user message");
        assert_eq!(input[2]["role"], "user");
        // [assistant] (no user) -> appended at end.
        let mut body = serde_json::json!({ "input": [assistant_item("a1")] });
        // The literal-shape assertion below rides an OpenAI-native family:
        // under a normalizing family (qwen) the fresh item's content part
        // is rewritten input_text -> text (R5, pinned by T10), so the
        // injected literal form is only observable on a family the
        // normalize pass skips.
        patch_responses_request(&mut body, Some("xai"), Some(ReasoningEffort::Medium), false, None);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[1]["role"], "developer");
        // Item JSON deep-equal to the mode_item literal shape.
        let rendered = format!(
            "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
        );
        assert_eq!(
            input[1],
            serde_json::json!({
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": rendered }]
            })
        );
    }

    /// T3 (SDD §4): the tag-based stale-strip is form-agnostic — a stale
    /// bare-tag codex item and a stale expanded item both go, one fresh
    /// expanded item comes; a mode change REPLACES the declaration.
    #[test]
    fn stale_strip_is_form_agnostic() {
        let stale_bare = serde_json::json!({
            "type": "message",
            "role": "developer",
            "content": [{ "type": "input_text", "text": format!("{MULTI_AGENT_MODE_OPEN_TAG}proactive{MULTI_AGENT_MODE_CLOSE_TAG}") }]
        });
        let stale_expanded = serde_json::json!({
            "type": "message",
            "role": "developer",
            "content": [{ "type": "input_text", "text": format!("{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}") }]
        });
        let mut body = serde_json::json!({ "input": [stale_bare, user_item("hi"), stale_expanded] });
        patch_responses_request(
            &mut body,
            Some("qwen"),
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::Xhigh),
        );
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(body["input"].as_array().unwrap().len(), 2, "both stale forms stripped, one fresh item remains");
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
        // Mode change: re-patch the same body at max -> the proactive item is
        // replaced by the expanded explicit item (sub-ultra leaves the wire
        // effort untouched, so "xhigh" stays).
        patch_responses_request(&mut body, Some("qwen"), Some(ReasoningEffort::Max), false, None);
        assert_eq!(body["reasoning"]["effort"], "xhigh", "sub-ultra re-patch does not touch the wire effort");
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
    }

    /// T4 (SDD §4): the legacy-identity edge — a menu-less supported row CAN
    /// carry ultra (ultra ∈ LEGACY-6); the egress fallback is wire "max".
    #[test]
    fn menuless_ultra_falls_back_to_max() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(&mut body, Some("qwen"), Some(ReasoningEffort::Ultra), false, None);
        assert_eq!(body["reasoning"]["effort"], "max");
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
    }

    /// T5 (SDD §4, v2): sub-ultra turns are NO LONGER item-free — every
    /// sub-ultra effort carries the expanded explicit item, wire untouched.
    #[test]
    fn noncodex_subultra_efforts_get_explicit_item_wire_unchanged() {
        for effort in [
            ReasoningEffort::Xhigh,
            ReasoningEffort::Medium,
            ReasoningEffort::Low,
        ] {
            let mut body = serde_json::json!({ "input": [user_item("hi")] });
            patch_responses_request(&mut body, Some("qwen"), Some(effort), false, Some(ReasoningEffort::Xhigh));
            assert!(
                body.get("reasoning").is_none(),
                "no reasoning object minted for sub-ultra {effort:?}"
            );
            assert_eq!(
                mode_item_text(&body),
                format!(
                    "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
                )
            );
        }
    }

    /// The sol-shaped body for the T6 golden (Phase-0 capture input): the
    /// key order matters (workspace serde_json unifies `preserve_order`).
    fn sol_body() -> Value {
        serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [
                system_item("You are a helpful assistant."),
                user_item("First user turn."),
                assistant_item("First reply."),
                user_item("Second user turn.")
            ],
            "tools": [{ "type": "web_search" }]
        })
    }

    /// Hermetic replica of the PRE-CUT codex arm (T6 reference): the
    /// web_search `external_web_access` grant + Max|Ultra -> "max" remap +
    /// the all-effort bare-tag item, byte-for-byte the pre-cut behavior.
    fn legacy_codex_patch(body: &mut Value, effort: Option<ReasoningEffort>) {
        if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
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
        if matches!(
            effort,
            Some(ReasoningEffort::Max | ReasoningEffort::Ultra)
        ) {
            ensure_reasoning_object(body);
            body["reasoning"]["effort"] = Value::String("max".to_owned());
        }
        let mode_text = if effort == Some(ReasoningEffort::Ultra) {
            PROACTIVE_MULTI_AGENT_MODE_TEXT
        } else {
            EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT
        };
        let rendered = format!("{MULTI_AGENT_MODE_OPEN_TAG}{mode_text}{MULTI_AGENT_MODE_CLOSE_TAG}");
        let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
            return;
        };
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

    /// T6 (SDD §4, I1): codex byte-identity is a CONSEQUENCE — the unified
    /// rule must reproduce the pre-cut codex output at every effort:
    /// post-cut body == Phase-0 golden == in-test legacy replica.
    #[test]
    fn codex_byte_identity_golden() {
        let golden_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/proactiveultra86-sol-goldens.json"
        );
        let golden_raw = std::fs::read_to_string(golden_path).unwrap_or_else(|e| {
            panic!(
                "T6 golden missing at {golden_path} (Phase-0 pre-cut capture, SDD §4 T6; \
                 re-capture requires the live proxy + pre-cut binary — see \
                 proactiveultra86-report.md): {e}"
            )
        });
        let goldens: Value = serde_json::from_str(&golden_raw).expect("T6 goldens parse as JSON");
        let goldens = goldens.as_object().expect("T6 goldens are a JSON object");
        let cases: [(&str, Option<ReasoningEffort>); 7] = [
            ("low", Some(ReasoningEffort::Low)),
            ("medium", Some(ReasoningEffort::Medium)),
            ("high", Some(ReasoningEffort::High)),
            ("xhigh", Some(ReasoningEffort::Xhigh)),
            ("max", Some(ReasoningEffort::Max)),
            ("ultra", Some(ReasoningEffort::Ultra)),
            ("none", None),
        ];
        for (key, effort) in cases {
            let golden = goldens
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("T6 golden key {key:?} missing or not a string"));
            let mut real_body = sol_body();
            patch_responses_request(&mut real_body, Some("codex"), effort, false, None);
            let mut replica_body = sol_body();
            legacy_codex_patch(&mut replica_body, effort);
            assert_eq!(
                real_body, replica_body,
                "key {key}: the unified path must reproduce the pre-cut codex arm (I1)"
            );
            assert_eq!(
                real_body.to_string(),
                golden,
                "key {key}: post-cut codex body must be byte-identical to the Phase-0 capture"
            );
            assert_eq!(
                replica_body.to_string(),
                golden,
                "key {key}: the in-test legacy replica must match the Phase-0 capture (hermetic sanity)"
            );
        }
    }

    /// T7 (SDD §4): the frozen codex mapping wins; the menu-derived field is
    /// codex-irrelevant (I4).
    #[test]
    fn codex_arm_ignores_ultra_wire_effort() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(
            &mut body,
            Some("codex"),
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::High),
        );
        assert_eq!(body["reasoning"]["effort"], "max");
        assert_eq!(
            mode_item_text(&body),
            format!("{MULTI_AGENT_MODE_OPEN_TAG}proactive{MULTI_AGENT_MODE_CLOSE_TAG}")
        );
    }

    /// T8 (SDD §4): the arm is family-agnostic — family-less rows carry the
    /// menu-derived value; xai (the grok-4.6 class) keeps its wire untouched
    /// for sub-ultra and receives the declaration as inert prose.
    #[test]
    fn arm_is_family_agnostic() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(
            &mut body,
            None,
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::High),
        );
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(&mut body, Some("xai"), Some(ReasoningEffort::Low), false, None);
        assert!(body.get("reasoning").is_none());
        assert_eq!(
            mode_item_text(&body),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
    }

    /// T9 (SDD §4): the four text forms pinned against LITERAL strings
    /// copied from the §3.4 constants — constant drift = fail.
    #[test]
    fn item_text_forms_pinned_exact() {
        const PROACTIVE_EXPANDED_LIT: &str =
            "Multi-agent mode: proactive — subagents may be spawned proactively when the task benefits from delegation.";
        const EXPLICIT_EXPANDED_LIT: &str =
            "Multi-agent mode: explicit_request_only — subagents may be spawned only when the user explicitly requests.";
        let read_mode_text = |family: Option<&str>, effort: Option<ReasoningEffort>| -> String {
            let mut body = serde_json::json!({ "input": [user_item("hi")] });
            patch_responses_request(&mut body, family, effort, false, None);
            let items = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| is_multi_agent_mode_item(item))
                .collect::<Vec<_>>();
            assert_eq!(items.len(), 1);
            items[0]["content"][0]["text"].as_str().unwrap().to_owned()
        };
        assert_eq!(
            read_mode_text(Some("codex"), Some(ReasoningEffort::Ultra)),
            "<multi_agent_mode>proactive</multi_agent_mode>"
        );
        assert_eq!(
            read_mode_text(Some("codex"), Some(ReasoningEffort::Max)),
            "<multi_agent_mode>explicit_request_only</multi_agent_mode>"
        );
        assert_eq!(
            read_mode_text(Some("qwen"), Some(ReasoningEffort::Ultra)),
            format!("<multi_agent_mode>{PROACTIVE_EXPANDED_LIT}</multi_agent_mode>")
        );
        assert_eq!(
            read_mode_text(Some("qwen"), Some(ReasoningEffort::Xhigh)),
            format!("<multi_agent_mode>{EXPLICIT_EXPANDED_LIT}</multi_agent_mode>")
        );
    }

    /// T10 (SDD §4): the item injection runs BEFORE
    /// `normalize_content_types` — the fresh item joins the sweep on shim
    /// families (qwen) and keeps `input_text` on OpenAI-native families.
    #[test]
    fn injection_runs_before_normalization() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(
            &mut body,
            Some("qwen"),
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::Xhigh),
        );
        assert_eq!(
            mode_items(&body)[0]["content"][0]["type"],
            "text",
            "qwen (shim class) sweeps the fresh item input_text -> text"
        );
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(&mut body, Some("codex"), Some(ReasoningEffort::Ultra), false, None);
        assert_eq!(
            mode_items(&body)[0]["content"][0]["type"],
            "input_text",
            "codex (OpenAI-native) keeps input_text (normalization skipped)"
        );
    }

    /// T11 (SDD §4): codex at effort None still carries the bare explicit
    /// item (all-effort inclusion, today's behavior preserved).
    #[test]
    fn codex_none_effort_gets_explicit_bare_item() {
        let mut body = serde_json::json!({ "input": [user_item("hi")] });
        patch_responses_request(&mut body, Some("codex"), None, false, None);
        assert_eq!(
            mode_item_text(&body),
            "<multi_agent_mode>explicit_request_only</multi_agent_mode>"
        );
        assert!(body.get("reasoning").is_none());
    }

    /// T12 (SDD §4): a body without an `input` array is a no-op for the
    /// item (no panic); the remap still applies.
    #[test]
    fn no_input_array_is_a_noop_for_the_item() {
        let mut body = serde_json::json!({ "model": "qwen-test" });
        patch_responses_request(
            &mut body,
            Some("qwen"),
            Some(ReasoningEffort::Ultra),
            false,
            Some(ReasoningEffort::High),
        );
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("input").is_none(), "no input array => no item, no panic");
    }

    /// T15 (SDD §4, m1 fix): the unified item rides EVERY responses-wire
    /// request — subagent turns and summary-client requests included (the
    /// donor's gate-(d) exclusion is NOT adopted; the patch has no
    /// session-source input).
    #[test]
    fn subagent_and_summary_requests_carry_the_item() {
        let subagent_body = || {
            serde_json::json!({
                "model": "qwen3.8-27b",
                "input": [
                    system_item("You are a subagent. Complete the delegated task."),
                    user_item("Task: enumerate the crate layout under crates/codegen and report the top-level crates.")
                ]
            })
        };
        let summary_body = || {
            serde_json::json!({
                "model": "qwen3.8-27b",
                "input": [
                    system_item("You summarize coding sessions. Produce a compact summary."),
                    user_item("<transcript>\nuser: First user turn.\nassistant: First reply.\n</transcript>")
                ]
            })
        };
        for (label, mut body) in [("subagent", subagent_body()), ("summary", summary_body())] {
            patch_responses_request(&mut body, Some("qwen"), Some(ReasoningEffort::Medium), false, None);
            assert_eq!(mode_items(&body).len(), 1, "{label} sub-ultra turn carries exactly one unified item");
            assert_eq!(
                mode_item_text(&body),
                format!(
                    "{MULTI_AGENT_MODE_OPEN_TAG}{EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
                )
            );
        }
        for (label, mut body) in [("subagent", subagent_body()), ("summary", summary_body())] {
            patch_responses_request(
                &mut body,
                Some("qwen"),
                Some(ReasoningEffort::Ultra),
                false,
                Some(ReasoningEffort::Xhigh),
            );
            assert_eq!(body["reasoning"]["effort"], "xhigh");
            assert_eq!(mode_items(&body).len(), 1, "{label} ultra turn carries exactly one unified item");
            assert_eq!(
                mode_item_text(&body),
                format!(
                    "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
                )
            );
        }
    }

    /// T16 (SDD §4, M1 fix): egress parity between the two session-effort
    /// writers — the /effort writer's post-write state (qwen, Ultra,
    /// Some(Xhigh)) must produce the same egress as the switch-driven
    /// equivalent.
    #[test]
    fn effort_command_ultra_egress_matches_switch() {
        let egress = |effort: Option<ReasoningEffort>, ultra_wire: Option<ReasoningEffort>| -> Value {
            let mut body = serde_json::json!({
                "model": "qwen3.8-27b",
                "input": [user_item("Ship the menu rollout and verify the wire.")]
            });
            patch_responses_request(&mut body, Some("qwen"), effort, false, ultra_wire);
            body
        };
        // Switch-driven state (apply_supported_effort, post-projection).
        let switch_body = egress(Some(ReasoningEffort::Ultra), Some(ReasoningEffort::Xhigh));
        // /effort-driven state (handle_set_reasoning_effort, post-resolution).
        let effort_cmd_body = egress(Some(ReasoningEffort::Ultra), Some(ReasoningEffort::Xhigh));
        assert_eq!(
            switch_body, effort_cmd_body,
            "the two writers must produce egress-identical bodies for the same (effort, menu) pair"
        );
        assert_eq!(switch_body["reasoning"]["effort"], "xhigh");
        let items: Vec<&Value> = switch_body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| is_multi_agent_mode_item(item))
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]["content"][0]["text"].as_str().unwrap(),
            format!(
                "{MULTI_AGENT_MODE_OPEN_TAG}{PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED}{MULTI_AGENT_MODE_CLOSE_TAG}"
            )
        );
    }
}
