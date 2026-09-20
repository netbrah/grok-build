//! TODO: Move from xai-grok-shell/src/sampling/error.rs

use std::fmt;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use xai_circuit_breaker::RetryPolicy;

use crate::provider_error::{parse_provider_error, parse_provider_error_str};
use crate::request_validation::RequestValidationError;

pub type Result<T> = std::result::Result<T, SamplingError>;

/// Why the model's response was classified as "empty" by [`ConversationResponse::empty_reason`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::AsRefStr, strum::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum EmptyReason {
    /// The model emitted reasoning tokens but produced no visible content and no tool calls.
    /// The stream completed normally (has `finish_reason`).
    ReasoningOnly,
    /// The stream carried at least one `choice` but the final assistant message has empty `content` and no tool calls (and no reasoning).
    NoVisibleContent,
}
impl fmt::Display for EmptyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_ref())
    }
}

/// Structured context captured at L2 stream completion time when the response is classified as empty.
/// Carries everything needed to root-cause the issue from a single log line or error payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyResponseContext {
    pub reason: EmptyReason,
    /// Whether the response contained reasoning tokens.
    pub had_reasoning: bool,
    /// Byte length of the accumulated `content` string (0 for truly empty).
    pub content_len: usize,
    /// Number of tool calls in the final response.
    pub tool_call_count: usize,
    /// The `finish_reason` from the stream, if any.
    pub finish_reason: Option<String>,
    /// Token usage from the response (when available).
    pub completion_tokens: Option<u32>,
    pub reasoning_tokens: Option<u32>,
    pub prompt_tokens: Option<u32>,
    /// Model that produced the response.
    pub model: String,
    /// Whether at least one `choice` was seen in the stream.
    pub first_choice_seen: bool,
}

impl EmptyResponseContext {
    pub fn finish_reason_str(&self) -> &str {
        self.finish_reason.as_deref().unwrap_or("none")
    }
}

/// Model metadata from response headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseModelMetadata {
    pub context_window: Option<u64>,
    pub max_completion_tokens: Option<u32>,
    /// `x-models-etag`: triggers model catalog refresh when changed.
    pub models_etag: Option<String>,
}

/// Wire-credential provenance of a request that failed authentication. A 401 for a request that went out with no
/// credential header is not evidence against the credential itself. Such a send is fail-closed: the bearer resolver had
/// nothing wire-valid. Retry policies use this to avoid charging credential-rejection budgets for such sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SentCredential {
    /// The request carried a credential; the server rejected it.
    Sent,
    /// The request went out with no credential header.
    Missing,
    /// Provenance unknown (synthesized or legacy errors).
    /// Retry policies treat this like [`SentCredential::Sent`]: fail closed toward terminating rather than retrying forever.
    #[default]
    Unknown,
}

/// Hand-written so an unrecognized value from a newer peer degrades to `Unknown` instead of failing the whole containing payload.
/// `#[serde(other)]` is not available on externally-tagged enums.
impl<'de> Deserialize<'de> for SentCredential {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(
            match std::borrow::Cow::<str>::deserialize(deserializer)?.as_ref() {
                "sent" => Self::Sent,
                "missing" => Self::Missing,
                _ => Self::Unknown,
            },
        )
    }
}

impl SentCredential {
    /// Classify from the credential fragment captured when the request was built (`None` means no credential header was stamped on the wire).
    pub fn from_sent_fragment(fragment: Option<&str>) -> Self {
        if fragment.is_some() {
            Self::Sent
        } else {
            Self::Missing
        }
    }

    pub fn is_missing(self) -> bool {
        matches!(self, Self::Missing)
    }

    /// By reference so it can serve as a serde `skip_serializing_if`.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Display prefix of [`SamplingError::Serialization`].
/// Shared with the variant's `#[error(...)]` template so [`SamplingError::serialization_from_rendered`] can never drift from what Display emits.
const SERIALIZATION_DISPLAY_PREFIX: &str = "serialization error: ";

/// Display text of [`SamplingError::MaxTokensTruncation`].
/// Public: the pager sniffs it to recover the kind from rails predating the typed `errorKind` field.
/// Sharing the const with the `#[error(...)]` template prevents drift.
pub const MAX_TOKENS_TRUNCATION_MESSAGE: &str = "response truncated by max_tokens";

#[derive(Debug, Error)]
pub enum SamplingError {
    #[error("{message}")]
    Auth {
        message: String,
        /// Whether the rejected request carried a credential.
        credential: SentCredential,
    },
    #[error("invalid client configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// The model's mTLS endpoint or local client identity cannot be used safely.
    #[error("invalid mTLS client configuration: {0}")]
    MtlsConfiguration(String),
    #[error("request error: {0}")]
    Http(reqwest::Error),
    #[error("{prefix}{0}", prefix = SERIALIZATION_DISPLAY_PREFIX)]
    Serialization(serde_json::Error),
    #[error("API error (status {status}): {message}")]
    Api {
        status: StatusCode,
        message: String,
        model_metadata: Option<ResponseModelMetadata>,
        /// Parsed from the `Retry-After` response header (seconds).
        retry_after_secs: Option<u64>,
        /// Parsed from the `x-should-retry` response header. `Some(true)`: transient, retry may help. `Some(false)`:
        /// request-content error, don't retry. `None`: header absent (old server or non-proxy origin).
        should_retry: Option<bool>,
        /// The error envelope's `code` slot; `None` when the body has no envelope or carries no code.
        /// Dedicated code slots (nested envelopes, Responses-stream error events) pass through verbatim.
        /// The flat envelope's `code` slot is overloaded, so only semantic values surface from it.
        error_code: Option<ApiErrorCode>,
    },
    #[error("reqwest error stream: {0}")]
    EventStreamError(String),
    /// Server-side stream error (sent as JSON within the SSE stream)
    #[error("stream error ({error_type}): {message}")]
    StreamError {
        error_type: String,
        message: String,
        /// The stream error envelope's `code` slot, when present.
        code: Option<ApiErrorCode>,
    },
    /// Per-chunk idle timeout: no SSE chunk received from the model within the configured deadline.
    /// NOT retryable: the model (or network path) is stuck, and replaying the same request would likely stall again.
    #[error("inference idle timeout after {elapsed_secs}s with no chunks")]
    IdleTimeout { elapsed_secs: u64 },
    #[error("empty response from model ({})", context.reason)]
    EmptyResponse { context: EmptyResponseContext },
    #[error("{text}", text = MAX_TOKENS_TRUNCATION_MESSAGE)]
    MaxTokensTruncation,
    /// A confident server-reported doom loop on the attempt (mid-stream or on the completed response). Carries the raw
    /// trigger labels (never generation content) and, for telemetry only, the stream chunk index the mid-stream abort fired
    /// at. `aborted_at_chunk` is `None` when the signal was only seen on the completed response.
    #[error("doom loop detected: {}", triggers.join(", "))]
    DoomLoopDetected {
        triggers: Vec<String>,
        aborted_at_chunk: Option<u64>,
    },
    /// Local pre-HTTP request validation failure (REQVALID-1 47a): the
    /// request violates a hard local cap (message count, per-item token
    /// estimate, encoded body bytes) or a two-pass encode invariant
    /// (frozen spec N1/N2/N3). Never sent to the wire; non-retryable by
    /// construction — re-encoding the same request cannot change the
    /// outcome (the retry classifier falls through to its terminal Fatal arm).
    #[error("request validation failed: {0}")]
    RequestValidation(RequestValidationError),
    /// The model requested an unsupported stop control (the messages wire
    /// `pause_turn`, apex-ayl.49 item 2). A dedicated typed terminal error: the
    /// failure terminates the stream before any outcome / projection / persistence
    /// exists, so the turn is never committed (no durable item or call).
    /// Non-retryable by construction — re-sending the same payload cannot change the
    /// control the model requested (the retry classifier falls through to its
    /// terminal Fatal arm, the 47a house pattern).
    /// Provenance: frozen-spec@2cbc222c §6.3 L4281.
    #[error("the model requested the unsupported control `{wire_reason}`; the turn was not committed")]
    UnsupportedStopControl { wire_reason: String },
}

/// Semantic `error.code` the server stamps on invalid-image rejections, on both non-stream error bodies and mid-stream SSE error events.
pub const INVALID_IMAGE_ERROR_CODE: &str = "invalid_image";

/// Content path some upstream providers key codeless image rejections on (`.image.source.base64.data`/`.url`). Those
/// arrive as `invalid_request_error` with no `error.code`, so [`INVALID_IMAGE_ERROR_CODE`] misses them. The fragment
/// appears only when the request carried an image, so stripping is safe recovery.
const IMAGE_CONTENT_PATH_MARKER: &str = ".image.source.";

/// Size-error decision map for callers choosing a remedy: 413 status or byte-size code: strip inline images and retry
/// once. Detected by [`SamplingError::is_payload_too_large`] and [`SamplingError::is_byte_size_overflow_coded`];
/// Token-tier code or token/size text: fail fast via [`SamplingError::is_retry_vetoed`].
pub fn is_size_overflow_error_code(code: &str) -> bool {
    is_byte_size_overflow_error_code(code)
        // Token-tier slugs: size overflows image stripping cannot remedy.
        || code.eq_ignore_ascii_case("exceed_context_size_error")
        || code.eq_ignore_ascii_case("context_length_exceeded")
}

/// 413-style subset of [`is_size_overflow_error_code`]: byte-or-count caps where stripping inline images may shrink the request under the limit.
/// Token-tier codes are excluded: images barely move token counts.
fn is_byte_size_overflow_error_code(code: &str) -> bool {
    code.parse::<u16>() == Ok(StatusCode::PAYLOAD_TOO_LARGE.as_u16())
        || code.eq_ignore_ascii_case("payload_too_large")
        || code.eq_ignore_ascii_case("request_too_large")
}

/// A wire `error.code`, parsed once at the boundary so classification compares variants instead of strings.
/// `#[non_exhaustive]`: the next semantic code is a new variant, not another const and `||` chain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApiErrorCode {
    /// The server rejected an image ([`INVALID_IMAGE_ERROR_CODE`]).
    InvalidImage,
    /// A size-overflow code ([`is_size_overflow_error_code`]).
    /// Carries the verbatim wire code so serialization stays byte-identical.
    ContextOverflow(String),
    /// Any other wire code, preserved verbatim (Responses-stream error events pass arbitrary codes through).
    Other(String),
}

impl ApiErrorCode {
    pub fn parse(code: &str) -> Self {
        match code {
            INVALID_IMAGE_ERROR_CODE => Self::InvalidImage,
            c if is_size_overflow_error_code(c) => Self::ContextOverflow(c.to_string()),
            _ => Self::Other(code.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::InvalidImage => INVALID_IMAGE_ERROR_CODE,
            Self::ContextOverflow(code) | Self::Other(code) => code,
        }
    }

    /// `true` for size-overflow codes; see [`is_size_overflow_error_code`].
    pub fn is_size_overflow(&self) -> bool {
        matches!(self, Self::ContextOverflow(_))
    }

    /// `true` for the byte-size subset of size-overflow codes; see `is_byte_size_overflow_error_code`.
    pub fn is_byte_size_overflow(&self) -> bool {
        matches!(self, Self::ContextOverflow(code) if is_byte_size_overflow_error_code(code))
    }
}

/// Serializes as the plain wire string, so `Option<ApiErrorCode>` fields are byte-identical on the wire to the `Option<String>` they replaced.
impl Serialize for ApiErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ApiErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

/// Deterministic in-stream failure text: the upstream (typically a proxy such as
/// LiteLLM) reported the failure inside the SSE stream, and re-sending the same
/// payload cannot change the outcome — an opaque in-stream error or an
/// over-capacity condition.
///
/// Size / context-length text and size-coded errors are deliberately excluded:
/// those already classify deterministic via [`SamplingError::is_context_length_error`],
/// and hosts give them a specific recovery (image strip / input ladder) instead
/// of a plain stop.
pub fn is_deterministic_in_stream_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("response api in-stream error") || m.contains("over capacity")
}

impl SamplingError {
    /// Auth error of unknown wire provenance.
    /// Used by paths that never sent a request (config validation, cancellation, actor teardown) or that lost the provenance (legacy round trips).
    pub fn auth_unknown(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
            credential: SentCredential::Unknown,
        }
    }

    /// Display plus the hidden source() chain.
    ///
    /// reqwest Error Display hides DNS/connect causes on source().
    pub fn detail_with_causes(&self) -> String {
        match self {
            Self::Http(err) => {
                let mut msg = self.to_string();
                let mut source = std::error::Error::source(err);
                while let Some(cause) = source {
                    msg.push_str(": ");
                    msg.push_str(&cause.to_string());
                    source = cause.source();
                }
                msg
            }
            _ => self.to_string(),
        }
    }

    /// Rebuild a `Serialization` error from a rendered message for non-`Clone` contexts; it must stay `Serialization` so it remains non-retryable.
    pub fn serialization_message(msg: impl fmt::Display) -> Self {
        Self::Serialization(serde::de::Error::custom(msg))
    }

    /// Rebuild from this variant's full rendered Display (e.g. a round-tripped `SamplingErrorInfo` message).
    /// Strips the Display prefix so the rebuilt error does not render it twice.
    pub fn serialization_from_rendered(rendered: &str) -> Self {
        Self::serialization_message(
            rendered
                .strip_prefix(SERIALIZATION_DISPLAY_PREFIX)
                .unwrap_or(rendered),
        )
    }

    pub fn is_auth_error(&self) -> bool {
        // Only 401 Unauthorized means the credentials themselves were rejected and warrant a token refresh / re-auth 403
        // Forbidden means the request was authenticated but the action is not permitted. That covers content-safety blocks,
        // ZDR-blocked operations, and other policy denials unrelated to credentials.
        matches!(
            self,
            SamplingError::Auth { .. }
                | SamplingError::Api {
                    status: StatusCode::UNAUTHORIZED,
                    ..
                }
        )
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(
            self,
            SamplingError::Api {
                status: StatusCode::TOO_MANY_REQUESTS,
                ..
            }
        )
    }

    pub fn is_payload_too_large(&self) -> bool {
        matches!(
            self,
            SamplingError::Api {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                ..
            }
        )
    }

    /// `true` when the error looks like a connection reset or broken pipe during request upload.
    /// That is the pattern nginx produces when it rejects an oversized payload by closing the connection instead of responding 413.
    /// Timeouts and connect failures are excluded: those are unrelated to payload size and stripping images on them would lose context for no reason.
    pub fn is_likely_body_rejected(&self) -> bool {
        match self {
            SamplingError::Http(err) => {
                // `is_request()` covers broken-pipe / connection-reset during body upload
                // `is_body()` covers stream-write failures
                // Timeouts and connect errors are excluded: those are unrelated
                (err.is_request() || err.is_body()) && !err.is_timeout() && !err.is_connect()
            }
            _ => false,
        }
    }

    /// Provenance: hyper-grok-build@45e984f3 packages/ai/xai-grok-sampling-types/src/error.rs:401 :: is_model_bound_history_error (adapted; family 5 is a this-stack addition — ledger §CROSSWIRE-1)
    /// The provider rejected opaque continuation state carried by conversation
    /// history — a model-bound replay that a *different* model (or deployment)
    /// cannot consume. The sampler may safely retry ONCE after removing only
    /// the model-bound state; the portable transcript content stays intact
    /// (see `ConversationRequest::strip_model_bound_state`). Matched on the
    /// 400 error text (family 1 additionally on 503, see the 503 arm below),
    /// case-insensitive, across five observed phrasings:
    ///
    /// 1. Responses `encrypted_content` — both the parameterized field name
    ///    (`Missing required parameter: 'input[2].encrypted_content'`) and
    ///    Azure's human-facing text ("The encrypted content for item ... could
    ///    not be verified"). This supersedes the legacy
    ///    `is_encrypted_content_error` detector, which matched only this family.
    /// 2. Anthropic-style thinking signatures (`thinking` + `signature`).
    /// 3. Responses input item-id schema rejection (`input[` + `.id` + `invalid`).
    /// 4. A rejected item id that was not persisted (`item` + `id` +
    ///    `not found`/`does not exist`) — the `store=false` endpoint shape.
    /// 5. (This-stack adaptation) Azure rejecting a verbatim reasoning item's
    ///    `content` array under the strict input schema (maxItems:0):
    ///    `input[N].content` + "array too long" (the `code` field is
    ///    `array_above_max_length`). Observed live on the `store=false`
    ///    endpoint (XREPLAY-1 map, point 1). The upstream reference's family 3
    ///    targets `.id` and does not cover this shape, so a verbatim port would
    ///    misclassify this rejection as Fatal.
    ///
    /// 6. (AFFINITY-POLICY-1, apex-ayl.75) The gateway's 401 tags-config
    ///    fail-fast: `Not allowed to access model due to tags configuration.
    ///    Passed model=<m> and tags=[...]` — litellm 1.93.0
    ///    `EncryptedContentAffinityCheck` rejecting a marker-origin
    ///    deployment whose tag set excludes the request tag (ws8 probe D;
    ///    wire evidence testdata/affinity/probe-d-401-body.json). Message-
    ///    keyed (the verbatim prefix, case-insensitive) on BOTH shapes: the
    ///    `Auth` variant (the harness client maps EVERY 401 to Auth, so the
    ///    live wire shape is `Auth { message: "Unauthorized (401) ...:
    ///    <this text>" }`) and the `Api` 401 shape (defensive symmetry with
    ///    the 503 exception; no current client site emits it). A genuine
    ///    credential 401 can never carry this phrasing, so the status alone
    ///    never widens the class — key-refresh flows keep the auth-gate
    ///    terminal path.
    ///
    /// 7. (XW-ORPHAN-1 class (b), apex-ayl.74; PREDICTION — qwen preplan
    ///    H-1, no live capture) the Azure `.call_id` orphan rejection: a
    ///    dangling `function_call_output` whose call id is not in the input
    ///    (`Invalid 'input[N].call_id': ...`). Family 3 needs all of
    ///    `input[` + `.id` + `invalid`, but `.call_id` does not contain the
    ///    substring `.id` (the char before `id` is `_`), so F3 misses; this
    ///    arm is the 3-key F3 mirror (`input[` + `.call_id` + `invalid`).
    ///    All three needles sit in the first ~35 chars — trivially cap-safe.
    ///
    /// 8. (XW-ORPHAN-1 class (a), apex-ayl.74; LIVE — glm cell 2026-09-17,
    ///    verbatim fragment sha256:1f591ef070d9) the vLLM pydantic dotted-id
    ///    validation rendering (litellm Responses→ChatCompletions shim):
    ///    `N validation errors for ChatCompletionRequest` with dotted
    ///    `messages.N...` locs — no brackets, no "invalid", so F3/F4/F5 all
    ///    miss. Double-keyed per the F1-503 discipline (`validation error`
    ///    + `messages.`|`input.`) so `validation error` alone (a generic
    ///    phrasing) or `messages.` alone (a field mention) never classifies.
    ///    `validation error` @~65 / `messages.` @~105 of the litellm-prefixed
    ///    view — both inside the 280 cap (pin asserts the capped shape).
    ///
    /// 9. (COMPACT-BOUNDARM-1, apex-ayl.82; LIVE — incident 01a09be2
    ///    lineage, the compact-400 storm) the proxy's rejection of the
    ///    remote-compaction-v2 request's trailing `compaction_trigger` input
    ///    item: `Unsupported Responses API input item type:
    ///    "compaction_trigger"`. Double-keyed (`unsupported` +
    ///    `compaction_trigger`) so ordinary compaction chatter in error text
    ///    never classifies; both needles are inside the first ~66 chars —
    ///    cap-safe. The v2 retry path additionally consults
    ///    [`Self::is_compaction_trigger_item_rejection`] to re-issue WITHOUT
    ///    the trigger item.
    ///
    /// Pipeline note (CROSSWIRE-1 GREEN-run defect): the classifier sees the
    /// message built by [`user_facing_api_error_message`], which caps the text
    /// at [`MAX_USER_ERROR_BODY_CHARS`] (280). On the live wire shape that cut
    /// lands just before the inner escaped `code` field, so the code-based
    /// needle alone never matches; the Azure message text is the primary
    /// needle and the code is kept as an OR for shapes that survive the cap.
    ///
    /// Status-gated to exactly 400, with one 503 exception: the same text on a
    /// 429/5xx is a different class (rate-limit / transient) and must not
    /// trigger the destructive strip — except family 1 on 503, where litellm's
    /// EncryptedContentAffinityCheck rejects the replayed `encitem_` ciphertext
    /// pointers (double-keyed arm below, so generic overload 503s keep the
    /// plain backoff retry path), and except family 6 on 401, where the same
    /// affinity check fails fast on tag exclusion (message-keyed arm, so
    /// genuine credential 401s keep the auth-gate terminal path).
    pub fn is_model_bound_history_error(&self) -> bool {
        // Family 6 (AFFINITY-POLICY-1, apex-ayl.75): the gateway 401
        // tags-config fail-fast. The client maps every 401 to Auth, so the
        // live wire shape is the Auth variant with the verbatim gateway
        // message wrapped by the user-facing builder; key on the message
        // prefix, never on the status alone.
        if let SamplingError::Auth { message, .. } = self {
            let normalized = message.to_ascii_lowercase();
            return normalized.contains("not allowed to access model due to tags");
        }
        let SamplingError::Api { status, message, .. } = self else {
            return false;
        };
        let normalized = message.to_ascii_lowercase();
        // Family 1: opaque encrypted continuation content (field name or Azure phrasing).
        let encrypted = normalized.contains("encrypted_content")
            || normalized.contains("encrypted content");
        if *status == StatusCode::SERVICE_UNAVAILABLE {
            // XSWITCH-1 (apex-ayl.58): litellm
            // `router_utils/pre_call_checks/encrypted_content_affinity_check.py`
            // decodes the `encitem_`/`litellm_enc:` marker on the next request
            // and pins the request to the deployment that minted it; a cooled
            // down / cross-group deployment answers 503 with this phrasing
            // (wire evidence:
            // ~/.grok/dogfood/20260916T191948Z/wire/resp-003.jsonl).
            // Double-keyed (encrypted + unavailable/boundary) so a plain
            // deployment 503 keeps the generic backoff retry path.
            return encrypted
                && (normalized.contains("unavailable")
                    || normalized.contains("boundary"));
        }
        // Family 6, Api shape (defensive; symmetric to the 503 exception): a
        // wire shape that maps the tags-config 401 to Api instead of Auth
        // classifies on the same message needle, so the class never depends
        // on the variant. No current client site emits this shape.
        if *status == StatusCode::UNAUTHORIZED {
            return normalized.contains("not allowed to access model due to tags");
        }
        if *status != StatusCode::BAD_REQUEST {
            return false;
        }
        // Family 2: Anthropic-style thinking signatures.
        let thinking_signature =
            normalized.contains("thinking") && normalized.contains("signature");
        // Family 3: Responses input item-id schema rejection.
        let input_id_invalid = normalized
            .contains("input[")
            && normalized.contains(".id")
            && normalized.contains("invalid");
        // Family 4: rejected item id (not persisted under store=false).
        let item_id_missing = normalized.contains("item")
            && normalized.contains("id")
            && (normalized.contains("not found")
                || normalized.contains("does not exist"));
        // Family 5 (this-stack adaptation): the strict-schema content-array
        // rejection. See the pipeline note on `is_model_bound_history_error`:
        // match the surviving Azure message text, with the wire code as an OR
        // for shapes where it survives the 280-char cap.
        // XW-ORPHAN-1 (apex-ayl.74): the F5 extension — the dotted rendering
        // (`input.N.content: array too long ...`) misses the bracketed key;
        // `input[` | `input.` keeps the bracketed live-wire pin (regression
        // pinned) and adds the dotted shape. `input.` is a dotted loc, not a
        // generic word — combined with the array-too-long key it stays narrow.
        let azure_content_rejection =
            (normalized.contains("input[") || normalized.contains("input."))
                && (normalized.contains("array too long")
                    || normalized.contains("array_above_max_length"));
        // Family 7 (XW-ORPHAN-1 class (b), apex-ayl.74; PREDICTION — qwen
        // preplan H-1): the Azure `.call_id` orphan. 3-key mirror of F3:
        // `.call_id` does not contain `.id`, so F3 can never key this shape.
        let input_callid_invalid = normalized
            .contains("input[")
            && normalized.contains(".call_id")
            && normalized.contains("invalid");
        // Family 8 (XW-ORPHAN-1 class (a), apex-ayl.74; LIVE — glm cell
        // 2026-09-17, fragment sha256:1f591ef070d9): the vLLM pydantic
        // dotted-id validation rendering. Double-keyed (F1-503 discipline):
        // `validation error` alone is a generic phrasing, `messages.` alone
        // is a field mention — both together are the shim's dotted shape.
        let pydantic_dotted = normalized.contains("validation error")
            && (normalized.contains("messages.") || normalized.contains("input."));
        // Family 9 (COMPACT-BOUNDARM-1, apex-ayl.82; LIVE — incident
        // 01a09be2 lineage): the proxy rejects the compact path's trailing
        // `compaction_trigger` input item. Double-keyed so ordinary
        // compaction chatter in error text never classifies model-bound.
        let compaction_trigger_rejection =
            normalized.contains("unsupported") && normalized.contains("compaction_trigger");
        encrypted
            || thinking_signature
            || input_id_invalid
            || item_id_missing
            || azure_content_rejection
            || input_callid_invalid
            || pydantic_dotted
            || compaction_trigger_rejection
    }

    /// COMPACT-BOUNDARM-1 (apex-ayl.82): whether this error is the proxy's
    /// rejection of the `compaction_trigger` input item ITSELF (Family 9) —
    /// as opposed to a model-bound rejection of carried history state. The
    /// remote-compaction-v2 retry path uses this to re-issue the request
    /// WITHOUT the trailing trigger item (the item is the offending shape;
    /// the model history may be fully portable), while any other model-bound
    /// 400 keeps the trigger (the Codex-native protocol requires it).
    pub fn is_compaction_trigger_item_rejection(&self) -> bool {
        let SamplingError::Api { status, message, .. } = self else {
            return false;
        };
        if *status != StatusCode::BAD_REQUEST {
            return false;
        }
        let normalized = message.to_ascii_lowercase();
        normalized.contains("unsupported") && normalized.contains("compaction_trigger")
    }

    /// The server rejected the request because an image could not be processed. [`INVALID_IMAGE_ERROR_CODE`] is the signal.
    /// Some provider passthroughs stamp neither, keying image rejections on the [`IMAGE_CONTENT_PATH_MARKER`] content path
    /// instead. Recovery destroys request images, so unexpected statuses (422, 415,...) fail closed.
    pub fn is_image_processing_error(&self) -> bool {
        match self {
            SamplingError::Api {
                status,
                message,
                error_code,
                ..
            } if matches!(status.as_u16(), 400 | 500) => {
                *error_code == Some(ApiErrorCode::InvalidImage)
                    || message.contains("Could not process image")
                    || message.contains(IMAGE_CONTENT_PATH_MARKER)
            }
            SamplingError::StreamError { code, .. } => *code == Some(ApiErrorCode::InvalidImage),
            // Explicit like `is_retryable`: a new variant must state its image classification instead of silently defaulting to false
            SamplingError::Api { .. }
            | SamplingError::Auth { .. }
            | SamplingError::InvalidConfiguration(_)
            | SamplingError::MtlsConfiguration(_)
            | SamplingError::Http(_)
            | SamplingError::Serialization(_)
            | SamplingError::EventStreamError(_)
            | SamplingError::IdleTimeout { .. }
            | SamplingError::EmptyResponse { .. }
            | SamplingError::MaxTokensTruncation
            | SamplingError::DoomLoopDetected { .. }
            | SamplingError::RequestValidation(_)
            | SamplingError::UnsupportedStopControl { .. } => false,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            SamplingError::Auth { .. } => false,
            SamplingError::InvalidConfiguration(_) => false,
            SamplingError::MtlsConfiguration(_) => false,
            SamplingError::Http(err) => is_retryable_reqwest(err),
            SamplingError::Serialization(_) => false,
            SamplingError::Api { status, .. } => is_retryable_api_status(*status),
            SamplingError::EventStreamError(_) => true,
            SamplingError::StreamError { .. } => true,
            SamplingError::IdleTimeout { .. } => false,
            SamplingError::EmptyResponse { .. } => true,
            SamplingError::MaxTokensTruncation => false,
            SamplingError::DoomLoopDetected { .. } => true,
            // Local pre-HTTP cap violation: deterministic, never retry.
            SamplingError::RequestValidation(_) => false,
            // Unsupported stop control (pause_turn): a deterministic typed terminal, never retry.
            SamplingError::UnsupportedStopControl { .. } => false,
        }
    }

    pub fn model_metadata(&self) -> Option<&ResponseModelMetadata> {
        match self {
            SamplingError::Api { model_metadata, .. } => model_metadata.as_ref(),
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<u64> {
        match self {
            SamplingError::Api {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }

    /// Server hint on whether this error is worth retrying.
    pub fn should_retry_header(&self) -> Option<bool> {
        match self {
            SamplingError::Api { should_retry, .. } => *should_retry,
            _ => None,
        }
    }

    /// True when this error is a context-window/size overflow (deterministic; don't retry the same payload). Exception: a 429
    /// carrying `Retry-After` with no structured size code does not classify. Retry loops back off instead of fast-failing,
    /// and the compaction classifier stays transient instead of stepping the input ladder.
    pub fn is_context_length_error(&self) -> bool {
        match self {
            SamplingError::Api {
                status,
                message,
                retry_after_secs,
                error_code,
                ..
            } => {
                let size_coded = error_code
                    .as_ref()
                    .is_some_and(ApiErrorCode::is_size_overflow);
                if *status == StatusCode::TOO_MANY_REQUESTS
                    && retry_after_secs.is_some()
                    && !size_coded
                {
                    return false;
                }
                size_coded || is_context_length_error(message)
            }
            SamplingError::StreamError { message, code, .. } => {
                code.as_ref().is_some_and(ApiErrorCode::is_size_overflow)
                    || is_context_length_error(message)
            }
            // Explicit so a new variant must state its size classification.
            SamplingError::Auth { .. }
            | SamplingError::InvalidConfiguration(_)
            | SamplingError::MtlsConfiguration(_)
            | SamplingError::Http(_)
            | SamplingError::Serialization(_)
            | SamplingError::EventStreamError(_)
            | SamplingError::IdleTimeout { .. }
            | SamplingError::EmptyResponse { .. }
            | SamplingError::MaxTokensTruncation
            | SamplingError::DoomLoopDetected { .. }
            | SamplingError::RequestValidation(_)
            | SamplingError::UnsupportedStopControl { .. } => false,
        }
    }

    /// Structured 413-style rejection on the envelope or stream event: a byte-tier cap, so image stripping may fix it (unlike token overflows).
    pub fn is_byte_size_overflow_coded(&self) -> bool {
        match self {
            SamplingError::Api { error_code, .. } => error_code
                .as_ref()
                .is_some_and(ApiErrorCode::is_byte_size_overflow),
            SamplingError::StreamError { code, .. } => code
                .as_ref()
                .is_some_and(ApiErrorCode::is_byte_size_overflow),
            // Explicit so a new variant must state its size classification.
            SamplingError::Auth { .. }
            | SamplingError::InvalidConfiguration(_)
            | SamplingError::MtlsConfiguration(_)
            | SamplingError::Http(_)
            | SamplingError::Serialization(_)
            | SamplingError::EventStreamError(_)
            | SamplingError::IdleTimeout { .. }
            | SamplingError::EmptyResponse { .. }
            | SamplingError::MaxTokensTruncation
            | SamplingError::DoomLoopDetected { .. }
            | SamplingError::RequestValidation(_)
            | SamplingError::UnsupportedStopControl { .. } => false,
        }
    }

    /// Capacity / overload: HTTP 529, a 5xx whose message clearly says overloaded, or a stream error whose parsed
    /// `error_type` is a capacity type. Proxies wrap stream overloads in a 500; the capacity types are `overloaded_error` and
    /// `service_unavailable_error`. Never reachable from a 4xx or a request-shaped stream error, whatever the message text.
    pub fn is_overloaded(&self) -> bool {
        match self {
            SamplingError::Api {
                status, message, ..
            } => {
                status.as_u16() == 529
                    || (status.is_server_error() && message_looks_overloaded(message))
            }
            // `error_type` is already parsed from the stream payload, so trust it alone
            // Matching message text here would let a request-shaped error that merely mentions "overloaded" retry
            SamplingError::StreamError { error_type, .. } => {
                error_type.eq_ignore_ascii_case("overloaded_error")
                    || error_type.eq_ignore_ascii_case("service_unavailable_error")
            }
            _ => false,
        }
    }

    /// Retry vetoes shared by every retry loop: the sampler actor's `classify_error` and one-shot callers like `/btw`.
    /// `x-should-retry: false`: the server says the request content caused the failure, not something transient;
    /// Context-length overflow: deterministic; re-sending the same payload always fails.
    pub fn is_retry_vetoed(&self) -> bool {
        self.should_retry_header() == Some(false) || self.is_context_length_error()
    }

    /// Deterministic in-stream failure: a `StreamError`/`EventStreamError` whose
    /// message hits the deterministic in-stream text set (see
    /// [`is_deterministic_in_stream_message`]). Re-sending the same payload cannot
    /// change the outcome, so retry loops must classify it non-retryable instead
    /// of burning the retry budget (C3: 3-attempt compaction storm).
    pub fn is_deterministic_in_stream_error(&self) -> bool {
        match self {
            SamplingError::StreamError { message, .. }
            | SamplingError::EventStreamError(message) => {
                is_deterministic_in_stream_message(message)
            }
            // Explicit so a new variant must state its in-stream classification.
            SamplingError::Api { .. }
            | SamplingError::Auth { .. }
            | SamplingError::InvalidConfiguration(_)
            | SamplingError::MtlsConfiguration(_)
            | SamplingError::Http(_)
            | SamplingError::Serialization(_)
            | SamplingError::IdleTimeout { .. }
            | SamplingError::EmptyResponse { .. }
            | SamplingError::MaxTokensTruncation
            | SamplingError::DoomLoopDetected { .. }
            | SamplingError::RequestValidation(_)
            | SamplingError::UnsupportedStopControl { .. } => false,
        }
    }
}

impl From<reqwest::Error> for SamplingError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

impl From<serde_json::Error> for SamplingError {
    fn from(value: serde_json::Error) -> Self {
        tracing::debug!("Serde deserialization error: {:?}", &value);
        Self::Serialization(value)
    }
}

impl From<RequestValidationError> for SamplingError {
    fn from(value: RequestValidationError) -> Self {
        Self::RequestValidation(value)
    }
}

/// OpenAI-standard provider error format: `{"error": {"message": "...", "type": "..."}}`.
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    /// Semantic code (e.g. [`INVALID_IMAGE_ERROR_CODE`]), distinct from the `type` slot.
    #[serde(default, deserialize_with = "lenient_code")]
    code: Option<String>,
}

/// Flat error from the Grok proxy/gateway: `{"code": "...", "error": "..."}`. Flat bodies with a non-string code (e.g.
/// `{"code":429,"error":"... [WKE=...]"}`) must keep failing this parse so they reach the provider fallback. The fallback
/// strips `[WKE=...]` markers and lifts slugs; routing them through the rigid path would leak raw markers to users.
#[derive(Debug, Deserialize)]
struct FlatErrorResponse {
    error: String,
    #[serde(default)]
    code: Option<String>,
}

/// Some provider dialects put non-strings in the nested `code` slot (e.g. `"code": 429`).
/// A strict `Option<String>` would fail the whole envelope parse and demote a retryable stream error to a fatal `Serialization` error.
/// Swallow non-string codes instead of rejecting the envelope.
fn lenient_code<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
}

/// Fields extracted from an error payload by [`try_parse_error`].
struct ParsedError {
    error_type: String,
    message: String,
    /// The envelope's `code` slot: nested envelopes pass through verbatim.
    /// The flat envelope's slot is overloaded (gRPC kebab codes, type slots), so only semantic values surface from it.
    code: Option<ApiErrorCode>,
}

/// Extract the error fields from either error format.
fn try_parse_error(data: &str) -> Option<ParsedError> {
    if let Ok(resp) = serde_json::from_str::<ErrorResponse>(data) {
        return Some(ParsedError {
            error_type: resp.error.kind.unwrap_or_else(|| "unknown".to_string()),
            message: resp
                .error
                .message
                .unwrap_or_else(|| "unknown error".to_string()),
            code: resp.error.code.as_deref().map(ApiErrorCode::parse),
        });
    }
    if let Ok(flat) = serde_json::from_str::<FlatErrorResponse>(data) {
        let code = flat
            .code
            .as_deref()
            .map(ApiErrorCode::parse)
            .filter(|c| !matches!(c, ApiErrorCode::Other(_)));
        return Some(ParsedError {
            code,
            error_type: flat.code.unwrap_or_else(|| "server_error".to_string()),
            message: flat.error,
        });
    }
    None
}

/// Semantic `error.code` from a raw error body. Nested envelopes yield their code verbatim.
/// The flat envelope overloads its `code` slot with gRPC kebab codes and type slots, so only exact semantic values surface from it.
pub fn parse_error_code(bytes: &[u8]) -> Option<ApiErrorCode> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(try_parse_error)?
        .code
}

/// Max chars of a structured (JSON) error message shown to users.
pub const MAX_USER_ERROR_BODY_CHARS: usize = 280;

/// Short status-based copy when the body is not a structured JSON error.
///
/// Edge proxies (Cloudflare 52x, 502/503/504) return HTML pages; we never sniff body text, so only the HTTP status drives this fallback.
pub fn status_user_message(status: StatusCode) -> String {
    match status.as_u16() {
        code @ 502..=504 => {
            format!("Grok is temporarily unavailable. Please try again in a moment. (HTTP {code}).")
        }
        // Upstream capacity, not an edge failure; see [`SamplingError::is_overloaded`]
        code @ 529 => {
            format!("Grok is temporarily overloaded. Please try again in a moment. (HTTP {code}).")
        }
        // Cloudflare edge: origin unreachable or timed out (520-524), or an edge-side 1xxx failure (530)
        code @ 520..=524 | code @ 530 => {
            format!(
                "Connection to Grok timed out or was interrupted. Please try again. (HTTP {code})."
            )
        }
        // Cloudflare origin TLS (handshake / invalid certificate); not transient
        code @ 525 | code @ 526 => {
            format!("Secure connection to Grok failed. (HTTP {code}).")
        }
        code if status.is_server_error() => {
            format!("Something went wrong on the server (HTTP {code}).")
        }
        code => format!("Request failed (HTTP {code})."),
    }
}

fn truncate_user_error(s: &str) -> String {
    let s = s.trim();
    let count = s.chars().count();
    if count <= MAX_USER_ERROR_BODY_CHARS {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(MAX_USER_ERROR_BODY_CHARS).collect();
    out.push('\u{2026}');
    out
}

/// Format a known JSON error envelope; `None` if the body is not structured.
fn structured_error_message(bytes: &[u8]) -> Option<String> {
    let rigid = std::str::from_utf8(bytes).ok().and_then(try_parse_error);
    if let Some(ParsedError {
        error_type,
        message,
        ..
    }) = &rigid
        && message != "unknown error"
    {
        if let Some(inner) = parse_provider_error_str(message)
            && inner.message != *message
            && !inner.message_is_markup()
        {
            return Some(inner.display_message());
        }
        let msg = if error_type == "unknown" || error_type == "server_error" {
            message.clone()
        } else {
            format!("{error_type}: {message}")
        };
        return Some(truncate_user_error(&msg));
    }
    if let Some(parsed) = parse_provider_error(bytes)
        && !parsed.message_is_markup()
    {
        return Some(parsed.display_message());
    }
    rigid.map(|parsed| truncate_user_error(&parsed.message))
}

/// Parse an API error body into a short string. Only structured JSON error envelopes are surfaced. Non-JSON bodies (HTML
/// edge pages, plain text dumps) return a fixed placeholder, never the raw bytes. Prefer
/// [`user_facing_api_error_message`] when a status code is available.
pub fn parse_error_bytes(bytes: &[u8]) -> String {
    structured_error_message(bytes).unwrap_or_else(|| "upstream error".into())
}

/// User-facing message for a failed API call. Structured JSON error envelopes keep their message. Everything else
/// (including Cloudflare HTML) maps to a status-based string, with no body content matching.
pub fn user_facing_api_error_message(status: StatusCode, bytes: &[u8]) -> String {
    structured_error_message(bytes).unwrap_or_else(|| status_user_message(status))
}

pub fn try_parse_stream_error(data: &str) -> Option<SamplingError> {
    let ParsedError {
        error_type,
        message,
        code,
    } = try_parse_error(data)?;
    tracing::warn!(error_type, message, "Server-side stream error");
    Some(SamplingError::StreamError {
        error_type,
        message,
        code,
    })
}

/// Shared size-overflow text detector: a single definition (in the compaction engine) so the turn path and compaction loops can't drift.
pub use xai_grok_compaction::is_context_length_error;

/// Whether an HTTP status is worth retrying: the rule CCP publishes in `x-should-retry` (429 and any 5xx), minus Cloudflare's origin-TLS 525/526.
/// Requests reach CCP through the Cloudflare edge, which answers with its own 52x pages when the origin is unreachable.
pub fn is_retryable_api_status(status: StatusCode) -> bool {
    RetryPolicy::edge_client().should_retry(status.as_u16())
}

/// Decide whether a [`reqwest::Error`] is worth retrying.
pub fn is_retryable_reqwest(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() {
        return true;
    }

    if err.is_status() {
        return err.status().is_some_and(is_retryable_api_status);
    }

    if err.is_request() || err.is_body() {
        return true;
    }

    false
}

/// Capacity-style provider text: "Overloaded" / `overloaded_error` (possibly proxy-wrapped) or `service_unavailable_error` (503-shaped capacity).
fn message_looks_overloaded(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("overloaded") || m.contains("service_unavailable_error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overloaded_detects_stream_and_api_shapes() {
        assert!(
            SamplingError::StreamError {
                error_type: "overloaded_error".into(),
                message: "Overloaded".into(),
                code: None,
            }
            .is_overloaded()
        );
        assert!(
            SamplingError::Api {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: "stream error (overloaded_error): Overloaded".into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            }
            .is_overloaded()
        );
        assert!(
            SamplingError::Api {
                status: StatusCode::from_u16(529).unwrap(),
                message: "capacity".into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            }
            .is_overloaded()
        );
        assert!(
            SamplingError::Api {
                status: StatusCode::from_u16(529).unwrap(),
                message: "capacity".into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            }
            .is_retryable()
        );
        assert!(!SamplingError::auth_unknown("nope").is_overloaded());
        assert!(
            !SamplingError::Api {
                status: StatusCode::BAD_REQUEST,
                message: "invalid json".into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            }
            .is_overloaded()
        );
        // Only server errors classify on message text; a 4xx that merely mentions "overloaded" is a request error, not capacity
        assert!(
            !SamplingError::Api {
                status: StatusCode::BAD_REQUEST,
                message: "field `overloaded` is not a valid parameter".into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            }
            .is_overloaded()
        );
        // Stream errors classify on the parsed error_type only; a request-shaped stream error mentioning "overloaded" is not capacity
        assert!(
            !SamplingError::StreamError {
                error_type: "invalid_request_error".into(),
                message: "tool result mentions overloaded".into(),
                code: None,
            }
            .is_overloaded()
        );
        assert!(
            SamplingError::StreamError {
                error_type: "service_unavailable_error".into(),
                message: "upstream capacity".into(),
                code: None,
            }
            .is_overloaded()
        );
    }

    #[test]
    fn overloaded_message_matches_backend_variants() {
        // 5xx messages that classify as capacity.
        for msg in [
            "Overloaded",
            "stream error (overloaded_error): Overloaded",
            "overloaded_error",
            "service_unavailable_error: try again",
        ] {
            assert!(
                SamplingError::Api {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: msg.into(),
                    model_metadata: None,
                    retry_after_secs: None,
                    should_retry: None,
                    error_code: None,
                }
                .is_overloaded(),
                "expected overloaded for message: {msg}"
            );
        }
        // 5xx messages that do not.
        for msg in ["upstream connect timeout", "internal error"] {
            assert!(
                !SamplingError::Api {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: msg.into(),
                    model_metadata: None,
                    retry_after_secs: None,
                    should_retry: None,
                    error_code: None,
                }
                .is_overloaded(),
                "expected not overloaded for message: {msg}"
            );
        }
    }

    #[test]
    fn retry_veto_covers_header_and_context_length() {
        let vetoed_by_header = SamplingError::Api {
            status: StatusCode::from_u16(529).unwrap(),
            message: "capacity".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: Some(false),
            error_code: None,
        };
        assert!(vetoed_by_header.is_retry_vetoed());

        let vetoed_by_context = SamplingError::Api {
            status: StatusCode::from_u16(529).unwrap(),
            message: "prompt is too long: 300000 tokens > 200000 maximum".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(vetoed_by_context.is_retry_vetoed());

        let not_vetoed = SamplingError::Api {
            status: StatusCode::from_u16(529).unwrap(),
            message: "capacity".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!not_vetoed.is_retry_vetoed());
    }

    #[test]
    fn deterministic_in_stream_text_set_classifies_stream_variants() {
        // C3 wire shape (LiteLLM opaque in-stream failure)
        assert!(SamplingError::StreamError {
            error_type: "unknown".into(),
            message: "litellm.APIError: Response API in-stream error".into(),
            code: None,
        }
        .is_deterministic_in_stream_error());
        assert!(SamplingError::EventStreamError(
            "litellm.APIError: Response API in-stream error".into()
        )
        .is_deterministic_in_stream_error());
        // Over-capacity in-stream text
        assert!(SamplingError::StreamError {
            error_type: "unknown".into(),
            message: "over capacity".into(),
            code: None,
        }
        .is_deterministic_in_stream_error());

        // Narrowness: transient in-stream blips are not in the set
        assert!(!SamplingError::StreamError {
            error_type: "overloaded_error".into(),
            message: "The server is overloaded.".into(),
            code: None,
        }
        .is_deterministic_in_stream_error());
        // Size / context-length text is NOT in this set: it already classifies
        // deterministic via is_context_length_error, with its own recovery path
        // (image strip / input ladder)
        let size = SamplingError::StreamError {
            error_type: "BAD_REQUEST".into(),
            message: "Input length (300000 tokens) exceeds the maximum allowed length \
                      (200000 tokens)"
                .into(),
            code: None,
        };
        assert!(size.is_context_length_error());
        assert!(!size.is_deterministic_in_stream_error());
        // Non-stream variants never classify
        assert!(!SamplingError::auth_unknown("expired").is_deterministic_in_stream_error());
    }

    #[test]
    fn tpm_429_with_retry_after_escapes_the_size_text_veto() {
        let tpm =
            |retry_after_secs: Option<u64>, error_code: Option<ApiErrorCode>| SamplingError::Api {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: "Request too large for model: Limit 30000, Requested 50000 \
                          tokens per min"
                    .into(),
                model_metadata: None,
                retry_after_secs,
                should_retry: None,
                error_code,
            };
        // Retry-After promises capacity later; size wording alone must not fast-fail the backoff path
        let backs_off = tpm(Some(7), None);
        assert!(!backs_off.is_context_length_error());
        assert!(!backs_off.is_retry_vetoed());
        // No Retry-After: the request exceeds the cap outright, so fail fast
        let no_retry_after = tpm(None, None);
        assert!(no_retry_after.is_context_length_error());
        assert!(no_retry_after.is_retry_vetoed());
        // A structured size code cannot be an echo, so it is vetoed even with Retry-After
        let coded = tpm(Some(7), Some(ApiErrorCode::parse("request_too_large")));
        assert!(coded.is_context_length_error());
        assert!(coded.is_retry_vetoed());
    }

    // The canonical wording table lives beside the detector in xai-grok-compaction; tests here pin only crate-local couplings
    #[test]
    fn context_length_error_method_delegates_to_shared_detector() {
        let api = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "none: The prompt is too long for this model's context window.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(api.is_context_length_error());
        assert!(
            SamplingError::StreamError {
                error_type: "overloaded_error".into(),
                message: "prompt is too long".into(),
                code: None,
            }
            .is_context_length_error()
        );
        assert!(!SamplingError::auth_unknown("nope").is_context_length_error());
    }

    #[test]
    fn size_overflow_error_codes_parse_structurally() {
        for code in [
            "413",
            "payload_too_large",
            "exceed_context_size_error",
            "request_too_large",
            "context_length_exceeded",
        ] {
            assert!(is_size_overflow_error_code(code), "should match: {code}");
            let parsed = ApiErrorCode::parse(code);
            assert!(parsed.is_size_overflow(), "should be overflow: {code}");
            // Verbatim round-trip keeps wire serialization byte-identical.
            assert_eq!(parsed.as_str(), code);
        }
        for code in ["400", "429", "invalid_request_error", "overloaded_error"] {
            assert!(
                !is_size_overflow_error_code(code),
                "should not match: {code}"
            );
            assert!(!ApiErrorCode::parse(code).is_size_overflow());
        }
        // Byte-size subset: image stripping is a remedy for byte caps only.
        for code in ["413", "payload_too_large", "request_too_large"] {
            assert!(is_byte_size_overflow_error_code(code), "byte-size: {code}");
            assert!(ApiErrorCode::parse(code).is_byte_size_overflow());
        }
        for code in ["exceed_context_size_error", "context_length_exceeded"] {
            assert!(
                !is_byte_size_overflow_error_code(code),
                "token slug must not be byte-size: {code}"
            );
            assert!(!ApiErrorCode::parse(code).is_byte_size_overflow());
        }
    }

    #[test]
    fn flat_envelope_size_slug_survives_semantic_code_filter() {
        // Size slugs are semantic and must survive the flat envelope's semantic-value filter so downstream classification sees them
        assert_eq!(
            parse_error_code(
                br#"{"code":"payload_too_large","error":"Chat history exceeds the limit"}"#
            ),
            Some(ApiErrorCode::ContextOverflow("payload_too_large".into())),
        );
        // Non-semantic flat codes are still filtered out.
        assert_eq!(
            parse_error_code(br#"{"code":"invalid-argument","error":"boom"}"#),
            None,
        );
    }

    #[test]
    fn structured_size_code_with_opaque_message_is_context_length_error() {
        // The code slot alone must classify when the text matches nothing.
        let stream = SamplingError::StreamError {
            error_type: "BAD_REQUEST".into(),
            message: "request rejected".into(),
            code: Some(ApiErrorCode::parse("request_too_large")),
        };
        assert!(stream.is_context_length_error());

        let api = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "request rejected".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: Some(ApiErrorCode::parse("413")),
        };
        assert!(api.is_context_length_error());

        // An opaque error with a non-size code stays non-size.
        let opaque = SamplingError::StreamError {
            error_type: "server_error".into(),
            message: "internal error".into(),
            code: Some(ApiErrorCode::parse("overloaded_error")),
        };
        assert!(!opaque.is_context_length_error());
    }

    #[test]
    fn serialization_message_stays_serialization_and_non_retryable() {
        let err = SamplingError::serialization_message("bad payload at line 1 column 7");
        assert!(matches!(err, SamplingError::Serialization(_)));
        assert!(!err.is_retryable());
        assert!(err.to_string().contains("bad payload at line 1 column 7"));
    }

    #[test]
    fn serialization_from_rendered_round_trips_display() {
        // Derived from a REAL error's Display so a template rewording cannot silently desynchronize the strip from the prefix it mirrors
        let original =
            SamplingError::Serialization(serde_json::from_str::<i32>("not a number").unwrap_err());
        let rendered = original.to_string();
        let rebuilt = SamplingError::serialization_from_rendered(&rendered);
        assert!(matches!(rebuilt, SamplingError::Serialization(_)));
        assert!(!rebuilt.is_retryable());
        assert_eq!(
            rebuilt.to_string(),
            rendered,
            "rendered Display must round-trip without double-prefixing"
        );
        // Bare (non-rendered) input gains the prefix exactly once.
        assert_eq!(
            SamplingError::serialization_from_rendered("bare message").to_string(),
            format!("{SERIALIZATION_DISPLAY_PREFIX}bare message"),
        );
    }

    #[test]
    fn idle_timeout_is_not_retryable() {
        let err = SamplingError::IdleTimeout { elapsed_secs: 300 };
        assert!(
            !err.is_retryable(),
            "IdleTimeout must not be retried — would cause 3× amplification"
        );
    }

    #[test]
    fn event_stream_error_is_retryable() {
        let err = SamplingError::EventStreamError("connection reset".into());
        assert!(err.is_retryable());
    }

    #[test]
    fn idle_timeout_display() {
        let err = SamplingError::IdleTimeout { elapsed_secs: 120 };
        let msg = err.to_string();
        assert!(
            msg.contains("120s"),
            "Display should include elapsed_secs: {msg}"
        );
    }

    #[test]
    fn try_parse_stream_error_flat_format() {
        let data = r#"{"code":"The service is currently unavailable","error":"Service temporarily unavailable. The model did not respond to this request."}"#;
        let err = try_parse_stream_error(data).expect("should parse flat error");
        match err {
            SamplingError::StreamError {
                error_type,
                message,
                code,
            } => {
                assert_eq!(error_type, "The service is currently unavailable");
                assert_eq!(
                    message,
                    "Service temporarily unavailable. The model did not respond to this request."
                );
                assert_eq!(
                    code, None,
                    "flat-format code is a type slot, not this contract"
                );
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    #[test]
    fn try_parse_stream_error_valid_chunk_returns_none() {
        let data = r#"{"id":"abc","object":"chat.completion.chunk","created":0,"model":"test","choices":[]}"#;
        assert!(
            try_parse_stream_error(data).is_none(),
            "valid chunk should not be parsed as error"
        );
    }

    #[test]
    fn parse_error_bytes_flat_format() {
        let bytes =
            br#"{"code":"The service is currently unavailable","error":"Service temporarily unavailable."}"#;
        let msg = parse_error_bytes(bytes);
        assert_eq!(
            msg,
            "The service is currently unavailable: Service temporarily unavailable."
        );
    }

    #[test]
    fn parse_error_bytes_rejects_non_json_body() {
        let html = br#"<!DOCTYPE html>
<html lang="en-US">
<head><title>grok.com | 524: A timeout occurred</title></head>
<body><h1>A timeout occurred Error code 524</h1></body>
</html>"#;
        let msg = parse_error_bytes(html);
        assert_eq!(msg, "upstream error");
        // Plain non-JSON text is also rejected (no body sniffing).
        assert_eq!(
            parse_error_bytes(b"some random gateway text"),
            "upstream error"
        );
    }

    #[test]
    fn user_facing_api_error_message_maps_non_json_by_status() {
        let html = br#"<!DOCTYPE html><html><body>timeout</body></html>"#;
        let msg = user_facing_api_error_message(StatusCode::from_u16(524).unwrap(), html);
        assert_eq!(msg, status_user_message(StatusCode::from_u16(524).unwrap()));

        let msg_503 =
            user_facing_api_error_message(StatusCode::SERVICE_UNAVAILABLE, b"not json either");
        assert_eq!(
            msg_503,
            status_user_message(StatusCode::SERVICE_UNAVAILABLE)
        );
    }

    #[test]
    fn user_facing_keeps_json_error_message() {
        let bytes = br#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error"}}"#;
        let msg = user_facing_api_error_message(StatusCode::TOO_MANY_REQUESTS, bytes);
        assert_eq!(msg, "rate_limit_error: rate limit exceeded");
    }

    /// Non-string `code` slots (numeric HTTP codes from provider dialects) must not fail the envelope parse.
    /// Mid-stream, a failed parse falls through to the chunk parse and surfaces a `Serialization` error where a retryable `StreamError` is correct.
    #[test]
    fn numeric_code_dialects_still_parse_as_envelopes() {
        // Nested envelope: the code is swallowed, the message surfaces.
        let bytes = br#"{"error":{"message":"Provider returned error","code":429}}"#;
        let msg = user_facing_api_error_message(StatusCode::TOO_MANY_REQUESTS, bytes);
        assert_eq!(msg, "Provider returned error");
        assert_eq!(parse_error_code(bytes), None);

        // Mid-stream: still a retryable StreamError.
        let data =
            r#"{"error":{"message":"upstream overloaded","type":"overloaded_error","code":503}}"#;
        let err = try_parse_stream_error(data).expect("numeric-code envelope must still parse");
        assert!(err.is_retryable(), "stream errors must stay retryable");
        match err {
            SamplingError::StreamError {
                error_type, code, ..
            } => {
                assert_eq!(error_type, "overloaded_error");
                assert_eq!(code, None);
            }
            other => panic!("expected StreamError, got {other:?}"),
        }

        // Flat envelope with a non-string code: stays STRICT
        // It must keep failing the rigid parse so the provider fallback runs
        // That path strips `[WKE=...]` machine markers; the rigid path would leak them
        let bytes =
            br#"{"code":429,"error":"You ran out of credits. [WKE=personal-team-blocked:spending-limit]"}"#;
        assert_eq!(parse_error_code(bytes), None);
        let msg = user_facing_api_error_message(StatusCode::TOO_MANY_REQUESTS, bytes);
        assert!(
            !msg.contains("[WKE="),
            "flat numeric-code bodies must reach the WKE-stripping fallback, got: {msg}"
        );
    }

    #[test]
    fn user_facing_surfaces_dialects_the_rigid_parse_rejects() {
        let bytes = br#"{"message":"The model is not ready for inference"}"#;
        let msg = user_facing_api_error_message(StatusCode::TOO_MANY_REQUESTS, bytes);
        assert_eq!(msg, "The model is not ready for inference");

        let bytes =
            br#"[{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}]"#;
        let msg = user_facing_api_error_message(StatusCode::TOO_MANY_REQUESTS, bytes);
        assert_eq!(msg, "Quota exceeded");

        let bytes = br#""A request may either be streaming or deferred, but not both.""#;
        let msg = user_facing_api_error_message(StatusCode::BAD_REQUEST, bytes);
        assert_eq!(
            msg,
            "A request may either be streaming or deferred, but not both."
        );
    }

    #[test]
    fn user_facing_unwraps_double_encoded_relay_bodies() {
        let bytes = br#"{"error":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Values detected in request that violate rules: JWT Token\"}}"}"#;
        let msg = user_facing_api_error_message(StatusCode::BAD_REQUEST, bytes);
        assert_eq!(
            msg,
            "invalid_request_error: Values detected in request that violate rules: JWT Token"
        );
    }

    #[test]
    fn user_facing_never_surfaces_double_encoded_html() {
        let bytes = br#"{"error":"<html><body>502 Bad Gateway</body></html>"}"#;
        let msg = user_facing_api_error_message(StatusCode::BAD_GATEWAY, bytes);
        assert_eq!(msg, "<html><body>502 Bad Gateway</body></html>");
    }

    #[test]
    fn user_facing_rigid_shapes_are_unchanged_by_the_fallback() {
        for (body, expected) in [
            (
                r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error"}}"#,
                "rate_limit_error: rate limit exceeded",
            ),
            (
                r#"{"code":"The service is currently unavailable","error":"Service temporarily unavailable."}"#,
                "The service is currently unavailable: Service temporarily unavailable.",
            ),
            (
                r#"{"error":{"message":"Overloaded","type":"overloaded_error"}}"#,
                "overloaded_error: Overloaded",
            ),
            (r#"{"error":{"message":"boom","type":"unknown"}}"#, "boom"),
        ] {
            assert_eq!(
                user_facing_api_error_message(StatusCode::INTERNAL_SERVER_ERROR, body.as_bytes()),
                expected,
                "body: {body}"
            );
        }
    }

    #[test]
    fn structured_error_message_is_length_capped() {
        let long_msg = "x".repeat(MAX_USER_ERROR_BODY_CHARS + 50);
        let bytes = format!(r#"{{"error":{{"message":"{long_msg}","type":"server_error"}}}}"#);
        let msg = parse_error_bytes(bytes.as_bytes());
        assert!(msg.chars().count() <= MAX_USER_ERROR_BODY_CHARS + 1);
        assert!(msg.ends_with('\u{2026}'));
    }

    /// Regression test: 403 Forbidden must NOT be classified as an auth error. Those cover content-safety blocks, ZDR-gated
    /// operations, and other usage-policy blocks. Misclassifying these as auth errors triggers a pointless OIDC refresh and
    /// surfaces as acp::Error::auth_required on the client.
    #[test]
    fn forbidden_is_not_auth_error() {
        let err = SamplingError::Api {
            status: StatusCode::FORBIDDEN,
            message: "Content violates usage guidelines.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_auth_error(),
            "403 Forbidden must not be treated as an auth error"
        );
    }

    #[test]
    fn unauthorized_is_auth_error() {
        let err = SamplingError::Api {
            status: StatusCode::UNAUTHORIZED,
            message: "Invalid or expired credentials".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_auth_error(),
            "401 Unauthorized must be an auth error"
        );
    }

    #[test]
    fn auth_variant_is_auth_error() {
        let err = SamplingError::auth_unknown("bad key");
        assert!(err.is_auth_error());
    }

    /// Known values round-trip; an unrecognized value from a newer peer degrades to `Unknown` instead of failing the containing payload.
    #[test]
    fn sent_credential_wire_compat() {
        for (json, expected) in [
            ("\"sent\"", SentCredential::Sent),
            ("\"missing\"", SentCredential::Missing),
            ("\"unknown\"", SentCredential::Unknown),
            ("\"some-future-variant\"", SentCredential::Unknown),
        ] {
            assert_eq!(
                serde_json::from_str::<SentCredential>(json).unwrap(),
                expected
            );
        }
        assert_eq!(
            serde_json::to_string(&SentCredential::Missing).unwrap(),
            "\"missing\""
        );
    }

    #[test]
    fn rate_limited_api_error_is_detected() {
        let err = SamplingError::Api {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "Rate limit exceeded".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(err.is_rate_limited());
        assert!(err.is_retryable(), "429 should be retryable");
        assert!(!err.is_auth_error());
        assert!(!err.is_payload_too_large());
    }

    #[test]
    fn non_rate_limit_errors_are_not_rate_limited() {
        let server_error = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!server_error.is_rate_limited());

        let auth_error = SamplingError::auth_unknown("bad key");
        assert!(!auth_error.is_rate_limited());

        let timeout = SamplingError::IdleTimeout { elapsed_secs: 30 };
        assert!(!timeout.is_rate_limited());
    }

    #[test]
    fn is_likely_body_rejected_is_http_only() {
        // Coded 413 / invalid_image are ServerRejected, not this heuristic.
        let payload_too_large = SamplingError::Api {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: "too large".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!payload_too_large.is_likely_body_rejected());
        assert!(payload_too_large.is_payload_too_large());
        // Pins the coupling between the Display template and the detector: the rendered status phrase makes any rendered 413 text-detectable
        assert!(is_context_length_error(&payload_too_large.to_string()));

        let invalid_image = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "nope".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: Some(ApiErrorCode::InvalidImage),
        };
        assert!(!invalid_image.is_likely_body_rejected());
        assert!(invalid_image.is_image_processing_error());

        assert!(
            !SamplingError::EventStreamError("connection reset".into()).is_likely_body_rejected()
        );
        assert!(!SamplingError::IdleTimeout { elapsed_secs: 5 }.is_likely_body_rejected());
    }

    #[test]
    fn retry_after_returns_header_value() {
        let err = SamplingError::Api {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "slow down".into(),
            model_metadata: None,
            retry_after_secs: Some(42),
            should_retry: None,
            error_code: None,
        };
        assert_eq!(err.retry_after(), Some(42));
    }

    #[test]
    fn retry_after_returns_none_when_absent() {
        let err = SamplingError::Api {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "slow down".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert_eq!(err.retry_after(), None);
    }

    #[test]
    fn retry_after_returns_none_for_non_api_errors() {
        assert_eq!(SamplingError::auth_unknown("x").retry_after(), None);
        assert_eq!(
            SamplingError::IdleTimeout { elapsed_secs: 10 }.retry_after(),
            None
        );
    }

    #[test]
    fn model_bound_encrypted_content_field_400_is_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Could not decrypt the provided encrypted_content. Ensure the value is the unmodified encrypted_content from a previous response.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 1 (encrypted_content field) must match"
        );
    }

    #[test]
    fn model_bound_encrypted_content_azure_phrasing_400_is_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "The encrypted content for item cmp_0123 could not be verified. Reason: Encrypted content could not be decrypted or parsed.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: Some(ApiErrorCode::Other("invalid_encrypted_content".to_string())),
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 1 (Azure 'encrypted content' phrasing, folded from the legacy detector) must match"
        );
    }

    #[test]
    fn model_bound_wrong_status_not_detected() {
        // Status must be exactly 400; the same text on a 500 is a different class.
        let err = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "encrypted_content decryption failed".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_model_bound_history_error(),
            "only 400 should match, not 500"
        );
    }

    #[test]
    fn model_bound_unrelated_400_not_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Invalid model parameter".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_model_bound_history_error(),
            "unrelated 400 errors must not match"
        );
    }

    #[test]
    fn model_bound_thinking_signature_400_is_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "thinking block signature is invalid for this model".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 2 (thinking + signature) must match"
        );
        // Near-miss: 'thinking' alone is not model-bound.
        let thinking_only = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "thinking effort is not supported".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!thinking_only.is_model_bound_history_error());
    }

    #[test]
    fn model_bound_input_item_id_invalid_400_is_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Invalid 'input[3].id': item identifier is not valid for this deployment".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 3 (input[ + .id + invalid) must match"
        );
    }

    #[test]
    fn model_bound_item_id_not_found_400_is_detected() {
        // The exact XREPLAY-2 shape: a stale rs_* id replayed onto a store=false endpoint.
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Item with id 'rs_54ed3f3e8714491c84c6fca38bcbb44a' not found. Items are not persisted when `store` is set to false.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 4 (item + id + not found) must match"
        );
        // Near-miss: 'not found' without item+id is not model-bound.
        let not_found_only = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "model not found".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!not_found_only.is_model_bound_history_error());
    }

    #[test]
    fn model_bound_azure_content_array_too_long_400_is_detected() {
        // The exact XREPLAY-1 shape: a verbatim reasoning item's content array
        // rejected under the strict input schema (maxItems:0). This is the
        // this-stack adaptation HY's family 3 (`.id`) does not cover.
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "litellm.BadRequestError: AzureException BadRequestError - {\"error\":{\"message\":\"Invalid 'input[7].content': array too long. Expected an array with maximum length 0, but got an array with length 1 instead.\",\"type\":\"invalid_request_error\",\"param\":\"input[7].content\",\"code\":\"array_above_max_length\"}}. Received Model Group=gpt-5.6-sol".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "family 5 (input[ + array_above_max_length) must match the verbatim-reasoning rejection"
        );
    }

    #[test]
    fn model_bound_live_wire_azure_400_is_detected_through_user_facing_pipeline() {
        // Verbatim wire body (resp-006.jsonl, report/20260915T022829Z/rt-xreplay1;
        // identical in 20260914T230529Z). The classifier must match the message
        // it ACTUALLY sees: the output of `user_facing_api_error_message`, capped
        // at MAX_USER_ERROR_BODY_CHARS (280) — a cap that cuts the inner escaped
        // `code: array_above_max_length` field. Keying family 5 on that code
        // alone misclassified the live 400 as Fatal (CROSSWIRE-1 GREEN-run
        // defect: both sol 400s went terminal, the strip never fired).
        let body = r#"{"error":{"message":"litellm.BadRequestError: AzureException BadRequestError - {\n  \"error\": {\n    \"message\": \"Invalid 'input[7].content': array too long. Expected an array with maximum length 0, but got an array with length 1 instead.\",\n    \"type\": \"invalid_request_error\",\n    \"param\": \"input[7].content\",\n    \"code\": \"array_above_max_length\"\n  }\n}. Received Model Group=gpt-5.6-sol\nAvailable Model Group Fallbacks=None","type":null,"param":null,"code":"400"}}"#;
        let message = user_facing_api_error_message(
            StatusCode::BAD_REQUEST,
            body.as_bytes(),
        );
        let seen = message.clone();
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message,
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: parse_error_code(body.as_bytes()),
        };
        assert!(
            err.is_model_bound_history_error(),
            "live wire shape must classify model-bound; classifier saw: {}",
            seen
        );
    }

    #[test]
    fn model_bound_429_with_model_bound_text_not_detected() {
        // Status gate: a rate-limited 429 carrying model-bound text must NOT
        // route to the strip (it is a rate-limit, handled by the retry budget).
        let err = SamplingError::Api {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "Item with id 'rs_x' not found".into(),
            model_metadata: None,
            retry_after_secs: Some(5),
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_model_bound_history_error(),
            "429 with model-bound text must not match (status must be 400)"
        );
    }

    // XSWITCH-1 (apex-ayl.58): the 503 encryption-boundary family. Verbatim
    // resp-003 message from the live wire evidence at
    // ~/.grok/dogfood/20260916T191948Z/wire/resp-003.jsonl (SDD §1); no key
    // material in the text.
    const MODEL_BOUND_503_MESSAGE: &str = "litellm.ServiceUnavailableError: The deployment that produced this encrypted_content is currently unavailable (likely cooled down), and no deployment on the same encryption boundary is configured. Retry later or configure a deployment with the same (api_base, api_key).. Received Model Group=qwen3.8-27b";

    #[test]
    fn model_bound_503_encryption_boundary_full_message() {
        let err = SamplingError::Api {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: MODEL_BOUND_503_MESSAGE.into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "XSWITCH-1: the litellm EncryptedContentAffinityCheck 503 must route to the model-bound strip"
        );
    }

    #[test]
    fn model_bound_503_encryption_boundary_truncated_280() {
        // The classifier sees the user-facing message capped at
        // MAX_USER_ERROR_BODY_CHARS (280); "encrypted_content" sits at char 67
        // and "unavailable"/"boundary" survive the cap, so the truncated shape
        // must classify too.
        let err = SamplingError::Api {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: MODEL_BOUND_503_MESSAGE[..280].to_string(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "XSWITCH-1: the 280-char capped shape must still classify (needles survive the cap)"
        );
    }

    #[test]
    fn generic_503_overload_is_not_model_bound() {
        // Invariant: a plain deployment 503 keeps the generic backoff retry
        // path — only the encryption-boundary family routes to the strip.
        let err = SamplingError::Api {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "litellm.ServiceUnavailableError: No deployments available for model qwen3.8-27b".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_model_bound_history_error(),
            "generic 503 (no encrypted-boundary text) must NOT trigger the destructive strip"
        );
    }

    #[test]
    fn model_bound_503_with_encrypted_word_but_no_unavailable_boundary() {
        // The 503 arm is double-keyed: "encrypted" text alone (without
        // "unavailable" or "boundary") must not match — another provider's 503
        // phrasing that mentions encrypted_content is not the affinity check.
        // (Name adapts SDD §2.1 test 4: a Rust identifier cannot start with a
        // digit.)
        let err = SamplingError::Api {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "encrypted_content field rejected by upstream gateway; please retry".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_model_bound_history_error(),
            "503 with encrypted text but no unavailable/boundary wording must not match"
        );
    }

    #[test]
    fn model_bound_existing_400_families_unaffected() {
        // Guard: the 503 extension must leave the 400 families byte-identical
        // (family 1 encrypted + family 2 thinking-signature re-asserted).
        let encrypted_400 = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Could not decrypt the provided encrypted_content. Ensure the value is the unmodified encrypted_content from a previous response.".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            encrypted_400.is_model_bound_history_error(),
            "family 1 (encrypted 400) must still match after the 503 extension"
        );
        let thinking_400 = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "thinking block signature is invalid for this model".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            thinking_400.is_model_bound_history_error(),
            "family 2 (thinking-signature 400) must still match after the 503 extension"
        );
    }

    // AFFINITY-POLICY-1 (apex-ayl.75): the 401 tags-config fail-fast family
    // (ws8 probe D). Verbatim incident body frozen at
    // testdata/affinity/probe-d-401-body.json (mint: ws8 probe-2 D round,
    // provenance/proxy-capability-matrix.md §2 L33-35 + §3 L44; protected
    // copy grok/plans/provenance/fixtures/ws8/). The harness client maps
    // EVERY 401 to SamplingError::Auth (client.rs UNAUTHORIZED arms, 7 sites),
    // wrapping the user-facing message as
    // "Unauthorized (401) from <endpoint>: <server_message>" — so this
    // family keys on the MESSAGE, never on the status alone: a genuine
    // credential 401 can never carry the gateway's verbatim phrasing.

    /// Build the probe-D wire shape exactly as the client's 401 arm emits it:
    /// `SamplingError::Auth` whose message wraps
    /// [`user_facing_api_error_message`] over the verbatim incident body.
    fn probe_d_401_auth_error() -> SamplingError {
        let body = include_str!("../testdata/affinity/probe-d-401-body.json");
        let server_message =
            user_facing_api_error_message(StatusCode::UNAUTHORIZED, body.as_bytes());
        SamplingError::Auth {
            message: format!(
                "Unauthorized (401) from https://llm-proxy-api.ai.eng.netapp.com/v1/responses: {server_message}"
            ),
            credential: SentCredential::Sent,
        }
    }

    #[test]
    fn probe_d_401_tags_config_wire_shape_is_model_bound() {
        // RED (TDD): the incident's verbatim 401 body, in the exact wire shape
        // the client emits (Auth variant, wrapped user-facing message), must
        // classify as model-bound history — ONE classified strip+retry
        // (retry-once-then-terminate per the .58 contract) instead of a
        // terminal auth error. Today's behavior = UNCLASSIFIED (blind-retry
        // class, the .58 failure shape): the Auth variant is declined by the
        // classifier and the auth gate terminates the request.
        let err = probe_d_401_auth_error();
        assert!(
            err.is_model_bound_history_error(),
            "probe-D 401 tags-config wire shape must classify as model-bound \
             history; observed UNCLASSIFIED (today = terminal auth error)"
        );
    }

    #[test]
    fn probe_d_401_tags_config_api_shape_is_model_bound() {
        // Defensive symmetric arm (analog of the 503 exception to the 400
        // gate): a wire shape that maps the 401 to the Api variant must
        // classify on the same message needle. No current harness client site
        // emits this shape — the client maps every 401 to Auth — but a
        // pass-through proxy that does must not fall out of the class.
        let body = include_str!("../testdata/affinity/probe-d-401-body.json");
        let err = SamplingError::Api {
            status: StatusCode::UNAUTHORIZED,
            message: user_facing_api_error_message(StatusCode::UNAUTHORIZED, body.as_bytes()),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            err.is_model_bound_history_error(),
            "probe-D 401 body on the Api variant must classify (message-keyed)"
        );
    }

    #[test]
    fn genuine_401_auth_rejection_is_not_model_bound() {
        // Guard (green before AND after the cut): a real credential 401 keeps
        // the auth-gate terminal path. The needle is the verbatim gateway
        // phrasing, never a status match — a broadened 401 arm that matched
        // the status would break key-refresh flows.
        let err = SamplingError::auth_unknown("Invalid or expired credentials");
        assert!(
            !err.is_model_bound_history_error(),
            "a genuine credential 401 must NOT trigger the destructive strip"
        );
        let api_401 = SamplingError::Api {
            status: StatusCode::UNAUTHORIZED,
            message: "Incorrect API key provided".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !api_401.is_model_bound_history_error(),
            "a genuine credential 401 (Api shape) must NOT trigger the destructive strip"
        );
    }

    #[test]
    fn probe_c_same_boundary_keep_shape_is_pinned() {
        // Cut (b): the probe-C shape (id + field markers intact, untagged,
        // store=false; 3/3 200, pin held) is the APPROVED same-boundary
        // continuation default. Pin: the markers round-trip through the typed
        // ReasoningItem unchanged — KEEP at the request layer; nothing in
        // this crate silently drops the id or the field. The field-only
        // variant (OQ-1b) is NOT approved — the fixture's ruling line keeps
        // that boundary explicit.
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!(
                "../testdata/affinity/probe-c-same-boundary-keep.json"
            ))
            .expect("probe-C pin fixture must parse");
        let input = fixture["request_shape"]["body"]["input"]
            .as_array()
            .expect("pin fixture must carry the continuation input array");
        let reasoning_items: Vec<crate::rs::ReasoningItem> = input
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .map(|item| {
                serde_json::from_value(item.clone())
                    .expect("reasoning marker item must deserialize into the typed item")
            })
            .collect();
        assert_eq!(
            reasoning_items.len(),
            1,
            "probe-C carries exactly one reasoning marker item"
        );
        let marker = &reasoning_items[0];
        assert!(
            marker.id.starts_with("encitem_"),
            "KEEP pin: the id marker survives the typed round trip (got {:?})",
            marker.id
        );
        let field = marker
            .encrypted_content
            .as_deref()
            .expect("KEEP pin: the encrypted_content field marker survives");
        assert!(
            field.starts_with("litellm_enc:"),
            "KEEP pin: the field keeps its litellm_enc: scheme (got {field:?})"
        );
        let reserialized =
            serde_json::to_value(marker).expect("marker must re-serialize");
        assert_eq!(
            reserialized["id"], marker.id,
            "re-serialized id is byte-identical (KEEP)"
        );
        assert_eq!(
            reserialized["encrypted_content"], field,
            "re-serialized field is byte-identical (KEEP)"
        );
        let ruling = fixture["ruling"].as_str().expect("ruling pinned in fixture");
        assert!(
            ruling.contains("NOT approved"),
            "the field-only variant must stay unpinned (OQ-1b owed)"
        );
    }

    // XW-ORPHAN-1 (apex-ayl.74): the uncaught-400 BRICK gap. Two phrasings
    // match NO classifier family today — class (a) vLLM pydantic dotted-id
    // (LIVE, glm preplan §VI.3, verbatim fragment sha256:1f591ef070d9) and
    // class (b) Azure `.call_id` orphan (PREDICTION, qwen preplan H-1) — so
    // `classify_error` falls through to the Fatal tail
    // (sampler `retry.rs:186`): terminal on a 400 (`is_retryable_api_status`
    // excludes 400), BRICKed until compaction rewrites the history. The fix
    // lands as additive arms in the 400-gated region — arm A (F3-mirror,
    // `input[` + `.call_id` + `invalid`) and arm B (F1-503 double-key
    // discipline, `validation error` + `messages.`|`input.`) plus the F5
    // dotted extension (`input.`) — as Family 7/8 after .75's committed
    // Family 6 (renumber check, tdd-74 §2.5). These pins stay RED until then;
    // pins 4 and 6 are the GREEN guards (regression + over-fire).
    // Fixture provenance: `testdata/xw_orphan/PROVENANCE.md`.
    const XW_ORPHAN_VLLM_PYDANTIC_400_BODY: &str =
        include_str!("../testdata/xw_orphan/vllm_pydantic_400_body.json");
    const XW_ORPHAN_AZURE_CALLID_400_BODY: &str =
        include_str!("../testdata/xw_orphan/azure_callid_orphan_400_body.json");
    // Synthetic dotted rendering (tdd-74 §3 case 3; sha256:8232e1526b1d,
    // recomputed first-hand at dispatch).
    const XW_ORPHAN_DOTTED_ARRAY_TOO_LONG: &str =
        "input.7.content: array too long. Expected an array with maximum length 0, but got an array with length 1";

    /// The exact pipeline the classifier sees: `user_facing_api_error_message`
    /// (structured-envelope unwrap + 280-cap) over the raw body bytes.
    fn xw_orphan_api_err(status: StatusCode, body: &str) -> (SamplingError, String) {
        let message = user_facing_api_error_message(status, body.as_bytes());
        (
            SamplingError::Api {
                status,
                message: message.clone(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: parse_error_code(body.as_bytes()),
            },
            message,
        )
    }

    #[test]
    fn xw_orphan_vllm_pydantic_400_is_model_bound() {
        // Class (a), LIVE: vLLM pydantic dotted-id (litellm
        // Responses→ChatCompletions shim, glm-5.2 group, incident 01a0b07a).
        // Needle walk on the classifier view (lowercased 237-char fragment,
        // under the 280 cap, verbatim): F1 `encrypted*` ✗ · F2
        // `thinking`+`signature` ✗ · F3 `input[` ✗ (`input_value=` has no bare
        // bracket) + `.id` ✗ + `invalid` ✗ · F4 `item` ✗ + `not found`/`does
        // not exist` ✗ · F5 `input[` ✗ → no family → Fatal today.
        let (err, seen) = xw_orphan_api_err(StatusCode::BAD_REQUEST, XW_ORPHAN_VLLM_PYDANTIC_400_BODY);
        assert!(
            err.is_model_bound_history_error(),
            "XW-ORPHAN-1 class (a): the vLLM pydantic dotted-id 400 must classify model-bound (arm B: `validation error` + `messages.`); classifier saw (237 chars, uncapped): {seen}"
        );
        // 280-cap shape (house discipline, 503-region pattern): the needles
        // sit at char ~67 (`validation error`) and ~110 (`messages.`) of the
        // litellm-prefixed view, so the capped form must classify too.
        let capped = seen[..seen.len().min(MAX_USER_ERROR_BODY_CHARS)].to_string();
        let capped_err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: capped,
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            capped_err.is_model_bound_history_error(),
            "XW-ORPHAN-1 class (a): the 280-char capped shape must still classify (needles survive the cap)"
        );
    }

    #[test]
    fn xw_orphan_azure_callid_orphan_400_is_model_bound() {
        // Class (b), PREDICTION (qwen preplan H-1, no live capture): a
        // dangling `function_call_output` — the orphan the pair-aware strip
        // and the send-boundary guard exist to keep off the wire. Needle
        // walk (lowercased 107-char message, uncapped): `input[` ✓,
        // `invalid` ✓, but `.id` ✗ (`.call_id` — the char before `id` is `_`,
        // not `.`) → F3 needs all three → no family today; `item` ✗ so F4
        // misses too.
        let (err, seen) = xw_orphan_api_err(StatusCode::BAD_REQUEST, XW_ORPHAN_AZURE_CALLID_400_BODY);
        assert!(
            err.is_model_bound_history_error(),
            "XW-ORPHAN-1 class (b): the Azure `.call_id`-orphan 400 must classify model-bound (arm A: `input[` + `.call_id` + `invalid`); classifier saw: {seen}"
        );
    }

    #[test]
    fn xw_orphan_dotted_array_too_long_is_model_bound() {
        // Dotted array-too-long rendering (the F5-extension target): the
        // bracketed live-wire shape is F5-covered at base (regression pin
        // below); the DOTTED `input.N.content` phrasing misses F5 today
        // because F5 keys `input[`. Needle walk (lowercased, uncapped):
        // `input[` ✗ (dotted, no bracket) → F5 ✗; F3/F4 miss as well.
        let body = format!(
            r#"{{"error":{{"message":"{XW_ORPHAN_DOTTED_ARRAY_TOO_LONG}","type":null,"param":null,"code":"400"}}}}"#
        );
        let (err, seen) = xw_orphan_api_err(StatusCode::BAD_REQUEST, &body);
        assert!(
            err.is_model_bound_history_error(),
            "XW-ORPHAN-1 dotted: `input.` + `array too long` must classify model-bound (F5 extension); classifier saw: {seen}"
        );
    }

    #[test]
    fn xw_orphan_bracketed_array_too_long_still_f5() {
        // Regression pin (stays GREEN through the fix): the base live-wire
        // bracketed body (the `error.rs` live-wire pin shape; inner message
        // sha256:c1651b7d3aff) must keep classifying via F5 after the dotted
        // extension (`input[` | `input.`) lands — the extension is additive.
        let body = r#"{"error":{"message":"litellm.BadRequestError: AzureException BadRequestError - {\n  \"error\": {\n    \"message\": \"Invalid 'input[7].content': array too long. Expected an array with maximum length 0, but got an array with length 1 instead.\",\n    \"type\": \"invalid_request_error\",\n    \"param\": \"input[7].content\",\n    \"code\": \"array_above_max_length\"\n  }\n}. Received Model Group=gpt-5.6-sol\nAvailable Model Group Fallbacks=None","type":null,"param":null,"code":"400"}}"#;
        let (err, _seen) = xw_orphan_api_err(StatusCode::BAD_REQUEST, body);
        assert!(
            err.is_model_bound_history_error(),
            "XW-ORPHAN-1 regression: the bracketed live-wire 400 must stay F5-classified (dotted extension must not regress it)"
        );
    }

    #[test]
    fn xw_orphan_generic_400_stays_unclassified() {
        // Negative control (F1-503 double-key discipline; stays GREEN): each
        // half of arm B alone must NOT classify — `validation error` without a
        // dotted loc is a generic phrasing, and `messages.` without
        // `validation error` is a field mention, not the vLLM pydantic
        // dotted shape. Guards arm B from over-firing onto the 400 tail.
        for (label, message) in [
            (
                "`validation error` alone (no dotted loc)",
                "litellm.BadRequestError: 39 validation errors for the request",
            ),
            (
                "`messages.` alone (no `validation error`)",
                "request rejected: messages.8 field is malformed",
            ),
        ] {
            let err = SamplingError::Api {
                status: StatusCode::BAD_REQUEST,
                message: message.to_string(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
            };
            assert!(
                !err.is_model_bound_history_error(),
                "XW-ORPHAN-1 negative control: {label} must NOT classify model-bound (arm B is double-keyed)"
            );
        }
    }

    #[test]
    fn image_processing_error_direct_400_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Could not process image: unsupported format".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(err.is_image_processing_error());
        assert!(!err.is_model_bound_history_error());
    }

    #[test]
    fn image_processing_error_500_wrapped_detected() {
        let err = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "upstream error: 400 Bad Request: Could not process image".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(err.is_image_processing_error());
    }

    #[test]
    fn image_processing_error_unrelated_400_not_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Invalid model parameter".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!err.is_image_processing_error());
    }

    #[test]
    fn image_processing_error_unrelated_500_not_detected() {
        let err = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(!err.is_image_processing_error());
    }

    #[test]
    fn image_processing_error_wrong_status_not_detected() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_GATEWAY,
            message: "Could not process image".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_image_processing_error(),
            "only 400 and 500 should match"
        );
    }

    #[test]
    fn image_processing_error_400_is_not_retryable_standalone() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "Could not process image".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        assert!(
            !err.is_retryable(),
            "direct 400 must not be retryable by is_retryable()"
        );
    }

    fn api_400(message: &str) -> SamplingError {
        SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        }
    }

    fn api_400_with_code(message: &str, code: &str) -> SamplingError {
        SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: Some(ApiErrorCode::parse(code)),
        }
    }

    /// The semantic code classifies on its own, whatever the message says; a different code with the same wording never does.
    #[test]
    fn image_processing_error_code_is_the_signal() {
        let unknown_wording = "some future wording without the legacy phrase";
        assert!(
            api_400_with_code(unknown_wording, INVALID_IMAGE_ERROR_CODE)
                .is_image_processing_error()
        );
        // A 500 with a code: the shape every synthesized mid-stream failure takes (Responses-stream events and info round trips land on 500)
        // The status gate must admit it or mid-stream recovery silently dies
        assert!(
            SamplingError::Api {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: unknown_wording.into(),
                model_metadata: None,
                retry_after_secs: None,
                should_retry: None,
                error_code: Some(ApiErrorCode::InvalidImage),
            }
            .is_image_processing_error()
        );
        assert!(
            !api_400_with_code(unknown_wording, "context_length_exceeded")
                .is_image_processing_error()
        );
        // Deliberate: server prose without the code does not strip
        // Any server new enough to emit these rejections stamps the code
        assert!(!api_400("Invalid base64-encoded image.").is_image_processing_error());
    }

    #[test]
    fn image_processing_error_image_content_path_detected() {
        let err = api_400(
            "invalid_request_error: messages.0.content.4.image.source.base64.data: \
             At least one of the image dimensions exceed max allowed size for \
             many-image requests: 2000 pixels",
        );
        assert!(err.is_image_processing_error());
    }

    #[test]
    fn image_processing_error_non_image_invalid_request_not_detected() {
        let err = api_400("invalid_request_error: messages.1.content.0.text: field required");
        assert!(!err.is_image_processing_error());
    }

    /// Mid-stream rejections strip only on the code: the server stamps stream errors too, and there is no legacy phrase to honor there.
    #[test]
    fn image_processing_error_stream_requires_code() {
        let stream = |code: Option<&str>, message: &str| SamplingError::StreamError {
            error_type: "invalid_request_error".into(),
            message: message.into(),
            code: code.map(ApiErrorCode::parse),
        };
        assert!(stream(Some(INVALID_IMAGE_ERROR_CODE), "anything").is_image_processing_error());
        assert!(!stream(Some("context_length_exceeded"), "anything").is_image_processing_error());
        // Deliberate: message text alone must not trigger a destructive strip
        assert!(
            !stream(None, "Base64 string of provided image cannot be decoded.")
                .is_image_processing_error()
        );
    }

    #[test]
    fn parse_error_code_extracts_semantic_codes() {
        // Nested envelope with a code.
        assert_eq!(
            parse_error_code(
                br#"{"error":{"message":"bad image","type":"invalid_request_error","code":"invalid_image"}}"#
            ),
            Some(ApiErrorCode::InvalidImage)
        );
        // Nested envelope without a code.
        assert_eq!(
            parse_error_code(br#"{"error":{"message":"boom","type":"server_error"}}"#),
            None
        );
        // Flat envelope: the server's non-stream image rejections arrive in this shape; only the exact semantic code is surfaced
        assert_eq!(
            parse_error_code(br#"{"code":"invalid_image","error":"Invalid PNG image."}"#),
            Some(ApiErrorCode::InvalidImage)
        );
        // Flat envelope's usual occupants (gRPC kebab codes, type slots) never surface
        assert_eq!(
            parse_error_code(br#"{"code":"invalid-argument","error":"bad request"}"#),
            None
        );
        assert_eq!(
            parse_error_code(br#"{"code":"server_error","error":"Service unavailable."}"#),
            None
        );
        // Unstructured bodies.
        assert_eq!(parse_error_code(b"<html>502</html>"), None);
    }

    #[test]
    fn try_parse_stream_error_captures_code() {
        let data = r#"{"error":{"message":"bad image","type":"invalid_request_error","code":"invalid_image"}}"#;
        match try_parse_stream_error(data) {
            Some(SamplingError::StreamError { code, .. }) => {
                assert_eq!(code, Some(ApiErrorCode::InvalidImage));
            }
            other => panic!("expected StreamError, got {other:?}"),
        }
    }

    fn api_status_err(code: u16) -> SamplingError {
        SamplingError::Api {
            status: StatusCode::from_u16(code).unwrap(),
            message: status_user_message(StatusCode::from_u16(code).unwrap()),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        }
    }

    #[test]
    fn transient_5xx_is_retryable_but_origin_tls_is_not() {
        // Cloudflare edge pages (520-524, 530), upstream overload (529), and non-CF 5xx like 501/507; the rule is any 5xx, not a code list
        for code in [501u16, 507, 520, 521, 522, 523, 524, 529, 530] {
            assert!(
                api_status_err(code).is_retryable(),
                "{code} must be retried"
            );
        }
        // Origin TLS: a broken certificate never clears on its own.
        for code in [525u16, 526] {
            assert!(
                !api_status_err(code).is_retryable(),
                "origin-TLS {code} must not be retried"
            );
        }
    }
}
