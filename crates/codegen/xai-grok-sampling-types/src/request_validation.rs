//! Hard local caps + typed non-retryable validation for the Anthropic Messages
//! wire request (REQVALID-1 stage 47a, bead apex-ayl.47).
//!
//! Normative quotes (frozen spec r23, `codex/docs/superpowers/specs/
//! 2026-08-21-anthropic-spawn-runtime-design.md`):
//! - N1 message count (L1863-1865): at most 100,000 entries; rejected with a
//!   field-specific non-retryable error BEFORE counting serialization or body
//!   allocation; the encoded-body cap still applies afterward.
//! - N2 encoded-body cap (L1872-1881): `MAX_ENCODED_MESSAGES_REQUEST_BYTES =
//!   32_000_000`; two-pass encode — counting pass (`checked_add`, no body
//!   buffer), then `try_reserve_exact(count)` + capped second pass whose
//!   length must equal the count; typed failures distinguish counter
//!   overflow, cap exceeded, allocation failure, and pass-length mismatch.
//! - N3 per-item token cap (L1883-1893): inclusive
//!   `MAX_MODEL_CONTEXT_ITEM_TOKENS = 10_000` over every final-projected
//!   item (post-coalesce messages, system blocks, complete tool definitions).
//! - N4 max_tokens range (L1860-1862): the full `u32` range (incl. 0) is
//!   accepted; the validator carries no profile/policy parameter.
//!
//! `validate_and_encode_messages_request` runs the gate-ordered checks
//! (N1 count → N3 per-item → N2 two-pass body) and returns the encoded
//! carrier whose bytes ARE the wire body (byte-identity pinned by T13).

use std::fmt;
use std::io::Write;

use serde::Serialize;
use xai_token_estimation::{BYTES_PER_TOKEN, estimate_tokens, estimate_image_tokens};

use crate::messages::{
    ContentBlock, ImageSource, Message, MessageContent, MessagesRequest, SystemParam,
    ToolResultContent,
};

/// N1 (spec L1863): "The request `messages` array contains at most 100,000
/// entries."
pub const MAX_MESSAGES_REQUEST_ITEMS: usize = 100_000;

/// N2 (spec L1874): "Codex conservatively defines
/// `MAX_ENCODED_MESSAGES_REQUEST_BYTES = 32_000_000`. This is a local
/// request-safety bound, not model capability metadata."
pub const MAX_ENCODED_MESSAGES_REQUEST_BYTES: u64 = 32_000_000;

/// N3 (spec L1883-1884): "Codex defines the inclusive
/// `MAX_MODEL_CONTEXT_ITEM_TOKENS = 10_000`." Inclusive: an item estimating
/// exactly 10_000 tokens is accepted (strictly-greater rejects).
pub const MAX_MODEL_CONTEXT_ITEM_TOKENS: u64 = 10_000;

/// One capped item of the request (0-based index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRef {
    MessageItem(usize),
    SystemBlock(usize),
    Tool(usize),
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemRef::MessageItem(i) => write!(f, "message[{i}]"),
            ItemRef::SystemBlock(i) => write!(f, "system-block[{i}]"),
            ItemRef::Tool(i) => write!(f, "tool[{i}]"),
        }
    }
}

/// Local pre-HTTP validation failure for the Messages wire request's hard
/// caps. Every variant is non-retryable by construction: these are
/// client-side, deterministic violations (N1/N2/N3) — re-encoding the same
/// request cannot change the outcome. Display phrasings are verified (T14)
/// to match no retryable classifier family (CROSSWIRE-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestValidationError {
    /// N1: the `messages` array exceeds MAX_MESSAGES_REQUEST_ITEMS.
    TooManyMessages { count: usize },
    /// N3: an item's model-visible token estimate exceeds
    /// MAX_MODEL_CONTEXT_ITEM_TOKENS (strictly-greater).
    ItemTokenLimitExceeded { item: ItemRef, estimated_tokens: u64 },
    /// N2: the compact JSON request exceeds MAX_ENCODED_MESSAGES_REQUEST_BYTES.
    EncodedBodyTooLarge { bytes: u64 },
    /// N2: the counting pass's byte counter overflowed (checked_add).
    CounterOverflow,
    /// N2: `try_reserve_exact(count)` failed to allocate the body buffer.
    AllocationFailed { bytes: u64 },
    /// N2: the second-pass serialization length differs from the count.
    PassLengthMismatch { counted: u64, actual: u64 },
    /// Local 5th (impossible in practice — every DTO field has an infallible
    /// Serialize impl, except f32 NaN/inf, which JSON cannot express; typed
    /// and non-retryable regardless): an item, or the
    /// whole body, could not be serialized to compact JSON. Body-level
    /// failures map here with `ItemRef::MessageItem(0)` as the request-level
    /// marker.
    ItemEncodingFailed { item: ItemRef },
}

impl fmt::Display for RequestValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestValidationError::TooManyMessages { count } => write!(
                f,
                "messages array has {count} entries; limit {MAX_MESSAGES_REQUEST_ITEMS} (non-retryable, local)"
            ),
            RequestValidationError::ItemTokenLimitExceeded {
                item,
                estimated_tokens,
            } => write!(
                f,
                "context item {item} estimates {estimated_tokens} tokens; per-item limit {MAX_MODEL_CONTEXT_ITEM_TOKENS} (non-retryable, local)"
            ),
            RequestValidationError::EncodedBodyTooLarge { bytes } => write!(
                f,
                "encoded request body is {bytes} bytes; limit {MAX_ENCODED_MESSAGES_REQUEST_BYTES} bytes (non-retryable, local)"
            ),
            RequestValidationError::CounterOverflow => write!(
                f,
                "encoded-body byte counter overflowed during the counting pass (non-retryable, local)"
            ),
            RequestValidationError::AllocationFailed { bytes } => write!(
                f,
                "failed to allocate {bytes} bytes for the encoded request body (non-retryable, local)"
            ),
            RequestValidationError::PassLengthMismatch { counted, actual } => write!(
                f,
                "two-pass encode mismatch: counted {counted} bytes, serialized {actual} bytes (non-retryable, local)"
            ),
            RequestValidationError::ItemEncodingFailed { item } => write!(
                f,
                "context item {item} could not be serialized (non-retryable, local)"
            ),
        }
    }
}

impl std::error::Error for RequestValidationError {}

/// The N2 encoded carrier: exactly the bytes that reach transport. Debug is
/// manual (byte count only) so the body never leaks into logs.
pub struct EncodedMessagesRequest(Vec<u8>);

impl EncodedMessagesRequest {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for EncodedMessagesRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EncodedMessagesRequest {{ bytes: {} }}", self.0.len())
    }
}

/// N2 counting writer: `checked_add` per write, no body buffer. On overflow
/// it sets `overflowed` and keeps returning Ok (the pass ends with
/// CounterOverflow at [`finish_counting`]).
#[derive(Debug, Default)]
struct CountingWriter {
    count: u64,
    overflowed: bool,
}

impl CountingWriter {
    fn add_bytes(&mut self, len: u64) {
        match self.count.checked_add(len) {
            Some(total) => self.count = total,
            None => self.overflowed = true,
        }
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.add_bytes(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pass-1 completion: an overflowed counter ends the pass with
/// CounterOverflow (T5 seam — the i64-scale writes drive `add_bytes`
/// directly; a real slice cannot reach u64::MAX length).
fn finish_counting(counting: &CountingWriter) -> Result<u64, RequestValidationError> {
    if counting.overflowed {
        Err(RequestValidationError::CounterOverflow)
    } else {
        Ok(counting.count)
    }
}

/// N2 `try_reserve_exact(count)` step: allocation failure is a distinct
/// typed error (T6 seam).
fn reserve_body(count: u64) -> Result<Vec<u8>, RequestValidationError> {
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(count as usize)
        .map_err(|_| RequestValidationError::AllocationFailed { bytes: count })?;
    Ok(buf)
}

/// N2 two-pass encode: count (checked_add, no buffer) → cap check →
/// try_reserve_exact → second-pass serialize → pass-length equality. The
/// serializer is parameterized: the spec path passes the compact-JSON
/// writer for the request; T7 injects a corrupting second pass.
fn encode_two_pass<S: FnMut(&mut dyn Write) -> std::io::Result<()>>(
    mut serialize: S,
    cap: u64,
) -> Result<Vec<u8>, RequestValidationError> {
    let mut counting = CountingWriter::default();
    serialize(&mut counting).map_err(|_| {
        RequestValidationError::ItemEncodingFailed {
            item: ItemRef::MessageItem(0),
        }
    })?;
    let counted = finish_counting(&counting)?;
    if counted > cap {
        return Err(RequestValidationError::EncodedBodyTooLarge { bytes: counted });
    }
    let mut buf = reserve_body(counted)?;
    serialize(&mut buf).map_err(|_| {
        RequestValidationError::ItemEncodingFailed {
            item: ItemRef::MessageItem(0),
        }
    })?;
    if buf.len() as u64 != counted {
        return Err(RequestValidationError::PassLengthMismatch {
            counted,
            actual: buf.len() as u64,
        });
    }
    Ok(buf)
}

/// N3 per-item gate: compact JSON of the item, then the model-visible token
/// estimator (formula NOT normative; the 10_000 constant is). Strictly
/// greater than the cap rejects, so exactly 10_000 is accepted (N3
/// inclusive).
fn check_item_tokens<T: Serialize>(item: &T, ref_: ItemRef) -> Result<(), RequestValidationError> {
    let item_json = serde_json::to_vec(item).map_err(|_| {
        RequestValidationError::ItemEncodingFailed {
            item: ref_,
        }
    })?;
    // serde_json output is always valid UTF-8; from_utf8_lossy borrows
    // (no allocation) and `estimate_tokens` uses byte length only.
    let estimated = estimate_tokens(&String::from_utf8_lossy(&item_json));
    if estimated > MAX_MODEL_CONTEXT_ITEM_TOKENS {
        return Err(RequestValidationError::ItemTokenLimitExceeded {
            item: ref_,
            estimated_tokens: estimated,
        });
    }
    Ok(())
}

/// N3 per-item gate for MESSAGES (REQVALID-1 47a-FIX, ruling R-1): the
/// compact-JSON byte count MINUS the raw Base64 image payload, PLUS a flat
/// `IMAGE_TOKEN_ESTIMATE` (765) per Base64 image part. Base64 data is
/// unescaped in compact JSON (the alphabet carries no character needing JSON
/// escaping), so the subtraction is exact. For an image-less message the
/// subtraction is zero and the estimate is byte-identical to the plain
/// bytes/4 path — `estimate_tokens` is exactly `len / BYTES_PER_TOKEN`.
/// `ImageSource::Url` parts are deliberately left at the plain JSON/4
/// estimate (small text; R-1 Url clause).
fn check_message_tokens(message: &Message, ref_: ItemRef) -> Result<(), RequestValidationError> {
    let item_json = serde_json::to_vec(message).map_err(|_| {
        RequestValidationError::ItemEncodingFailed {
            item: ref_,
        }
    })?;
    let (payload_bytes, image_count) = base64_image_stats(message);
    let text_bytes = item_json.len() as u64 - payload_bytes;
    let estimated = text_bytes / BYTES_PER_TOKEN + estimate_image_tokens(image_count);
    if estimated > MAX_MODEL_CONTEXT_ITEM_TOKENS {
        return Err(RequestValidationError::ItemTokenLimitExceeded {
            item: ref_,
            estimated_tokens: estimated,
        });
    }
    Ok(())
}

/// (raw-payload bytes, count) over every Base64 image part in the message's
/// content blocks, including images nested in tool results.
fn base64_image_stats(message: &Message) -> (u64, u64) {
    match &message.content {
        MessageContent::Text(_) => (0, 0),
        MessageContent::Blocks(blocks) => image_stats_in_blocks(blocks),
    }
}

fn image_stats_in_blocks(blocks: &[ContentBlock]) -> (u64, u64) {
    let mut payload_bytes = 0u64;
    let mut image_count = 0u64;
    for block in blocks {
        match block {
            ContentBlock::Image { source, .. } => {
                if let ImageSource::Base64 { data, .. } = source {
                    payload_bytes += data.len() as u64;
                    image_count += 1;
                }
            }
            ContentBlock::ToolResult { content, .. } => {
                if let ToolResultContent::Blocks(nested) = content {
                    let (nested_bytes, nested_count) = image_stats_in_blocks(nested);
                    payload_bytes += nested_bytes;
                    image_count += nested_count;
                }
            }
            _ => {}
        }
    }
    (payload_bytes, image_count)
}

/// REQVALID-1 47a entry point. Gate order (cheap → expensive; N1's "before
/// counting serialization or body allocation" is structural):
/// 1. N1 message count — ZERO serialization.
/// 2. N3 per-item token cap on the final projected form.
/// 3. N2 two-pass encoded-body cap.
///
/// N4: NO max_tokens range check (full `u32` incl. 0 accepted); the
/// signature takes NO profile/policy parameter.
pub fn validate_and_encode_messages_request(
    request: &MessagesRequest,
) -> Result<EncodedMessagesRequest, RequestValidationError> {
    // Gate 1 (N1, spec L1863): message count — zero serialization.
    if request.messages.len() > MAX_MESSAGES_REQUEST_ITEMS {
        return Err(RequestValidationError::TooManyMessages {
            count: request.messages.len(),
        });
    }
    // Gate 2 (N3, spec L1883): per-item token cap over the final projected
    // form — every element of `messages` (post-coalesce form), each system
    // item, each complete tool definition.
    for (i, message) in request.messages.iter().enumerate() {
        check_message_tokens(message, ItemRef::MessageItem(i))?;
    }
    if let Some(system) = &request.system {
        match system {
            // Text(s) → ONE item (base instructions); Blocks(v) → ONE item
            // per TextBlock (stage SDD §2.1 enumeration).
            SystemParam::Text(_) => check_item_tokens(system, ItemRef::SystemBlock(0))?,
            SystemParam::Blocks(blocks) => {
                for (i, block) in blocks.iter().enumerate() {
                    check_item_tokens(block, ItemRef::SystemBlock(i))?;
                }
            }
        }
    }
    if let Some(tools) = &request.tools {
        // ONE item per complete ToolParam: the input schema is a
        // byte-subset of the whole definition and the estimator is monotone
        // in byte count, so the definition subsumes the schema-alone row
        // (recorded mapping decision, stage SDD §2).
        for (i, tool) in tools.iter().enumerate() {
            check_item_tokens(tool, ItemRef::Tool(i))?;
        }
    }
    // Gate 3 (N2, spec L1873): two-pass encoded-body cap — counting pass
    // (checked_add, no body buffer) → cap → try_reserve_exact → second
    // pass → pass-length equality. Only the carrier bytes reach transport.
    let bytes = encode_two_pass(
        |w: &mut dyn Write| {
            serde_json::to_writer(w, request)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        },
        MAX_ENCODED_MESSAGES_REQUEST_BYTES,
    )?;
    Ok(EncodedMessagesRequest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{
        build_messages_request, AssistantItem, ConversationItem, ConversationRequest, ToolCall,
    };
    use crate::error::{SamplingError, is_context_length_error, is_deterministic_in_stream_message};
    use crate::messages::{
        CacheControl, ContentBlock, ImageSource, Message, MessageContent, MessageRole,
        MessagesRequest,
        Metadata, OutputConfig, SystemParam, TextBlock, ThinkingConfig, ToolChoiceParam, ToolParam,
        ToolResultContent,
    };
    use crate::presence::RequestPresence;
    use xai_token_estimation::{estimate_tokens, IMAGE_TOKEN_ESTIMATE};

    // ------------------------------------------------------------------
    // Fixture helpers
    // ------------------------------------------------------------------

    /// Token estimate of an item's compact JSON under the in-tree estimator.
    /// Formula pinned first-hand: `estimate_tokens(s) = s.len() / 4` (floor) —
    /// its in-tree tests pin 3B→0, 4B→1, 4000B→1000.
    fn est(item_json: &[u8]) -> u64 {
        let s = String::from_utf8_lossy(item_json);
        estimate_tokens(&s)
    }

    fn text_message(role: MessageRole, text: &str) -> Message {
        Message {
            role,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn minimal_request(messages: Vec<Message>) -> MessagesRequest {
        MessagesRequest {
            model: "m".to_string(),
            messages,
            max_tokens: 1,
            ..Default::default()
        }
    }

    fn request_with_system(
        messages: Vec<Message>,
        system: Option<SystemParam>,
    ) -> MessagesRequest {
        let mut request = minimal_request(messages);
        request.system = system;
        request
    }

    fn request_with_tools(messages: Vec<Message>, tools: Vec<ToolParam>) -> MessagesRequest {
        let mut request = minimal_request(messages);
        request.tools = Some(tools);
        request
    }

    /// T3/T4 fixture: a request whose compact JSON body is exactly
    /// MAX_ENCODED_MESSAGES_REQUEST_BYTES (+ `over_by`) spread across
    /// 1024 messages, each individually under the N3 per-item cap.
    ///
    /// Mapping decision (reported): the SDD's "single Text message" at-cap
    /// fixture is unreachable under the SDD's own gate order (count →
    /// per-item → body): a single 32MB item estimates ~8M tokens and trips
    /// the N3 per-item cap (10_000) before the body cap can fire. N2's "many
    /// individually small values cannot bypass it" is the operative reading,
    /// and this is the only at-cap shape that can validate Ok.
    const T3_MESSAGE_COUNT: usize = 1024;
    /// Uniform item byte length for the first N-1 messages (28 B overhead + pad).
    const T3_UNIFORM_ITEM_BYTES: u64 = 31_242; // est 7_810 ≤ 10_000

    fn t3_request(over_by: u64) -> MessagesRequest {
        let probe_item = serde_json::to_vec(&text_message(MessageRole::User, "")).unwrap();
        let item_overhead = probe_item.len() as u64; // `{"role":"user","content":""}` = 28 B
        let probe_body = serde_json::to_vec(&minimal_request(vec![text_message(
            MessageRole::User,
            "",
        )]))
        .unwrap();
        // prefix `{"model":"m","messages":[` + suffix `],"max_tokens":1}` = 42 B
        let overhead = probe_body.len() as u64 - item_overhead;
        let fixed = overhead
            + (T3_MESSAGE_COUNT as u64 - 1) // inter-item commas
            + (T3_MESSAGE_COUNT as u64 - 1) * T3_UNIFORM_ITEM_BYTES;
        let last_item = MAX_ENCODED_MESSAGES_REQUEST_BYTES + over_by - fixed;
        assert!(
            (item_overhead..=40_003).contains(&last_item),
            "last item must stay under the per-item cap, got {last_item} bytes"
        );
        let uniform_pad = (T3_UNIFORM_ITEM_BYTES - item_overhead) as usize;
        let last_pad = (last_item - item_overhead) as usize;
        let mut messages = Vec::with_capacity(T3_MESSAGE_COUNT);
        for _ in 0..T3_MESSAGE_COUNT - 1 {
            messages.push(text_message(MessageRole::User, &"a".repeat(uniform_pad)));
        }
        messages.push(text_message(MessageRole::User, &"a".repeat(last_pad)));
        minimal_request(messages)
    }

    fn rich_control_request() -> MessagesRequest {
        let mut request = minimal_request(vec![
            text_message(MessageRole::User, "use the tool"),
            Message {
                role: MessageRole::Assistant,
                content: MessageContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "calling the tool".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "lookup".to_string(),
                        input: serde_json::json!({ "key": 1 }),
                        cache_control: None,
                    },
                ]),
            },
            Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: ToolResultContent::Text("ok".to_string()),
                    is_error: false,
                    cache_control: None,
                }]),
            },
        ]);
        request.system = Some(SystemParam::Blocks(vec![
            TextBlock {
                r#type: "text".to_string(),
                text: "base instructions".to_string(),
                cache_control: Some(CacheControl::ephemeral_with_ttl("1h")),
            },
            TextBlock {
                r#type: "text".to_string(),
                text: "more instructions".to_string(),
                cache_control: None,
            },
        ]));
        request.tools = Some(vec![
            ToolParam {
                name: "lookup".to_string(),
                description: Some("look things up".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "key": { "type": "integer" } }
                }),
            },
            ToolParam {
                name: "plain".to_string(),
                description: None,
                input_schema: serde_json::json!({ "type": "object" }),
            },
        ]);
        request.tool_choice = Some(ToolChoiceParam::Tool { name: "lookup".to_string() });
        request.temperature = Some(0.7);
        request.top_p = Some(0.9);
        request.top_k = Some(40);
        request.stream = Some(true);
        request.stop_sequences = Some(vec!["\n".to_string()]);
        request.thinking = Some(ThinkingConfig::Enabled { budget_tokens: 1024 });
        request.output_config = Some(OutputConfig {
            effort: RequestPresence::value("high".to_string()),
            format: RequestPresence::omitted(),
        });
        request.metadata = Some(Metadata {
            user_id: RequestPresence::value("user-1".to_string()),
        });
        request
    }

    /// fixture_f-style conversation from the existing sampling-types test
    /// fixtures (tool round-trip + model row + budget) — the largest
    /// realistic builder output used by the T13 golden.
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

    fn all_variants() -> Vec<RequestValidationError> {
        vec![
            RequestValidationError::TooManyMessages { count: 100_001 },
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::MessageItem(0),
                estimated_tokens: 10_001,
            },
            RequestValidationError::EncodedBodyTooLarge { bytes: 32_000_001 },
            RequestValidationError::CounterOverflow,
            RequestValidationError::AllocationFailed { bytes: 4 },
            RequestValidationError::PassLengthMismatch { counted: 5, actual: 6 },
            RequestValidationError::ItemEncodingFailed { item: ItemRef::Tool(1) },
        ]
    }

    /// T14a Display sweep: mirrors the classifier's text families first-hand:
    /// `is_model_bound_history_error` families 1-5 (error.rs, 400-gated),
    /// `is_deterministic_in_stream_message` (error.rs), `is_context_length_error`
    /// (re-exported from xai-grok-compaction).
    fn display_hits_retryable_family(display: &str) -> Option<&'static str> {
        let m = display.to_ascii_lowercase();
        if m.contains("encrypted_content") || m.contains("encrypted content") {
            return Some("model-bound family 1");
        }
        if m.contains("thinking") && m.contains("signature") {
            return Some("model-bound family 2");
        }
        if m.contains("input[") && m.contains(".id") && m.contains("invalid") {
            return Some("model-bound family 3");
        }
        if m.contains("item")
            && m.contains("id")
            && (m.contains("not found") || m.contains("does not exist"))
        {
            return Some("model-bound family 4");
        }
        if m.contains("input[")
            && (m.contains("array too long") || m.contains("array_above_max_length"))
        {
            return Some("model-bound family 5");
        }
        if is_deterministic_in_stream_message(&m) {
            return Some("deterministic in-stream");
        }
        if is_context_length_error(&m) {
            return Some("context length");
        }
        None
    }

    // ------------------------------------------------------------------
    // T1-T15 (stage SDD §3.3)
    // ------------------------------------------------------------------

    /// T1: exactly 100,000 otherwise-valid messages are accepted (N1).
    #[test]
    fn messages_100_000_accept() {
        let mut messages = Vec::with_capacity(MAX_MESSAGES_REQUEST_ITEMS);
        for i in 0..MAX_MESSAGES_REQUEST_ITEMS {
            let role = if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            messages.push(text_message(role, "a"));
        }
        let request = minimal_request(messages);
        let encoded = validate_and_encode_messages_request(&request)
            .expect("exactly 100_000 otherwise-valid messages must be accepted (N1)");
        assert_eq!(
            encoded.as_bytes(),
            serde_json::to_vec(&request).unwrap().as_slice()
        );
    }

    /// T2: 100,001 messages are rejected with the typed count variant,
    /// before any serialization (N1), and the wire error is terminal.
    #[test]
    fn messages_100_001_reject() {
        let mut messages = Vec::with_capacity(MAX_MESSAGES_REQUEST_ITEMS + 1);
        for _ in 0..=MAX_MESSAGES_REQUEST_ITEMS {
            messages.push(text_message(MessageRole::User, "a"));
        }
        let request = minimal_request(messages);
        let err = validate_and_encode_messages_request(&request)
            .expect_err("100_001 messages must be rejected before any serialization (N1)");
        assert_eq!(
            err,
            RequestValidationError::TooManyMessages {
                count: MAX_MESSAGES_REQUEST_ITEMS + 1
            }
        );
        let sampling = SamplingError::from(err.clone());
        assert!(
            matches!(sampling, SamplingError::RequestValidation(_)),
            "the wire error must carry the typed variant"
        );
        assert!(!sampling.is_retryable(), "a local cap violation is terminal (T14)");
    }

    /// T3: a body exactly at the encoded cap is accepted (N2). See the
    /// `t3_request` mapping decision for why the body is many items.
    #[test]
    fn body_at_cap_accept() {
        let request = t3_request(0);
        assert_eq!(
            serde_json::to_vec(&request).unwrap().len() as u64,
            MAX_ENCODED_MESSAGES_REQUEST_BYTES,
            "fixture body must land exactly at the cap"
        );
        let encoded = validate_and_encode_messages_request(&request)
            .expect("a body exactly at the cap must be accepted (N2)");
        assert_eq!(encoded.len() as u64, MAX_ENCODED_MESSAGES_REQUEST_BYTES);
    }

    /// T4: one byte over the encoded cap is rejected with the typed bytes
    /// variant (N2).
    #[test]
    fn body_over_cap_reject() {
        let request = t3_request(1);
        assert_eq!(
            serde_json::to_vec(&request).unwrap().len() as u64,
            MAX_ENCODED_MESSAGES_REQUEST_BYTES + 1,
            "fixture body must land exactly one byte over the cap"
        );
        let err = validate_and_encode_messages_request(&request)
            .expect_err("a body one byte over the cap must be rejected (N2)");
        assert_eq!(
            err,
            RequestValidationError::EncodedBodyTooLarge {
                bytes: MAX_ENCODED_MESSAGES_REQUEST_BYTES + 1
            }
        );
    }

    /// T5: i64-scale writes drive the checked_add counter into overflow
    /// (direct unit) and the counting pass ends with CounterOverflow.
    #[test]
    fn counter_overflow_variant() {
        let mut writer = CountingWriter::default();
        writer.add_bytes(i64::MAX as u64);
        assert!(!writer.overflowed, "2^63-1 fits");
        writer.add_bytes(i64::MAX as u64);
        assert!(!writer.overflowed, "2^64-2 still fits");
        writer.add_bytes(2);
        assert!(writer.overflowed, "2^64 must set the overflow flag");
        assert_eq!(
            finish_counting(&writer),
            Err(RequestValidationError::CounterOverflow)
        );
    }

    /// T6: the reserve step maps an allocation failure to AllocationFailed.
    /// Reachability note (reported): the cap is checked BEFORE the reserve,
    /// so the spec path cannot reach this branch — this pins the branch's
    /// existence and its error mapping.
    #[test]
    fn allocation_failed_variant() {
        let err = reserve_body(usize::MAX as u64)
            .expect_err("a usize::MAX reservation must fail to allocate");
        assert_eq!(
            err,
            RequestValidationError::AllocationFailed {
                bytes: usize::MAX as u64
            }
        );
        let buf = reserve_body(16).expect("a small reservation allocates");
        assert_eq!(buf.len(), 0);
    }

    /// T7: a corrupting second pass (injected via the serializer parameter:
    /// 5 bytes counted, 6 serialized) ends with PassLengthMismatch. Same
    /// reachability note as T6.
    #[test]
    fn pass_length_mismatch_variant() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let err = encode_two_pass(
            |w: &mut dyn std::io::Write| {
                calls.set(calls.get() + 1);
                let payload: &[u8] = if calls.get() == 1 { b"hello" } else { b"hello!" };
                w.write_all(payload)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            },
            MAX_ENCODED_MESSAGES_REQUEST_BYTES,
        );
        assert_eq!(
            err,
            Err(RequestValidationError::PassLengthMismatch {
                counted: 5,
                actual: 6
            })
        );
    }

    /// T8: an item whose compact JSON estimates EXACTLY 10_000 tokens is
    /// accepted (N3 inclusive).
    #[test]
    fn item_at_cap_accept() {
        let overhead = serde_json::to_vec(&text_message(MessageRole::User, "")).unwrap().len()
            as u64;
        let pad = (40_000 - overhead) as usize;
        let message = text_message(MessageRole::User, &"a".repeat(pad));
        assert_eq!(
            est(&serde_json::to_vec(&message).unwrap()),
            MAX_MODEL_CONTEXT_ITEM_TOKENS
        );
        let request = minimal_request(vec![message]);
        let encoded = validate_and_encode_messages_request(&request)
            .expect("an item estimating exactly 10_000 tokens must be accepted (N3 inclusive)");
        assert!(!encoded.is_empty());
    }

    /// T9: an item estimating 10_001 tokens is rejected with the typed
    /// variant (N3).
    #[test]
    fn item_over_cap_reject() {
        let overhead = serde_json::to_vec(&text_message(MessageRole::User, "")).unwrap().len()
            as u64;
        let pad = (40_004 - overhead) as usize; // 40_004 B → est 10_001
        let message = text_message(MessageRole::User, &"a".repeat(pad));
        let estimated = est(&serde_json::to_vec(&message).unwrap());
        assert_eq!(estimated, MAX_MODEL_CONTEXT_ITEM_TOKENS + 1);
        let request = minimal_request(vec![message]);
        let err = validate_and_encode_messages_request(&request)
            .expect_err("an item estimating 10_001 tokens must be rejected (N3)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::MessageItem(0),
                estimated_tokens: MAX_MODEL_CONTEXT_ITEM_TOKENS + 1,
            }
        );
    }

    /// T10: system items — Text over-cap rejects as SystemBlock(0); a single
    /// over-cap TextBlock in Blocks rejects as SystemBlock(0); under-cap
    /// Blocks pass; Text exactly at cap is accepted (N3 inclusive).
    #[test]
    fn system_text_over_cap_reject() {
        // SystemParam::Text projects as a bare JSON string (2 quote bytes).
        let over_pad = (40_004 - 2) as usize; // (len + 2) / 4 = 10_001
        let over = SystemParam::Text("a".repeat(over_pad));
        assert_eq!(
            est(&serde_json::to_vec(&over).unwrap()),
            MAX_MODEL_CONTEXT_ITEM_TOKENS + 1
        );
        let request =
            request_with_system(vec![text_message(MessageRole::User, "hi")], Some(over));
        let err = validate_and_encode_messages_request(&request)
            .expect_err("an over-cap system Text item must be rejected (N3)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::SystemBlock(0),
                estimated_tokens: MAX_MODEL_CONTEXT_ITEM_TOKENS + 1,
            }
        );

        // Blocks: one over-cap TextBlock rejects as SystemBlock(0)
        // ({"type":"text","text":""} overhead = 25 B).
        let block_over = TextBlock {
            r#type: "text".to_string(),
            text: "a".repeat(40_004 - 25),
            cache_control: None,
        };
        let request = request_with_system(
            vec![text_message(MessageRole::User, "hi")],
            Some(SystemParam::Blocks(vec![block_over])),
        );
        let err = validate_and_encode_messages_request(&request)
            .expect_err("an over-cap TextBlock must be rejected (N3)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::SystemBlock(0),
                estimated_tokens: MAX_MODEL_CONTEXT_ITEM_TOKENS + 1,
            }
        );

        // Under-cap Blocks each pass.
        let under = |text: &str| TextBlock {
            r#type: "text".to_string(),
            text: text.to_string(),
            cache_control: None,
        };
        let request = request_with_system(
            vec![text_message(MessageRole::User, "hi")],
            Some(SystemParam::Blocks(vec![
                under(&"a".repeat(10_000)),
                under(&"b".repeat(10_000)),
            ])),
        );
        validate_and_encode_messages_request(&request)
            .expect("under-cap system Blocks must pass (N3)");

        // Text exactly at the cap is accepted (inclusive).
        let at_cap = SystemParam::Text("a".repeat(40_000 - 2));
        assert_eq!(
            est(&serde_json::to_vec(&at_cap).unwrap()),
            MAX_MODEL_CONTEXT_ITEM_TOKENS
        );
        let request =
            request_with_system(vec![text_message(MessageRole::User, "hi")], Some(at_cap));
        validate_and_encode_messages_request(&request)
            .expect("a system Text estimating exactly 10_000 must be accepted (N3 inclusive)");
    }

    /// T11: one cap item per complete ToolParam — over-cap rejects as
    /// Tool(0), under-cap passes. The input schema is a byte-subset of the
    /// whole definition and the estimator is monotone in byte count, so
    /// validating the complete definition subsumes the schema-alone row
    /// (recorded mapping decision).
    #[test]
    fn tool_definition_over_cap_reject() {
        let schema = serde_json::json!({ "type": "object", "properties": {} });
        let base = ToolParam {
            name: "t".to_string(),
            description: Some(String::new()),
            input_schema: schema.clone(),
        };
        let base_len = serde_json::to_vec(&base).unwrap().len() as u64;
        let over = ToolParam {
            name: "t".to_string(),
            description: Some("a".repeat((40_004 - base_len) as usize)),
            input_schema: schema.clone(),
        };
        let estimated = est(&serde_json::to_vec(&over).unwrap());
        assert_eq!(estimated, MAX_MODEL_CONTEXT_ITEM_TOKENS + 1);
        let request =
            request_with_tools(vec![text_message(MessageRole::User, "hi")], vec![over]);
        let err = validate_and_encode_messages_request(&request)
            .expect_err("an over-cap tool definition must be rejected (N3)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::Tool(0),
                estimated_tokens: MAX_MODEL_CONTEXT_ITEM_TOKENS + 1,
            }
        );

        // Under-cap ToolParam passes.
        let under = ToolParam {
            name: "t".to_string(),
            description: Some("a small tool".to_string()),
            input_schema: schema,
        };
        let request =
            request_with_tools(vec![text_message(MessageRole::User, "hi")], vec![under]);
        validate_and_encode_messages_request(&request)
            .expect("an under-cap tool definition must pass (N3)");
    }

    /// T12: max_tokens 0 and u32::MAX are both accepted (N4) — the validator
    /// carries no profile/policy parameter (compile-level guarantee).
    #[test]
    fn max_tokens_zero_and_max_accept() {
        for max_tokens in [0u32, u32::MAX] {
            let mut request = minimal_request(vec![text_message(MessageRole::User, "hi")]);
            request.max_tokens = max_tokens;
            let encoded = validate_and_encode_messages_request(&request)
                .expect(&format!("max_tokens {max_tokens} must be accepted (N4)"));
            assert!(!encoded.is_empty());
        }
    }

    /// T13 GOLDEN: the carrier is byte-for-byte identical to the old
    /// `.json()` path (`serde_json::to_vec(&request.inner)`) for every
    /// previously-valid request. Two-run stability is proven by the driver
    /// (REQVALID_GOLDEN_OUT set to a distinct path per run, outputs diffed).
    #[test]
    fn encoded_bytes_wire_identity() {
        let fixtures = vec![
            // 1: simple user/assistant.
            minimal_request(vec![
                text_message(MessageRole::User, "hello there"),
                text_message(MessageRole::Assistant, "hi, how can I help?"),
            ]),
            // 2: system Blocks + tools + every control field.
            rich_control_request(),
            // 3: largest realistic shape: builder output over a
            // fixture_f-style conversation (existing sampling-types fixture).
            build_messages_request(&realistic_conversation_fixture()),
        ];
        let mut golden = Vec::new();
        for (index, request) in fixtures.iter().enumerate() {
            let encoded = validate_and_encode_messages_request(request)
                .expect(&format!("fixture {index} must validate"));
            let legacy = serde_json::to_vec(request).expect("legacy serialization");
            assert_eq!(
                encoded.as_bytes(),
                legacy.as_slice(),
                "fixture {index} wire bytes must be identical to the legacy .json() path"
            );
            golden.extend_from_slice(&legacy);
            golden.push(b'\n');
        }
        if let Some(out) = std::env::var("REQVALID_GOLDEN_OUT").ok() {
            std::fs::write(&out, &golden).expect("golden write");
        }
    }

    /// T14a: every variant is non-retryable at the predicate layer, matches
    /// no 400-gated/status family, and its Display phrasing matches no
    /// retryable classifier text family. T14b (the retry actor's actual
    /// `classify_error`) lives in the sampler's retry.rs test module.
    #[test]
    fn error_is_non_retryable() {
        for err in all_variants() {
            let sampling = SamplingError::RequestValidation(err.clone());
            assert!(!sampling.is_retryable(), "{err:?} must be non-retryable");
            // The 400-gated and status-carrying families are unreachable for
            // a local pre-HTTP error (it carries no status at all).
            assert!(!sampling.is_model_bound_history_error());
            assert!(!sampling.is_payload_too_large());
            assert!(!sampling.is_byte_size_overflow_coded());
            assert!(!sampling.is_deterministic_in_stream_error());
            let display = sampling.to_string();
            assert!(
                display_hits_retryable_family(&display).is_none(),
                "Display matched a retryable family: {display}"
            );
        }
    }

    /// T15: adjacent same-role fragments each under-cap PRE-coalesce become
    /// ONE over-cap coalesced item — the validator rejects the final form
    /// (N3 "Validation uses the final projected form").
    #[test]
    fn coalesced_items_validated_as_final_form() {
        let fragment = 20_000usize;
        let conv = ConversationRequest {
            items: vec![
                ConversationItem::user(&"a".repeat(fragment)),
                ConversationItem::user(&"b".repeat(fragment)),
            ],
            model: Some("claude-sonnet-5".to_string()),
            max_output_tokens: Some(1),
            ..Default::default()
        };
        let request = build_messages_request(&conv);
        assert_eq!(
            request.messages.len(),
            1,
            "adjacent same-role fragments must coalesce into one item"
        );
        // Each PRE-coalesce fragment is under-cap on its own.
        for text in ["a".repeat(fragment), "b".repeat(fragment)] {
            let solo_item = serde_json::to_vec(&Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(vec![ContentBlock::Text {
                    text,
                    cache_control: None,
                }]),
            })
            .unwrap();
            assert!(
                est(&solo_item) < MAX_MODEL_CONTEXT_ITEM_TOKENS,
                "pre-coalesce fragment must be under-cap"
            );
        }
        let coalesced_est = est(&serde_json::to_vec(&request.messages[0]).unwrap());
        assert!(
            coalesced_est > MAX_MODEL_CONTEXT_ITEM_TOKENS,
            "coalesced item must be over-cap: {coalesced_est}"
        );
        let err = validate_and_encode_messages_request(&request)
            .expect_err("the over-cap COALESCED item must be rejected (N3 final form)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::MessageItem(0),
                estimated_tokens: coalesced_est,
            }
        );
    }

    // ------------------------------------------------------------------
    // M-1 fix tests (REQVALID-1 47a-FIX, ruling R-1): Base64 image parts are
    // flat-priced at IMAGE_TOKEN_ESTIMATE (765) instead of bytes/4 over the
    // raw payload; text-only estimation stays byte-identical.
    // ------------------------------------------------------------------

    /// R-1 fixture: the fixed JSON frame (bytes) of a user message whose
    /// content is [Text(empty), Image(base64 with empty data)] — everything
    /// the text pad and the base64 payload are added on top of.
    fn image_message_overhead() -> u64 {
        let msg = Message {
            role: MessageRole::User,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: String::new(),
                    cache_control: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: String::new(),
                    },
                    cache_control: None,
                },
            ]),
        };
        serde_json::to_vec(&msg).unwrap().len() as u64
    }

    /// M-1 (RED-3): one message, one Base64 image (40_960 chars of payload),
    /// plus a small text part. Pre-fix the ENTIRE compact JSON is priced at
    /// bytes/4 (≈10_270 est) ⇒ hard-rejected pre-HTTP — the regression (pre-cut
    /// these requests were sent and recovered via 413/image-strip). Post-fix
    /// the image is flat-priced at IMAGE_TOKEN_ESTIMATE ⇒ the item passes N3.
    #[test]
    fn t_img_red_base64_image_passes_n3() {
        let message = Message {
            role: MessageRole::User,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "hi".to_string(),
                    cache_control: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "A".repeat(40_960),
                    },
                    cache_control: None,
                },
            ]),
        };
        // Pre-fix this panics with
        // ItemTokenLimitExceeded { item: MessageItem(0), estimated_tokens: ~10_270 }
        // — the 40_960-char payload priced at bytes/4 instead of the flat
        // IMAGE_TOKEN_ESTIMATE.
        let encoded = validate_and_encode_messages_request(&minimal_request(vec![message]))
            .expect("a Base64 image must be flat-priced at IMAGE_TOKEN_ESTIMATE (R-1), not bytes/4");
        assert!(!encoded.is_empty());
    }

    /// M-1 (R-1): image (40 KB base64) + a text part whose non-image JSON is
    /// exactly 36_000 B (9_000 tokens) ⇒ 9_000 + 765 = 9_765 ≤ 10_000 ⇒
    /// accepted post-fix. Pre-fix: ≈19_240 est ⇒ rejected.
    #[test]
    fn t_img_mixed_large_image_under_cap_total() {
        let overhead = image_message_overhead();
        let pad = (36_000 - overhead) as usize;
        let message = Message {
            role: MessageRole::User,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "a".repeat(pad),
                    cache_control: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "A".repeat(40_960),
                    },
                    cache_control: None,
                },
            ]),
        };
        // The non-image portion of the compact JSON is exactly 36_000 B.
        let item_json = serde_json::to_vec(&message).unwrap();
        assert_eq!(item_json.len() as u64 - 40_960, 36_000);
        let encoded = validate_and_encode_messages_request(&minimal_request(vec![message]))
            .expect("9_000 (text) + 765 (flat image) = 9_765 <= 10_000 must pass (R-1)");
        assert!(!encoded.is_empty());
    }

    /// M-1 (R-1): the flat-priced image still COUNTS against the cap — guards
    /// against an "images free" over-correction. Text 39_000 B (9_750) + one
    /// image (765) = 10_515 > 10_000 ⇒ ItemTokenLimitExceeded. The payload is
    /// deliberately a different size than the under-cap test (8_192 chars) to
    /// pin size-independence of the flat price.
    #[test]
    fn t_img_priced_image_counts_against_cap() {
        let overhead = image_message_overhead();
        let pad = (39_000 - overhead) as usize;
        let message = Message {
            role: MessageRole::User,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "a".repeat(pad),
                    cache_control: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".to_string(),
                        data: "A".repeat(8_192),
                    },
                    cache_control: None,
                },
            ]),
        };
        let item_json = serde_json::to_vec(&message).unwrap();
        assert_eq!(item_json.len() as u64 - 8_192, 39_000);
        let err = validate_and_encode_messages_request(&minimal_request(vec![message]))
            .expect_err("9_750 (text) + 765 (flat image) = 10_515 > 10_000 must be rejected (R-1)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::MessageItem(0),
                estimated_tokens: 9_750 + IMAGE_TOKEN_ESTIMATE,
            }
        );
    }

    /// M-1 regression pin: a text-only item over the cap is rejected exactly
    /// as pre-fix (bytes/4 unchanged for image-less items).
    #[test]
    fn t_img_text_only_over_cap_still_rejected() {
        let overhead = serde_json::to_vec(&text_message(MessageRole::User, "")).unwrap().len()
            as u64;
        let pad = (40_004 - overhead) as usize; // 40_004 B → est 10_001
        let message = text_message(MessageRole::User, &"a".repeat(pad));
        let request = minimal_request(vec![message]);
        let err = validate_and_encode_messages_request(&request)
            .expect_err("a text-only over-cap item must stay rejected (regression pin)");
        assert_eq!(
            err,
            RequestValidationError::ItemTokenLimitExceeded {
                item: ItemRef::MessageItem(0),
                estimated_tokens: MAX_MODEL_CONTEXT_ITEM_TOKENS + 1,
            }
        );
    }

    /// M-1 (R-1 Url clause): `ImageSource::Url` parts are NOT flat-priced —
    /// they stay at the plain JSON/4 estimate. Pinned at both the inclusive
    /// boundary (40_000 B ⇒ 10_000 accepted) and the over-cap boundary
    /// (40_004 B ⇒ 10_001 rejected).
    #[test]
    fn t_img_url_image_estimated_at_plain_json_quarter() {
        fn url_message(url: String) -> Message {
            Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(vec![ContentBlock::Image {
                    source: ImageSource::Url { url },
                    cache_control: None,
                }]),
            }
        }
        let overhead = serde_json::to_vec(&url_message(String::new())).unwrap().len() as u64;
        for (target, expect_ok) in [(40_000u64, true), (40_004u64, false)] {
            let message = url_message("u".repeat((target - overhead) as usize));
            assert_eq!(
                est(&serde_json::to_vec(&message).unwrap()),
                target / 4,
                "a Url image must be estimated at plain JSON/4 (R-1 Url clause)"
            );
            let result =
                validate_and_encode_messages_request(&minimal_request(vec![message]));
            assert_eq!(result.is_ok(), expect_ok, "target {target} B");
        }
    }
}
