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
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use xai_grok_sampler::{
    ApiBackend, RequestId, RetryPolicy, SamplerActor, SamplerConfig, SamplingEvent,
};
use xai_grok_sampling_types::{
    ContentPart, ConversationItem, ConversationRequest, SamplingError, UserItem,
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
