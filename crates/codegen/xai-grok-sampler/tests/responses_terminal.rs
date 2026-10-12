//! Runnable contract for the synthetic Responses terminal corpus.
//!
//! This test intentionally owns only fixture integrity and replay plumbing.
//! Production terminal classification begins in EMPTY-RETRY-1 Task 2.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use xai_grok_sampler::{
    ApiBackend, NamedEvent, OutputPresence, RequestId, RetryLane, RetryPolicy, SamplerActor,
    SamplerConfig, SamplingEvent, TerminalClass, TerminalFacts, TerminalReason, classify_error,
    classify_terminal,
};
use xai_grok_sampling_types::{
    ContentPart, ConversationItem, ConversationRequest, LengthPolicy, SamplingError, StopReason,
    TokenUsage, UserItem,
};
use xai_grok_test_support::{
    InferenceEndpoint, InferenceRequestMatcher, MockInferenceServer, ScriptedResponse, SseEvent,
};

const TERMINAL_IDS: [&str; 12] = [
    "V1", "V2", "N1", "E1", "E2", "E3", "E4", "L1", "L2", "T1", "F1", "C1",
];
const PRIMARY_MODEL: &str = "fixture-primary";
const FALLBACK_MODEL: &str = "gpt-5.6-terra";
const OUTBOUND_REASONING_CIPHERTEXT: &str = "synthetic-first-class-baseline-ciphertext-v1";
const OUTBOUND_CARRIER_CIPHERTEXT: &str = "synthetic-codex-raw-input-carrier-v1";
const FALLBACK_V1_REASONING_CIPHERTEXT: &str = "synthetic-first-class-fallback-v1-ciphertext";
const FALLBACK_V2_REASONING_CIPHERTEXT: &str = "synthetic-first-class-fallback-v2-ciphertext";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FixtureWire {
    Responses,
    Chat,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalCase {
    id: String,
    wire: FixtureWire,
    dialect: Option<String>,
    terminal_class: String,
    lane: String,
    frame_side_effect_free: bool,
    raw_output_presence: String,
    output_len: Option<usize>,
    raw_text_bytes: usize,
    trimmed_text_bytes: usize,
    message_items: usize,
    reasoning_items: usize,
    tool_activity: bool,
    tool_items_complete: bool,
    usage: Option<Value>,
    cancel_after_event: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProviderState {
    None,
    SyntheticEncryptedContent,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReplayGate {
    visible_output_observed: bool,
    tool_activity: bool,
    irreversible_effect: bool,
    retry_only_before_output: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FallbackSequence {
    id: String,
    cap: usize,
    fallback_available: bool,
    primary_cases: Vec<String>,
    fallback_case: Option<String>,
    replay_gate: ReplayGate,
    baseline_provider_state: ProviderState,
    fallback_response_provider_state: ProviderState,
    baseline_provider_items: Vec<Value>,
    fallback_response_provider_items: Vec<Value>,
    expected_attempt_indices: Vec<usize>,
    expected_attempt_roles: Vec<String>,
    expected_attempt_models: Vec<String>,
    expected_terminal_class: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFrame {
    #[serde(default)]
    event: Option<String>,
    data: Value,
}

struct FixtureReplay {
    wire: FixtureWire,
    frames: Vec<FixtureFrame>,
}

impl FixtureReplay {
    fn load(root: &Path, case: &TerminalCase) -> Self {
        Self {
            wire: case.wire,
            frames: read_json(&root.join(&case.id).join("events.json")),
        }
    }

    fn endpoint(&self) -> &'static str {
        match self.wire {
            FixtureWire::Responses => "/v1/responses",
            FixtureWire::Chat => "/v1/chat/completions",
        }
    }

    fn backend(&self) -> ApiBackend {
        match self.wire {
            FixtureWire::Responses => ApiBackend::Responses,
            FixtureWire::Chat => ApiBackend::ChatCompletions,
        }
    }

    fn inference_endpoint(&self) -> InferenceEndpoint {
        match self.wire {
            FixtureWire::Responses => InferenceEndpoint::Responses,
            FixtureWire::Chat => InferenceEndpoint::ChatCompletions,
        }
    }

    fn scripted_response(&self) -> ScriptedResponse {
        let events = self
            .frames
            .iter()
            .map(|frame| {
                let data = serde_json::to_string(&frame.data).expect("serialize fixture frame");
                match &frame.event {
                    Some(event) => SseEvent::with_event(event.clone(), data),
                    None => SseEvent::data(data),
                }
            })
            .collect();
        ScriptedResponse::sse(events)
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/responses_terminal")
}

fn read_json<T: DeserializeOwned>(path: &Path) -> T {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse fixture {}: {error}", path.display()))
}

fn case<'a>(cases: &'a [TerminalCase], id: &str) -> &'a TerminalCase {
    cases
        .iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("missing terminal oracle {id}"))
}

fn frame_marker(wire: FixtureWire, frame: &FixtureFrame) -> String {
    match wire {
        FixtureWire::Responses => frame.data["type"]
            .as_str()
            .expect("Responses frame has a type")
            .to_owned(),
        FixtureWire::Chat => match frame.data.pointer("/choices/0/finish_reason") {
            Some(Value::String(reason)) => format!("chat.{reason}"),
            _ => "chat.delta".to_owned(),
        },
    }
}

fn expected_frame_order(id: &str) -> &'static [&'static str] {
    match id {
        "V1" => &[
            "response.created",
            "response.output_text.delta",
            "response.completed",
        ],
        "V2" => &[
            "response.created",
            "response.reasoning_summary_text.delta",
            "response.output_item.added",
            "response.completed",
        ],
        "N1" => &[
            "response.created",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.completed",
        ],
        "E1" | "E4" | "C1" => &["response.created", "response.completed"],
        "E2" => &[
            "response.created",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.completed",
        ],
        "E3" => &[
            "response.created",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.delta",
            "response.completed",
        ],
        "L1" => &[
            "response.created",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.incomplete",
        ],
        "L2" => &["chat.delta", "chat.length"],
        "T1" => &["response.created"],
        "F1" => &["response.created", "response.failed"],
        other => panic!("unrecognized fixture id {other}"),
    }
}

fn validate_response_frame(frame: &FixtureFrame, index: usize, case_id: &str) {
    let object = frame
        .data
        .as_object()
        .unwrap_or_else(|| panic!("{case_id} frame {index} data is not a JSON object"));
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{case_id} frame {index} has no type"));
    assert!(
        event_type.starts_with("response."),
        "{case_id} frame {index} is not a Responses event: {event_type}"
    );
    assert_eq!(
        object.get("sequence_number").and_then(Value::as_u64),
        Some(index as u64),
        "{case_id} Responses sequence"
    );

    if matches!(
        event_type,
        "response.created" | "response.completed" | "response.incomplete" | "response.failed"
    ) {
        let response = object
            .get("response")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("{case_id} {event_type} has no response object"));
        assert!(response.get("id").is_some_and(Value::is_string));
        assert_eq!(
            response.get("object").and_then(Value::as_str),
            Some("response")
        );
        assert!(response.get("created_at").is_some_and(Value::is_number));
        assert!(response.get("model").is_some_and(Value::is_string));
        assert!(response.get("status").is_some_and(Value::is_string));
        assert!(response.get("output").is_some_and(Value::is_array));
    }
}

fn validate_chat_frame(frame: &FixtureFrame, index: usize, case_id: &str) {
    let object = frame
        .data
        .as_object()
        .unwrap_or_else(|| panic!("{case_id} frame {index} data is not a JSON object"));
    assert!(object.get("id").is_some_and(Value::is_string));
    assert_eq!(
        object.get("object").and_then(Value::as_str),
        Some("chat.completion.chunk")
    );
    assert!(object.get("created").is_some_and(Value::is_number));
    assert!(object.get("model").is_some_and(Value::is_string));
    let choices = object
        .get("choices")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{case_id} frame {index} has no choices array"));
    assert!(!choices.is_empty(), "{case_id} frame {index} has no choice");
    for choice in choices {
        assert!(choice.get("index").is_some_and(Value::is_number));
        assert!(choice.get("delta").is_some_and(Value::is_object));
        assert!(
            choice
                .get("finish_reason")
                .is_some_and(|reason| { reason.is_null() || reason.is_string() })
        );
    }
}

fn observed_frames<'a>(case: &TerminalCase, replay: &'a FixtureReplay) -> &'a [FixtureFrame] {
    match case.cancel_after_event {
        Some(index) => replay
            .frames
            .get(..=index)
            .unwrap_or_else(|| panic!("{} cancel index is outside its fixture", case.id)),
        None => &replay.frames,
    }
}

fn terminal_response(frames: &[FixtureFrame]) -> Option<&Value> {
    frames.iter().rev().find_map(|frame| {
        matches!(
            frame.data.get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed")
        )
        .then(|| &frame.data["response"])
    })
}

fn response_items(frames: &[FixtureFrame]) -> &[Value] {
    terminal_response(frames)
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn response_text(items: &[Value]) -> String {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flat_map(|content| content.iter())
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect()
}

fn terminal_output_text<'a>(case_id: &str, replay: &'a FixtureReplay) -> &'a str {
    let response = terminal_response(&replay.frames)
        .unwrap_or_else(|| panic!("{case_id} has no terminal response"));
    let content = response
        .pointer("/output/0/content")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{case_id} has no first message content array"));
    assert_eq!(content.len(), 1, "{case_id} message content part count");
    assert_eq!(
        content[0].get("type").and_then(Value::as_str),
        Some("output_text"),
        "{case_id} message content part type"
    );
    content[0]
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{case_id} has no explicit output_text text"))
}

fn response_text_deltas<'a>(case_id: &str, replay: &'a FixtureReplay) -> Vec<&'a str> {
    replay
        .frames
        .iter()
        .filter(|frame| {
            frame.data.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        })
        .map(|frame| {
            frame
                .data
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{case_id} output_text delta is not a string"))
        })
        .collect()
}

fn has_tool_activity(replay: &FixtureReplay) -> bool {
    replay.frames.iter().any(|frame| {
        matches!(
            frame.data.get("type").and_then(Value::as_str),
            Some("response.output_item.added" | "response.function_call_arguments.delta")
        ) || frame
            .data
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
            || frame
                .data
                .pointer("/response/output")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call")
                    })
                })
    })
}

fn tool_items_complete(replay: &FixtureReplay) -> bool {
    if !has_tool_activity(replay) {
        return true;
    }
    let mut arguments = Vec::new();
    for frame in &replay.frames {
        if let Some(items) = frame
            .data
            .pointer("/response/output")
            .and_then(Value::as_array)
        {
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    if item.get("status").and_then(Value::as_str) == Some("in_progress") {
                        return false;
                    }
                    if let Some(value) = item.get("arguments").and_then(Value::as_str) {
                        arguments.push(value.to_owned());
                    }
                }
            }
        }
        if let Some(calls) = frame
            .data
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                if let Some(value) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    arguments.push(value.to_owned());
                }
            }
        }
    }
    !arguments.is_empty()
        && arguments
            .iter()
            .all(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
}

fn observed_output(frames: &[FixtureFrame]) -> (&'static str, Option<usize>) {
    let Some(response) = terminal_response(frames) else {
        return ("missing", None);
    };
    match response.get("output") {
        None => ("missing", None),
        Some(Value::Null) => ("null", None),
        Some(Value::Array(output)) => ("array", Some(output.len())),
        Some(other) => panic!("terminal output has invalid shape: {other}"),
    }
}

fn observed_usage(case: &TerminalCase, frames: &[FixtureFrame]) -> Option<Value> {
    match case.wire {
        FixtureWire::Responses => terminal_response(frames)
            .and_then(|response| response.get("usage"))
            .filter(|usage| !usage.is_null())
            .cloned(),
        FixtureWire::Chat => frames
            .iter()
            .rev()
            .find_map(|frame| frame.data.get("usage"))
            .filter(|usage| !usage.is_null())
            .cloned(),
    }
}

fn validate_case_metadata(case: &TerminalCase, replay: &FixtureReplay) {
    let expected_class_and_lane = match case.id.as_str() {
        "V1" | "N1" => ("valid_text", "none"),
        "V2" => ("valid_tool", "none"),
        "E1" => ("exact_empty", "semantic"),
        "E2" => ("whitespace_empty", "semantic"),
        "E3" => ("reasoning_only", "semantic"),
        "E4" => ("zero_output", "semantic"),
        "L1" | "L2" => ("length", "length"),
        "T1" => ("transport_eof", "transport"),
        "F1" => ("provider_failed", "transport"),
        "C1" => ("cancelled", "none"),
        other => panic!("unrecognized case {other}"),
    };
    assert_eq!(
        (case.terminal_class.as_str(), case.lane.as_str()),
        expected_class_and_lane,
        "{} terminal oracle",
        case.id
    );

    match case.wire {
        FixtureWire::Responses => assert!(
            matches!(case.dialect.as_deref(), Some("openai" | "xai" | "codex")),
            "{} has an invalid Responses dialect",
            case.id
        ),
        FixtureWire::Chat => assert_eq!(case.dialect, None, "Chat has no Responses dialect"),
    }

    let order: Vec<_> = replay
        .frames
        .iter()
        .map(|frame| frame_marker(case.wire, frame))
        .collect();
    assert_eq!(
        order,
        expected_frame_order(&case.id),
        "{} frame order",
        case.id
    );
    for (index, frame) in replay.frames.iter().enumerate() {
        assert!(
            frame.event.as_ref().is_none_or(|event| !event.is_empty()),
            "{} frame {index} has an empty SSE event name",
            case.id
        );
        match case.wire {
            FixtureWire::Responses => validate_response_frame(frame, index, &case.id),
            FixtureWire::Chat => validate_chat_frame(frame, index, &case.id),
        }
    }

    let observed = observed_frames(case, replay);
    let items = response_items(observed);
    let text = response_text(items);
    assert_eq!(
        text.len(),
        case.raw_text_bytes,
        "{} raw text bytes",
        case.id
    );
    assert_eq!(
        text.trim().len(),
        case.trimmed_text_bytes,
        "{} trimmed text bytes",
        case.id
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
            .count(),
        case.message_items,
        "{} message item count",
        case.id
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .count(),
        case.reasoning_items,
        "{} reasoning item count",
        case.id
    );
    assert_eq!(
        has_tool_activity(replay),
        case.tool_activity,
        "{} tool activity",
        case.id
    );
    assert_eq!(
        tool_items_complete(replay),
        case.tool_items_complete,
        "{} tool completeness",
        case.id
    );
    assert_eq!(
        case.frame_side_effect_free, !case.tool_activity,
        "{} side-effect prerequisite",
        case.id
    );
    assert_eq!(
        observed_output(observed),
        (case.raw_output_presence.as_str(), case.output_len),
        "{} raw output shape",
        case.id
    );
    assert_eq!(
        observed_usage(case, observed),
        case.usage,
        "{} usage oracle",
        case.id
    );
}

fn sequence<'a>(sequences: &'a [FallbackSequence], id: &str) -> &'a FallbackSequence {
    sequences
        .iter()
        .find(|sequence| sequence.id == id)
        .unwrap_or_else(|| panic!("missing fallback sequence {id}"))
}

fn replay_gate_allows_replay(gate: ReplayGate) -> bool {
    !gate.tool_activity
        && !gate.irreversible_effect
        && !(gate.retry_only_before_output && gate.visible_output_observed)
}

fn first_class_ciphertexts(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .filter_map(|item| item.get("encrypted_content").and_then(Value::as_str))
        .collect()
}

fn codex_raw_input_ciphertexts(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("backend_tool_call")
                && item.pointer("/kind/tool_type").and_then(Value::as_str)
                    == Some("codex_raw_input")
        })
        .filter_map(|item| {
            item.pointer("/kind/raw/encrypted_content")
                .and_then(Value::as_str)
        })
        .collect()
}

fn validate_provider_state(sequence: &FallbackSequence) {
    for (label, items) in [
        ("baseline", &sequence.baseline_provider_items),
        (
            "fallback response",
            &sequence.fallback_response_provider_items,
        ),
    ] {
        serde_json::from_value::<Vec<ConversationItem>>(Value::Array(items.clone()))
            .unwrap_or_else(|error| panic!("{} {label} provider items: {error}", sequence.id));
    }

    let baseline_reasoning = first_class_ciphertexts(&sequence.baseline_provider_items);
    let baseline_carriers = codex_raw_input_ciphertexts(&sequence.baseline_provider_items);
    match sequence.baseline_provider_state {
        ProviderState::None => assert!(
            sequence.baseline_provider_items.is_empty(),
            "{} declares no baseline provider state but carries provider items",
            sequence.id
        ),
        ProviderState::SyntheticEncryptedContent => {
            assert_eq!(
                baseline_reasoning.len(),
                1,
                "{} synthetic baseline has one first-class ciphertext",
                sequence.id
            );
            assert_eq!(
                baseline_carriers.len(),
                1,
                "{} synthetic baseline has one opaque carrier ciphertext",
                sequence.id
            );
        }
    }

    let fallback_reasoning = first_class_ciphertexts(&sequence.fallback_response_provider_items);
    match sequence.fallback_response_provider_state {
        ProviderState::None => assert!(
            sequence.fallback_response_provider_items.is_empty(),
            "{} declares no fallback response provider state but carries provider items",
            sequence.id
        ),
        ProviderState::SyntheticEncryptedContent => {
            assert_eq!(
                fallback_reasoning.len(),
                1,
                "{} synthetic fallback response has one first-class ciphertext",
                sequence.id
            );
            assert!(
                sequence.fallback_case.is_some(),
                "{} has fallback response state without a fallback attempt",
                sequence.id
            );
        }
    }
}

fn validate_fallback_sequences(cases: &[TerminalCase], sequences: &[FallbackSequence]) {
    let expected_sequence_ids = [
        "semantic-e1-terra-success",
        "semantic-e2-terra-success",
        "semantic-e3-terra-success",
        "semantic-e4-terra-success",
        "visible-output-policy-disabled-allows-terra",
        "transport-eof-excludes-terra",
        "provider-failure-excludes-terra",
        "responses-length-excludes-terra",
        "chat-length-excludes-terra",
        "cancelled-excludes-terra",
        "tool-activity-excludes-terra",
        "visible-output-policy-excludes-terra",
        "irreversible-effect-excludes-terra",
        "terra-unavailable-keeps-final-primary-slot",
        "terra-semantic-failure-is-terminal",
        "terra-eof-failure-is-terminal",
        "terra-provider-failure-is-terminal",
        "cap-one-is-primary-only",
        "mixed-no-fallback-stays-with-primary",
        "outbound-ciphertext-sanitizes-reasoning-only",
        "accepted-v1-handback-sanitizes-ciphertext",
        "accepted-v2-handback-sanitizes-ciphertext",
    ];
    assert_eq!(
        sequences
            .iter()
            .map(|sequence| sequence.id.as_str())
            .collect::<Vec<_>>(),
        expected_sequence_ids
    );
    assert_eq!(
        sequences
            .iter()
            .map(|sequence| sequence.id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        sequences.len(),
        "fallback sequence IDs must be unique"
    );

    let cases_by_id: BTreeMap<_, _> = cases.iter().map(|case| (case.id.as_str(), case)).collect();
    for sequence in sequences {
        assert!(sequence.cap > 0, "{} has a zero attempt cap", sequence.id);
        assert!(
            !sequence.primary_cases.is_empty(),
            "{} has no primary case",
            sequence.id
        );
        let attempt_count =
            sequence.primary_cases.len() + usize::from(sequence.fallback_case.is_some());
        assert!(
            attempt_count <= sequence.cap,
            "{} exceeds its attempt cap",
            sequence.id
        );
        assert_eq!(
            sequence.expected_attempt_indices,
            (1..=attempt_count).collect::<Vec<_>>(),
            "{} attempt indices",
            sequence.id
        );
        assert_eq!(
            sequence.expected_attempt_roles.len(),
            attempt_count,
            "{} role count",
            sequence.id
        );
        assert_eq!(
            sequence.expected_attempt_models.len(),
            attempt_count,
            "{} model count",
            sequence.id
        );
        assert!(
            sequence
                .primary_cases
                .iter()
                .all(|id| cases_by_id.contains_key(id.as_str())),
            "{} references an unknown primary case",
            sequence.id
        );
        assert!(
            sequence
                .fallback_case
                .as_deref()
                .is_none_or(|id| cases_by_id.contains_key(id)),
            "{} references an unknown fallback case",
            sequence.id
        );
        assert!(
            sequence.fallback_available || sequence.fallback_case.is_none(),
            "{} schedules unavailable fallback",
            sequence.id
        );
        assert!(
            sequence.expected_attempt_roles[..sequence.primary_cases.len()]
                .iter()
                .all(|role| role == "primary"),
            "{} primary role order",
            sequence.id
        );
        assert!(
            sequence.expected_attempt_models[..sequence.primary_cases.len()]
                .iter()
                .all(|model| model == PRIMARY_MODEL),
            "{} primary model order",
            sequence.id
        );

        if sequence.fallback_case.is_some() {
            assert!(
                replay_gate_allows_replay(sequence.replay_gate),
                "{} schedules fallback through a closed replay gate",
                sequence.id
            );
            assert!(
                sequence.cap >= 3,
                "{} uses a vacuous fallback cap",
                sequence.id
            );
            assert!(
                sequence.primary_cases.len() >= 2,
                "{} lacks two primary attempts before fallback",
                sequence.id
            );
            assert_eq!(
                attempt_count, sequence.cap,
                "{} fallback must occupy the final slot",
                sequence.id
            );
            assert!(
                matches!(
                    sequence.primary_cases.last().map(String::as_str),
                    Some("E1" | "E2" | "E3" | "E4")
                ),
                "{} fallback trigger is not E1-E4",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_indices[attempt_count - 2],
                sequence.cap - 1,
                "{} penultimate primary index",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_indices[attempt_count - 1],
                sequence.cap,
                "{} fallback index",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_roles[attempt_count - 2].as_str(),
                "primary",
                "{} penultimate role",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_models[attempt_count - 2].as_str(),
                PRIMARY_MODEL,
                "{} penultimate model",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_roles.last().map(String::as_str),
                Some("fallback"),
                "{} fallback role",
                sequence.id
            );
            assert_eq!(
                sequence.expected_attempt_models.last().map(String::as_str),
                Some(FALLBACK_MODEL),
                "{} fallback model",
                sequence.id
            );
        } else {
            assert!(
                sequence
                    .expected_attempt_roles
                    .iter()
                    .all(|role| role == "primary"),
                "{} contains a fallback role without a fallback case",
                sequence.id
            );
            assert!(
                sequence
                    .expected_attempt_models
                    .iter()
                    .all(|model| model == PRIMARY_MODEL),
                "{} contains a fallback model without a fallback case",
                sequence.id
            );
        }

        let terminal_id = sequence
            .fallback_case
            .as_deref()
            .or_else(|| sequence.primary_cases.last().map(String::as_str))
            .expect("non-empty sequence");
        assert_eq!(
            cases_by_id[terminal_id].terminal_class, sequence.expected_terminal_class,
            "{} terminal class references its final case",
            sequence.id
        );
        validate_provider_state(sequence);
    }

    for semantic_id in ["E1", "E2", "E3", "E4"] {
        assert!(
            sequences.iter().any(|sequence| {
                sequence.fallback_available
                    && sequence.primary_cases.len() >= 2
                    && sequence.primary_cases.last().map(String::as_str) == Some(semantic_id)
                    && sequence.fallback_case.as_deref() == Some("V1")
                    && sequence.cap >= 3
            }),
            "missing non-vacuous Terra-success sequence for {semantic_id}"
        );
    }

    for (id, expected_primary_cases) in [
        ("transport-eof-excludes-terra", &["T1", "V1"][..]),
        ("provider-failure-excludes-terra", &["F1", "V1"][..]),
        ("responses-length-excludes-terra", &["L1"][..]),
        ("chat-length-excludes-terra", &["L2"][..]),
        ("cancelled-excludes-terra", &["C1"][..]),
        ("tool-activity-excludes-terra", &["V2"][..]),
        ("visible-output-policy-excludes-terra", &["E1"][..]),
        ("irreversible-effect-excludes-terra", &["E1"][..]),
    ] {
        let excluded = sequence(sequences, id);
        assert!(
            excluded.fallback_available,
            "{id} must test exclusion with Terra available"
        );
        assert_eq!(
            excluded
                .primary_cases
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected_primary_cases,
            "{id} primary cases"
        );
        assert_eq!(excluded.fallback_case, None, "{id} schedules no Terra");
    }

    let unavailable = sequence(sequences, "terra-unavailable-keeps-final-primary-slot");
    assert!(!unavailable.fallback_available);
    assert_eq!(
        unavailable
            .primary_cases
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["E4", "E4", "V1"]
    );
    assert_eq!(unavailable.fallback_case, None);

    let cap_one = sequence(sequences, "cap-one-is-primary-only");
    assert_eq!(cap_one.cap, 1);
    assert_eq!(cap_one.expected_attempt_indices, [1]);
    assert_eq!(cap_one.expected_attempt_roles, ["primary"]);
    assert_eq!(cap_one.expected_attempt_models, [PRIMARY_MODEL]);
    assert_eq!(cap_one.fallback_case, None);

    let open_gate = ReplayGate {
        visible_output_observed: false,
        tool_activity: false,
        irreversible_effect: false,
        retry_only_before_output: true,
    };
    assert_eq!(
        sequence(sequences, "semantic-e1-terra-success").replay_gate,
        open_gate
    );
    assert!(replay_gate_allows_replay(open_gate));

    let visible_block = sequence(sequences, "visible-output-policy-excludes-terra");
    assert_eq!(
        visible_block.replay_gate,
        ReplayGate {
            visible_output_observed: true,
            ..open_gate
        }
    );
    assert!(!replay_gate_allows_replay(visible_block.replay_gate));

    let policy_disabled = sequence(sequences, "visible-output-policy-disabled-allows-terra");
    assert_eq!(
        policy_disabled.replay_gate,
        ReplayGate {
            retry_only_before_output: false,
            ..visible_block.replay_gate
        }
    );
    assert!(replay_gate_allows_replay(policy_disabled.replay_gate));

    let tool_block = sequence(sequences, "tool-activity-excludes-terra");
    assert_eq!(
        tool_block.replay_gate,
        ReplayGate {
            tool_activity: true,
            ..open_gate
        }
    );
    assert!(!replay_gate_allows_replay(tool_block.replay_gate));

    let effect_block = sequence(sequences, "irreversible-effect-excludes-terra");
    assert_eq!(
        effect_block.replay_gate,
        ReplayGate {
            irreversible_effect: true,
            ..open_gate
        }
    );
    assert!(!replay_gate_allows_replay(effect_block.replay_gate));

    let outbound = sequence(sequences, "outbound-ciphertext-sanitizes-reasoning-only");
    assert_eq!(
        outbound.baseline_provider_state,
        ProviderState::SyntheticEncryptedContent
    );
    assert_eq!(
        outbound.fallback_response_provider_state,
        ProviderState::None
    );
    assert_eq!(
        first_class_ciphertexts(&outbound.baseline_provider_items),
        [OUTBOUND_REASONING_CIPHERTEXT]
    );
    assert_eq!(
        codex_raw_input_ciphertexts(&outbound.baseline_provider_items),
        [OUTBOUND_CARRIER_CIPHERTEXT]
    );
    assert_ne!(
        OUTBOUND_REASONING_CIPHERTEXT.as_bytes(),
        OUTBOUND_CARRIER_CIPHERTEXT.as_bytes(),
        "first-class and opaque carrier sentinels are distinct"
    );

    let handback_v1 = sequence(sequences, "accepted-v1-handback-sanitizes-ciphertext");
    assert_eq!(handback_v1.fallback_case.as_deref(), Some("V1"));
    assert_eq!(handback_v1.baseline_provider_state, ProviderState::None);
    assert_eq!(
        handback_v1.fallback_response_provider_state,
        ProviderState::SyntheticEncryptedContent
    );
    assert_eq!(
        first_class_ciphertexts(&handback_v1.fallback_response_provider_items),
        [FALLBACK_V1_REASONING_CIPHERTEXT]
    );

    let handback_v2 = sequence(sequences, "accepted-v2-handback-sanitizes-ciphertext");
    assert_eq!(handback_v2.fallback_case.as_deref(), Some("V2"));
    assert_eq!(handback_v2.baseline_provider_state, ProviderState::None);
    assert_eq!(
        handback_v2.fallback_response_provider_state,
        ProviderState::SyntheticEncryptedContent
    );
    assert_eq!(
        first_class_ciphertexts(&handback_v2.fallback_response_provider_items),
        [FALLBACK_V2_REASONING_CIPHERTEXT]
    );

    assert_eq!(
        sequences
            .iter()
            .filter(|sequence| {
                sequence.baseline_provider_state == ProviderState::SyntheticEncryptedContent
            })
            .count(),
        1,
        "one outbound sanitation source row"
    );
    assert_eq!(
        sequences
            .iter()
            .filter(|sequence| {
                sequence.fallback_response_provider_state
                    == ProviderState::SyntheticEncryptedContent
            })
            .count(),
        2,
        "separate accepted V1 and V2 handback rows"
    );
}

fn expected_manifest_paths() -> Vec<String> {
    let mut paths: Vec<_> = TERMINAL_IDS
        .iter()
        .map(|id| format!("{id}/events.json"))
        .chain(
            ["cases.json", "fallback_sequences.json", "PROVENANCE.md"]
                .into_iter()
                .map(str::to_owned),
        )
        .collect();
    paths.sort();
    paths
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_manifest(root: &Path) {
    let manifest_path = root.join("MANIFEST.sha256");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let rows: Vec<_> = manifest.lines().collect();
    assert_eq!(rows.len(), 15, "manifest hashes exactly fifteen paths");

    let mut paths = Vec::with_capacity(rows.len());
    for row in rows {
        let (expected_hash, relative_path) = row
            .split_once("  ")
            .unwrap_or_else(|| panic!("invalid shasum row: {row}"));
        assert_eq!(expected_hash.len(), 64, "SHA-256 width for {relative_path}");
        assert!(
            expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "non-hex SHA-256 for {relative_path}"
        );
        assert!(!relative_path.starts_with('/'), "manifest path is relative");
        let actual_hash = sha256_hex(
            &std::fs::read(root.join(relative_path))
                .unwrap_or_else(|error| panic!("read manifest path {relative_path}: {error}")),
        );
        assert_eq!(
            actual_hash, expected_hash,
            "manifest hash for {relative_path}"
        );
        paths.push(relative_path.to_owned());
    }
    assert_eq!(
        paths,
        expected_manifest_paths(),
        "global bytewise path order"
    );
}

fn test_request() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from("exercise terminal fixture"),
            }],
            ..Default::default()
        })],
        ..Default::default()
    }
}

fn test_config(
    server: &MockInferenceServer,
    case: &TerminalCase,
    replay: &FixtureReplay,
) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("fixture-key".to_owned()),
        base_url: server.url(),
        model: PRIMARY_MODEL.to_owned(),
        api_backend: replay.backend(),
        model_family: case.dialect.clone(),
        max_retries: Some(0),
        idle_timeout_secs: Some(5),
        ..Default::default()
    }
}

async fn replay_to_completion(case: &TerminalCase, replay: &FixtureReplay) {
    let server = MockInferenceServer::start()
        .await
        .unwrap_or_else(|error| panic!("start mock for {}: {error}", case.id));
    let mut expectation = server.expect_response(
        format!("terminal fixture {}", case.id),
        InferenceRequestMatcher::auxiliary(replay.inference_endpoint()),
        replay.scripted_response(),
    );
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(&server, case, replay),
        RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        event_tx,
    );
    let request_id = RequestId::from(format!("fixture-{}", case.id));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handle.submit_and_collect(request_id, test_request()),
    )
    .await
    .unwrap_or_else(|_| panic!("{} replay did not terminate", case.id));
    assert!(
        !matches!(&result, Err(SamplingError::Serialization(_))),
        "{} fixture failed client deserialization: {result:?}",
        case.id
    );
    tokio::time::timeout(Duration::from_secs(5), expectation.wait_satisfied())
        .await
        .unwrap_or_else(|_| panic!("{} scripted response was not consumed", case.id));
    assert_eq!(
        server.request_count_for(replay.endpoint()),
        1,
        "{} endpoint dispatch count",
        case.id
    );
}

async fn replay_cancelled(case: &TerminalCase, replay: &FixtureReplay) {
    let server = MockInferenceServer::start()
        .await
        .unwrap_or_else(|error| panic!("start mock for {}: {error}", case.id));
    let mut expectation = server.expect_response_blocked(
        "terminal fixture C1",
        InferenceRequestMatcher::auxiliary(replay.inference_endpoint()),
        replay.scripted_response(),
    );
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(&server, case, replay),
        RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        event_tx,
    );
    let request_id = RequestId::from("fixture-C1");
    handle.submit(request_id.clone(), test_request());

    tokio::time::timeout(Duration::from_secs(5), expectation.wait_blocked())
        .await
        .expect("C1 reached the barrier after its preceding frame");
    assert!(
        !expectation.is_satisfied(),
        "C1 terminal crossed before cancel"
    );
    handle.cancel(request_id.clone());

    let cancellation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event_rx.recv().await {
                Some(SamplingEvent::Failed {
                    request_id: failed_id,
                    error,
                }) if failed_id == request_id => break error,
                Some(_) => {}
                None => panic!("sampler event channel closed before C1 cancellation"),
            }
        }
    })
    .await
    .expect("C1 cancellation produced a terminal sampler event");
    assert!(cancellation.message.contains("cancelled"));

    expectation.release();
    assert_eq!(server.request_count_for(replay.endpoint()), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_contract() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let ids: Vec<_> = cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "V1", "V2", "N1", "E1", "E2", "E3", "E4", "L1", "L2", "T1", "F1", "C1"
        ]
    );
    assert_eq!(
        ids.iter().copied().collect::<BTreeSet<_>>().len(),
        TERMINAL_IDS.len(),
        "one oracle per terminal ID"
    );

    let fixture_ids: BTreeSet<_> = std::fs::read_dir(&root)
        .expect("read terminal fixture root")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        fixture_ids,
        TERMINAL_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<BTreeSet<_>>(),
        "one fixture directory per terminal ID"
    );

    let mut replays = BTreeMap::new();
    for terminal_case in &cases {
        let replay = FixtureReplay::load(&root, terminal_case);
        validate_case_metadata(terminal_case, &replay);
        let _scripted = replay.scripted_response();
        assert_eq!(
            replay.endpoint(),
            match terminal_case.wire {
                FixtureWire::Responses => "/v1/responses",
                FixtureWire::Chat => "/v1/chat/completions",
            }
        );
        assert_eq!(
            replay.backend(),
            match terminal_case.wire {
                FixtureWire::Responses => ApiBackend::Responses,
                FixtureWire::Chat => ApiBackend::ChatCompletions,
            }
        );
        assert!(replays.insert(terminal_case.id.clone(), replay).is_none());
    }

    for (id, expected_text, expected_deltas) in [
        ("V1", "ok", &["ok"][..]),
        ("N1", "ok", &["", "ok"][..]),
        ("E1", "", &[][..]),
        ("E2", " \n", &[" ", "\n"][..]),
    ] {
        let replay = &replays[id];
        assert_eq!(
            terminal_output_text(id, replay),
            expected_text,
            "{id} exact terminal output text"
        );
        let actual_deltas = response_text_deltas(id, replay);
        assert_eq!(
            actual_deltas.as_slice(),
            expected_deltas,
            "{id} exact output_text delta sequence"
        );
    }

    assert_eq!(case(&cases, "E1").raw_text_bytes, 0);
    assert_eq!(case(&cases, "E2").raw_text_bytes, 2);
    assert_eq!(case(&cases, "E2").trimmed_text_bytes, 0);
    assert_eq!(replays["L2"].endpoint(), "/v1/chat/completions");
    assert_eq!(replays["L2"].backend(), ApiBackend::ChatCompletions);
    assert_eq!(
        replays["N1"]
            .frames
            .last()
            .map(|frame| frame_marker(FixtureWire::Responses, frame)),
        Some("response.completed".to_owned())
    );
    assert_eq!(case(&cases, "C1").cancel_after_event, Some(0));
    assert_eq!(replays["C1"].frames.len(), 2);

    let sequences: Vec<FallbackSequence> = read_json(&root.join("fallback_sequences.json"));
    validate_fallback_sequences(&cases, &sequences);

    let v2_items = response_items(&replays["V2"].frames);
    assert_eq!(
        v2_items
            .iter()
            .filter_map(|item| item.get("type").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        ["reasoning", "function_call"],
        "V2 reasoning precedes its function call"
    );
    assert_eq!(
        v2_items[0]
            .get("encrypted_content")
            .and_then(Value::as_str)
            .map(str::as_bytes),
        Some(FALLBACK_V2_REASONING_CIPHERTEXT.as_bytes()),
        "V2 first-class ciphertext sentinel is byte-stable"
    );
    assert_eq!(
        v2_items[0],
        sequence(&sequences, "accepted-v2-handback-sanitizes-ciphertext")
            .fallback_response_provider_items[0],
        "V2 handback provider item mirrors the replayed response item"
    );
    validate_manifest(&root);

    for terminal_case in &cases {
        let replay = &replays[&terminal_case.id];
        if terminal_case.id == "C1" {
            replay_cancelled(terminal_case, replay).await;
        } else {
            replay_to_completion(terminal_case, replay).await;
        }
    }
}

fn output_presence(frames: &[FixtureFrame]) -> OutputPresence {
    match observed_output(frames) {
        ("missing", None) => OutputPresence::Missing,
        ("null", None) => OutputPresence::Null,
        ("array", Some(len)) => OutputPresence::Array(len),
        other => panic!("unsupported fixture output presence: {other:?}"),
    }
}

fn fixture_token_usage(wire: FixtureWire, frames: &[FixtureFrame]) -> Option<TokenUsage> {
    let usage = match wire {
        FixtureWire::Responses => terminal_response(frames)?.get("usage")?,
        FixtureWire::Chat => frames
            .iter()
            .rev()
            .find_map(|frame| frame.data.get("usage"))?,
    };
    let read = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or_default() as u32;
    Some(match wire {
        FixtureWire::Responses => TokenUsage {
            prompt_tokens: read("input_tokens"),
            completion_tokens: read("output_tokens"),
            total_tokens: read("total_tokens"),
            reasoning_tokens: usage
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            cached_prompt_tokens: usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            ..Default::default()
        },
        FixtureWire::Chat => TokenUsage {
            prompt_tokens: read("prompt_tokens"),
            completion_tokens: read("completion_tokens"),
            total_tokens: read("total_tokens"),
            ..Default::default()
        },
    })
}

fn terminal_reason(raw: Option<&str>) -> Option<TerminalReason> {
    raw.map(|reason| match reason {
        "max_output_tokens" | "max_time_limit" | "length" => TerminalReason::Length,
        "refusal" => TerminalReason::Refusal,
        "content_filter" => TerminalReason::ContentFilter,
        "max_prompt_tokens" | "context_window_exceeded" => TerminalReason::ContextWindow,
        other => TerminalReason::Other(other.to_owned()),
    })
}

/// Build classifier input from the fixture wire itself. `cases.json` supplies only
/// the expected class/lane and dialect selector; it never supplies observed facts.
fn facts_from_fixture(case: &TerminalCase, replay: &FixtureReplay) -> TerminalFacts {
    let frames = observed_frames(case, replay);
    let terminal_frame = frames.iter().rev().find(|frame| match case.wire {
        FixtureWire::Responses => matches!(
            frame.data.get("type").and_then(Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed")
        ),
        FixtureWire::Chat => frame
            .data
            .pointer("/choices/0/finish_reason")
            .is_some_and(|reason| !reason.is_null()),
    });

    let cancelled = case.cancel_after_event.is_some();
    let transport_eof = terminal_frame.is_none() && !cancelled;
    let (named_event, status, reason, response_id, response_model) = match case.wire {
        FixtureWire::Responses => {
            let event_type = terminal_frame
                .and_then(|frame| frame.data.get("type"))
                .and_then(Value::as_str);
            let response = terminal_frame.and_then(|frame| frame.data.get("response"));
            let named_event = match event_type {
                Some("response.completed") => Some(NamedEvent::Completed),
                Some("response.incomplete") => Some(NamedEvent::Incomplete),
                Some("response.failed") => Some(NamedEvent::Failed),
                _ => None,
            };
            let status = if cancelled {
                Some("cancelled".to_owned())
            } else if transport_eof {
                Some("transport_eof".to_owned())
            } else {
                response
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let reason = terminal_reason(
                response
                    .and_then(|value| value.pointer("/incomplete_details/reason"))
                    .and_then(Value::as_str),
            );
            let response_id = response
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let response_model = response
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            (named_event, status, reason, response_id, response_model)
        }
        FixtureWire::Chat => {
            let finish_reason = terminal_frame
                .and_then(|frame| frame.data.pointer("/choices/0/finish_reason"))
                .and_then(Value::as_str);
            let named_event = finish_reason.map(|reason| {
                if reason == "length" {
                    NamedEvent::Incomplete
                } else {
                    NamedEvent::Completed
                }
            });
            let status = if cancelled {
                Some("cancelled".to_owned())
            } else if transport_eof {
                Some("transport_eof".to_owned())
            } else {
                named_event.as_ref().map(|event| match event {
                    NamedEvent::Completed => "completed".to_owned(),
                    NamedEvent::Incomplete => "incomplete".to_owned(),
                    NamedEvent::Failed => "failed".to_owned(),
                })
            };
            let response_id = terminal_frame
                .and_then(|frame| frame.data.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let response_model = terminal_frame
                .and_then(|frame| frame.data.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            (
                named_event,
                status,
                terminal_reason(finish_reason),
                response_id,
                response_model,
            )
        }
    };

    let items = response_items(frames);
    let text = response_text(items);
    let refusal = items.iter().any(|item| {
        item.get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
            })
    });

    TerminalFacts {
        wire: replay.backend(),
        dialect: case.dialect.clone(),
        named_event,
        status,
        reason,
        raw_output_presence: output_presence(frames),
        message_items: items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
            .count(),
        reasoning_items: items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .count(),
        raw_text_bytes: text.len(),
        trimmed_text_bytes: text.trim().len(),
        tool_activity: has_tool_activity(replay),
        tool_items_complete: tool_items_complete(replay),
        refusal,
        response_id,
        response_model,
        usage: fixture_token_usage(case.wire, frames),
        cost_usd_ticks: None,
    }
}

fn class_from_oracle(value: &str) -> TerminalClass {
    match value {
        "valid_text" => TerminalClass::ValidText,
        "valid_tool" => TerminalClass::ValidTool,
        "exact_empty" => TerminalClass::ExactEmpty,
        "whitespace_empty" => TerminalClass::WhitespaceEmpty,
        "reasoning_only" => TerminalClass::ReasoningOnly,
        "zero_output" => TerminalClass::ZeroOutput,
        "length" => TerminalClass::Length,
        "transport_eof" => TerminalClass::TransportEof,
        "provider_failed" => TerminalClass::ProviderFailed,
        "cancelled" => TerminalClass::Cancelled,
        other => panic!("unknown terminal class oracle {other}"),
    }
}

fn lane_from_oracle(value: &str) -> RetryLane {
    match value {
        "none" => RetryLane::None,
        "semantic" => RetryLane::Semantic,
        "length" => RetryLane::Length,
        "transport" => RetryLane::Transport,
        other => panic!("unknown retry lane oracle {other}"),
    }
}

fn completed_facts() -> TerminalFacts {
    TerminalFacts {
        wire: ApiBackend::Responses,
        dialect: Some("openai".to_owned()),
        named_event: Some(NamedEvent::Completed),
        status: Some("completed".to_owned()),
        reason: None,
        raw_output_presence: OutputPresence::Array(0),
        message_items: 0,
        reasoning_items: 0,
        raw_text_bytes: 0,
        trimmed_text_bytes: 0,
        tool_activity: false,
        tool_items_complete: true,
        refusal: false,
        response_id: Some("resp_test".to_owned()),
        response_model: Some(PRIMARY_MODEL.to_owned()),
        usage: None,
        cost_usd_ticks: None,
    }
}

fn terminal_frame_mut(replay: &mut FixtureReplay) -> &mut serde_json::Map<String, Value> {
    replay
        .frames
        .iter_mut()
        .rev()
        .find_map(|frame| {
            matches!(
                frame.data.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.incomplete" | "response.failed")
            )
            .then(|| {
                frame
                    .data
                    .get_mut("response")
                    .and_then(Value::as_object_mut)
                    .expect("terminal response object")
            })
        })
        .expect("Responses replay has a terminal frame")
}

fn replace_terminal_output(replay: &mut FixtureReplay, presence: OutputPresence) {
    let response = terminal_frame_mut(replay);
    match presence {
        OutputPresence::Missing => {
            response.remove("output");
        }
        OutputPresence::Null => {
            response.insert("output".to_owned(), Value::Null);
        }
        OutputPresence::Array(_) => panic!("test helper only creates missing/null terminals"),
    }
}

fn set_added_tool_status(replay: &mut FixtureReplay, status: Option<&str>) {
    let item = replay
        .frames
        .iter_mut()
        .find(|frame| {
            frame.data.get("type").and_then(Value::as_str) == Some("response.output_item.added")
        })
        .and_then(|frame| frame.data.get_mut("item"))
        .and_then(Value::as_object_mut)
        .expect("Responses replay has an added tool item");
    match status {
        Some(status) => {
            item.insert("status".to_owned(), Value::String(status.to_owned()));
        }
        None => {
            item.remove("status");
        }
    }
}

fn incomplete_cause_replay(
    root: &Path,
    terminal_case: &TerminalCase,
    reason: &str,
) -> FixtureReplay {
    let mut replay = FixtureReplay::load(root, terminal_case);
    let terminal = replay
        .frames
        .iter_mut()
        .rev()
        .find(|frame| {
            matches!(
                frame.data.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.incomplete" | "response.failed")
            )
        })
        .expect("Responses replay has a terminal frame");
    terminal.data["type"] = Value::String("response.incomplete".to_owned());
    let response = terminal
        .data
        .get_mut("response")
        .and_then(Value::as_object_mut)
        .expect("terminal response object");
    response.insert("status".to_owned(), Value::String("incomplete".to_owned()));
    response.insert("incomplete_details".to_owned(), json!({ "reason": reason }));
    replay
}

fn refusal_replay(root: &Path, terminal_case: &TerminalCase) -> FixtureReplay {
    let mut replay = FixtureReplay::load(root, terminal_case);
    let response = terminal_frame_mut(&mut replay);
    response["output"][0]["content"] = json!([{
        "type": "refusal",
        "refusal": "synthetic refusal"
    }]);
    replay
}

async fn replay_result(
    case: &TerminalCase,
    replay: &FixtureReplay,
    dialect_override: Option<&str>,
    length_policy: LengthPolicy,
) -> xai_grok_sampler::CompletionResult {
    let server = MockInferenceServer::start()
        .await
        .unwrap_or_else(|error| panic!("start mock for {}: {error}", case.id));
    let mut expectation = server.expect_response(
        format!("terminal behavior {}", case.id),
        InferenceRequestMatcher::auxiliary(replay.inference_endpoint()),
        replay.scripted_response(),
    );
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let mut config = test_config(&server, case, replay);
    config.model_family = dialect_override
        .map(str::to_owned)
        .or_else(|| case.dialect.clone());
    let handle = SamplerActor::spawn(
        config,
        RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        event_tx,
    );
    let mut request = test_request();
    request.length_policy = length_policy;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handle.submit_and_collect(
            RequestId::from(format!("terminal-behavior-{}", case.id)),
            request,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("{} behavior replay did not terminate", case.id));
    tokio::time::timeout(Duration::from_secs(5), expectation.wait_satisfied())
        .await
        .unwrap_or_else(|_| panic!("{} behavior response was not consumed", case.id));
    assert_eq!(server.request_count_for(replay.endpoint()), 1);
    result
}

async fn replay_result_with_retry_budget(
    case: &TerminalCase,
    replay: &FixtureReplay,
    length_policy: LengthPolicy,
    max_retries: u32,
    queued_replays: usize,
) -> (xai_grok_sampler::CompletionResult, usize, Vec<Value>) {
    let server = MockInferenceServer::start()
        .await
        .unwrap_or_else(|error| panic!("start mock for {}: {error}", case.id));
    let mut expectation = server.expect_response(
        format!("terminal retry behavior {}", case.id),
        InferenceRequestMatcher::auxiliary(replay.inference_endpoint()),
        replay.scripted_response(),
    );
    for _ in 0..queued_replays {
        server.enqueue_response(replay.endpoint(), replay.scripted_response());
    }
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let mut config = test_config(&server, case, replay);
    config.max_retries = Some(max_retries);
    let handle = SamplerActor::spawn(
        config,
        RetryPolicy {
            max_retries,
            ..Default::default()
        },
        event_tx,
    );
    let mut request = test_request();
    request.length_policy = length_policy;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        handle.submit_and_collect(
            RequestId::from(format!("terminal-retry-behavior-{}", case.id)),
            request,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("{} retry behavior replay did not terminate", case.id));
    tokio::time::timeout(Duration::from_secs(5), expectation.wait_satisfied())
        .await
        .unwrap_or_else(|_| panic!("{} retry behavior response was not consumed", case.id));
    (
        result,
        server.request_count_for(replay.endpoint()),
        server.request_bodies(),
    )
}

fn assert_single_primary_attempt(request_count: usize, request_bodies: &[Value], label: &str) {
    assert_eq!(request_count, 1, "{label} must not retry");
    assert_eq!(request_bodies.len(), 1, "{label} request body count");
    assert_eq!(request_bodies[0]["model"], PRIMARY_MODEL, "{label} model");
    assert_ne!(
        request_bodies[0]["model"], FALLBACK_MODEL,
        "{label} must not be eligible for Terra fallback"
    );
}

fn assert_protocol_terminal_error(
    result: xai_grok_sampler::CompletionResult,
    class: TerminalClass,
    label: &str,
) {
    match result {
        Err(SamplingError::Api {
            status,
            message,
            should_retry,
            ..
        }) => {
            assert_eq!(status.as_u16(), 400, "{label} status");
            assert_eq!(should_retry, Some(false), "{label} retry veto");
            assert!(
                message.contains(&format!("{class:?}")),
                "{label} preserves typed class in observable error: {message}"
            );
        }
        Err(SamplingError::EmptyResponse { context }) => {
            panic!("{label} entered semantic-empty retry: {context:?}")
        }
        Err(SamplingError::MaxTokensTruncation) => {
            panic!("{label} entered length salvage")
        }
        other => panic!("{label} unexpected actor outcome: {other:?}"),
    }
}

#[test]
fn terminal_matrix() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let mut seen = BTreeSet::new();
    for terminal_case in &cases {
        assert!(
            seen.insert(terminal_case.id.as_str()),
            "duplicate case {}",
            terminal_case.id
        );
        let replay = FixtureReplay::load(&root, terminal_case);
        let observed = classify_terminal(&facts_from_fixture(terminal_case, &replay));
        assert_eq!(
            observed,
            (
                class_from_oracle(&terminal_case.terminal_class),
                lane_from_oracle(&terminal_case.lane),
            ),
            "{} production classifier verdict",
            terminal_case.id
        );
    }
    assert_eq!(seen.len(), TERMINAL_IDS.len());
}

#[test]
fn named_cause_beats_output() {
    let mut facts = completed_facts();
    facts.message_items = 1;
    facts.raw_text_bytes = 2;
    facts.trimmed_text_bytes = 2;

    facts.named_event = Some(NamedEvent::Failed);
    facts.status = Some("completed".to_owned());
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ProviderFailed, RetryLane::Transport)
    );

    facts.named_event = Some(NamedEvent::Incomplete);
    facts.reason = Some(TerminalReason::Length);
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::Length, RetryLane::Length)
    );

    facts.reason = Some(TerminalReason::ContentFilter);
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ContentFilter, RetryLane::None)
    );
}

#[test]
fn refusal_fact_beats_empty_shape() {
    let mut facts = completed_facts();
    facts.raw_output_presence = OutputPresence::Array(1);
    facts.message_items = 1;
    facts.refusal = true;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::Refusal, RetryLane::None)
    );

    facts.named_event = Some(NamedEvent::Failed);
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::Refusal, RetryLane::None),
        "an independently observed refusal wins even over contradictory named failure"
    );
}

#[test]
fn incomplete_tool_is_length() {
    let mut facts = completed_facts();
    facts.named_event = Some(NamedEvent::Incomplete);
    facts.status = Some("incomplete".to_owned());
    facts.reason = Some(TerminalReason::Length);
    facts.raw_output_presence = OutputPresence::Array(1);
    facts.tool_activity = true;
    facts.tool_items_complete = false;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::Length, RetryLane::Length)
    );
}

#[test]
fn missing_null_without_output_are_malformed() {
    for presence in [OutputPresence::Missing, OutputPresence::Null] {
        let mut facts = completed_facts();
        facts.raw_output_presence = presence;
        assert_eq!(
            classify_terminal(&facts),
            (TerminalClass::Malformed, RetryLane::None)
        );

        facts.reasoning_items = 1;
        assert_eq!(
            classify_terminal(&facts),
            (TerminalClass::Malformed, RetryLane::None),
            "missing/null plus reasoning-only is malformed, never E3"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_null_with_accumulated_output_are_valid_not_e4() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));

    for presence in [OutputPresence::Missing, OutputPresence::Null] {
        let mut text_facts = completed_facts();
        text_facts.raw_output_presence = presence.clone();
        text_facts.message_items = 1;
        text_facts.raw_text_bytes = 2;
        text_facts.trimmed_text_bytes = 2;
        assert_eq!(
            classify_terminal(&text_facts),
            (TerminalClass::ValidText, RetryLane::None)
        );

        let mut tool_facts = completed_facts();
        tool_facts.raw_output_presence = presence.clone();
        tool_facts.tool_activity = true;
        tool_facts.tool_items_complete = true;
        assert_eq!(
            classify_terminal(&tool_facts),
            (TerminalClass::ValidTool, RetryLane::None)
        );

        for dialect in ["codex", "xai"] {
            let v1 = case(&cases, "V1");
            let mut replay = FixtureReplay::load(&root, v1);
            replace_terminal_output(&mut replay, presence.clone());
            let (response, _) = replay_result(v1, &replay, Some(dialect), LengthPolicy::Fail)
                .await
                .unwrap_or_else(|error| panic!("V1 {dialect} {presence:?} must survive: {error}"));
            assert_eq!(response.assistant_text(), "ok", "V1 {dialect} {presence:?}");

            let v2 = case(&cases, "V2");
            let mut replay = FixtureReplay::load(&root, v2);
            set_added_tool_status(&mut replay, Some("completed"));
            replace_terminal_output(&mut replay, presence.clone());
            let (response, _) = replay_result(v2, &replay, Some(dialect), LengthPolicy::Fail)
                .await
                .unwrap_or_else(|error| panic!("V2 {dialect} {presence:?} must survive: {error}"));
            assert_eq!(response.tool_calls().len(), 1, "V2 {dialect} {presence:?}");
            assert_eq!(&*response.tool_calls()[0].id, "call_v2");
            assert_eq!(response.tool_calls()[0].name.as_str(), "fixture_tool");
            assert_eq!(&*response.tool_calls()[0].arguments, "{}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_before_terminal_is_c1() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let c1 = case(&cases, "C1");
    let replay = FixtureReplay::load(&root, c1);
    assert_eq!(
        classify_terminal(&facts_from_fixture(c1, &replay)),
        (TerminalClass::Cancelled, RetryLane::None)
    );
    replay_cancelled(c1, &replay).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_terminal_wins_late_cancel() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let f1 = case(&cases, "F1");
    let replay = FixtureReplay::load(&root, f1);
    let server = MockInferenceServer::start().await.expect("mock server");
    let mut expectation = server.expect_response(
        "provider terminal before late cancel",
        InferenceRequestMatcher::auxiliary(replay.inference_endpoint()),
        replay.scripted_response(),
    );
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(&server, f1, &replay),
        RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        event_tx,
    );
    let request_id = RequestId::from("provider-terminal-late-cancel");
    let result = handle
        .submit_and_collect(request_id.clone(), test_request())
        .await;
    expectation.wait_satisfied().await;
    handle.cancel(request_id);
    let error = result.expect_err("provider failure remains terminal");
    assert!(error.to_string().contains("synthetic provider failure"));
    assert!(!error.to_string().contains("cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_reconstruction_preserves_l1_l2() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    for id in ["L1", "L2"] {
        let terminal_case = case(&cases, id);
        let replay = FixtureReplay::load(&root, terminal_case);
        let mut reconstructed = facts_from_fixture(terminal_case, &replay);
        reconstructed.raw_output_presence = OutputPresence::Missing;
        reconstructed.message_items = 0;
        reconstructed.reasoning_items = 0;
        reconstructed.raw_text_bytes = 0;
        reconstructed.trimmed_text_bytes = 0;
        assert_eq!(
            classify_terminal(&reconstructed),
            (TerminalClass::Length, RetryLane::Length),
            "{id} named cause survives proxy envelope reconstruction"
        );

        let result = replay_result(terminal_case, &replay, None, LengthPolicy::Fail).await;
        assert!(
            matches!(result, Err(SamplingError::MaxTokensTruncation)),
            "{id} must reach the length policy: {result:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusal_is_fail_fast() {
    let mut facts = completed_facts();
    facts.raw_output_presence = OutputPresence::Array(1);
    facts.message_items = 1;
    facts.refusal = true;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::Refusal, RetryLane::None)
    );

    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let e1 = case(&cases, "E1");
    let replay = refusal_replay(&root, e1);
    assert_eq!(
        classify_terminal(&facts_from_fixture(e1, &replay)),
        (TerminalClass::Refusal, RetryLane::None)
    );
    let (result, request_count, request_bodies) =
        replay_result_with_retry_budget(e1, &replay, LengthPolicy::CompletePartial, 2, 0).await;
    assert_protocol_terminal_error(result, TerminalClass::Refusal, "refusal");
    assert_single_primary_attempt(request_count, &request_bodies, "refusal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_filter_is_distinct_and_never_salvaged() {
    let mut facts = completed_facts();
    facts.named_event = Some(NamedEvent::Incomplete);
    facts.status = Some("incomplete".to_owned());
    facts.reason = Some(TerminalReason::ContentFilter);
    facts.message_items = 1;
    facts.raw_text_bytes = 7;
    facts.trimmed_text_bytes = 7;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ContentFilter, RetryLane::None)
    );

    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let e1 = case(&cases, "E1");
    let replay = incomplete_cause_replay(&root, e1, "content_filter");
    assert_eq!(
        classify_terminal(&facts_from_fixture(e1, &replay)),
        (TerminalClass::ContentFilter, RetryLane::None)
    );
    let (result, request_count, request_bodies) =
        replay_result_with_retry_budget(e1, &replay, LengthPolicy::CompletePartial, 2, 0).await;
    let (response, _) = result.expect("content filter is a typed completed outcome");
    assert_eq!(response.stop_reason, Some(StopReason::ContentFilter));
    assert!(response.assistant_text().is_empty());
    assert!(response.tool_calls().is_empty());
    assert_single_primary_attempt(request_count, &request_bodies, "content filter");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_window_is_distinct_and_never_salvaged() {
    let mut facts = completed_facts();
    facts.named_event = Some(NamedEvent::Incomplete);
    facts.status = Some("incomplete".to_owned());
    facts.reason = Some(TerminalReason::ContextWindow);
    facts.tool_activity = true;
    facts.tool_items_complete = true;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ContextWindow, RetryLane::None)
    );

    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let e1 = case(&cases, "E1");
    let replay = incomplete_cause_replay(&root, e1, "max_prompt_tokens");
    assert_eq!(
        classify_terminal(&facts_from_fixture(e1, &replay)),
        (TerminalClass::ContextWindow, RetryLane::None)
    );
    let (result, request_count, request_bodies) =
        replay_result_with_retry_budget(e1, &replay, LengthPolicy::CompletePartial, 2, 0).await;
    let (response, _) = result.expect("context window is a typed completed outcome");
    assert_eq!(
        response.stop_reason,
        Some(StopReason::ContextWindowExceeded)
    );
    assert!(response.assistant_text().is_empty());
    assert!(response.tool_calls().is_empty());
    assert_single_primary_attempt(request_count, &request_bodies, "context window");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_incomplete_is_distinct_and_never_salvaged() {
    let mut facts = completed_facts();
    facts.named_event = Some(NamedEvent::Incomplete);
    facts.status = Some("incomplete".to_owned());
    facts.reason = Some(TerminalReason::Other("provider_timeout".to_owned()));
    facts.message_items = 1;
    facts.raw_text_bytes = 7;
    facts.trimmed_text_bytes = 7;
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ProviderIncomplete, RetryLane::None)
    );

    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let e1 = case(&cases, "E1");
    let replay = incomplete_cause_replay(&root, e1, "provider_timeout");
    assert_eq!(
        classify_terminal(&facts_from_fixture(e1, &replay)),
        (TerminalClass::ProviderIncomplete, RetryLane::None)
    );
    let (result, request_count, request_bodies) =
        replay_result_with_retry_budget(e1, &replay, LengthPolicy::CompletePartial, 2, 0).await;
    assert_protocol_terminal_error(
        result,
        TerminalClass::ProviderIncomplete,
        "unknown incomplete",
    );
    assert_single_primary_attempt(request_count, &request_bodies, "unknown incomplete");
}

#[test]
fn deterministic_provider_failure_is_fail_fast() {
    let mut facts = completed_facts();
    facts.named_event = Some(NamedEvent::Failed);
    facts.status = Some("failed".to_owned());
    assert_eq!(
        classify_terminal(&facts),
        (TerminalClass::ProviderFailed, RetryLane::Transport)
    );

    let error = SamplingError::EventStreamError(
        "Response API in-stream error: deterministic fixture failure".to_owned(),
    );
    assert!(matches!(
        classify_error(&error, 0, 3, 2),
        xai_grok_sampler::RetryDecision::Fatal(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_failure_does_not_retry_with_transport_budget() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let f1 = case(&cases, "F1");
    let replay = FixtureReplay::load(&root, f1);
    assert_eq!(
        classify_terminal(&facts_from_fixture(f1, &replay)),
        (TerminalClass::ProviderFailed, RetryLane::Transport)
    );

    let (result, request_count, request_bodies) =
        replay_result_with_retry_budget(f1, &replay, LengthPolicy::CompletePartial, 2, 1).await;
    assert_single_primary_attempt(request_count, &request_bodies, "provider failure");
    match result {
        Err(SamplingError::Api {
            status,
            message,
            should_retry,
            ..
        }) => {
            assert_eq!(status.as_u16(), 500);
            assert!(message.contains("synthetic provider failure"));
            assert_eq!(
                should_retry,
                Some(false),
                "the consumed F1 terminal must deterministically veto retry"
            );
        }
        other => panic!("F1 must preserve its provider failure: {other:?}"),
    }
}

fn partial_tool_without_item_completion(
    root: &Path,
    terminal_case: &TerminalCase,
    presence: OutputPresence,
) -> FixtureReplay {
    let mut replay = FixtureReplay::load(root, terminal_case);
    replay.frames.retain(|frame| {
        !matches!(
            frame.data.get("type").and_then(Value::as_str),
            Some(
                "response.function_call_arguments.delta"
                    | "response.function_call_arguments.done"
                    | "response.output_item.done"
            )
        )
    });
    let added = replay
        .frames
        .iter_mut()
        .find(|frame| {
            frame.data.get("type").and_then(Value::as_str) == Some("response.output_item.added")
        })
        .expect("L1 has an added function call");
    let item = added
        .data
        .get_mut("item")
        .and_then(Value::as_object_mut)
        .expect("L1 added item object");
    item.insert("arguments".to_owned(), Value::String("{}".to_owned()));
    item.remove("status");

    let terminal_index = replay
        .frames
        .iter()
        .position(|frame| {
            frame.data.get("type").and_then(Value::as_str) == Some("response.incomplete")
        })
        .expect("L1 has an incomplete terminal");
    replay.frames.insert(
        terminal_index,
        FixtureFrame {
            event: None,
            data: json!({
                "type": "response.function_call_arguments.done",
                "sequence_number": 2,
                "item_id": "fc_l1",
                "output_index": 0,
                "arguments": "{}",
                "name": "fixture_tool"
            }),
        },
    );
    replace_terminal_output(&mut replay, presence);
    replay
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_tool_without_item_completion_is_not_salvaged() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let l1 = case(&cases, "L1");
    for presence in [OutputPresence::Missing, OutputPresence::Null] {
        let replay = partial_tool_without_item_completion(&root, l1, presence.clone());
        let result = replay_result(l1, &replay, None, LengthPolicy::CompleteToolCalls).await;
        assert!(
            matches!(result, Err(SamplingError::MaxTokensTruncation)),
            "{presence:?} terminal must not salvage arguments.done without item completion: {result:?}"
        );
    }
}

fn tool_length_replay(
    root: &Path,
    terminal_case: &TerminalCase,
    arguments: &str,
    status: &str,
) -> FixtureReplay {
    let mut replay = FixtureReplay::load(root, terminal_case);
    for frame in &mut replay.frames {
        if frame.data.get("type").and_then(Value::as_str) == Some("response.output_item.added") {
            let item = frame
                .data
                .get_mut("item")
                .and_then(Value::as_object_mut)
                .expect("added function-call item");
            item.insert("arguments".to_owned(), Value::String(arguments.to_owned()));
            item.insert("status".to_owned(), Value::String(status.to_owned()));
        }
    }
    let response = terminal_frame_mut(&mut replay);
    let item = response
        .get_mut("output")
        .and_then(Value::as_array_mut)
        .and_then(|items| items.first_mut())
        .and_then(Value::as_object_mut)
        .expect("terminal function-call item");
    item.insert("arguments".to_owned(), Value::String(arguments.to_owned()));
    item.insert("status".to_owned(), Value::String(status.to_owned()));
    replay
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn length_policy_respects_tool_completion_and_argument_validity() {
    let root = fixture_root();
    let cases: Vec<TerminalCase> = read_json(&root.join("cases.json"));
    let l1 = case(&cases, "L1");
    let shapes = [
        ("completed-valid", "{}", "completed", true),
        ("completed-invalid", "{\"path\":", "completed", false),
        ("in-progress-valid", "{}", "in_progress", false),
    ];
    for (shape, arguments, status, structurally_salvageable) in shapes {
        for policy in [
            LengthPolicy::Fail,
            LengthPolicy::CompleteToolCalls,
            LengthPolicy::CompletePartial,
        ] {
            let replay = tool_length_replay(&root, l1, arguments, status);
            let result = replay_result(l1, &replay, None, policy).await;
            let expect_success = structurally_salvageable && policy != LengthPolicy::Fail;
            if expect_success {
                let (response, _) = result.unwrap_or_else(|error| {
                    panic!("{shape} under {policy:?} must be salvaged: {error}")
                });
                assert_eq!(response.stop_reason, Some(StopReason::Length));
                assert_eq!(response.tool_calls().len(), 1);
                assert_eq!(&*response.tool_calls()[0].arguments, arguments);
            } else {
                assert!(
                    matches!(result, Err(SamplingError::MaxTokensTruncation)),
                    "{shape} under {policy:?} must fail as length: {result:?}"
                );
            }
        }
    }
}
