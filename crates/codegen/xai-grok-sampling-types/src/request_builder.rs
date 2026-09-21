//! Anthropic Messages request typestate + typed builder (REQVALID-1 47b,
//! frozen spec r23 B1/B2/B3/B4, bead apex-ayl.47).
//!
//! Normative shape (spec L1815-1852; name-mapped per SDD §2.8:
//! `MessageCreateParams` → `MessagesRequest`, `MessageParam` → `Message`,
//! `MessageSequenceError` → `RequestValidationError`):
//!
//! ```rust,ignore
//! pub struct DraftMessageSequence { /* private messages */ }
//! impl DraftMessageSequence {
//!     pub fn new() -> Self;
//!     pub fn push_message(&mut self, message: Message) -> Result<(), RequestValidationError>;
//! }
//! pub struct DraftMessagesRequest { /* private request + obligations */ }
//! impl DraftMessagesRequest {
//!     pub fn validate(self) -> Result<ValidatedMessagesRequest, RequestValidationError>;
//! }
//! pub struct ValidatedMessagesRequest(MessagesRequest);
//! impl ValidatedMessagesRequest {
//!     pub fn encode(&self) -> Result<EncodedMessagesRequest, RequestValidationError>;
//! }
//! ```
//!
//! B3: the draft has NO outbound serialization API and no raw request-field
//! access — only `ValidatedMessagesRequest` can produce the private-field
//! `EncodedMessagesRequest` that transport accepts. `DraftMessagesRequest`
//! deliberately does NOT impl `Serialize` (T4 compile-fail fixture);
//! `MessagesRequest` keeps its serde derives (the encode path and the 46b/
//! 47a byte goldens need them) but its fields are private and
//! `MessagesRequestBuilder` is the only construction path in code (T3/T5
//! compile-fail fixtures pin no struct-literal / raw-Value construction).
//!
//! B4 closed invariant set (V1, D-7): required fields non-empty, user-first
//! role order, tool call/result pairing, the pinned mutually exclusive field
//! pairs, STRICT thinking budget < max_tokens, ≤ 4 cache markers on
//! last-markable blocks, and `stream ∈ {None, Some(true)}` (OQ-2). The
//! validator performs NO model-capability guessing from a slug (T18 source
//! audit) and an exhaustive NO-WILDCARD match over MessageRole /
//! ContentBlock / ThinkingConfig / ToolChoiceParam / OutputFormat makes a
//! new enum variant a build break (T15).
//!
//! Gate order (D-1): `validate` runs the V1 invariants FIRST with zero
//! serialization; `encode` runs the 47a gate order N1 → N3 (R-1 image
//! pricing) → N2 unchanged. The combined 47a entry point
//! (`validate_and_encode_messages_request`) delegates to the same two
//! engines, so every 47a byte-identity pin (T8/T9 text-only boundaries,
//! T13 goldens) stays load-bearing and UNTOUCHED.

use std::collections::HashMap;

use crate::messages::{
    ContentBlock, Message, MessageContent, MessageRole, MessagesRequest, Metadata,
    MessagesRequestParts, OutputConfig, OutputFormat, SystemParam, ThinkingConfig,
    ToolChoiceParam, ToolParam, ToolResultContent,
};
use crate::presence::RequestPresence;
use crate::request_validation::{encode_caps, EncodedMessagesRequest, RequestValidationError};

// ============================================================================
// OQ-8: the typed tool input schema leaf
// ============================================================================

/// OQ-8 ruling (47b): the typed leaf for a tool `input_schema`. A JSON
/// schema IS arbitrary protocol data, so the builder's tool setter takes
/// this newtype instead of a raw `serde_json::Value` (raw Value/map/bytes
/// at request-field level stays E0308 — T5). Serialization-transparent by
/// construction: the DTO field stays `serde_json::Value`, so the wire bytes
/// are exactly the inner value — no wrapper key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSchema(serde_json::Value);

impl JsonSchema {
    pub fn new(value: serde_json::Value) -> Self {
        Self(value)
    }
    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
    pub fn into_value(self) -> serde_json::Value {
        self.0
    }
}

impl From<serde_json::Value> for JsonSchema {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

// ============================================================================
// B2: the closed invariant parameter set
// ============================================================================

/// B2/D-7: the closed V1 invariant parameter set. Every check in
/// `validate` reads its threshold from this object — the set is CLOSED
/// (private fields, single constructor): a new invariant is a code change,
/// never a caller-supplied knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestObligations {
    /// ≤ 4 cache_control markers per request (CAT-2 four-breakpoint shape;
    /// the pipeline's `apply_cache_breakpoints` places at most 4).
    max_cache_markers: usize,
    require_user_first_message: bool,
    require_model_non_empty: bool,
    /// STRICT `budget < max_tokens` (thinking Enabled only).
    strict_budget_below_max_tokens: bool,
    /// `stream` must be omitted or `true` (OQ-2; PIPE L3937-3938).
    stream_true_when_present: bool,
    /// thinking (Enabled|Adaptive) × top_k / × top_p are mutually exclusive.
    thinking_excludes_sampling_knobs: bool,
}

impl RequestObligations {
    /// The only constructor: the closed set. No field is caller-settable.
    pub fn closed_set() -> Self {
        Self {
            max_cache_markers: 4,
            require_user_first_message: true,
            require_model_non_empty: true,
            strict_budget_below_max_tokens: true,
            stream_true_when_present: true,
            thinking_excludes_sampling_knobs: true,
        }
    }
    pub(crate) fn max_cache_markers(&self) -> usize {
        self.max_cache_markers
    }
}

// ============================================================================
// B1: the concrete Anthropic typestate + typed builder (47b)
//
// RED-2 (this pass): pass-through shape — the API exists and is exercised by
// T1-T19, but `build()` is a struct-literal passthrough (always Ok),
// `push_message` performs no checks, and the invariant validator is an
// exhaustive no-op skeleton. GREEN replaces the no-ops with the D-7 checks
// in gate order; the T-surface is unchanged between the two passes.
// ============================================================================

/// B1: the outbound message-array draft. `push_message` is the incremental
/// construction path; `from_messages` is the trusted pipeline path
/// (the already-cleaned `build_messages_request` output) and records the
/// open-tool-use state without re-checking.
pub struct DraftMessageSequence {
    messages: Vec<Message>,
    /// Incremental tool-pairing state: tool_use id -> outstanding count
    /// (a tool_result consumes one outstanding use of the same id).
    open_tool_uses: HashMap<String, usize>,
}

impl DraftMessageSequence {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            open_tool_uses: HashMap::new(),
        }
    }

    pub fn push_message(&mut self, message: Message) -> Result<(), RequestValidationError> {
        // D-7 push-time check 1: the first message of a sequence must be
        // user (T7; the pipeline's D2 `[Continue]` repair keeps live output
        // user-first, so this backstop is reachable only on direct draft
        // construction).
        if self.messages.is_empty()
            && match message.role {
                MessageRole::User => false,
                MessageRole::Assistant => true,
            }
        {
            return Err(RequestValidationError::InvalidRoleOrder {
                index: 0,
                role: message.role,
            });
        }
        // D-7 push-time check 2: incremental tool pairing — a tool_result
        // must consume an open tool_use of the same id (over-close ⇒
        // UnpairedToolResult, T8a).
        if let MessageContent::Blocks(blocks) = &message.content {
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    cache_control,
                } = block
                {
                    let _ = (content, is_error, cache_control);
                    let open = self.open_tool_uses.get(tool_use_id).copied().unwrap_or(0);
                    if open == 0 {
                        return Err(RequestValidationError::UnpairedToolResult {
                            id: tool_use_id.clone(),
                        });
                    }
                }
            }
        }
        self.record_open_uses(&message);
        self.messages.push(message);
        Ok(())
    }

    /// Trusted pipeline path: unchecked (the producer already runs
    /// `clean_orphaned_items` + the D2 leading-user repair); the
    /// open-tool-use state is still recorded so a later
    /// `push_message` sees a consistent pairing view.
    pub fn from_messages(messages: Vec<Message>) -> Self {
        let mut this = Self {
            messages: Vec::with_capacity(messages.len()),
            open_tool_uses: HashMap::new(),
        };
        for message in &messages {
            this.record_open_uses(message);
        }
        this
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    fn record_open_uses(&mut self, message: &Message) {
        let MessageContent::Blocks(blocks) = &message.content else {
            return;
        };
        for block in blocks {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    *self.open_tool_uses.entry(id.clone()).or_default() += 1;
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if let Some(count) = self.open_tool_uses.get_mut(tool_use_id)
                        && *count > 0
                    {
                        *count -= 1;
                    }
                }
                _ => {}
            }
        }
    }
}

/// B1/B2: the only construction path in code for `MessagesRequest`.
/// Setters take CONCRETE types (no `impl Into`): raw `serde_json::Value` /
/// `Vec<u8>` / map input at request-field level is a compile error (T5).
/// `message_sequence` consumes the sequence — the only outbound
/// message-array path (B1).
pub struct MessagesRequestBuilder {
    model: Option<String>,
    sequence: Option<DraftMessageSequence>,
    max_tokens: Option<u32>,
    system: Option<SystemParam>,
    tools: Vec<ToolParam>,
    tool_choice: Option<ToolChoiceParam>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<u32>,
    stream: Option<bool>,
    stop_sequences: Option<Vec<String>>,
    thinking: Option<ThinkingConfig>,
    output_config: Option<OutputConfig>,
    metadata: Option<Metadata>,
}

impl MessagesRequestBuilder {
    pub fn new() -> Self {
        Self {
            model: None,
            sequence: None,
            max_tokens: None,
            system: None,
            tools: Vec::new(),
            tool_choice: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stream: None,
            stop_sequences: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    pub fn model(self, model: String) -> Self {
        let mut this = self;
        this.model = Some(model);
        this
    }

    pub fn message_sequence(self, sequence: DraftMessageSequence) -> Self {
        let mut this = self;
        this.sequence = Some(sequence);
        this
    }

    pub fn max_tokens(self, max_tokens: u32) -> Self {
        let mut this = self;
        this.max_tokens = Some(max_tokens);
        this
    }

    pub fn system(self, system: SystemParam) -> Self {
        let mut this = self;
        this.system = Some(system);
        this
    }

    /// OQ-8: per-tool append. The schema leaf is the typed `JsonSchema`
    /// newtype (serialization-transparent: the DTO field stays
    /// `serde_json::Value`, so the wire bytes are exactly the inner value).
    pub fn tool(
        self,
        name: String,
        description: Option<String>,
        input_schema: JsonSchema,
    ) -> Self {
        let mut this = self;
        this.tools.push(ToolParam {
            name,
            description,
            input_schema: input_schema.into_value(),
            cache_control: None,
        });
        this
    }

    pub fn tool_choice(self, tool_choice: ToolChoiceParam) -> Self {
        let mut this = self;
        this.tool_choice = Some(tool_choice);
        this
    }

    pub fn temperature(self, temperature: Option<f32>) -> Self {
        let mut this = self;
        this.temperature = temperature;
        this
    }

    pub fn top_p(self, top_p: Option<f32>) -> Self {
        let mut this = self;
        this.top_p = top_p;
        this
    }

    pub fn top_k(self, top_k: Option<u32>) -> Self {
        let mut this = self;
        this.top_k = top_k;
        this
    }

    pub fn stream(self, stream: Option<bool>) -> Self {
        let mut this = self;
        this.stream = stream;
        this
    }

    pub fn stop_sequences(self, stop_sequences: Vec<String>) -> Self {
        let mut this = self;
        this.stop_sequences = Some(stop_sequences);
        this
    }

    pub fn thinking(self, thinking: ThinkingConfig) -> Self {
        let mut this = self;
        this.thinking = Some(thinking);
        this
    }

    pub fn output_config(self, output_config: OutputConfig) -> Self {
        let mut this = self;
        this.output_config = Some(output_config);
        this
    }

    pub fn metadata(self, metadata: Metadata) -> Self {
        let mut this = self;
        this.metadata = Some(metadata);
        this
    }

    /// GREEN (D-7 at build): the required-field checks — model unset or
    /// empty ⇒ `MissingRequiredField { field: "model" }`; sequence not set
    /// ⇒ `MissingRequiredField { field: "messages" }` (an EMPTY consumed
    /// sequence passes build; the validator's empty-messages sentinel
    /// catches it at validate). Construction routes through the in-crate
    /// `from_parts` seam (the fields are private to the messages module).
    pub fn build(self) -> Result<MessagesRequest, RequestValidationError> {
        let model = match self.model {
            Some(model) if !model.is_empty() => model,
            _ => {
                return Err(RequestValidationError::MissingRequiredField {
                    field: "model",
                })
            }
        };
        let sequence = self
            .sequence
            .ok_or_else(|| RequestValidationError::MissingRequiredField {
                field: "messages",
            })?;
        Ok(MessagesRequest::from_parts(MessagesRequestParts {
            model,
            messages: sequence.messages().to_vec(),
            max_tokens: self.max_tokens.unwrap_or(0),
            system: self.system,
            tools: if self.tools.is_empty() {
                None
            } else {
                Some(self.tools)
            },
            tool_choice: self.tool_choice,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            stream: self.stream,
            stop_sequences: self.stop_sequences,
            thinking: self.thinking,
            output_config: self.output_config,
            metadata: self.metadata,
        }))
    }
}

/// B3: the request draft. NO outbound serialization API and no raw
/// field access — `validate` is the only way out, and it yields the
/// typestate proof that the checks occurred. Deliberately does NOT
/// impl `Serialize` (T4 compile-fail fixture).
pub struct DraftMessagesRequest {
    request: MessagesRequest,
    obligations: RequestObligations,
}

impl DraftMessagesRequest {
    pub fn new(request: MessagesRequest) -> Self {
        Self {
            request,
            obligations: RequestObligations::closed_set(),
        }
    }

    /// D-1 gate order: the V1 invariants FIRST, zero serialization; the
    /// caps engine (N1 → N3 → N2) runs at `encode` on the validated
    /// typestate.
    pub fn validate(self) -> Result<ValidatedMessagesRequest, RequestValidationError> {
        validate_messages_request_invariants(&self.request, &self.obligations)?;
        Ok(ValidatedMessagesRequest(self.request))
    }

    /// Crate-internal escape for the trusted pipeline producer
    /// (`build_messages_request`): its output is validated at the client
    /// funnel before transport, so the draft's outbound surface stays
    /// `validate`-only for every other caller.
    pub(crate) fn into_inner(self) -> MessagesRequest {
        self.request
    }
}

/// B3: the only typestate from which the transport carrier can be
/// produced. Holding one proves the V1 checks occurred.
#[derive(Debug)]
pub struct ValidatedMessagesRequest(MessagesRequest);

impl ValidatedMessagesRequest {
    /// The 47a caps engine (gate order N1 → N3 (R-1 image pricing) → N2
    /// two-pass body), unchanged by the 47b split (D-1).
    pub fn encode(&self) -> Result<EncodedMessagesRequest, RequestValidationError> {
        encode_caps(&self.0)
    }
}

// ============================================================================
// D-7: the closed V1 invariant set (B4)
// ============================================================================

/// REQVALID-1 47b GREEN (D-7): walk one block list with the exhaustive
/// NO-WILDCARD match (every `ContentBlock` variant named — a new variant
/// is a build break), performing the pairing and cache-marker data
/// collection for `validate_messages_request_invariants`:
/// - tool pairing (only when `check_pairing`): `tool_use` opens,
///   `tool_result` consumes one open use of the same id; an over-close
///   rejects immediately (`UnpairedToolResult`). `open_order` keeps
///   first-seen order so the unanswered-use report stays deterministic.
/// - cache markers: counts every `cache_control` and remembers the first
///   marker that does NOT sit on the last markable block of its list
///   (markable = Text/Image/ToolUse/ToolResult; Thinking/RedactedThinking/
///   Unknown blocks have no marker field by construction). The count and
///   placement gates fire later, in the documented gate order.
fn check_block_list(
    blocks: &[ContentBlock],
    open_tool_uses: &mut HashMap<String, usize>,
    open_order: &mut Vec<String>,
    marker_count: &mut usize,
    misplaced: &mut Option<usize>,
    check_pairing: bool,
) -> Result<(), RequestValidationError> {
    let mut last_markable: Option<usize> = None;
    let mut list_marker: Option<usize> = None;
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Text { text, cache_control } => {
                let _ = text;
                last_markable = Some(block_index);
                if cache_control.is_some() {
                    *marker_count += 1;
                    list_marker = Some(block_index);
                }
            }
            ContentBlock::Image { source, cache_control } => {
                let _ = source;
                last_markable = Some(block_index);
                if cache_control.is_some() {
                    *marker_count += 1;
                    list_marker = Some(block_index);
                }
            }
            ContentBlock::ToolUse {
                id,
                name,
                input,
                cache_control,
            } => {
                let _ = (name, input);
                if check_pairing {
                    if !open_tool_uses.contains_key(id) {
                        open_order.push(id.clone());
                    }
                    *open_tool_uses.entry(id.clone()).or_default() += 1;
                }
                last_markable = Some(block_index);
                if cache_control.is_some() {
                    *marker_count += 1;
                    list_marker = Some(block_index);
                }
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
            } => {
                match content {
                    ToolResultContent::Text(text) => {
                        let _ = text;
                    }
                    ToolResultContent::Blocks(nested) => {
                        // Uniform block-list rule: nested tool-result
                        // content is its own list for count/placement
                        // (the pipeline never places markers there).
                        check_block_list(
                            nested,
                            open_tool_uses,
                            open_order,
                            marker_count,
                            misplaced,
                            false,
                        )?;
                    }
                }
                let _ = is_error;
                if check_pairing {
                    let open = open_tool_uses.get(tool_use_id).copied().unwrap_or(0);
                    if open == 0 {
                        return Err(RequestValidationError::UnpairedToolResult {
                            id: tool_use_id.clone(),
                        });
                    }
                    *open_tool_uses
                        .get_mut(tool_use_id)
                        .expect("open > 0 above") -= 1;
                }
                last_markable = Some(block_index);
                if cache_control.is_some() {
                    *marker_count += 1;
                    list_marker = Some(block_index);
                }
            }
            ContentBlock::Thinking { thinking, signature } => {
                let _ = (thinking, signature);
            }
            ContentBlock::RedactedThinking { data } => {
                let _ = data;
            }
            ContentBlock::Unknown { kind } => {
                let _ = kind;
            }
        }
    }
    if let (Some(marker_index), Some(last)) = (list_marker, last_markable) {
        if marker_index != last && misplaced.is_none() {
            *misplaced = Some(marker_index);
        }
    }
    Ok(())
}

/// The V1 invariant gate over the closed parameter set (B4; D-7).
/// GREEN: the D-7 checks in the documented gate order — model non-empty →
/// first-role/empty → tool pairing → mutual exclusion → thinking budget →
/// cache-marker count → cache-marker placement → stream — over the
/// exhaustive NO-WILDCARD match surface (T15: every variant token of
/// MessageRole / MessageContent / ContentBlock / SystemParam /
/// ToolResultContent / ThinkingConfig / ToolChoiceParam / OutputFormat is
/// named so a new variant is a build break).
pub(crate) fn validate_messages_request_invariants(
    request: &MessagesRequest,
    obligations: &RequestObligations,
) -> Result<(), RequestValidationError> {
    // Gate 1: model non-empty (build() rejects unset/empty already; this
    // is the closed-set pass over trusted-path requests).
    if obligations.require_model_non_empty && request.model().is_empty() {
        return Err(RequestValidationError::MissingRequiredField {
            field: "model",
        });
    }

    // Gate 2: first-role/empty (m-11). The empty array reports the
    // sentinel (index 0, role User — a real leading-assistant violation
    // always carries role Assistant, so the phrasing is unambiguous).
    if obligations.require_user_first_message {
        match request.messages().first() {
            None => {
                return Err(RequestValidationError::InvalidRoleOrder {
                    index: 0,
                    role: MessageRole::User,
                })
            }
            Some(first) => match first.role {
                MessageRole::User => {}
                MessageRole::Assistant => {
                    return Err(RequestValidationError::InvalidRoleOrder {
                        index: 0,
                        role: MessageRole::Assistant,
                    })
                }
            },
        }
    }

    // Gate 3 (tool pairing) + gate 6/7 data (cache markers): the
    // exhaustive block walk over the messages, then the system blocks.
    let mut open_tool_uses: HashMap<String, usize> = HashMap::new();
    let mut open_order: Vec<String> = Vec::new();
    let mut marker_count: usize = 0;
    let mut misplaced: Option<usize> = None;
    for message in request.messages() {
        match message.role {
            MessageRole::User => {}
            MessageRole::Assistant => {}
        }
        match &message.content {
            MessageContent::Text(text) => {
                let _ = text;
            }
            MessageContent::Blocks(blocks) => {
                check_block_list(
                    blocks,
                    &mut open_tool_uses,
                    &mut open_order,
                    &mut marker_count,
                    &mut misplaced,
                    true,
                )?;
            }
        }
    }
    if let Some(system) = request.system() {
        match system {
            SystemParam::Text(text) => {
                let _ = text;
            }
            SystemParam::Blocks(blocks) => {
                // System blocks are all markable (TextBlock), so the
                // last-markable index is simply the last index; the
                // uniform block-list rule applies with no pairing.
                let last = blocks.len().saturating_sub(1);
                for (index, block) in blocks.iter().enumerate() {
                    if block.cache_control.is_some() {
                        marker_count += 1;
                        if index != last && misplaced.is_none() {
                            misplaced = Some(index);
                        }
                    }
                }
            }
        }
    }
    // Gate 3 tail: a final unanswered tool_use (first in first-seen order).
    if let Some(id) = open_order
        .iter()
        .find(|id| open_tool_uses.get(*id).copied().unwrap_or(0) > 0)
    {
        return Err(RequestValidationError::UnansweredToolUse { id: id.clone() });
    }

    // Gate 4 (mutual exclusion) + gate 5 (STRICT budget): the closed
    // thinking set — the pair checks fire before the budget check.
    if let Some(thinking) = request.thinking() {
        match thinking {
            ThinkingConfig::Enabled { budget_tokens } => {
                if obligations.thinking_excludes_sampling_knobs {
                    if request.top_k().is_some() {
                        return Err(RequestValidationError::MutuallyExclusiveFields {
                            a: "thinking",
                            b: "top_k",
                        });
                    }
                    if request.top_p().is_some() {
                        return Err(RequestValidationError::MutuallyExclusiveFields {
                            a: "thinking",
                            b: "top_p",
                        });
                    }
                }
                if obligations.strict_budget_below_max_tokens
                    && !(*budget_tokens < request.max_tokens())
                {
                    return Err(RequestValidationError::ThinkingBudgetExceedsMaxTokens {
                        budget: *budget_tokens,
                        max_tokens: request.max_tokens(),
                    });
                }
            }
            ThinkingConfig::Adaptive { display } => {
                let _ = display;
                if obligations.thinking_excludes_sampling_knobs {
                    if request.top_k().is_some() {
                        return Err(RequestValidationError::MutuallyExclusiveFields {
                            a: "thinking",
                            b: "top_k",
                        });
                    }
                    if request.top_p().is_some() {
                        return Err(RequestValidationError::MutuallyExclusiveFields {
                            a: "thinking",
                            b: "top_p",
                        });
                    }
                }
            }
            ThinkingConfig::Disabled => {}
        }
    }

    // Gate 6: ≤ 4 cache_control markers (closed-set threshold).
    if marker_count > obligations.max_cache_markers() {
        return Err(RequestValidationError::CacheMarkerCountExceeded {
            count: marker_count,
        });
    }
    // Gate 7: last-markable placement.
    if let Some(at) = misplaced {
        return Err(RequestValidationError::CacheMarkerMisplaced { at });
    }
    // Gate 8: stream omitted or true (OQ-2).
    if obligations.stream_true_when_present {
        if let Some(stream) = request.stream() {
            if !stream {
                return Err(RequestValidationError::StreamFieldInvalid { value: stream });
            }
        }
    }

    // Closed-set surface (T15): the remaining wire enums stay exhaustively
    // named — no V1 invariant reads them; a new variant is a build break.
    if let Some(choice) = request.tool_choice() {
        match choice {
            ToolChoiceParam::Auto { .. } => {}
            ToolChoiceParam::Any { .. } => {}
            ToolChoiceParam::Tool { name, .. } => {
                let _ = name;
            }
            // F5 (apex-ayl.114): the GA none variant — no V1 invariant reads
            // it either (closed-set completion; a future variant breaks here).
            ToolChoiceParam::None => {}
        }
    }
    if let Some(output_config) = request.output_config() {
        let _ = &output_config.effort;
        if let RequestPresence::Value(format) = &output_config.format {
            let OutputFormat::JsonSchema { schema } = format;
            let _ = schema;
        }
    }
    Ok(())
}

// ============================================================================
// T1-T19 test surface (SDD §3.3 + §3.3a amendments; written once, RED-1 → GREEN)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{
        build_messages_request, AssistantItem, ConversationItem, ConversationRequest, ToolCall,
    };
    use crate::messages::{CacheControl, ImageSource, OutputFormat, TextBlock};
    use crate::request_validation::validate_and_encode_messages_request;

    // ------------------------------------------------------------------
    // Fixture helpers
    // ------------------------------------------------------------------

    fn user_text(text: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn assistant_text(text: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn user_blocks(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: MessageRole::User,
            content: MessageContent::Blocks(blocks),
        }
    }

    fn assistant_blocks(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: MessageContent::Blocks(blocks),
        }
    }

    fn text_block(text: &str, marker: Option<CacheControl>) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
            cache_control: marker,
        }
    }

    /// The M-2R amended rich-control shape built through the typed builder
    /// (field-complete: every `MessagesRequest` field set). The three
    /// `RequestPresence` parameters cover the A0 states.
    fn build_rich(
        effort: RequestPresence<String>,
        format: RequestPresence<OutputFormat>,
        user_id: RequestPresence<String>,
    ) -> MessagesRequest {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("use the tool")).expect("user push");
        seq.push_message(assistant_blocks(vec![
            text_block("calling the tool", None),
            ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "lookup".to_string(),
                input: serde_json::json!({ "key": 1 }),
                cache_control: None,
            },
        ]))
        .expect("assistant push");
        seq.push_message(user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: ToolResultContent::Text("ok".to_string()),
            is_error: false,
            cache_control: None,
        }]))
        .expect("tool_result push");
        MessagesRequestBuilder::new()
            .model("m".to_string())
            .max_tokens(8192)
            .message_sequence(seq)
            .system(SystemParam::Blocks(vec![
                TextBlock {
                    r#type: "text".to_string(),
                    text: "base instructions".to_string(),
                    cache_control: None,
                },
                TextBlock {
                    r#type: "text".to_string(),
                    text: "more instructions".to_string(),
                    cache_control: Some(CacheControl::ephemeral_with_ttl("1h")),
                },
            ]))
            .tool(
                "lookup".to_string(),
                Some("look things up".to_string()),
                JsonSchema::from(serde_json::json!({
                    "type": "object",
                    "properties": { "key": { "type": "integer" } }
                })),
            )
            .tool(
                "plain".to_string(),
                None,
                JsonSchema::from(serde_json::json!({ "type": "object" })),
            )
            .tool_choice(ToolChoiceParam::Tool {
                name: "lookup".to_string(),
                disable_parallel_tool_use: None,
            })
            .temperature(Some(0.7))
            .top_p(None)
            .top_k(None)
            .stream(Some(true))
            .stop_sequences(vec!["\n".to_string()])
            .thinking(ThinkingConfig::Enabled { budget_tokens: 1024 })
            .output_config(OutputConfig { effort, format })
            .metadata(Metadata { user_id })
            .build()
            .expect("field-complete request must build")
    }

    /// fixture_f-style conversation (tool round-trip + model row + budget) —
    /// the largest realistic builder output (copied verbatim from the 47a
    /// T13 fixture so the T19 #3 golden stays byte-identical to pre-47b).
    fn realistic_conversation_fixture() -> ConversationRequest {
        ConversationRequest {
            items: vec![
                ConversationItem::user("user turn one"),
                ConversationItem::Assistant(AssistantItem {
                    content: "assistant reply one".into(),
                    tool_calls: vec![ToolCall {
                        id: "call_f1".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"x"}"#.into(),
                    }],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
                ConversationItem::tool_result("call_f1", "result one"),
                ConversationItem::user("user turn two"),
            ],
            model: Some("claude-sonnet-5".to_string()),
            reasoning_effort: None,
            max_output_tokens: Some(4096),
            ..Default::default()
        }
    }

    /// A one-message request with optional control fields — the small
    /// probe shape for T10/T11/T12/T13/T14/T16.
    fn probe_request(
        max_tokens: u32,
        thinking: Option<ThinkingConfig>,
        top_p: Option<f32>,
        top_k: Option<u32>,
        stream: Option<bool>,
        system: Option<SystemParam>,
        messages: Vec<Message>,
    ) -> MessagesRequest {
        let mut seq = DraftMessageSequence::new();
        for message in messages {
            seq.push_message(message).expect("probe sequence push");
        }
        let mut builder = MessagesRequestBuilder::new()
            .model("m".to_string())
            .max_tokens(max_tokens)
            .message_sequence(seq)
            .top_p(top_p)
            .top_k(top_k)
            .stream(stream);
        if let Some(system) = system {
            builder = builder.system(system);
        }
        if let Some(thinking) = thinking {
            builder = builder.thinking(thinking);
        }
        builder.build().expect("probe request must build")
    }

    fn marked_text_block(text: &str) -> ContentBlock {
        text_block(text, Some(CacheControl::ephemeral()))
    }

    // ------------------------------------------------------------------
    // D-6 compile-fail harness (hand-rolled rustc; no new deps — OQ-1)
    // ------------------------------------------------------------------

    fn deps_dir() -> std::path::PathBuf {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // `cfg!(release)` is never set by cargo (hence the unexpected-cfg
        // warning); the profile is observable via debug_assertions — release
        // builds disable it, so map to the matching target/<profile>/deps.
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        manifest.join(format!("../../../target/{profile}/deps"))
    }

    fn newest_rlib(stem: &str) -> std::path::PathBuf {
        let dir = deps_dir();
        let prefix = format!("lib{stem}-");
        let mut best: Option<(Option<std::time::SystemTime>, std::path::PathBuf)> = None;
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("D-6 harness: cannot read deps dir {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".rlib") {
                let mtime = entry.metadata().and_then(|m| m.modified()).ok();
                if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                    best = Some((mtime, entry.path()));
                }
            }
        }
        best.map(|(_, p)| p)
            .unwrap_or_else(|| panic!("D-6 harness: no {stem} rlib under {}", dir.display()))
    }

    /// Compile `source` as a standalone lib against the just-built rlibs and
    /// return rustc's stderr. `current_dir` = CARGO_MANIFEST_DIR so the rustup
    /// shim resolves the worktree's pinned toolchain (never `-o /dev/null` —
    /// temp-dir failure).
    fn compile_fixture(name: &str, source: &str) -> String {
        let dir = deps_dir();
        let fixture = dir.join(format!("47b_cf_{name}.rs"));
        let out = dir.join(format!("47b_cf_{name}.rmeta"));
        std::fs::write(&fixture, source).expect("D-6 harness: write fixture");
        let output = std::process::Command::new("rustc")
            .arg("--edition")
            .arg("2021")
            .arg("--crate-type")
            .arg("lib")
            .arg(&fixture)
            .arg("--emit=metadata")
            .arg("-o")
            .arg(&out)
            .arg("-L")
            .arg(format!("dependency={}", dir.display()))
            .arg("--extern")
            .arg(format!(
                "xai_grok_sampling_types={}",
                newest_rlib("xai_grok_sampling_types").display()
            ))
            .arg("--extern")
            .arg(format!(
                "serde_json={}",
                newest_rlib("serde_json").display()
            ))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap_or_else(|e| panic!("D-6 harness: rustc failed to start: {e}"));
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let _ = std::fs::remove_file(&fixture);
        let _ = std::fs::remove_file(&out);
        stderr
    }

    fn assert_compile_fail(name: &str, source: &str, expected_code: &str) {
        let stderr = compile_fixture(name, source);
        assert!(
            stderr.contains(expected_code),
            "D-6 harness ({name}): expected {expected_code}; rustc stderr:\n{stderr}"
        );
    }

    // ------------------------------------------------------------------
    // T1-T19
    // ------------------------------------------------------------------

    /// T1: builder minimal request (model + 1 user message + max_tokens)
    /// → build → validate → encode Ok.
    #[test]
    fn builder_minimal_request_validates() {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("hello")).expect("user push");
        let request = MessagesRequestBuilder::new()
            .model("m".to_string())
            .max_tokens(64)
            .message_sequence(seq)
            .build()
            .expect("minimal request must build");
        let validated = DraftMessagesRequest::new(request)
            .validate()
            .expect("minimal request must pass V1");
        let encoded = validated.encode().expect("minimal request must encode");
        assert!(!encoded.as_bytes().is_empty());
    }

    /// T2: EVERY field set, all three `RequestPresence` states covered →
    /// encode bytes stable and equal to the legacy serde bytes (golden
    /// guard-pin; the 46b presence semantics stay byte-pinned).
    #[test]
    fn builder_field_complete_request_golden() {
        let request = build_rich(
            RequestPresence::value("high".to_string()),
            RequestPresence::Null,
            RequestPresence::value("user-1".to_string()),
        );
        // Field-complete construction is test-enforced (A1 inventory, B2).
        assert_eq!(request.model(), "m");
        assert_eq!(request.max_tokens(), 8192);
        assert_eq!(request.messages().len(), 3);
        assert!(request.system().is_some());
        assert_eq!(request.tools().map(|t| t.len()), Some(2));
        assert!(matches!(
            request.tool_choice(),
            Some(ToolChoiceParam::Tool { .. })
        ));
        assert_eq!(request.temperature(), Some(0.7));
        assert_eq!(request.top_p(), None);
        assert_eq!(request.top_k(), None);
        assert_eq!(request.stream(), Some(true));
        assert_eq!(
            request.stop_sequences().map(|s| s.len()),
            Some(1)
        );
        assert!(matches!(
            request.thinking(),
            Some(ThinkingConfig::Enabled { budget_tokens: 1024 })
        ));
        assert!(request.output_config().is_some());
        assert!(request.metadata().is_some());

        // Golden: encode twice → byte-identical; == the legacy serde bytes.
        let validated = DraftMessagesRequest::new(request.clone())
            .validate()
            .expect("field-complete request must pass V1");
        let first = validated.encode().expect("first encode");
        let second = validated.encode().expect("second encode");
        assert_eq!(first.as_bytes(), second.as_bytes());
        let legacy = serde_json::to_vec(&request).expect("legacy serialization");
        assert_eq!(first.as_bytes(), &legacy[..]);

        // Null-state coverage: each of the 3 presence fields can carry the
        // explicit JSON null on the wire.
        let nulled = build_rich(
            RequestPresence::Null,
            RequestPresence::Null,
            RequestPresence::Null,
        );
        let wire = serde_json::to_string(&nulled).expect("serialize nulled");
        assert!(wire.contains(r#""effort":null"#), "{wire}");
        assert!(wire.contains(r#""format":null"#), "{wire}");
        assert!(wire.contains(r#""user_id":null"#), "{wire}");

        // Omitted-state coverage: no member emitted at all.
        let omitted = build_rich(
            RequestPresence::Omitted,
            RequestPresence::Omitted,
            RequestPresence::Omitted,
        );
        let wire = serde_json::to_string(&omitted).expect("serialize omitted");
        assert!(!wire.contains("effort"), "{wire}");
        assert!(!wire.contains("format"), "{wire}");
        assert!(!wire.contains("user_id"), "{wire}");
    }

    /// T3 (D-6, m-5): direct struct-literal construction of `MessagesRequest`
    /// must fail to compile once the fields are private (RED-3 flip; fields
    /// are pub pre-flip, so this is the harness red until then). Probed
    /// against this toolchain (rustc 1.94.0, the exact D-6 harness
    /// invocation): private-field struct literals report E0451 ("fields of
    /// struct are private"), not E0603 (which is reserved for nonexistent
    /// fields) — the OQ (aa) correction to the SDD's code label; the
    /// intent (construction is a compile error) is unchanged.
    #[test]
    fn bypass_direct_field_construction_fails() {
        assert_compile_fail(
            "t3_struct_literal",
            r#"
pub fn f() {
    let _ = xai_grok_sampling_types::messages::MessagesRequest {
        model: "m".to_string(),
        messages: vec![],
        max_tokens: 1,
        ..Default::default()
    };
}
"#,
            "E0451",
        );
    }

    /// T4 (D-6): the draft typestate has NO outbound serialization API (B3)
    /// — `serde_json::to_string(&draft)` must fail with E0277 (harness-green
    /// from RED-2: the draft exists and deliberately has no `Serialize`).
    #[test]
    fn bypass_draft_serialize_fails() {
        assert_compile_fail(
            "t4_draft_serialize",
            r#"
pub fn f(d: xai_grok_sampling_types::request_builder::DraftMessagesRequest) {
    let _ = serde_json::to_string(&d);
}
"#,
            "E0277",
        );
    }

    /// T5 (D-6): builder setters take concrete types — raw `serde_json::Value`
    /// input must fail with E0308 (harness-green from RED-2: typed setters).
    #[test]
    fn bypass_untyped_inputs_fails() {
        assert_compile_fail(
            "t5_untyped_input",
            r#"
use xai_grok_sampling_types::request_builder::MessagesRequestBuilder;
pub fn f() {
    let _ = MessagesRequestBuilder::new().model(serde_json::json!("m"));
}
"#,
            "E0308",
        );
    }

    /// T6: the sequence API round-trips the REAL `build_messages_request`
    /// output byte-identically (R1 coalescing parity — guard-pin).
    #[test]
    fn sequence_coalescing_goldens() {
        let conv = realistic_conversation_fixture();
        let pipeline = build_messages_request(&conv);

        let mut seq = DraftMessageSequence::new();
        for message in pipeline.messages() {
            seq.push_message(message.clone())
                .expect("pipeline output must pass the push-time checks");
        }
        let pipeline_messages_bytes =
            serde_json::to_vec(&pipeline.messages()).expect("serialize pipeline messages");
        let seq_messages_bytes =
            serde_json::to_vec(&seq.messages()).expect("serialize sequence messages");
        assert_eq!(seq_messages_bytes, pipeline_messages_bytes);

        // Full-request parity: rebuild every field through the builder.
        let mut builder = MessagesRequestBuilder::new()
            .model(pipeline.model().to_string())
            .max_tokens(pipeline.max_tokens())
            .message_sequence(seq)
            .temperature(pipeline.temperature())
            .top_p(pipeline.top_p())
            .top_k(pipeline.top_k())
            .stream(pipeline.stream());
        if let Some(system) = pipeline.system() {
            builder = builder.system(system.clone());
        }
        if let Some(tools) = pipeline.tools() {
            for tool in tools {
                builder = builder.tool(
                    tool.name.clone(),
                    tool.description.clone(),
                    JsonSchema::new(tool.input_schema.clone()),
                );
            }
        }
        if let Some(tool_choice) = pipeline.tool_choice() {
            builder = builder.tool_choice(tool_choice.clone());
        }
        if let Some(stop_sequences) = pipeline.stop_sequences() {
            builder = builder.stop_sequences(stop_sequences.to_vec());
        }
        if let Some(thinking) = pipeline.thinking() {
            builder = builder.thinking(thinking.clone());
        }
        if let Some(output_config) = pipeline.output_config() {
            builder = builder.output_config(output_config.clone());
        }
        if let Some(metadata) = pipeline.metadata() {
            builder = builder.metadata(metadata.clone());
        }
        let rebuilt = builder.build().expect("pipeline output must rebuild");
        assert_eq!(
            serde_json::to_vec(&rebuilt).expect("serialize rebuilt"),
            serde_json::to_vec(&pipeline).expect("serialize pipeline")
        );
    }

    /// T7: a leading assistant is rejected at push (the D2 `[Continue]`
    /// repair keeps live pipeline output user-first; this backstop is
    /// reachable only on direct draft construction).
    #[test]
    fn sequence_leading_assistant_rejected() {
        let mut seq = DraftMessageSequence::new();
        let err = seq
            .push_message(assistant_text("hi"))
            .expect_err("leading assistant must be rejected at push");
        assert!(
            matches!(
                err,
                RequestValidationError::InvalidRoleOrder {
                    index: 0,
                    role: MessageRole::Assistant
                }
            ),
            "got: {err:?}"
        );
    }

    /// T8a: a tool_result without a matching open tool_use is rejected at
    /// push (incremental pairing).
    #[test]
    fn sequence_orphan_tool_result_rejected() {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("go")).expect("user push");
        let err = seq
            .push_message(user_blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "nope".to_string(),
                content: ToolResultContent::Text("orphan".to_string()),
                is_error: false,
                cache_control: None,
            }]))
            .expect_err("orphan tool_result must be rejected at push");
        assert!(
            matches!(
                err,
                RequestValidationError::UnpairedToolResult { ref id } if id == "nope"
            ),
            "got: {err:?}"
        );
    }

    /// T8b: the real pipeline complement — `build_messages_request` on
    /// orphan-laden history produces a pairing-clean DTO (guard-pin on the
    /// existing `clean_orphaned_items` behavior; T8a's push check is the
    /// backstop the pipeline already satisfies).
    #[test]
    fn pipeline_orphan_items_guard_pinned() {
        let conv = ConversationRequest {
            items: vec![
                ConversationItem::user("hi"),
                ConversationItem::tool_result("orphan_id", "no matching call"),
                ConversationItem::user("again"),
            ],
            model: Some("claude-sonnet-5".to_string()),
            max_output_tokens: Some(64),
            ..Default::default()
        };
        let request = build_messages_request(&conv);
        let mut open: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for message in request.messages() {
            if let MessageContent::Blocks(blocks) = &message.content {
                for block in blocks {
                    match block {
                        ContentBlock::ToolUse { id, .. } => {
                            *open.entry(id.clone()).or_default() += 1;
                        }
                        ContentBlock::ToolResult { tool_use_id, .. } => {
                            let count = open.get_mut(tool_use_id).expect(
                                "pipeline output must be pairing-clean",
                            );
                            *count -= 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        for (id, count) in &open {
            assert_eq!(*count, 0, "unanswered tool_use {id} in pipeline output");
        }
        // The orphan tool_result was dropped by the item-level cleanup.
        for message in request.messages() {
            if let MessageContent::Blocks(blocks) = &message.content {
                assert!(
                    !blocks.iter().any(|b| matches!(
                        b,
                        ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "orphan_id"
                    )),
                    "orphan tool_result survived the pipeline cleanup"
                );
            }
        }
    }

    /// T9: a final assistant tool_use with no following tool_result is
    /// rejected at validate (`UnansweredToolUse`).
    #[test]
    fn unanswered_tool_use_rejected() {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("use the tool")).expect("user push");
        seq.push_message(assistant_blocks(vec![
            text_block("calling", None),
            ContentBlock::ToolUse {
                id: "c1".to_string(),
                name: "lookup".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            },
        ]))
        .expect("assistant push");
        // A plain text user message does NOT answer the open tool_use.
        seq.push_message(user_text("never answered")).expect("user push");
        let request = MessagesRequestBuilder::new()
            .model("m".to_string())
            .max_tokens(64)
            .message_sequence(seq)
            .build()
            .expect("builds (pairing is a validate-time check)");
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("unanswered tool_use must be rejected at validate");
        assert!(
            matches!(
                err,
                RequestValidationError::UnansweredToolUse { ref id } if id == "c1"
            ),
            "got: {err:?}"
        );
    }

    /// T10: thinking (Enabled|Adaptive) × top_k are mutually exclusive
    /// (closed set — no slug guessing). max_tokens 8192 keeps the budget
    /// check from firing first.
    #[test]
    fn thinking_plus_top_k_rejected() {
        let request = probe_request(
            8192,
            Some(ThinkingConfig::Enabled { budget_tokens: 1024 }),
            None,
            Some(40),
            None,
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("thinking + top_k must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::MutuallyExclusiveFields { a, b }
                    if a == "thinking" && b == "top_k"
            ),
            "got: {err:?}"
        );
    }

    /// M-2R dedicated negative (the excluded old-fixture combination):
    /// thinking Adaptive × top_p are mutually exclusive.
    #[test]
    fn m2r_thinking_top_p_rejected() {
        let request = probe_request(
            8192,
            Some(ThinkingConfig::Adaptive { display: None }),
            Some(0.9),
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("thinking + top_p must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::MutuallyExclusiveFields { a, b }
                    if a == "thinking" && b == "top_p"
            ),
            "got: {err:?}"
        );
    }

    /// T11: STRICT `budget < max_tokens` (thinking Enabled only) + the N4
    /// prewarm pins: max_tokens 0 + Disabled accepts; max_tokens 0 +
    /// Enabled rejects (every budget at a 0 max).
    #[test]
    fn thinking_budget_vs_max_tokens() {
        // (a) budget == max_tokens ⇒ reject.
        let request = probe_request(
            1024,
            Some(ThinkingConfig::Enabled { budget_tokens: 1024 }),
            None,
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("budget == max_tokens must be rejected (STRICT <)");
        assert!(
            matches!(
                err,
                RequestValidationError::ThinkingBudgetExceedsMaxTokens {
                    budget: 1024,
                    max_tokens: 1024
                }
            ),
            "got: {err:?}"
        );
        // (b) budget > max_tokens ⇒ reject.
        let request = probe_request(
            1024,
            Some(ThinkingConfig::Enabled { budget_tokens: 2048 }),
            None,
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("budget > max_tokens must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::ThinkingBudgetExceedsMaxTokens {
                    budget: 2048,
                    max_tokens: 1024
                }
            ),
            "got: {err:?}"
        );
        // (c) max_tokens 0 + thinking Disabled ⇒ accept (N4 prewarm).
        let request = probe_request(
            0,
            Some(ThinkingConfig::Disabled),
            None,
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        DraftMessagesRequest::new(request)
            .validate()
            .expect("max_tokens 0 + Disabled must accept (N4 prewarm)");
        // (d) max_tokens 0 + thinking Enabled ⇒ reject (0 < 0 fails).
        let request = probe_request(
            0,
            Some(ThinkingConfig::Enabled { budget_tokens: 0 }),
            None,
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("budget 0 at max_tokens 0 must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::ThinkingBudgetExceedsMaxTokens {
                    budget: 0,
                    max_tokens: 0
                }
            ),
            "got: {err:?}"
        );
        // (e) 1024 < 8192 ⇒ accept.
        let request = probe_request(
            8192,
            Some(ThinkingConfig::Enabled { budget_tokens: 1024 }),
            None,
            None,
            None,
            None,
            vec![user_text("hi")],
        );
        DraftMessagesRequest::new(request)
            .validate()
            .expect("budget below max_tokens must accept");
    }

    /// T12: five cache_control markers (system head + four message tips) ⇒
    /// `CacheMarkerCountExceeded { count: 5 }` (the count gate precedes the
    /// placement gate — every marker here is on a last markable block).
    #[test]
    fn cache_markers_five_rejected() {
        let request = probe_request(
            64,
            None,
            None,
            None,
            None,
            Some(SystemParam::Blocks(vec![TextBlock {
                r#type: "text".to_string(),
                text: "system".to_string(),
                cache_control: Some(CacheControl::ephemeral()),
            }])),
            vec![
                user_blocks(vec![marked_text_block("u1")]),
                assistant_blocks(vec![marked_text_block("a1")]),
                user_blocks(vec![marked_text_block("u2")]),
                assistant_blocks(vec![marked_text_block("a2")]),
            ],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("five cache markers must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::CacheMarkerCountExceeded { count: 5 }
            ),
            "got: {err:?}"
        );
    }

    /// T13 (M-2R dedicated negative): cache_control on the FIRST of two
    /// system blocks ⇒ `CacheMarkerMisplaced { at: 0 }` (the marker must sit
    /// on the last markable block of its block list).
    #[test]
    fn cache_marker_misplaced_rejected() {
        let request = probe_request(
            64,
            None,
            None,
            None,
            None,
            Some(SystemParam::Blocks(vec![
                TextBlock {
                    r#type: "text".to_string(),
                    text: "first".to_string(),
                    cache_control: Some(CacheControl::ephemeral()),
                },
                TextBlock {
                    r#type: "text".to_string(),
                    text: "second".to_string(),
                    cache_control: None,
                },
            ])),
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("misplaced cache marker must be rejected");
        assert!(
            matches!(
                err,
                RequestValidationError::CacheMarkerMisplaced { at: 0 }
            ),
            "got: {err:?}"
        );
    }

    /// T14: four last-markable markers (the ≤ 4 bound is inclusive) ⇒ Ok
    /// (guard-pin on the pipeline's `apply_cache_breakpoints` shape).
    #[test]
    fn cache_markers_four_accept() {
        let request = probe_request(
            64,
            None,
            None,
            None,
            None,
            Some(SystemParam::Blocks(vec![TextBlock {
                r#type: "text".to_string(),
                text: "system".to_string(),
                cache_control: Some(CacheControl::ephemeral()),
            }])),
            vec![
                user_blocks(vec![marked_text_block("u1")]),
                assistant_blocks(vec![marked_text_block("a1")]),
                user_blocks(vec![marked_text_block("u2")]),
            ],
        );
        DraftMessagesRequest::new(request)
            .validate()
            .expect("four last-markable markers must accept");
    }

    /// T15: closed supported-feature set — an exhaustive NO-WILDCARD match
    /// over MessageRole / MessageContent / ContentBlock / SystemParam /
    /// ToolResultContent / ThinkingConfig / ToolChoiceParam / OutputFormat.
    /// A synthetic new variant breaks the build (structural); (a) audits the
    /// source for the full variant-token set and the absence of wildcard
    /// arms in the validator body, (b) runtime-probes every variant through
    /// a validating request.
    #[test]
    fn closed_feature_set_exhaustive() {
        // (a) source audit: all 22 variant tokens (2+2+7+2+2+3+3+1) appear
        // in the non-test source, and the validator body carries no
        // wildcard binding (`_`-leading line or `| _` arm).
        let source = include_str!("request_builder.rs");
        let non_test = source
            .split("#[cfg(test)]")
            .next()
            .expect("test module marker");
        let tokens = [
            "MessageRole::User",
            "MessageRole::Assistant",
            "MessageContent::Text",
            "MessageContent::Blocks",
            "ContentBlock::Text",
            "ContentBlock::Image",
            "ContentBlock::ToolUse",
            "ContentBlock::ToolResult",
            "ContentBlock::Thinking",
            "ContentBlock::RedactedThinking",
            "ContentBlock::Unknown",
            "SystemParam::Text",
            "SystemParam::Blocks",
            "ToolResultContent::Text",
            "ToolResultContent::Blocks",
            "ThinkingConfig::Enabled",
            "ThinkingConfig::Adaptive",
            "ThinkingConfig::Disabled",
            "ToolChoiceParam::Auto",
            "ToolChoiceParam::Any",
            "ToolChoiceParam::Tool",
            "OutputFormat::JsonSchema",
        ];
        for token in &tokens {
            assert!(
                non_test.contains(token),
                "T15: non-test source is missing the exhaustive-match token {token}"
            );
        }
        let body_start = non_test
            .find("fn validate_messages_request_invariants")
            .expect("T15: validator fn present in non-test source");
        let body = &non_test[body_start..];
        let body = &body[..body.find("\n}\n").expect("T15: validator body end")];
        for line in body.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with('_'),
                "T15: wildcard binding in the validator body: {line}"
            );
            assert!(
                !trimmed.contains("| _"),
                "T15: wildcard arm in the validator body: {line}"
            );
        }

        // (b) runtime probe: one request exercising EVERY variant of every
        // enum validates Ok (the matches are semantically live, not just
        // compile-time exhaustive).
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("start")).expect("user push");
        seq.push_message(assistant_blocks(vec![
            ContentBlock::Text {
                text: "t".to_string(),
                cache_control: None,
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".to_string(),
                    data: "AAAA".to_string(),
                },
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "c1".to_string(),
                name: "probe".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            },
            ContentBlock::Thinking {
                thinking: "hmm".to_string(),
                signature: "sig".to_string(),
            },
            ContentBlock::RedactedThinking {
                data: "enc".to_string(),
            },
            ContentBlock::Unknown {
                kind: "future_block".to_string(),
            },
        ]))
        .expect("assistant push");
        seq.push_message(user_blocks(vec![
            ContentBlock::ToolResult {
                tool_use_id: "c1".to_string(),
                content: ToolResultContent::Blocks(vec![ContentBlock::Text {
                    text: "ok".to_string(),
                    cache_control: None,
                }]),
                is_error: false,
                cache_control: None,
            },
        ]))
        .expect("tool_result push");
        let request = MessagesRequestBuilder::new()
            .model("m".to_string())
            .max_tokens(8192)
            .message_sequence(seq)
            .system(SystemParam::Text("system".to_string()))
            .tool_choice(ToolChoiceParam::Any {
                disable_parallel_tool_use: None,
            })
            .thinking(ThinkingConfig::Enabled { budget_tokens: 1024 })
            .output_config(OutputConfig {
                effort: RequestPresence::value("high".to_string()),
                format: RequestPresence::value(OutputFormat::JsonSchema {
                    schema: serde_json::json!({ "type": "object" }),
                }),
            })
            .build()
            .expect("full-variant probe must build");
        DraftMessagesRequest::new(request)
            .validate()
            .expect("every-variant request must pass V1");

        // The remaining closed-set variants, one request each.
        for (choice, thinking, max_tokens) in [
            (
                ToolChoiceParam::Auto {
                    disable_parallel_tool_use: None,
                },
                ThinkingConfig::Disabled,
                0u32,
            ),
            (
                ToolChoiceParam::Tool {
                    name: "probe".to_string(),
                    disable_parallel_tool_use: None,
                },
                ThinkingConfig::Adaptive { display: None },
                64,
            ),
        ] {
            let mut seq = DraftMessageSequence::new();
            seq.push_message(user_text("hi")).expect("user push");
            let request = MessagesRequestBuilder::new()
                .model("m".to_string())
                .max_tokens(max_tokens)
                .message_sequence(seq)
                .tool_choice(choice)
                .thinking(thinking)
                .build()
                .expect("probe must build");
            DraftMessagesRequest::new(request)
                .validate()
                .expect("closed-set probe must pass V1");
        }
    }

    /// T16: `stream` must be omitted or true (OQ-2 default ruling, PIPE
    /// L3937-3938) — `Some(false)` ⇒ `StreamFieldInvalid { value: false }`.
    #[test]
    fn stream_field_ruling_pin() {
        let request = probe_request(
            64,
            None,
            None,
            None,
            Some(false),
            None,
            vec![user_text("hi")],
        );
        let err = DraftMessagesRequest::new(request)
            .validate()
            .expect_err("stream=false must be rejected (OQ-2)");
        assert!(
            matches!(
                err,
                RequestValidationError::StreamFieldInvalid { value: false }
            ),
            "got: {err:?}"
        );
        // None and Some(true) accept.
        for stream in [None, Some(true)] {
            let request = probe_request(
                64,
                None,
                None,
                None,
                stream,
                None,
                vec![user_text("hi")],
            );
            DraftMessagesRequest::new(request)
                .validate()
                .unwrap_or_else(|e| panic!("stream {stream:?} must accept: {e:?}"));
        }
    }

    /// T17: model unset or empty ⇒ `MissingRequiredField { field: "model" }`
    /// (semantics: unset or empty) at build.
    #[test]
    fn empty_model_rejected() {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("hi")).expect("user push");
        let err = MessagesRequestBuilder::new()
            .model("".to_string())
            .max_tokens(64)
            .message_sequence(seq)
            .build()
            .expect_err("empty model must be rejected at build");
        assert!(
            matches!(
                err,
                RequestValidationError::MissingRequiredField { field: "model" }
            ),
            "got: {err:?}"
        );
        // Unset model (no .model() call) is the same class.
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("hi")).expect("user push");
        let err = MessagesRequestBuilder::new()
            .max_tokens(64)
            .message_sequence(seq)
            .build()
            .expect_err("missing model must be rejected at build");
        assert!(
            matches!(
                err,
                RequestValidationError::MissingRequiredField { field: "model" }
            ),
            "got: {err:?}"
        );
    }

    /// T18: B4 "does not guess model capabilities from a slug" — the
    /// validate path references no capability-guessing symbol.
    #[test]
    fn no_slug_capability_guessing() {
        let source = include_str!("request_builder.rs");
        let non_test = source.split("#[cfg(test)]").next().expect("test module marker");
        for forbidden in [
            "is_anthropic_model",
            "catalog_family",
            "alias_slug",
            "messages_model",
        ] {
            assert!(
                !non_test.contains(forbidden),
                "T18: the validator module references the slug-guessing symbol {forbidden}"
            );
        }
    }

    /// T19 (M-2R): the 47a T13's 3 fixtures — #1 minimal, #2 the AMENDED
    /// rich-control shape, #3 the largest realistic pipeline output — each
    /// encodes byte-identically across the 47a combined entry, the 47b
    /// validate→encode split, and the legacy serde path. Two runs (distinct
    /// `REQVALID_GOLDEN_OUT` paths) must diff empty. The "== pre-47b bytes"
    /// contract covers #1/#3 (fixture #2's pre-47b bytes are the archived
    /// 47a goldens, /tmp/reqvalid-47a-golden-{1,2}.bin — see report).
    fn t19_three_paths_equal(request: &MessagesRequest) {
        let via_47a =
            validate_and_encode_messages_request(request).expect("47a combined entry");
        let via_47b = DraftMessagesRequest::new(request.clone())
            .validate()
            .expect("47b V1 invariants")
            .encode()
            .expect("47b caps encode");
        let legacy = serde_json::to_vec(request).expect("legacy serialization");
        assert_eq!(
            via_47b.as_bytes(),
            &legacy[..],
            "47b split must stay byte-identical to the legacy path"
        );
        assert_eq!(
            via_47a.as_bytes(),
            via_47b.as_bytes(),
            "47a combined entry must stay byte-identical to the 47b split"
        );
    }

    #[test]
    fn v1_byte_identity_golden() {
        let mut seq = DraftMessageSequence::new();
        seq.push_message(user_text("hello there")).expect("user push");
        seq.push_message(assistant_text("hi, how can I help?")).expect("assistant push");
        let fixtures = vec![
            // 1: simple user/assistant (pre-47b byte contract).
            MessagesRequestBuilder::new()
                .model("m".to_string())
                .max_tokens(1)
                .message_sequence(seq)
                .build()
                .expect("fixture 1 builds"),
            // 2: system Blocks + tools + every control field — the M-2R
            // amended shape (effort Value / format Omitted / user_id Value,
            // byte-identical to the amended `rich_control_request`).
            build_rich(
                RequestPresence::value("high".to_string()),
                RequestPresence::Omitted,
                RequestPresence::value("user-1".to_string()),
            ),
            // 3: largest realistic shape: builder output over the
            // fixture_f-style conversation (pre-47b byte contract).
            build_messages_request(&realistic_conversation_fixture()),
        ];
        let mut golden = Vec::new();
        for (index, request) in fixtures.iter().enumerate() {
            t19_three_paths_equal(request);
            // Encode twice: the carrier bytes are deterministic.
            let validated = DraftMessagesRequest::new(request.clone())
                .validate()
                .expect("fixture must validate");
            let first = validated.encode().expect("fixture first encode");
            let second = validated.encode().expect("fixture second encode");
            assert_eq!(
                first.as_bytes(),
                second.as_bytes(),
                "fixture {index} must encode deterministically"
            );
            golden.extend_from_slice(first.as_bytes());
            golden.push(b'\n');
        }
        if let Some(out) = std::env::var("REQVALID_GOLDEN_OUT").ok() {
            std::fs::write(&out, &golden).expect("golden write");
        }
    }
}
