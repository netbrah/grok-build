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
//!    guarantees by construction.
//! 3. Opaque-reasoning replay (the point of the native wire):
//!    - thought summaries (`Reasoning.summary`) replay as `thought: true`
//!      text parts ahead of the assistant's own parts, cap-free, unless the
//!      row sets `thinking_replay = "off"` (the Messages wire's switch,
//!      honoured here with the same meaning: replay older thoughts or not);
//!    - the `thoughtSignature` (`Reasoning.encrypted_content`) replays on
//!      the part Gemini issued it on: the FIRST `functionCall` part of the
//!      step when the step called functions (parallel calls carry one
//!      signature, on the first), else the LAST text part. Every other
//!      `functionCall` part of the history that has no genuine signature
//!      gets [`SKIP_THOUGHT_SIGNATURE_VALIDATOR`]: Gemini 3 rejects an
//!      unsigned current-turn call with a 400, previous-turn calls are not
//!      validated, and over-substituting is the documented, harmless side
//!      (history migrated from another backend, a signature stripped by the
//!      switch-time projector, a model that emitted none).
//!    - R8 model-identity suppression (MW-2 parity): when the owning
//!      assistant's `model_id` is `Some(m)` with `m` ≠ the request slug, its
//!      thoughts and signature are NOT replayed (a signature is bound to the
//!      model that minted it) — the calls then take the bypass value. An
//!      owner with no `model_id`, or no owner, is NOT suppressed.
//! 4. Head/tail repair: a history that opens or closes on a `model` content
//!    gets a `[Continue]` / `[Awaiting tool result]` user sentinel (Messages
//!    parity; the model-visible sentinel is deliberate).
//! 5. `systemInstruction` from every `System` item (concatenated as text
//!    parts, order kept); tools as `functionDeclarations` carrying the stored
//!    JSON Schema verbatim via `parametersJsonSchema`; `tool_choice` →
//!    `functionCallingConfig`; sampling knobs + `thinkingConfig` →
//!    `generationConfig`.
//!
//! Not projected: hosted tools (`hosted_tools`, `server_tools`,
//! `mcp_servers` — xAI/Anthropic server-side surfaces with no Gemini
//! equivalent here), `prompt_cache_key`, cache breakpoints, `user_id`,
//! `disable_parallel_tool_use` (no wire knob).

use super::*;
use crate::ReasoningEffort;
use crate::gemini::{
    Content, FunctionCall, FunctionCallingConfig, FunctionCallingMode, FunctionDeclaration,
    FunctionResponse, GenerateContentRequest, GenerationConfig, Part, SKIP_THOUGHT_SIGNATURE_VALIDATOR,
    ThinkingConfig, ThinkingLevel, Tool, ToolConfig,
};

/// Sentinel user text appended when the history closes on a model content
/// that still awaits a function response.
const AWAITING_TOOL_RESULT_SENTINEL: &str = "[Awaiting tool result]";
/// Sentinel user text when the history opens or closes on a plain model content.
const CONTINUE_SENTINEL: &str = "[Continue]";

/// Build the Gemini `generateContent` body for a conversation request.
/// `model` is copied onto the result for the URL path (never serialized).
pub fn build_generate_content_request(req: &ConversationRequest) -> GenerateContentRequest {
    let items = super::messages::clean_orphaned_items(&req.items);
    let model_slug = req.model.as_deref().unwrap_or_default();
    let r8_active = !model_slug.is_empty();
    let replay_thoughts = req.thinking_replay.as_deref() != Some("off");

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
                model_buffer.flush(&mut contents);
                flush_user(&mut pending_user, &mut contents);
                if !s.content.is_empty() {
                    system_parts.push(Part::text(s.content.as_ref()));
                }
            }
            ConversationItem::User(u) => {
                model_buffer.flush(&mut contents);
                pending_user.extend(content_parts_to_gemini(&u.content));
            }
            ConversationItem::ToolResult(t) => {
                model_buffer.flush(&mut contents);
                let name = function_name_for_result(&items, &t.tool_call_id);
                pending_user.push(Part::function_response(FunctionResponse {
                    id: None,
                    name,
                    response: function_response_payload(&t.content, t.is_error),
                }));
                for img in &t.images {
                    if let ContentPart::Image { url } = img {
                        pending_user.push(image_part_or_fallback(url));
                    }
                }
            }
            ConversationItem::Assistant(a) => {
                flush_user(&mut pending_user, &mut contents);
                model_buffer.owner_model = Some(a.model_id.as_deref());
                if !a.content.is_empty() {
                    model_buffer.text.push(Part::text(a.content.as_ref()));
                }
                for tc in &a.tool_calls {
                    model_buffer.calls.push(Part::function_call(FunctionCall {
                        id: None,
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
                let r8_suppressed = r8_active && matches!(owner_model, Some(m) if m != model_slug);
                if r8_suppressed {
                    continue;
                }
                if replay_thoughts {
                    for sp in &r.summary {
                        let rs::SummaryPart::SummaryText(t) = sp;
                        if !t.text.is_empty() {
                            model_buffer.thoughts.push(Part::thought(t.text.clone()));
                        }
                    }
                }
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
    model_buffer.flush(&mut contents);
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
                    parameters_json_schema: Some(t.parameters.clone()),
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

    let mut generation_config = GenerationConfig {
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: req.top_k,
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
/// (thoughts, text, function calls) and the step's signature is attached at
/// flush time per the placement rule in the module docs.
#[derive(Default)]
struct ModelBuffer<'a> {
    thoughts: Vec<Part>,
    text: Vec<Part>,
    calls: Vec<Part>,
    signature: Option<String>,
    /// `Some(model_id)` once an Assistant joined the buffer (its `model_id`
    /// may itself be `None`); `None` = no assistant in the buffer yet.
    owner_model: Option<Option<&'a str>>,
}

impl ModelBuffer<'_> {
    fn is_empty(&self) -> bool {
        self.thoughts.is_empty() && self.text.is_empty() && self.calls.is_empty()
    }

    fn flush(&mut self, contents: &mut Vec<Content>) {
        if self.is_empty() {
            // A signature with nothing to ride on cannot be replayed.
            *self = Self::default();
            return;
        }
        let mut parts: Vec<Part> = Vec::with_capacity(
            self.thoughts.len() + self.text.len() + self.calls.len(),
        );
        parts.append(&mut self.thoughts);
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
        for part in parts.iter_mut().skip(first_call_at) {
            if part.function_call.is_some() && part.thought_signature.is_none() {
                part.thought_signature = Some(SKIP_THOUGHT_SIGNATURE_VALIDATOR.to_owned());
            }
        }
        contents.push(Content::model(parts));
        *self = Self::default();
    }
}

fn flush_user(pending: &mut Vec<Part>, contents: &mut Vec<Content>) {
    if !pending.is_empty() {
        contents.push(Content::user(std::mem::take(pending)));
    }
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
        if let Some((mime_type, data)) = rest.split_once(";base64,")
            && !mime_type.is_empty()
            && !data.is_empty()
        {
            return Part::inline_data(mime_type, data);
        }
        return Part::text(format!("[invalid image: {url}]"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Part::file_data(None, url);
    }
    Part::text(format!("[invalid image: {url}]"))
}

/// Whether the slug is a Gemini 3 generation model (level-controlled
/// thinking) rather than a 2.5-style budget-controlled one.
pub fn is_thinking_level_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let rest = lower
        .rsplit('/')
        .next()
        .and_then(|s| s.strip_prefix("gemini-"))
        .unwrap_or("");
    rest.starts_with('3') || rest.starts_with(['4', '5', '6', '7', '8', '9'])
}

/// Effort → `thinkingConfig`. Thought summaries are always requested (they
/// are the user-visible reasoning channel). No effort = model default depth.
/// Gemini 3 takes a level; earlier generations a token budget (`-1` dynamic,
/// `0` off, else a fixed cap chosen inside every 2.5 model's accepted range).
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
            ReasoningEffort::None | ReasoningEffort::Minimal => ThinkingLevel::Minimal,
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
