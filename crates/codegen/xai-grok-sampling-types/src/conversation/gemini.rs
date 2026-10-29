//! Conversation → Gemini `generateContent` request projection.
//!
//! Sibling of `conversation/messages.rs` (`build_messages_request`) and
//! `conversation/responses.rs` for the native Google wire
//! (`ApiBackend::GenerateContent`). Dialect-specific: the storage form
//! (`ConversationItem`) stays wire-agnostic; this file owns every Gemini
//! shape decision.
//!
//! Pipeline (in order):
//! 1. Item-level orphan cleanup — the shared D5 stage-1 pass
//!    (`messages::clean_orphaned_items`): an unpaired `functionCall` /
//!    `functionResponse` is unsendable on this wire too.
//! 2. Item → content translation with same-role merge: consecutive
//!    user-side items (`User`, `ToolResult`) become ONE `user` content;
//!    consecutive model-side items (`Assistant`, its `Reasoning` sibling,
//!    degraded backend/discovery items) become ONE `model` content. The
//!    wire tolerates non-alternating roles, but every `functionResponse`
//!    answering a parallel call set must share one content, which the merge
//!    guarantees by construction. Media a tool produced nests under its own
//!    `functionResponse.parts` (multimodal function responses, Gemini 3+)
//!    rather than floating as sibling parts; a result that is only media
//!    gets the `"(see attached image)"` output placeholder so the response
//!    object is never empty. When a user content answers a model content's
//!    calls, its `functionResponse` parts are re-ordered to the call order
//!    (matching by id, or by name when ids are absent) and hoisted ahead of any text/media
//!    (gemini-cli `refineToolResponses` parity).
//!    Call ids: on Gemini 3+ every `functionCall` / `functionResponse`
//!    carries the stored call id. Native same-model ids are preserved;
//!    foreign ids use `[A-Za-z0-9_-]`, at most 64 chars, with collision
//!    resolution shared by calls and results.
//!    Gemini 3 issues an `id` on its calls and expects it back on the
//!    matching response ("if populated, the client … return the response
//!    with the matching id"); a long tool history replayed without ids
//!    degrades into textual pseudo-calls flagged `MALFORMED_FUNCTION_CALL`.
//!    Ids the decoder minted locally are sent too — a client-populated,
//!    consistent correlation key is what the field is for. Older
//!    generations get no ids (positional pairing only).
//! 3. Opaque-reasoning replay (the point of the native wire):
//!    - the `thoughtSignature` (`Reasoning.encrypted_content`) replays on
//!      the part Gemini issued it on: the FIRST `functionCall` part of the
//!      step when the step called functions (parallel calls carry one
//!      signature, on the first), else the LAST text part. Every other
//!      `functionCall` part of the history that has no genuine signature
//!      gets [`SKIP_THOUGHT_SIGNATURE_VALIDATOR`]: Gemini 3 rejects an
//!      unsigned current-turn call with a 400, previous-turn calls are not
//!      validated, and over-substituting is the documented, harmless side
//!      (history migrated from another backend, a signature stripped by the
//!      switch-time projector, a model that emitted none). The bypass value
//!      is a public Gemini API (`generativelanguage`, and any pass-through
//!      proxy in front of it such as LiteLLM `/gemini/v1beta`) contract:
//!      Vertex AI rejects it, so a Vertex resource-path slug
//!      (`projects/…`, `publishers/…`, `endpoints/…`) leaves unsigned calls
//!      without a `thoughtSignature` instead;
//!    - thought summaries (`Reasoning.summary`) are NOT replayed. The
//!      signature alone carries the model's reasoning state; summaries are
//!      lossy user-facing text that would only cost input tokens and perturb
//!      the implicit-cache prefix (gemini-cli strips every `thought: true`
//!      part before sending; `thinking_replay` is a Messages-wire switch
//!      with no meaning here);
//!    - R8 model-identity suppression (MW-2 parity): when the owning
//!      assistant's `model_id` is `Some(m)` and `m` is not the request
//!      model, its signature is NOT replayed (a signature is bound to the
//!      model that minted it) — the calls then take the bypass value. The
//!      comparison is [`same_gemini_model`]: path prefixes and case are
//!      ignored and a version/date/preview suffix on either side is
//!      tolerated for older histories that stored the stream's canonical
//!      `modelVersion`. New turns store the requested model for replay
//!      ownership, so a gateway or latest-model alias does not look like a
//!      model switch. An owner with no `model_id`, or no owner, is NOT suppressed.
//! 4. Head/tail repair: a history that opens or closes on a `model` content
//!    gets a `[Continue]` / `[Awaiting tool result]` user sentinel (Messages
//!    parity; the model-visible sentinel is deliberate).
//! 5. `systemInstruction` from every `System` item (concatenated as text
//!    parts, order kept); tools as `functionDeclarations` carrying the stored
//!    JSON Schema via `parametersJsonSchema`, verbatim except that a string
//!    or number `const` becomes a single-value `enum` (Gemini's accepted
//!    schema subset ignores `const` but honours `enum`; MCP tool schemas
//!    carry `const` routinely); `tool_choice` →
//!    `functionCallingConfig`; `thinkingConfig` + the remaining knobs →
//!    `generationConfig`. On level-controlled models (Gemini 3+, the only
//!    generation this backend targets) `temperature` / `topP` / `topK` are
//!    dropped: Google deprecates the sampling parameters there in favour of
//!    model-tuned defaults ("use model defaults to avoid degrading reasoning
//!    performance"), and the harness's sidecar calls carry low temperatures
//!    tuned for other providers.
//!
//! Not projected: hosted tools (`hosted_tools`, `server_tools`,
//! `mcp_servers` — xAI/Anthropic server-side surfaces with no Gemini
//! equivalent here), `prompt_cache_key`, cache breakpoints, `user_id`,
//! `disable_parallel_tool_use` (no wire knob).

use super::*;
use crate::ReasoningEffort;
use crate::gemini::{
    Content, FunctionCall, FunctionCallingConfig, FunctionCallingMode, FunctionDeclaration,
    FunctionResponse, FunctionResponsePart, GenerateContentRequest, GenerationConfig, Part,
    SKIP_THOUGHT_SIGNATURE_VALIDATOR, ThinkingConfig, ThinkingLevel, Tool, ToolConfig,
};
use std::collections::{HashMap, HashSet};

/// Sentinel user text appended when the history closes on a model content
/// that still awaits a function response.
const AWAITING_TOOL_RESULT_SENTINEL: &str = "[Awaiting tool result]";
/// Sentinel user text when the history opens or closes on a plain model content.
const CONTINUE_SENTINEL: &str = "[Continue]";
/// `functionResponse.response.output` for a result that is only media.
const MEDIA_ONLY_OUTPUT: &str = "(see attached image)";

/// Wire-shape decisions that depend on the configured slug (module docs,
/// steps 2 and 3).
#[derive(Clone, Copy)]
struct WirePolicy {
    /// Gemini 3+: echo the stored call id on `functionCall` / `functionResponse`.
    echo_call_ids: bool,
    /// Public API: unsigned `functionCall` parts take the bypass signature.
    /// Vertex AI rejects the sentinel, so there they stay unsigned.
    unsigned_call_sentinel: bool,
}

/// Build the Gemini `generateContent` body for a conversation request.
/// `model` is copied onto the result for the URL path (never serialized).
pub fn build_generate_content_request(req: &ConversationRequest) -> GenerateContentRequest {
    let items = super::messages::clean_orphaned_items(&req.items);
    let model_slug = req.model.as_deref().unwrap_or_default();
    let r8_active = !model_slug.is_empty();
    let level_model = is_thinking_level_model(model_slug);
    let policy = WirePolicy {
        echo_call_ids: level_model,
        unsigned_call_sentinel: !is_vertex_resource(model_slug),
    };
    let call_ids = if policy.echo_call_ids {
        wire_call_ids(&items, model_slug)
    } else {
        HashMap::new()
    };

    // Forward-owner lookup: per index, the model_id of the nearest FORWARD
    // Assistant (`None` = none, or one without a model_id). Consumed by a
    // Reasoning item that opens a model buffer (sits BEFORE its assistant —
    // the decoder's emission order).
    let forward_model: Vec<Option<&str>> = {
        let mut lookup = vec![None; items.len()];
        let mut next: Option<&str> = None;
        for i in (0..items.len()).rev() {
            lookup[i] = next;
            if let ConversationItem::Assistant(a) = &items[i] {
                next = a.model_id.as_deref();
            }
        }
        lookup
    };

    let mut system_parts: Vec<Part> = Vec::new();
    let mut contents: Vec<Content> = Vec::new();
    let mut pending_user: Vec<Part> = Vec::new();
    let mut model_buffer = ModelBuffer::default();

    for (idx, item) in items.iter().enumerate() {
        match item {
            ConversationItem::System(s) => {
                model_buffer.flush(&mut contents, policy);
                flush_user(&mut pending_user, &mut contents);
                if !s.content.is_empty() {
                    system_parts.push(Part::text(s.content.as_ref()));
                }
            }
            ConversationItem::User(u) => {
                model_buffer.flush(&mut contents, policy);
                pending_user.extend(content_parts_to_gemini(&u.content));
            }
            ConversationItem::ToolResult(t) => {
                model_buffer.flush(&mut contents, policy);
                let name = function_name_for_result(&items, &t.tool_call_id);
                let mut media: Vec<FunctionResponsePart> = Vec::new();
                let mut fallbacks: Vec<Part> = Vec::new();
                for img in &t.images {
                    if let ContentPart::Image { url } = img {
                        match function_response_media(url) {
                            Ok(part) => media.push(part),
                            Err(text) => fallbacks.push(Part::text(text)),
                        }
                    }
                }
                let content: &str =
                    if !t.is_error && t.content.trim().is_empty() && !media.is_empty() {
                        MEDIA_ONLY_OUTPUT
                    } else {
                        &t.content
                    };
                pending_user.push(Part::function_response(FunctionResponse {
                    id: call_ids.get(t.tool_call_id.as_str()).cloned(),
                    name,
                    response: function_response_payload(content, t.is_error),
                    parts: media,
                }));
                pending_user.extend(fallbacks);
            }
            ConversationItem::Assistant(a) => {
                flush_user(&mut pending_user, &mut contents);
                model_buffer.owner_model = Some(a.model_id.as_deref());
                if !a.content.is_empty() {
                    model_buffer.text.push(Part::text(a.content.as_ref()));
                }
                for tc in &a.tool_calls {
                    model_buffer.calls.push(Part::function_call(FunctionCall {
                        id: call_ids.get(tc.id.as_ref()).cloned(),
                        name: tc.name.clone(),
                        args: Some(tool_call_args(&tc.arguments, &tc.name)),
                    }));
                }
            }
            ConversationItem::Reasoning(r) => {
                flush_user(&mut pending_user, &mut contents);
                let owner_model: Option<&str> = match model_buffer.owner_model {
                    Some(m) => m,
                    None => forward_model[idx],
                };
                let r8_suppressed = r8_active
                    && matches!(owner_model, Some(m) if !same_gemini_model(m, model_slug));
                if r8_suppressed {
                    tracing::debug!(
                        owner = owner_model.unwrap_or_default(),
                        request = model_slug,
                        "R8: reasoning signature minted by another model; not replayed"
                    );
                    continue;
                }
                // Thought summaries are deliberately not replayed (module
                // docs, step 3): the signature is the whole reasoning state.
                if let Some(sig) = r.encrypted_content.as_deref().filter(|s| !s.is_empty()) {
                    // Last-wins within one step; one signature per model step
                    // is the wire's own shape.
                    model_buffer.signature = Some(sig.to_owned());
                }
            }
            // No Gemini client-wire site for xAI/Anthropic server-side tool
            // records: degrade to their text summary so the transcript keeps
            // its meaning (the Messages wire's non-carrier treatment).
            ConversationItem::BackendToolCall(b) => {
                flush_user(&mut pending_user, &mut contents);
                let summary = b.text_summary();
                if !summary.is_empty() {
                    model_buffer.text.push(Part::text(summary));
                }
            }
            ConversationItem::Discovery { item } => {
                flush_user(&mut pending_user, &mut contents);
                let summary = item.text_summary();
                if !summary.is_empty() {
                    model_buffer.text.push(Part::text(summary));
                }
            }
        }
    }
    model_buffer.flush(&mut contents, policy);
    flush_user(&mut pending_user, &mut contents);

    repair_head_and_tail(&mut contents);

    let system_instruction = (!system_parts.is_empty()).then(|| Content {
        role: None,
        parts: system_parts,
    });

    let tools: Vec<Tool> = if req.tools.is_empty() {
        Vec::new()
    } else {
        vec![Tool {
            function_declarations: req
                .tools
                .iter()
                .map(|t| FunctionDeclaration {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters_json_schema: Some(rewrite_const_as_enum(&t.parameters)),
                    parameters: None,
                })
                .collect(),
        }]
    };

    let tool_config = if tools.is_empty() {
        None
    } else {
        req.tool_choice.as_ref().map(|choice| ToolConfig {
            function_calling_config: Some(match choice {
                ConversationToolChoice::Auto => FunctionCallingConfig {
                    mode: Some(FunctionCallingMode::Auto),
                    allowed_function_names: Vec::new(),
                },
                ConversationToolChoice::None => FunctionCallingConfig {
                    mode: Some(FunctionCallingMode::None),
                    allowed_function_names: Vec::new(),
                },
                ConversationToolChoice::Required => FunctionCallingConfig {
                    mode: Some(FunctionCallingMode::Any),
                    allowed_function_names: Vec::new(),
                },
                ConversationToolChoice::Function(name) => FunctionCallingConfig {
                    mode: Some(FunctionCallingMode::Any),
                    allowed_function_names: vec![name.clone()],
                },
            }),
        })
    };

    // Level-controlled models (Gemini 3+): the sampling parameters are
    // deprecated in favour of model-tuned defaults; sending them degrades
    // reasoning. Sidecar calls (recap, laziness, image describe, ...) carry
    // low temperatures tuned for other providers, so this is a real drop.
    let (temperature, top_p, top_k) = if level_model {
        if req.temperature.is_some() || req.top_p.is_some() || req.top_k.is_some() {
            tracing::debug!(
                model = model_slug,
                temperature = ?req.temperature,
                top_p = ?req.top_p,
                top_k = ?req.top_k,
                "dropping sampling parameters on a level-controlled Gemini model (model defaults apply)"
            );
        }
        (None, None, None)
    } else {
        (req.temperature, req.top_p, req.top_k)
    };
    let mut generation_config = GenerationConfig {
        temperature,
        top_p,
        top_k,
        max_output_tokens: req.max_output_tokens,
        stop_sequences: req.stop_sequences.clone().unwrap_or_default(),
        response_mime_type: None,
        response_json_schema: None,
        thinking_config: Some(thinking_config(model_slug, req.reasoning_effort)),
    };
    // A response schema is refused alongside function declarations
    // (`supports_native_schema()` is false for this backend, so the shell
    // routes structured output through the StructuredOutput tool when tools
    // are present); a tool-free request takes the native JSON mode.
    if tools.is_empty()
        && let Some(schema) = req.json_schema.as_ref()
    {
        generation_config.response_mime_type = Some("application/json".to_owned());
        generation_config.response_json_schema = Some(schema.clone());
    }

    GenerateContentRequest {
        model: model_slug.to_owned(),
        contents,
        system_instruction,
        tools,
        tool_config,
        generation_config: (!generation_config.is_empty()).then_some(generation_config),
    }
}

/// Accumulator for one `model` content: parts are kept in their wire order
/// (text, then function calls) and the step's signature is attached at
/// flush time per the placement rule in the module docs.
#[derive(Default)]
struct ModelBuffer<'a> {
    text: Vec<Part>,
    calls: Vec<Part>,
    signature: Option<String>,
    /// `Some(model_id)` once an Assistant joined the buffer (its `model_id`
    /// may itself be `None`); `None` = no assistant in the buffer yet.
    owner_model: Option<Option<&'a str>>,
}

impl ModelBuffer<'_> {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.calls.is_empty()
    }

    fn flush(&mut self, contents: &mut Vec<Content>, policy: WirePolicy) {
        if self.is_empty() {
            // A signature with nothing to ride on cannot be replayed.
            *self = Self::default();
            return;
        }
        let mut parts: Vec<Part> = Vec::with_capacity(self.text.len() + self.calls.len());
        parts.append(&mut self.text);
        let first_call_at = parts.len();
        parts.append(&mut self.calls);
        let has_calls = first_call_at < parts.len();

        if let Some(sig) = self.signature.take() {
            let target = if has_calls {
                first_call_at
            } else {
                parts.len() - 1
            };
            parts[target].thought_signature = Some(sig);
        }
        if policy.unsigned_call_sentinel {
            for part in parts.iter_mut().skip(first_call_at) {
                if part.function_call.is_some() && part.thought_signature.is_none() {
                    part.thought_signature = Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_owned());
                }
            }
        }
        contents.push(Content::model(parts));
        *self = Self::default();
    }
}

/// Close the pending `user` content. When it answers the preceding model
/// content's function calls, its `functionResponse` parts are hoisted ahead
/// of any other part and re-ordered to the call order. Google pairs a
/// response with its call by `id` when both carry one (Gemini 3+, module
/// docs step 2) and positionally by name otherwise, so a result stored out
/// of call order (parallel tools finishing in any order) would answer the
/// wrong call when names repeat; keeping the order right on every
/// generation also keeps the replayed history byte-stable for the implicit
/// cache. Responses for names the model never called keep their relative
/// order after the matched ones (the orphan cleanup makes that a non-case).
fn flush_user(pending: &mut Vec<Part>, contents: &mut Vec<Content>) {
    if pending.is_empty() {
        return;
    }
    let mut parts = std::mem::take(pending);
    let calls: Vec<&FunctionCall> = contents
        .last()
        .filter(|c| c.is_model())
        .map(|c| {
            c.parts
                .iter()
                .filter_map(|p| p.function_call.as_ref())
                .collect()
        })
        .unwrap_or_default();
    if parts.iter().any(|p| p.function_response.is_some()) {
        parts = order_function_responses(parts, &calls);
    }
    contents.push(Content::user(parts));
}

fn order_function_responses(parts: Vec<Part>, calls: &[&FunctionCall]) -> Vec<Part> {
    let mut responses: Vec<Option<Part>> = Vec::new();
    let mut others: Vec<Part> = Vec::new();
    for part in parts {
        if part.function_response.is_some() {
            responses.push(Some(part));
        } else {
            others.push(part);
        }
    }
    let mut ordered: Vec<Part> = Vec::with_capacity(responses.len() + others.len());
    for call in calls {
        if let Some(slot) = responses.iter_mut().find(|slot| {
            slot.as_ref().is_some_and(|p| {
                p.function_response.as_ref().is_some_and(|fr| {
                    fr.name == call.name
                        && match (&call.id, &fr.id) {
                            (Some(call_id), Some(response_id)) => call_id == response_id,
                            _ => true,
                        }
                })
            })
        }) {
            ordered.extend(slot.take());
        }
    }
    ordered.extend(responses.into_iter().flatten());
    ordered.extend(others);
    ordered
}

/// Head/tail repair (Messages parity): the wire generates as `model`, so a
/// history that already ends on a model content would be a prefill, and one
/// that opens on a model content has no user turn to answer.
pub(super) fn repair_head_and_tail(contents: &mut Vec<Content>) {
    if contents.first().is_some_and(Content::is_model) {
        contents.insert(0, Content::user(vec![Part::text(CONTINUE_SENTINEL)]));
    }
    if let Some(last) = contents.last()
        && last.is_model()
    {
        let sentinel = if last.parts.iter().any(|p| p.function_call.is_some()) {
            AWAITING_TOOL_RESULT_SENTINEL
        } else {
            CONTINUE_SENTINEL
        };
        contents.push(Content::user(vec![Part::text(sentinel)]));
    }
}

/// `functionResponse.name` must repeat the called function's name; the
/// stored result carries only the call id, so the name is looked up on the
/// owning assistant (pairing is guaranteed by the orphan cleanup).
fn function_name_for_result(items: &[ConversationItem], tool_call_id: &str) -> String {
    items
        .iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(a) => a
                .tool_calls
                .iter()
                .find(|tc| &*tc.id == tool_call_id)
                .map(|tc| tc.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            tracing::warn!(
                tool_call_id,
                "tool result with no owning call survived orphan cleanup; sending an unnamed functionResponse"
            );
            String::new()
        })
}

/// `functionResponse.response` MUST be a JSON object. The documented
/// convention is `{"output": ...}` for a result and `{"error": ...}` for a
/// failure; a result that already is a JSON object is passed through as-is
/// (it keeps structured tool outputs structured), anything else is wrapped.
fn function_response_payload(content: &str, is_error: bool) -> serde_json::Value {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok();
    if is_error {
        return serde_json::json!({ "error": parsed.unwrap_or_else(|| serde_json::Value::String(content.to_owned())) });
    }
    match parsed {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        Some(other) => serde_json::json!({ "output": other }),
        None => serde_json::json!({ "output": content }),
    }
}

/// Preserve native ids and assign portable, distinct ids to foreign calls.
/// Reserving unchanged ids first prevents a normalized id from stealing one.
fn wire_call_ids<'a>(items: &'a [ConversationItem], model_slug: &str) -> HashMap<&'a str, String> {
    let calls: Vec<(&str, bool)> = items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Assistant(a) => Some(a),
            _ => None,
        })
        .flat_map(|a| {
            let native = a
                .model_id
                .as_deref()
                .is_some_and(|owner| same_gemini_model(owner, model_slug));
            a.tool_calls
                .iter()
                .map(move |call| (call.id.as_ref(), native))
        })
        .collect();
    let mut ids = HashMap::new();
    let mut used = HashSet::new();
    for &(id, native) in &calls {
        if !id.is_empty() && (native || wire_call_id(id).as_deref() == Some(id)) {
            ids.insert(id, id.to_owned());
            used.insert(id.to_owned());
        }
    }
    for (id, _) in calls {
        if ids.contains_key(id) {
            continue;
        }
        let Some(base) = wire_call_id(id) else {
            continue;
        };
        let mut candidate = base.clone();
        let mut suffix = 1usize;
        while used.contains(&candidate) {
            let tail = format!("_{suffix}");
            candidate = format!("{}{}", &base[..base.len().min(64 - tail.len())], tail);
            suffix += 1;
        }
        used.insert(candidate.clone());
        ids.insert(id, candidate);
    }
    ids
}

/// Portable form for ids inherited from another model. Native ids need not
/// satisfy these cross-provider compatibility constraints.
fn wire_call_id(id: &str) -> Option<String> {
    let normalised: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    (!normalised.is_empty()).then_some(normalised)
}

/// Whether the slug is a Vertex AI resource path rather than a public-API
/// model name (`gemini-3-pro`, `models/gemini-3-pro`). The client appends a
/// slug containing `/` to the base URL verbatim, so a Vertex row configures
/// `projects/P/locations/L/publishers/google/models/M`,
/// `publishers/google/models/M` or `endpoints/E`. A Vertex base URL paired
/// with a bare slug is not recognised: configure the resource path (the
/// documented Vertex shape) for the Vertex-specific wire rules to apply.
fn is_vertex_resource(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    ["projects/", "publishers/", "endpoints/"]
        .iter()
        .any(|marker| lower.contains(marker))
}

const JSON_SCHEMA_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "unevaluatedProperties",
    "propertyNames",
    "items",
    "prefixItems",
    "additionalItems",
    "unevaluatedItems",
    "contains",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    "contentSchema",
];

const JSON_SCHEMA_NAME_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

/// Rewrite a string or number `const` into a single-value `enum` wherever a
/// subschema has no `enum` of its own. Gemini's `parametersJsonSchema`
/// accepts a JSON Schema subset that ignores `const` but honours `enum`, so
/// a discriminator such as `{"const": "move"}` would otherwise be dropped
/// from the declaration. Only keyword positions that hold a schema (or a
/// name → schema map, or a list of schemas) are descended into: literal
/// data under `default`, `examples`, `enum` or `const` is never touched.
pub fn rewrite_const_as_enum(schema: &serde_json::Value) -> serde_json::Value {
    fn rewrite(schema: &serde_json::Value, is_name_map: bool) -> serde_json::Value {
        match schema {
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|item| rewrite(item, false)).collect())
            }
            serde_json::Value::Object(map) => {
                let mut result = serde_json::Map::with_capacity(map.len());
                for (key, value) in map {
                    let rewritten =
                        if is_name_map || JSON_SCHEMA_SUBSCHEMA_KEYWORDS.contains(&key.as_str()) {
                            rewrite(value, false)
                        } else if JSON_SCHEMA_NAME_MAP_KEYWORDS.contains(&key.as_str()) {
                            rewrite(value, true)
                        } else {
                            value.clone()
                        };
                    result.insert(key.clone(), rewritten);
                }
                if !is_name_map
                    && !result.contains_key("enum")
                    && result
                        .get("const")
                        .is_some_and(|v| v.is_string() || v.is_number())
                    && let Some(value) = result.remove("const")
                {
                    result.insert("enum".to_owned(), serde_json::Value::Array(vec![value]));
                }
                serde_json::Value::Object(result)
            }
            other => other.clone(),
        }
    }
    rewrite(schema, false)
}

/// `functionCall.args` is a JSON object; a non-object or unparseable stored
/// argument string is a local artifact and degrades to `{}` (the Messages
/// wire's `tool_call_input` rule).
fn tool_call_args(arguments: &str, tool_name: &str) -> serde_json::Value {
    if arguments.trim().is_empty() {
        return serde_json::json!({});
    }
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) if value.is_object() => value,
        Ok(value) => {
            tracing::warn!(
                tool = %tool_name,
                %value,
                "tool_call arguments parsed to a non-object JSON value; substituting an empty object"
            );
            serde_json::json!({})
        }
        Err(err) => {
            tracing::warn!(
                tool = %tool_name,
                "unparseable tool_call arguments; substituting an empty object: {err}"
            );
            serde_json::json!({})
        }
    }
}

fn content_parts_to_gemini(parts: &[ContentPart]) -> Vec<Part> {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => (!text.is_empty()).then(|| Part::text(text.as_ref())),
            ContentPart::Image { url } => Some(image_part_or_fallback(url)),
        })
        .collect()
}

/// `data:<mime>;base64,<data>` → `inlineData`; `http(s)://` → `fileData`
/// (MIME type unknown, left to the service); anything else degrades to a
/// labelled text part so a broken reference never fails the request.
fn image_part_or_fallback(url: &str) -> Part {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((mime_type, data)) = split_data_url(rest) {
            return Part::inline_data(mime_type, data);
        }
        return Part::text(format!("[invalid image: {url}]"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Part::file_data(None, url);
    }
    Part::text(format!("[invalid image: {url}]"))
}

/// Tool-produced media for `functionResponse.parts`. `Err` carries the
/// degraded text to send as a sibling text part instead: a nested `fileData`
/// requires a MIME type, so an `http(s)` URL with no recognisable image
/// extension and any malformed reference fall back to text.
fn function_response_media(url: &str) -> Result<FunctionResponsePart, String> {
    if let Some(rest) = url.strip_prefix("data:") {
        return split_data_url(rest)
            .map(|(mime_type, data)| FunctionResponsePart::inline_data(mime_type, data))
            .ok_or_else(|| format!("[invalid image: {url}]"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return match image_mime_from_url(url) {
            Some(mime) => Ok(FunctionResponsePart::file_data(Some(mime.to_owned()), url)),
            None => Err(format!("[image: {url}]")),
        };
    }
    Err(format!("[invalid image: {url}]"))
}

fn split_data_url(rest: &str) -> Option<(&str, &str)> {
    rest.split_once(";base64,")
        .filter(|(mime_type, data)| !mime_type.is_empty() && !data.is_empty())
}

fn image_mime_from_url(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        _ => return None,
    })
}

/// R8 identity test between the `modelVersion` an assistant item recorded
/// and the configured slug. Both are reduced to their last path segment,
/// lower-cased; equal names match, and so does a name that extends the
/// other by a version / date / preview suffix (`-001`, `-preview-05-20`,
/// `-exp`, `-latest`, `@001`), because Google reports the canonical,
/// dated version while rows configure the alias (or a Vertex resource path:
/// `projects/p/locations/l/publishers/google/models/<slug>`). A variant
/// suffix (`-lite`, `-8b`, `-thinking`) is a different model: its signature
/// may be rejected, so it is NOT matched.
pub fn same_gemini_model(owner: &str, slug: &str) -> bool {
    fn canonical(model: &str) -> String {
        model
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
    fn is_version_suffix(suffix: &str) -> bool {
        let Some(rest) = suffix.strip_prefix(['-', '@']) else {
            return false;
        };
        if rest.is_empty() {
            return false;
        }
        rest.split(['-', '.']).all(|seg| {
            !seg.is_empty()
                && (seg.bytes().all(|b| b.is_ascii_digit())
                    || matches!(seg, "preview" | "exp" | "latest"))
        })
    }
    let (owner, slug) = (canonical(owner), canonical(slug));
    if owner.is_empty() || slug.is_empty() {
        return false;
    }
    if owner == slug {
        return true;
    }
    let (short, long) = if owner.len() < slug.len() {
        (owner.as_str(), slug.as_str())
    } else {
        (slug.as_str(), owner.as_str())
    };
    long.strip_prefix(short).is_some_and(is_version_suffix)
}

/// Major generation number parsed from the first `gemini-<N>` in the slug
/// (any path prefix or proxy alias before it is skipped): `gemini-3.1-pro`
/// → 3, `models/gemini-2.5-flash` → 2, `my-gemini-4-argon` → 4. `None` when
/// the slug carries no `gemini-<digit>` marker.
fn gemini_generation(model: &str) -> Option<u32> {
    let lower = model.to_ascii_lowercase();
    let mut haystack = lower.as_str();
    while let Some(at) = haystack.find("gemini-") {
        let rest = &haystack[at + "gemini-".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        haystack = rest;
    }
    None
}

/// Whether the slug takes level-controlled thinking (`thinkingLevel`,
/// Gemini 3+) rather than a 2.5-style token budget. A slug with no
/// recognisable generation (a proxy alias such as `prod-coder`) is treated
/// as level-controlled: this backend targets Gemini 3.5+ and the budget
/// field is a hard 400 on the newest generation, while an unexpected level
/// on an old model is the cheaper failure.
pub fn is_thinking_level_model(model: &str) -> bool {
    gemini_generation(model).is_none_or(|generation| generation >= 3)
}

/// Whether `thinkingLevel: MINIMAL` is accepted: Gemini 3 Flash models only
/// (3 Pro / 3.1 Pro and Gemini 4 reject it with a 400).
fn supports_minimal_level(model: &str) -> bool {
    gemini_generation(model) == Some(3) && model.to_ascii_lowercase().contains("flash")
}

/// Effort → `thinkingConfig`. Thought summaries are always requested (they
/// are the user-visible reasoning channel). No effort = model default depth
/// (`high` on Gemini 3, `medium` on Gemini 4). Level-controlled models take a
/// level: `none`/`minimal` map to `MINIMAL` where the model accepts it and
/// clamp to `LOW` elsewhere (thinking cannot be disabled on these models);
/// earlier generations take a token budget (`-1` dynamic, `0` off, else a
/// fixed cap chosen inside every 2.5 model's accepted range).
pub fn thinking_config(model: &str, effort: Option<ReasoningEffort>) -> ThinkingConfig {
    let mut config = ThinkingConfig {
        include_thoughts: Some(true),
        thinking_budget: None,
        thinking_level: None,
    };
    let Some(effort) = effort else {
        return config;
    };
    if is_thinking_level_model(model) {
        config.thinking_level = Some(match effort {
            ReasoningEffort::None | ReasoningEffort::Minimal => {
                if supports_minimal_level(model) {
                    ThinkingLevel::Minimal
                } else {
                    ThinkingLevel::Low
                }
            }
            ReasoningEffort::Low => ThinkingLevel::Low,
            ReasoningEffort::Medium => ThinkingLevel::Medium,
            ReasoningEffort::High
            | ReasoningEffort::Xhigh
            | ReasoningEffort::Max
            | ReasoningEffort::Ultra => ThinkingLevel::High,
        });
    } else {
        config.thinking_budget = Some(match effort {
            ReasoningEffort::None => 0,
            ReasoningEffort::Minimal => 512,
            ReasoningEffort::Low => 2048,
            ReasoningEffort::Medium => -1,
            ReasoningEffort::High => 8192,
            ReasoningEffort::Xhigh | ReasoningEffort::Max | ReasoningEffort::Ultra => 24576,
        });
    }
    config
}
