use super::super::replay_buffer_send_update_tests::{
    ReplaySendUpdateFixture, make_replay_send_update_fixture,
};
use super::*;

fn own_request(actor: &SessionActor, request_id: &xai_grok_sampler::RequestId) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    actor.turn_stream_drained.lock().insert(
        request_id.clone(),
        crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
    );
}

/// Ownership stamped with an explicit generation, the way `submit_turn_request` stamps it.
/// Turn-phase recording drops a stamp whose generation is not the profile's current one, so a test
/// that reads ttft has to own the request at the generation its turn actually started.
fn own_request_at_generation(
    actor: &SessionActor,
    request_id: &xai_grok_sampler::RequestId,
    generation: u64,
) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    actor.turn_stream_drained.lock().insert(
        request_id.clone(),
        crate::session::acp_session::StreamOwnership {
            generation,
            waiter: Some(tx),
        },
    );
}

// ── StreamingTurnCapture tests ─────────────────────────────────.
// The out-of-band per-turn capture covers the "user cancelled mid-reasoning" and "model burned the context window in reasoning tokens" cases.
// It is deliberately NOT pushed into `chat_state`, so the model never sees the partial on later turns.

/// `handle_sampling_event::ChannelToken` for the `Reasoning` and `Text` channels must accumulate into the session's streaming capture.
/// The trace upload can then serialize it even when the canonical `record_assistant_response` path is skipped (cancel / max tokens).
#[tokio::test(flavor = "current_thread")]
async fn channel_tokens_accumulate_into_streaming_capture() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            // Stamp the prompt id and turn number the way the prompt handler would have before the sampler starts streaming
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-stream-1".to_string());
            actor.current_turn_number.set(7);

            // Stream a few reasoning chunks followed by a text chunk.
            let req = RequestId::random();
            own_request(&actor, &req);
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1_700_000_000_000,
                })
                .await;
            for chunk in ["Let me ", "think ", "step by step. "] {
                actor
                    .handle_sampling_event(SamplingEvent::ChannelToken {
                        request_id: req.clone(),
                        channel: SamplingChannel::Reasoning,
                        text: chunk.to_string(),
                        chunk_index: 0,
                    })
                    .await;
            }
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Text,
                    text: "Answer: 42".to_string(),
                    chunk_index: 0,
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.prompt_id.as_deref(),
                Some("prompt-stream-1"),
                "prompt id must be stamped on the capture",
            );
            assert_eq!(cap.turn_number, 7, "turn number must be stamped");
            assert_eq!(
                cap.started_at_ms,
                Some(1_700_000_000_000),
                "started_at_ms must come from StreamStarted",
            );
            assert_eq!(
                cap.reasoning_text, "Let me think step by step. ",
                "reasoning chunks must concatenate in arrival order",
            );
            assert_eq!(
                cap.response_text, "Answer: 42",
                "text channel chunks must concatenate separately",
            );
            assert_eq!(cap.reasoning_chunks, 3);
            assert_eq!(cap.text_chunks, 1);
            assert!(!cap.truncated);
            assert_eq!(
                cap.phase,
                CapturePhase::ResponseText,
                "phase must reflect the most recent channel — text \
                     arrived last, so the model was cut off mid-response",
            );
        })
        .await;
}

/// A same-prompt `StreamStarted` restart (a doomloop retry) must accumulate a second generation rather than wipe the first.
/// This guards the `if cap.prompt_id != prompt_id` branch in the `StreamStarted` arm, which the pure-struct tests bypass.
#[tokio::test(flavor = "current_thread")]
async fn same_prompt_restart_accumulates_segments_via_handler() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doomloop".to_string());

            let req = RequestId::random();
            own_request(&actor, &req);
            // First generation.
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "first gen reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            // Same-prompt restart (doomloop retry): must NOT wipe the first.
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 2,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req,
                    channel: SamplingChannel::Reasoning,
                    text: "second gen reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.segments.len(),
                1,
                "the first generation must be folded into segments, not wiped",
            );
            assert_eq!(cap.segments[0].reasoning_text, "first gen reasoning");
            assert_eq!(
                cap.reasoning_text, "second gen reasoning",
                "the second generation is the in-progress slot",
            );
            assert_eq!(cap.attempt_count, 2, "both same-prompt generations counted");
        })
        .await;
}

/// On `SamplingEvent::Completed` the canonical response is committed via `record_assistant_response`.
/// Its generation is discarded from the out-of-band capture: the in-progress slot is cleared, not folded into `segments`.
/// Prior uncommitted same-turn generations are left intact.
#[tokio::test(flavor = "current_thread")]
async fn completed_event_clears_slot_keeps_prior_uncommitted_segments() {
    use xai_grok_sampler::{InferenceLatencyStats, RequestId, SamplingChannel, SamplingEvent};
    use xai_grok_sampling_types::{ConversationItem, ConversationResponse};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-completed".to_string());

            let req = RequestId::random();
            own_request(&actor, &req);
            // A prior uncommitted generation (e.g. a doomloop retry).
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "prior uncommitted reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            // A same-prompt restart folds the prior generation into `segments` and opens the in-progress slot for the generation that will commit
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "committed reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;

            // Completion clears the in-progress slot without wiping the capture.
            let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("Answer".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 0,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.segments.len(),
                1,
                "the prior uncommitted generation must be retained",
            );
            assert_eq!(
                cap.segments[0].reasoning_text,
                "prior uncommitted reasoning"
            );
            assert!(
                cap.reasoning_text.is_empty(),
                "Completed must clear the committed generation from the in-progress slot",
            );
        })
        .await;
}

/// Regression: the sampler-event drainer must release the per-turn stream-drain barrier when (and only when) it processes the `Completed` event.
/// `run_turn_via_sampler` awaits this barrier before the turn loop emits the canonical client `ToolCall`s.
/// Without it the tool call's `send_update` on the turn-loop task could interleave between two still-draining text chunks on the drainer task.
#[tokio::test(flavor = "current_thread")]
async fn completed_event_releases_stream_drain_barrier_and_timeout_keeps_request_ownership() {
    use xai_grok_sampler::{InferenceLatencyStats, RequestId, SamplingChannel, SamplingEvent};
    use xai_grok_sampling_types::{ConversationItem, ConversationResponse};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-barrier".to_string());

            // Install the barrier exactly as `run_turn_via_sampler` does.
            let req = RequestId::random();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );

            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;

            // A mid-stream text chunk must NOT release the barrier; only the terminal Completed does
            // Otherwise the tool call could still race ahead of trailing text
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Text,
                    text: "the scrollback blo".to_string(),
                    chunk_index: 0,
                })
                .await;
            assert!(
                actor.turn_stream_drained.lock().contains_key(&req),
                "a mid-stream text chunk must NOT release the stream-drain barrier"
            );

            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: RequestId::random(),
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("unrelated".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 1,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;
            assert!(
                actor.turn_stream_drained.lock().contains_key(&req),
                "a different request must not release the active turn barrier"
            );
            assert_eq!(
                "the scrollback blo",
                actor.streaming_turn_capture.lock().response_text,
                "a different request must not clear the active capture"
            );

            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req.clone(),
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("blocks".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 1,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;

            // Completed took the sender and fired it, so the turn loop's await resolves and it can now emit the tool call (after the text)
            assert!(
                !actor.turn_stream_drained.lock().contains_key(&req),
                "Completed must take the stream-drain barrier sender"
            );
            assert!(
                rx.await.is_ok(),
                "Completed must fire the stream-drain barrier so \
                 run_turn_via_sampler can proceed to emit tool calls in order"
            );

            // A timeout drops only the ordering waiter
            // Request ownership stays until the terminal event applies its side effects
            let late_req = RequestId::random();
            let (late_tx, late_rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                late_req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(late_tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: late_req.clone(),
                    timestamp_ms: 2,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: late_req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "committed request reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .turn_stream_drained
                .lock()
                .get_mut(&late_req)
                .expect("late request remains owned")
                .waiter
                .take();
            assert!(late_rx.await.is_err(), "the ordering waiter was dropped");
            assert!(
                actor.turn_stream_drained.lock().contains_key(&late_req),
                "timeout must retain terminal request ownership"
            );
            while fixture.event_rx.try_recv().is_ok() {}

            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: late_req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: " queued after timeout".to_string(),
                    chunk_index: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::Retrying {
                    request_id: late_req.clone(),
                    attempt: 1,
                    max_retries: 2,
                    kind: xai_grok_sampler::SamplingErrorKind::DoomLoopDetected,
                    reason: "queued retry after timeout".to_string(),
                    doom_loop_triggers: Some(vec!["tail_repetition:8@thinking".to_string()]),
                    doom_loop_aborted_at_chunk: Some(42),
                })
                .await;
            assert_eq!(
                actor.streaming_turn_capture.lock().reasoning_text,
                "committed request reasoning queued after timeout",
                "timeout must retain queued chunks for the owned request"
            );
            assert_eq!(
                1,
                actor.doom_loop_turn_tally.lock().attempts,
                "timeout must retain queued retry metadata for the owned request"
            );
            tokio::task::yield_now().await;
            let event = fixture
                .event_rx
                .try_recv()
                .expect("timeout-owned chunk must enter the client event queue");
            assert!(matches!(
                event,
                SessionEvent::Notification(SessionNotification::Acp(notification))
                    if matches!(notification.update, acp::SessionUpdate::AgentThoughtChunk(_))
            ));

            let newer_req = RequestId::random();
            let (newer_tx, _newer_rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                newer_req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(newer_tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: newer_req.clone(),
                    timestamp_ms: 3,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: newer_req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "newer request reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;

            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: late_req.clone(),
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("late".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 1,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;
            assert!(
                !actor.turn_stream_drained.lock().contains_key(&late_req),
                "late terminal event must finish lifecycle processing and release ownership"
            );
            let capture = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                capture.reasoning_text, "newer request reasoning",
                "late completion must not clear the newer request's live capture"
            );
            assert!(
                capture
                    .segments
                    .iter()
                    .filter(|segment| segment.request_id.as_deref() == Some(late_req.as_str()))
                    .all(|segment| {
                        segment.reasoning_text.is_empty() && segment.response_text.is_empty()
                    }),
                "the timeout-owned request's committed content must be removed from prior segments"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unowned_stream_events_do_not_mutate_capture_or_notify_the_next_turn() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let sent = fixture.sent;
            let stale = RequestId::random();

            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: stale.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: stale.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "stale reasoning".to_string(),
                    chunk_index: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ToolCallDelta {
                    request_id: stale,
                    tool_index: 0,
                    id: Some("stale-call".to_string()),
                    name: Some("read_file".to_string()),
                    arguments_delta: Some("{}".to_string()),
                })
                .await;
            tokio::task::yield_now().await;

            assert!(actor.streaming_turn_capture.lock().is_empty());
            assert!(
                sent.lock().await.is_empty(),
                "unowned stream events must not reach the next turn's UI"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unowned_backend_tool_completion_closes_visible_card() {
    use xai_grok_sampler::{RequestId, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let signals = fixture.actor.signals_handle();
            let actor = Arc::new(fixture.actor);

            actor
                .handle_sampling_event(SamplingEvent::BackendToolCallCompleted {
                    request_id: RequestId::random(),
                    call_id: "backend-call".to_string(),
                    name: "web_search".to_string(),
                    result: Some(serde_json::json!({"status": "failed"})),
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("backend completion must enqueue an update")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            assert!(matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.0.as_ref() == "backend-call"
                        && update.fields.status == Some(acp::ToolCallStatus::Failed)
            ));
            let snapshot = signals.snapshot().await.expect("signals actor stays live");
            assert_eq!(
                0, snapshot.tool_failure_count,
                "unowned backend completion may close UI but not mutate telemetry"
            );
            assert_eq!(0, snapshot.error_count);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unowned_retry_does_not_notify_the_next_turn() {
    use xai_grok_sampler::{RequestId, SamplingErrorKind, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let sent = fixture.sent;

            actor
                .handle_sampling_event(SamplingEvent::Retrying {
                    request_id: RequestId::random(),
                    attempt: 1,
                    max_retries: 2,
                    kind: SamplingErrorKind::DoomLoopDetected,
                    reason: "late retry from cancelled request".to_string(),
                    doom_loop_triggers: Some(vec!["tail_repetition:8@thinking".to_string()]),
                    doom_loop_aborted_at_chunk: Some(42),
                })
                .await;
            tokio::task::yield_now().await;

            assert!(
                sent.lock().await.is_empty(),
                "an unowned retry must not emit a stale RetryState notification"
            );
            assert_eq!(0, actor.doom_loop_turn_tally.lock().attempts);
        })
        .await;
}

/// `SamplingEvent::Failed` (fired by the sampler for cancellation, `MaxTokensTruncation`, etc.) must NOT clear the accumulator.
/// The consumer needs to take it via `TakeStreamingCapture` and upload it as `streaming_partial.json`.
#[tokio::test(flavor = "current_thread")]
async fn failed_event_preserves_streaming_capture_for_takeout() {
    use xai_grok_sampler::{
        RequestId, SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent,
    };
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-failed".to_string());

            let req = RequestId::random();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );

            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "I should consider...".to_string(),
                    chunk_index: 0,
                })
                .await;

            actor
                .handle_sampling_event(SamplingEvent::DoomLoopSignals {
                    request_id: req.clone(),
                    triggers: vec!["exact_repetition:42x3@thinking".to_string()],
                })
                .await;

            // Simulate a sampler-side terminal failure after a discarded attempt reported doom-loop signals
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: req.clone(),
                    error: SamplingErrorInfo {
                        kind: SamplingErrorKind::MaxTokensTruncation,
                        status_code: None,
                        message: "max output tokens reached".to_string(),
                        is_retryable: false,
                        retry_after_secs: None,
                        should_retry: None,
                        error_code: None,
                        model_metadata: None,
                        empty_response_context: None,
                        doom_loop_triggers: None,
                        doom_loop_aborted_at_chunk: None,
                        credential: xai_grok_sampling_types::SentCredential::Unknown,
                    },
                })
                .await;

            assert!(
                !actor.turn_stream_drained.lock().contains_key(&req),
                "Failed must take the terminal drain barrier sender"
            );
            assert!(
                rx.await.is_ok(),
                "Failed must release terminal handling after preceding events drain"
            );
            assert_eq!(
                actor.doom_loop_turn_tally.lock().triggers,
                vec!["exact_repetition:42x3@thinking".to_string()],
                "incidence labels preceding Failed stay in the current turn"
            );

            let errors_before_timeout = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot")
                .error_count;
            let timed_out = RequestId::random();
            actor.turn_stream_drained.lock().insert(
                timed_out.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(None),
            );
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: timed_out,
                    error: SamplingErrorInfo {
                        kind: SamplingErrorKind::Api,
                        status_code: Some(500),
                        message: "late owned failure".to_string(),
                        is_retryable: false,
                        retry_after_secs: None,
                        should_retry: None,
                        error_code: None,
                        model_metadata: None,
                        empty_response_context: None,
                        doom_loop_triggers: None,
                        doom_loop_aborted_at_chunk: None,
                        credential: xai_grok_sampling_types::SentCredential::Unknown,
                    },
                })
                .await;
            let errors_after_timeout = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot")
                .error_count;
            assert_eq!(
                errors_before_timeout + 1,
                errors_after_timeout,
                "a timeout-owned terminal failure must still record its error metric"
            );

            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: RequestId::random(),
                    error: SamplingErrorInfo {
                        kind: SamplingErrorKind::Api,
                        status_code: Some(500),
                        message: "stale failure".to_string(),
                        is_retryable: false,
                        retry_after_secs: None,
                        should_retry: None,
                        error_code: None,
                        model_metadata: None,
                        empty_response_context: None,
                        doom_loop_triggers: None,
                        doom_loop_aborted_at_chunk: None,
                        credential: xai_grok_sampling_types::SentCredential::Unknown,
                    },
                })
                .await;
            let errors_after_unowned = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot")
                .error_count;
            assert_eq!(
                errors_after_timeout, errors_after_unowned,
                "an unowned terminal failure must not mutate session telemetry"
            );

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.reasoning_text, "I should consider...",
                "Failed must NOT discard the accumulator — the trace \
                     consumer is going to take it next via \
                     TakeStreamingCapture so the partial reasoning is \
                     preserved for inspection",
            );
            assert!(!cap.is_empty());
            assert_eq!(
                cap.phase,
                CapturePhase::Reasoning,
                "MaxTokensTruncation hit while the model was still \
                     thinking, so the partial must be labeled as tied to \
                     the reasoning phase",
            );
        })
        .await;
}

/// Observe-only (`max_retries = 0`): a first completion carrying confident signals had nothing discarded.
/// It must not be classified as accepted after budget: no tally, no counters, no capture stamp.
/// The signals only warn on the accepted response.
#[tokio::test(flavor = "current_thread")]
async fn observe_only_confident_completion_stays_warn_only() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            fixture.actor.doom_loop_recovery =
                Some(xai_grok_sampling_types::DoomLoopRecoveryPolicy {
                    max_threshold: 8,
                    max_retries: 0,
                    ..Default::default()
                });
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-observe".to_string());

            let req = RequestId::random();
            let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "loop loop loop".to_string(),
                    chunk_index: 0,
                })
                .await;
            // The first completion carries confident signals with NO prior Retrying
            let response = xai_grok_sampling_types::ConversationResponse {
                items: vec![xai_grok_sampling_types::ConversationItem::assistant(
                    "answer kept as-is",
                )],
                stop_reason: None,
                usage: None,
                cost_usd_ticks: None,
                message_chunks_emitted: 1,
                doom_loop_signals: vec![xai_grok_sampling_types::doom_loop::DoomLoopSignal::parse(
                    "tail_repetition:8@thinking",
                )],
                stop_message: None,
                message_id: None,
                raw_stop_reason: None,
                stop_sequence: None,
            };
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(response),
                    metrics: Default::default(),
                })
                .await;

            let tally = actor.doom_loop_turn_tally.lock().clone();
            assert!(
                tally.detected(),
                "the server signal is tracked for incidence"
            );
            assert!(!tally.fired(), "no resample happened: no recovery event");
            assert_eq!(
                tally.triggers,
                vec!["tail_repetition:8@thinking".to_string()]
            );
            assert!(!tally.accepted_after_budget);
            assert_eq!(tally.attempts, 0);

            let cap = actor.streaming_turn_capture.lock().clone();
            assert!(
                !cap.has_doom_loop_segments(),
                "no accepted-stamp for an undiscarded turn"
            );
            assert!(cap.segments.is_empty());

            let signals = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot");
            assert_eq!(signals.doom_loop_recovery_attempts, 0);
            assert_eq!(signals.doom_loop_recovery_accepted_after_budget, 0);
            assert_eq!(signals.doom_loop_recovery_top_trigger, None);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exact_repetition_completion_is_tracked_for_incidence_only() {
    use xai_grok_sampler::{RequestId, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let response = xai_grok_sampling_types::ConversationResponse {
                items: vec![xai_grok_sampling_types::ConversationItem::assistant(
                    "answer",
                )],
                stop_reason: None,
                usage: None,
                cost_usd_ticks: None,
                message_chunks_emitted: 1,
                doom_loop_signals: vec![xai_grok_sampling_types::doom_loop::DoomLoopSignal::parse(
                    "exact_repetition:42x3@response",
                )],
                stop_message: None,
                message_id: None,
                raw_stop_reason: None,
                stop_sequence: None,
            };
            let req = RequestId::random();
            let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(response),
                    metrics: Default::default(),
                })
                .await;

            let tally = actor.doom_loop_turn_tally.lock().clone();
            assert!(tally.detected());
            assert!(!tally.fired());
            assert_eq!(
                tally.triggers,
                vec!["exact_repetition:42x3@response".to_string()]
            );
            assert_eq!(
                tally.detection_summary(),
                crate::session::doom_loop_telemetry::DoomLoopDetectionSummary {
                    detector_kinds: vec!["exact_repetition".to_owned()],
                    channels: vec!["response".to_owned()],
                    tightest_tail_threshold: None,
                    max_exact_sequence_tokens: Some(42),
                    max_exact_repeat_count: Some(3),
                }
            );
        })
        .await;
}

/// A recovered turn's capture carries doom-stamped segments.
/// The doomed generation's Retrying (kind `DoomLoopDetected`, with triggers and abort chunk) stamps the in-progress slot.
/// The resample's `StreamStarted` folds it into `segments`, and an accept after budget folds a text-free stamped segment on `Completed`.
#[tokio::test(flavor = "current_thread")]
async fn doom_loop_recovery_stamps_capture_segments_and_counters() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingErrorKind, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            // Set the policy so the Completed arm can classify an accept after budget
            fixture.actor.doom_loop_recovery =
                Some(xai_grok_sampling_types::DoomLoopRecoveryPolicy::default());
            let actor = Arc::new(fixture.actor);

            // The same prompt id makes the resample's StreamStarted fold the doomed slot instead of beginning a new turn
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doom".to_string());

            let req = RequestId::random();
            let (tx, _rx) = tokio::sync::oneshot::channel::<()>();
            actor.turn_stream_drained.lock().insert(
                req.clone(),
                crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
            );
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "loop loop loop".to_string(),
                    chunk_index: 0,
                })
                .await;
            // Incidence carries every label on the doomed attempt, including exact repetition that does not participate in the recovery policy
            actor
                .handle_sampling_event(SamplingEvent::DoomLoopSignals {
                    request_id: req.clone(),
                    triggers: vec![
                        "tail_repetition:8@thinking".to_string(),
                        "exact_repetition:42x3@thinking".to_string(),
                    ],
                })
                .await;
            // The recovery discards the doomed attempt and resamples.
            actor
                .handle_sampling_event(SamplingEvent::Retrying {
                    request_id: req.clone(),
                    attempt: 1,
                    max_retries: 2,
                    kind: SamplingErrorKind::DoomLoopDetected,
                    reason: "doom loop detected: tail_repetition:8@thinking".to_string(),
                    doom_loop_triggers: Some(vec!["tail_repetition:8@thinking".to_string()]),
                    doom_loop_aborted_at_chunk: Some(421),
                })
                .await;
            // Replaying the same ordinal must be idempotent.
            actor
                .handle_sampling_event(SamplingEvent::Retrying {
                    request_id: req.clone(),
                    attempt: 1,
                    max_retries: 2,
                    kind: SamplingErrorKind::DoomLoopDetected,
                    reason: "duplicate queued retry".to_string(),
                    doom_loop_triggers: Some(vec!["tail_repetition:8@thinking".to_string()]),
                    doom_loop_aborted_at_chunk: Some(421),
                })
                .await;
            assert_eq!(1, actor.doom_loop_turn_tally.lock().attempts);

            // Resample: a fresh generation folds the doomed slot.
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "still looping".to_string(),
                    chunk_index: 0,
                })
                .await;
            // Budget spent: the accepted response keeps confident signals.
            let response = xai_grok_sampling_types::ConversationResponse {
                items: vec![xai_grok_sampling_types::ConversationItem::assistant(
                    "still looping answer",
                )],
                stop_reason: None,
                usage: None,
                cost_usd_ticks: None,
                message_chunks_emitted: 1,
                doom_loop_signals: vec![xai_grok_sampling_types::doom_loop::DoomLoopSignal::parse(
                    "tail_repetition:4@thinking",
                )],
                stop_message: None,
                message_id: None,
                raw_stop_reason: None,
                stop_sequence: None,
            };
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(response),
                    metrics: Default::default(),
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(cap.segments.len(), 2, "doomed fold + text-free accept");
            let resampled = cap.segments[0].doom_loop.as_ref().expect("stamped");
            assert_eq!(resampled.action, "resampled");
            assert_eq!(resampled.attempt, 1);
            assert_eq!(resampled.aborted_at_chunk, Some(421));
            assert_eq!(
                resampled.doom_loop_triggers,
                vec!["tail_repetition:8@thinking".to_string()]
            );
            assert_eq!(cap.segments[0].reasoning_text, "loop loop loop");
            let accepted = cap.segments[1].doom_loop.as_ref().expect("stamped");
            assert_eq!(accepted.action, "accepted_after_budget");
            assert!(
                cap.segments[1].reasoning_text.is_empty(),
                "committed text lives in history, not the capture"
            );
            assert!(cap.has_doom_loop_segments());

            let tally = actor.doom_loop_turn_tally.lock().clone();
            assert_eq!(tally.attempts, 1);
            assert!(tally.accepted_after_budget);
            assert_eq!(
                tally.triggers,
                vec![
                    "tail_repetition:8@thinking".to_string(),
                    "exact_repetition:42x3@thinking".to_string(),
                    "tail_repetition:4@thinking".to_string(),
                ],
                "discarded-attempt exact signals remain in incidence telemetry"
            );
            assert_eq!(
                tally.top_trigger.as_deref(),
                Some("tail_repetition:4@thinking"),
                "tightest across resample + accept"
            );

            let signals = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot");
            assert_eq!(signals.doom_loop_recovery_attempts, 1);
            assert_eq!(signals.doom_loop_recovery_accepted_after_budget, 1);
            assert_eq!(signals.doom_loop_recovery_aborted_chunks, 421);
            assert_eq!(
                signals.doom_loop_recovery_top_trigger.as_deref(),
                Some("tail_repetition:4@thinking")
            );
        })
        .await;
}

/// A `ToolCallDelta` arriving after the model streamed some reasoning must re-label the live capture as the tool-call phase.
/// It must preserve the reasoning text already accumulated, so a partial taken then shows the model was cut off mid tool-call.
#[tokio::test(flavor = "current_thread")]
async fn tool_call_delta_marks_streaming_capture_phase() {
    use xai_grok_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-toolcall".to_string());

            let req = RequestId::random();
            own_request(&actor, &req);
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "I'll call a tool.".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ToolCallDelta {
                    request_id: req,
                    tool_index: 0,
                    id: Some("call-1".to_string()),
                    name: Some("read_file".to_string()),
                    arguments_delta: Some("{\"path\":".to_string()),
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.phase,
                CapturePhase::ToolCall,
                "ToolCallDelta must re-label the active capture as the \
                     tool-call phase",
            );
            assert_eq!(
                cap.reasoning_text, "I'll call a tool.",
                "marking the tool-call phase must not discard the \
                     reasoning accumulated before the tool call",
            );
        })
        .await;
}

/// `ToolCallDelta` on an idle (empty, never-begun) slot must not fabricate a phase; there is no partial to attribute it to.
#[tokio::test(flavor = "current_thread")]
async fn tool_call_delta_on_idle_slot_leaves_phase_pending() {
    use xai_grok_sampler::{RequestId, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);

            // No StreamStarted / prompt id stamped: the slot is idle.
            actor
                .handle_sampling_event(SamplingEvent::ToolCallDelta {
                    request_id: RequestId::random(),
                    tool_index: 0,
                    id: Some("call-1".to_string()),
                    name: Some("read_file".to_string()),
                    arguments_delta: None,
                })
                .await;

            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(cap.phase, CapturePhase::Pending);
            assert!(cap.is_empty());
        })
        .await;
}

/// `StreamingTurnCapture::append` must respect the byte cap so that a runaway extended-thinking turn cannot blow the actor's memory.
/// The capture is marked `truncated = true` once the cap is hit, but the structure is still serializable.
#[test]
fn streaming_capture_appender_respects_byte_cap() {
    let mut cap = StreamingTurnCapture::default();
    assert_eq!(
        cap.phase,
        CapturePhase::Pending,
        "a fresh capture starts in the pending phase",
    );
    let chunk = "a".repeat(STREAMING_CAPTURE_MAX_BYTES / 2);
    cap.append(true, &chunk);
    cap.append(true, &chunk);
    assert!(!cap.truncated, "exactly at the cap should not be truncated");
    cap.append(true, "b");
    assert!(
        cap.truncated,
        "going one byte past the cap must flip the truncated flag"
    );
    // Subsequent appends should no-op (still truncated, no growth).
    let pre_len = cap.reasoning_text.len();
    cap.append(true, "ccccccccccc");
    assert_eq!(cap.reasoning_text.len(), pre_len);

    // Must still round-trip through serde_json (the upload path serializes via `serde_json::to_vec_pretty`)
    let bytes = serde_json::to_vec_pretty(&cap).expect("serialize capture");
    let parsed: StreamingTurnCapture = serde_json::from_slice(&bytes).expect("round-trip capture");
    assert!(parsed.truncated);
    assert_eq!(parsed.reasoning_text.len(), cap.reasoning_text.len());
    assert_eq!(
        parsed.phase,
        CapturePhase::Reasoning,
        "phase must survive the serde round-trip the upload path uses",
    );
}

/// A multi-generation reasoning-only turn must yield a capture whose `segments` hold every uncommitted generation, in order.
/// The struct tests in `streaming_capture.rs` and `same_prompt_restart_accumulates_segments_via_handler` pin that a restart folds rather than wipes.
/// Two generations suffice: a wiped slot on each same-turn `StreamStarted` would leave only one.
#[tokio::test(start_paused = true)]
async fn reasoning_only_doomloop_turn_captures_every_generation_as_segments() {
    use xai_grok_sampler::{
        RequestId, SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent,
    };
    use xai_grok_sampling_types::{EmptyReason, EmptyResponseContext};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                event_rx,
                sent: _sent,
                persistence_rx: _persistence_rx,
            } = make_replay_send_update_fixture().await;
            let actor = Arc::new(actor);

            // The prompt handler stamps the prompt id before any sampler event.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doomloop".to_string());

            // Two reasoning-only generations under the SAME prompt id: enough to prove more than the last survives
            // A same-prompt `StreamStarted` folds the prior in-progress generation into `segments`
            let req = RequestId::random();
            own_request(&actor, &req);
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "thinking attempt 1".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 2,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "thinking attempt 2".to_string(),
                    chunk_index: 0,
                })
                .await;

            // This is the sampler's terminal event once retries are exhausted
            // It carries the same empty-response context the L2 stream attaches for a reasoning-only completion
            let error = SamplingErrorInfo {
                kind: SamplingErrorKind::EmptyResponse,
                status_code: None,
                message: "empty response from model (reasoning_only)".to_string(),
                is_retryable: false,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
                model_metadata: None,
                empty_response_context: Some(EmptyResponseContext {
                    reason: EmptyReason::ReasoningOnly,
                    had_reasoning: true,
                    content_len: 0,
                    tool_call_count: 0,
                    finish_reason: Some("stop".to_string()),
                    completion_tokens: Some(0),
                    reasoning_tokens: Some(4096),
                    prompt_tokens: Some(128),
                    model: "grok-test".to_string(),
                    first_choice_seen: true,
                }),
                doom_loop_triggers: None,
                doom_loop_aborted_at_chunk: None,
                credential: xai_grok_sampling_types::SentCredential::Unknown,
            };

            // Drainer side: the terminal `Failed` is telemetry-only and must NOT collapse the accumulated doomloop segments
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: req,
                    error: error.clone(),
                })
                .await;

            // Turn-loop side: a reasoning-only empty response is non-recoverable, so it is a terminal error
            // It stamps the classification onto the capture
            // `SamplerFailureRecovery` is not `Debug`, so match rather than `expect_err`
            let Err(_terminal) = actor
                .handle_sampling_failure(
                    error,
                    0,
                    TransientRetryState {
                        step_attempts: 0,
                        prompt_attempts: 0,
                        episode_start: None,
                        enabled: true,
                    },
                    false,
                    TurnParkState::Fresh,
                )
                .await
            else {
                panic!("a reasoning_only empty response must be a terminal error, not recoverable");
            };

            // Take the capture exactly as the trace upload does: through the real `TakeStreamingCapture` command
            // The command finalizes the uncommitted generations for upload
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
            let (_chat_tx, chat_rx) = mpsc::unbounded_channel::<xai_chat_state::ChatStateEvent>();
            let codebase_indexes = Arc::new(parking_lot::Mutex::new(
                xai_grok_workspace::file_system::CodebaseIndexManager::new(),
            ));
            tokio::task::spawn_local(super::run_session(
                actor.clone(),
                cmd_rx,
                chat_rx,
                event_rx,
                None,
                codebase_indexes,
                std::path::PathBuf::from("/tmp"),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            ));

            let (respond_to, capture_rx) = tokio::sync::oneshot::channel();
            cmd_tx
                .send(SessionCommand::TakeStreamingCapture {
                    prompt_id: "prompt-doomloop".to_string(),
                    respond_to,
                })
                .unwrap();
            let capture = tokio::time::timeout(Duration::from_secs(2), capture_rx)
                .await
                .expect("TakeStreamingCapture must respond within 2s")
                .expect("the take responder must not be dropped")
                .expect("a reasoning-only doomloop turn must yield a non-empty capture");

            // This path's unique guarantee: every uncommitted reasoning-only generation is kept as its own segment, in order
            // The terminal classification also came through the command's finalize
            // Finer-grained properties (timestamps, joined-view separators, token magnitude) are owned by the `streaming_capture.rs` struct tests
            assert_eq!(
                capture.segments.len(),
                2,
                "both reasoning-only generations must be retained as segments",
            );
            assert_eq!(capture.segments[0].reasoning_text, "thinking attempt 1");
            assert_eq!(capture.segments[1].reasoning_text, "thinking attempt 2");
            assert_eq!(capture.empty_reason.as_deref(), Some("reasoning_only"));
        })
        .await;
}

/// An owned `ToolSearchCallReceived` must open a visible search card the same way a backend tool
/// start does: in-progress, tagged `tool_search`, with the model's query and limit as a JSON object.
#[tokio::test(flavor = "current_thread")]
async fn owned_tool_search_call_opens_in_progress_card() {
    use xai_grok_sampler::{RequestId, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    query: "deploy the canary".to_string(),
                    limit: 5,
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("an owned search call must enqueue a tool card")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            let acp::SessionUpdate::ToolCall(call) = &notification.update else {
                panic!("expected an ACP ToolCall, got {:?}", notification.update);
            };
            assert_eq!(call.tool_call_id.0.as_ref(), "search-call");
            assert_eq!(call.status, acp::ToolCallStatus::InProgress);
            assert_eq!(call.kind, acp::ToolKind::Search);
            assert!(
                call.title.contains("deploy the canary"),
                "the card title must name the query: {}",
                call.title
            );
            assert_eq!(
                call.raw_input.as_ref(),
                Some(&serde_json::json!({"query": "deploy the canary", "limit": 5})),
                "raw_input must be the JSON object the model asked for, not a string"
            );
            assert_eq!(
                call.meta
                    .as_ref()
                    .and_then(|meta| meta.get("tool_search"))
                    .and_then(serde_json::Value::as_bool),
                Some(true),
                "the card must be tagged as a tool_search card, mirroring the backend card's meta"
            );
        })
        .await;
}

/// The `ToolSearchCompleted` arm closes the card opened by the call, keyed on the same `call_id`, and
/// reports how many definitions the search loaded. What admits the close is the unconditional
/// `closes_visible_card` exemption — the `matches!` binding of that name in `handle_sampling_event`
/// (`sampling_events.rs`), consulted by that function's
/// `!request_owned && !closes_visible_card && !resolves_pending_strip` early return — not this
/// fixture's `own_request` call: the request is owned here because a real turn owns its
/// request, and the ownership-independent close is pinned by
/// [`owned_tool_search_card_is_closed_by_an_unowned_completion`].
#[tokio::test(flavor = "current_thread")]
async fn tool_search_completion_closes_the_card_with_result_count() {
    use xai_grok_sampler::{RequestId, SamplingEvent, ToolSearchStatus};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCompleted {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    result_count: 2,
                    status: ToolSearchStatus::Completed,
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("an owned search completion must enqueue an update")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                panic!(
                    "expected an ACP ToolCallUpdate, got {:?}",
                    notification.update
                );
            };
            assert_eq!(update.tool_call_id.0.as_ref(), "search-call");
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Completed),
                "a completed search must land as an ACP completion"
            );
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::json!({"result_count": 2, "status": "completed"})),
                "raw_output must be a JSON object carrying the count and the lowercase status word"
            );
        })
        .await;
}

/// Ownership gate: an opening search event from a request the turn no longer owns must not be
/// forwarded onto the FIFO pipeline — opening a card unowned would show the next turn a search it
/// never asked for. Only `BackendToolCallCompleted` and `ToolSearchCompleted` are exempt, and that
/// exemption is unconditional (`closes_visible_card`, see the note at its definition).
/// The falsifier is the `event_rx` channel: the `send_update` wrapper in `updates.rs` delegates to
/// `send_update_full` in that same file, which terminates at that function's
/// `let _ = self.event_tx.send(SessionEvent::Notification(...))`, and this fixture consumes
/// `event_rx` in the test itself. Nothing forwards that event to `notifications.gateway` — the
/// task that fills the fixture's `sent` vector drains `gateway_rx` instead, spawned by
/// `make_replay_send_update_fixture` in
/// `acp_session_tests/replay_buffer_send_update_tests.rs` — so `sent` stays empty for every
/// mutation of this gate and asserting on it would prove nothing. UI-level delivery is covered by
/// the replay buffer suite, not here.
#[tokio::test(flavor = "current_thread")]
async fn unowned_tool_search_call_does_not_leak_into_the_next_turn() {
    use xai_grok_sampler::{RequestId, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let stale = RequestId::random();

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: stale,
                    call_id: "stale-search-call".to_string(),
                    query: "stale query".to_string(),
                    limit: 3,
                })
                .await;
            tokio::task::yield_now().await;

            assert!(
                fixture.event_rx.try_recv().is_err(),
                "an unowned search call must not enqueue an ACP notification"
            );
        })
        .await;
}

/// The card opened while the turn owned the request must still close when its completion arrives
/// after a turn boundary dropped the ownership entry. Dropping that close leaves the pager card
/// spinning forever, so the close is admitted unowned exactly like a backend tool completion.
#[tokio::test(flavor = "current_thread")]
async fn owned_tool_search_card_is_closed_by_an_unowned_completion() {
    use xai_grok_sampler::{RequestId, SamplingEvent, ToolSearchStatus};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);
            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: req.clone(),
                    call_id: "search-call".to_string(),
                    query: "deploy the canary".to_string(),
                    limit: 5,
                })
                .await;
            let opened =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("the owned call must open the card")
                    .expect("event channel stays open");
            assert!(matches!(
                opened,
                SessionEvent::Notification(SessionNotification::Acp(_))
            ));

            actor.turn_stream_drained.lock().remove(&req);
            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCompleted {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    result_count: 3,
                    status: ToolSearchStatus::Completed,
                })
                .await;
            let closed =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("a completion for a visible card must close it even unowned")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = closed else {
                panic!("expected ACP notification event");
            };
            assert!(matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.0.as_ref() == "search-call"
                        && update.fields.status == Some(acp::ToolCallStatus::Completed)
            ));
        })
        .await;
}

/// A failed search must surface as an ACP `Failed` card, never as a silent success — the user
/// has to see that no tool definitions were loaded. `raw_output.status` carries the IR's own
/// failure word, `error`, not a shell-invented second spelling.
#[tokio::test(flavor = "current_thread")]
async fn failed_tool_search_completion_maps_to_failed_acp_status() {
    use xai_grok_sampler::{RequestId, SamplingEvent, ToolSearchStatus};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCompleted {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    result_count: 0,
                    status: ToolSearchStatus::Error,
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("a failed search completion must enqueue an update")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                panic!(
                    "expected an ACP ToolCallUpdate, got {:?}",
                    notification.update
                );
            };
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Failed),
                "ToolSearchStatus::Error must map onto the ACP failure status"
            );
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::json!({"result_count": 0, "status": "error"}))
            );
        })
        .await;
}

/// A status this build cannot name gets no word in `raw_output` at all. The IR's vocabulary is
/// `in_progress` / `completed` / `error` only, and the type deliberately carries no `Serialize`
/// because a derived impl would emit `"unknown"` — "a status string nothing in the corpus has ever
/// carried", as counted over 235 parsed documents by the no-`Serialize` corpus measurement block in
/// `ToolSearchStatus`'s doc, at
/// `xai-grok-sampling-types/src/conversation/tool_search.rs`. `"status": null` is measured at 0
/// occurrences there as well, so the key is omitted rather than nulled. The ACP status still carries
/// the decision, and it is THIS CUT's decision rather than the IR's: `Failed`, because an unmodelled
/// terminal may not render as a success and `Failed` is the only ACP status that says so without
/// claiming a terminal the wire never sent. The IR rules on a different question there — what an
/// unmodelled status means for PAIRING, answered on `ToolSearchItem::has_unmodelled_status`, where
/// the item routes away from both `Incomplete` and `Paired` — and this map borrows the structure of
/// that refusal, not its authority.
#[tokio::test(flavor = "current_thread")]
async fn unknown_tool_search_completion_omits_the_status_key_and_fails_the_card() {
    use xai_grok_sampler::{RequestId, SamplingEvent, ToolSearchStatus};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCompleted {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    result_count: 7,
                    status: ToolSearchStatus::Unknown,
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("an unknown terminal must still close the card")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                panic!(
                    "expected an ACP ToolCallUpdate, got {:?}",
                    notification.update
                );
            };
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Failed),
                "an unmodelled terminal may not render as a completed card"
            );
            let raw_output = update
                .fields
                .raw_output
                .as_ref()
                .expect("the result count must still be reported for an unknown terminal");
            // Whole-object equality IS the falsifier: `serde_json::Value` equality compares key sets,
            // so a re-added or nulled `status` dies here. A separate `get("status").is_none()` would
            // be strictly entailed by this assert and could never fire on its own.
            assert_eq!(
                raw_output,
                &serde_json::json!({"result_count": 7}),
                "`status` must be absent for a terminal the IR cannot name"
            );
        })
        .await;
}

/// The skeleton terminal has its own e2e verdict: a stream that only ever delivered the skeleton
/// must leave the card IN PROGRESS rather than invent a terminal the provider never wrote.
/// `ToolSearchStatus::InProgress` is the IR's streamed-skeleton state — its variant doc in
/// `xai-grok-sampling-types/src/conversation/tool_search.rs` reads "Streamed skeleton: present but
/// not yet final" — the typed map keeps it as `acp::ToolCallStatus::InProgress`
/// (`acp_session_impl/tool_dispatch.rs`, the total `tool_search_call_status` map), and unlike
/// `Unknown` this spelling IS attested on
/// the wire — `"in_progress"` x1 on a captured `tool_search_call`, the CX3 stream skeleton, counted
/// in the `tool_search_call` clause of the no-`Serialize` corpus measurement block in
/// `ToolSearchStatus`'s doc (the clause that also opens the `tool_search_output` counts) — so
/// `raw_output` carries the word. `result_count` is 0 because the skeleton carries no definitions yet.
#[tokio::test(flavor = "current_thread")]
async fn in_progress_tool_search_completion_leaves_the_card_in_progress() {
    use xai_grok_sampler::{RequestId, SamplingEvent, ToolSearchStatus};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            let req = RequestId::random();
            own_request(&actor, &req);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: req.clone(),
                    call_id: "search-call".to_string(),
                    query: "deploy the canary".to_string(),
                    limit: 5,
                })
                .await;
            let opened =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("the owned call must open the card")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = opened else {
                panic!("expected an ACP notification opening the card");
            };
            let acp::SessionUpdate::ToolCall(call) = &notification.update else {
                panic!("expected an ACP ToolCall, got {:?}", notification.update);
            };
            assert_eq!(call.tool_call_id.0.as_ref(), "search-call");
            assert_eq!(
                call.status,
                acp::ToolCallStatus::InProgress,
                "the call must open the card in progress"
            );

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCompleted {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    result_count: 0,
                    status: ToolSearchStatus::InProgress,
                })
                .await;
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), fixture.event_rx.recv())
                    .await
                    .expect("a skeleton terminal must still update the card")
                    .expect("event channel stays open");
            let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
                panic!("expected ACP notification event");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                panic!(
                    "expected an ACP ToolCallUpdate, got {:?}",
                    notification.update
                );
            };
            assert_eq!(update.tool_call_id.0.as_ref(), "search-call");
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::InProgress),
                "a skeleton may not be promoted to a terminal the wire never wrote"
            );
            assert_eq!(
                update.fields.raw_output,
                Some(serde_json::json!({"result_count": 0, "status": "in_progress"})),
                "raw_output must carry the count and the IR's own `in_progress` spelling"
            );
        })
        .await;
}

/// A search call is real output on the stream, so it stamps turn ttft exactly like a text or
/// reasoning token does. Without it a turn whose first output was a discovery call reports no
/// time-to-first-token at all.
#[tokio::test(flavor = "current_thread")]
async fn owned_tool_search_call_stamps_turn_first_token() {
    use xai_grok_sampler::{RequestId, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            actor.turn_phases.start();
            let req = RequestId::random();
            own_request_at_generation(&actor, &req, actor.turn_phases.current_generation());

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    query: "deploy the canary".to_string(),
                    limit: 5,
                })
                .await;

            let phases = actor.turn_phases.complete();
            assert!(
                phases.ttft_ms.is_some(),
                "a search call is the turn's first output and must stamp ttft"
            );
            assert!(
                phases.ttfm_ms.is_none(),
                "a search call is not meaningful text output, so ttfm stays unstamped"
            );
            // No control actor: a second actor would have had nothing to observe. `turn_phases` is
            // a per-actor `Arc::default()` created in `spawn.rs`, every first-token stamp on it goes
            // through `record_turn_first_token`, and the control this block replaces asserted on an
            // actor that had never handled an event. What the neighbours pin: the generation drop is
            // pinned by `tool_search_call_owned_at_another_generation_does_not_stamp_turn_first_token`
            // below; `unowned_tool_search_call_does_not_leak_into_the_next_turn` pins the CARD path
            // only — its sole observable is `event_rx`, and it neither starts nor reads `turn_phases`,
            // so it cannot see a stamp. What is NOT pinned is named rather than claimed: for an
            // unowned call to stamp this turn's ttft it would need a generation, and
            // `record_turn_first_token` takes one only from `turn_generation`, which reads the same
            // `turn_stream_drained` map, under the same key, that the ownership gate at the top of
            // `handle_sampling_event` reads — so a call that gate rejects has no entry to read one
            // from, and hoisting this stamp above the gate changes nothing observable. The gap opens
            // only if that lookup ever falls back to the live generation.
        })
        .await;
}

/// The stamp is REQUEST-scoped, not turn-scoped. `record_turn_first_token(Some(&id))` in
/// `sampling_events.rs` takes the generation off that request's OWN ownership entry via
/// `turn_generation` in that same file, and `TurnPhaseProfile::record_first_token`
/// (`xai-grok-telemetry/src/turn_phases.rs`) forwards to the private
/// `PhaseState::record_first_token` in that same file, whose
/// `generation != self.generation || completed.is_some() || first_token.is_some()` guard is the
/// drop that discards a stamp whose generation is not the profile's current one. Here the
/// ownership entry is stamped one generation off the turn being measured — the divergence
/// `Some(&id)` exists to express — so the stamp must be dropped and ttft must stay `None`.
///
/// This is the only falsifier for the request-scoped argument: `record_turn_first_token(None)` reads
/// `turn_generation(None) == Some(current_generation())`, so it stamps the current turn regardless of
/// which request produced the call, and every other test in this file owns the request at exactly the
/// current generation and cannot tell the two apart.
#[tokio::test(flavor = "current_thread")]
async fn tool_search_call_owned_at_another_generation_does_not_stamp_turn_first_token() {
    use xai_grok_sampler::{RequestId, SamplingEvent};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            actor.turn_phases.start();
            let req = RequestId::random();
            let not_current = actor.turn_phases.current_generation() + 1;
            own_request_at_generation(&actor, &req, not_current);

            actor
                .handle_sampling_event(SamplingEvent::ToolSearchCallReceived {
                    request_id: req,
                    call_id: "search-call".to_string(),
                    query: "deploy the canary".to_string(),
                    limit: 5,
                })
                .await;

            let phases = actor.turn_phases.complete();
            assert!(
                phases.ttft_ms.is_none(),
                "a call whose request is owned at generation {not_current}, not the turn's current \
                 one, must not stamp this turn's ttft"
            );
        })
        .await;
}
