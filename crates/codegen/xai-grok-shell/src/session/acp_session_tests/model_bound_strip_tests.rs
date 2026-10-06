//! Model-bound strip persistence policy (`acp_session_impl/model_bound_strip.rs`).
//! XSWITCH-1 (apex-ayl.58): covers the buffer on `ModelBoundStateStripped`, the deferred
//! persist that waits for the stripped retry to terminal (`Completed` or `Failed`), and the
//! portable-transcript contract of the stored history.

use super::support::*;
use super::*;
use xai_grok_sampler::{InferenceLatencyStats, RequestId, SamplingEvent};
use xai_grok_sampling_types::{
    BackendToolCallItem, BackendToolKind, ConversationItem, ConversationResponse,
};

fn model_bound_history() -> Vec<ConversationItem> {
    use xai_grok_sampling_types as t;
    let web_search: t::rs::WebSearchToolCall = serde_json::from_value(serde_json::json!({
        "action": {"type": "search", "query": "opaque state"},
        "id": "ws_mbs",
        "status": "completed"
    }))
    .expect("valid web search fixture");
    vec![
        ConversationItem::user("question for source model"),
        ConversationItem::Reasoning(t::rs::ReasoningItem {
            id: "rs_mbs".to_string(),
            summary: vec![t::rs::SummaryPart::SummaryText(t::rs::SummaryTextContent {
                text: "private continuation".to_string(),
            })],
            content: None,
            encrypted_content: Some("provider-signature".to_string()),
            status: None,
        }.into()),
        ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::WebSearch(web_search),
        }),
        ConversationItem::assistant("portable answer"),
    ]
}

fn has_model_bound_items(conv: &[ConversationItem]) -> bool {
    conv.iter()
        .any(|item| matches!(item, ConversationItem::Reasoning(_) | ConversationItem::BackendToolCall(_)))
}

async fn seed(actor: &SessionActor, items: Vec<ConversationItem>) {
    actor.chat_state_handle.replace_conversation(items);
    let conv = actor.chat_state_handle.get_conversation().await;
    assert!(has_model_bound_items(&conv), "precondition: model-bound items seeded");
}

/// The deferred apply runs inline in the ordered drainer; yield for a bounded settle.
async fn settle() {
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), async {
        loop {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn wait_for_conversation(
    actor: &SessionActor,
    cond: impl Fn(&[ConversationItem]) -> bool,
) -> Vec<ConversationItem> {
    let poll = async {
        loop {
            let conv = actor.chat_state_handle.get_conversation().await;
            if cond(&conv) {
                return conv;
            }
            tokio::task::yield_now().await;
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), poll).await {
        Ok(conv) => conv,
        Err(_) => actor.chat_state_handle.get_conversation().await,
    }
}

fn own_request(actor: &SessionActor, request_id: &RequestId) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    actor.turn_stream_drained.lock().insert(
        request_id.clone(),
        crate::session::acp_session::StreamOwnership::with_waiter(Some(tx)),
    );
}

fn completed_event(request_id: &RequestId) -> SamplingEvent {
    SamplingEvent::Completed {
        request_id: request_id.clone(),
        response: Box::new(ConversationResponse {
            items: vec![ConversationItem::assistant("recovered")],
            stop_reason: None,
            usage: None,
            cost_usd_ticks: None,
            message_chunks_emitted: 1,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        }),
        metrics: InferenceLatencyStats::default(),
    }
}

fn failed_event(request_id: &RequestId) -> SamplingEvent {
    SamplingEvent::Failed {
        request_id: request_id.clone(),
        error: xai_grok_sampler::SamplingErrorInfo {
            kind: xai_grok_sampler::SamplingErrorKind::Api,
            message: "503 Service Unavailable".to_string(),
            status_code: Some(503),
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
    }
}

fn model_bound_stripped(request_id: &RequestId, stripped: usize) -> SamplingEvent {
    SamplingEvent::ModelBoundStateStripped {
        request_id: request_id.clone(),
        stripped,
    }
}

/// The durable path: a confirmed model-bound strip is buffered on the event (stored history
/// untouched) and persisted when the stripped retry terminals `Completed`, leaving the
/// portable transcript in stored history.
#[tokio::test(flavor = "current_thread")]
async fn model_bound_stripped_persists_on_completed_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed(&actor, model_bound_history()).await;
            let rid = RequestId::from("req-mbs-completed");
            own_request(&actor, &rid);

            actor
                .handle_sampling_event(model_bound_stripped(&rid, 2))
                .await;
            settle().await;
            let buffered = actor.chat_state_handle.get_conversation().await;
            assert!(
                has_model_bound_items(&buffered),
                "the strip must be buffered, not persisted, on the event: {buffered:?}"
            );
            assert!(
                actor.pending_model_bound_strip.lock().contains_key(&rid),
                "a pending model-bound strip entry must exist after the event"
            );

            actor.handle_sampling_event(completed_event(&rid)).await;
            let conv = wait_for_conversation(&actor, |c| !has_model_bound_items(c)).await;
            assert_eq!(conv.len(), 2, "the portable user + assistant items survive");
            assert!(
                actor.pending_model_bound_strip.lock().is_empty(),
                "the pending entry must be cleared after the persist"
            );
        })
        .await;
}

/// A `Failed` terminal persists too: the server rejected the markers either way, and the
/// portable transcript is the correct stored state for the next turn.
#[tokio::test(flavor = "current_thread")]
async fn model_bound_stripped_persists_on_failed_terminal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed(&actor, model_bound_history()).await;
            let rid = RequestId::from("req-mbs-failed");
            own_request(&actor, &rid);

            actor
                .handle_sampling_event(model_bound_stripped(&rid, 2))
                .await;
            settle().await;

            actor.handle_sampling_event(failed_event(&rid)).await;
            let conv = wait_for_conversation(&actor, |c| !has_model_bound_items(c)).await;
            assert_eq!(conv.len(), 2, "the portable items survive the failed terminal");
            assert!(
                actor.pending_model_bound_strip.lock().is_empty(),
                "the pending entry must be cleared after the persist"
            );
        })
        .await;
}

/// Without a `ModelBoundStateStripped` event the terminal must not touch stored history.
#[tokio::test(flavor = "current_thread")]
async fn no_model_bound_strip_event_means_no_persist() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed(&actor, model_bound_history()).await;
            let rid = RequestId::from("req-mbs-none");
            own_request(&actor, &rid);

            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                has_model_bound_items(&conv),
                "no strip event must mean stored history is untouched: {conv:?}"
            );
        })
        .await;
}

/// The stored conversation may have been replaced (e.g. by a rewind) after the strip event
/// was buffered: the persist is a typed NoMatch and leaves the new history intact.
#[tokio::test(flavor = "current_thread")]
async fn model_bound_strip_on_already_portable_history_is_no_match() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed(&actor, model_bound_history()).await;
            let rid = RequestId::from("req-mbs-nomatch");
            own_request(&actor, &rid);

            actor
                .handle_sampling_event(model_bound_stripped(&rid, 2))
                .await;
            settle().await;
            // Rewind-shaped replacement: stored history is now portable-only.
            actor
                .chat_state_handle
                .replace_conversation(vec![
                    ConversationItem::user("rewound question"),
                    ConversationItem::assistant("rewound answer"),
                ]);
            let _ = actor.chat_state_handle.get_conversation().await; // sync point

            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;

            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(conv.len(), 2, "the replaced history must be intact");
            assert!(
                !has_model_bound_items(&conv),
                "the replaced history was already portable"
            );
            assert!(
                actor.pending_model_bound_strip.lock().is_empty(),
                "the pending entry must be cleared even on NoMatch"
            );
        })
        .await;
}

/// Rewind claims the history: queued model-bound strip work is invalidated synchronously
/// (mirror of the image-strip `cancel_pending_image_strips_for_rewind` wiring — the
/// concordance MINOR on XSWITCH-1). The strip must not apply against the post-rewind
/// state; a session that needs it again re-pays the 503 + strip cycle (the accepted
/// CROSSWIRE-1 baseline), which is the conservative, barrier-safe direction.
#[tokio::test(flavor = "current_thread")]
async fn rewind_cancels_queued_model_bound_strip() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor =
                Arc::new(create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await);
            seed(&actor, model_bound_history()).await;

            let rid = RequestId::from("req-mbs-rewind-queued");
            own_request(&actor, &rid);
            actor
                .handle_sampling_event(model_bound_stripped(&rid, 2))
                .await;
            assert!(
                actor.pending_model_bound_strip.lock().contains_key(&rid),
                "precondition: strip queued for deferred persist"
            );

            // Give the seeded turn a tracked prompt so the rewind target is valid
            // (handle_rewind early-returns success:false for target >= current index).
            let mut snapshot = actor
                .chat_state_handle
                .snapshot()
                .await
                .expect("snapshot available");
            if let ConversationItem::User(first) = &mut snapshot.conversation[0] {
                first.prompt_index = Some(0);
            }
            snapshot.prompt_index = 1;
            snapshot.prompt_texts = vec!["switch turn".into()];
            actor.chat_state_handle.restore_snapshot(snapshot);
            let _ = actor.chat_state_handle.get_conversation().await;

            let response = actor
                .handle_rewind(RewindRequest {
                    target_prompt_index: 0,
                    force: true,
                    mode: RewindMode::ConversationOnly,
                })
                .await
                .expect("rewind call completes");
            assert!(
                response.success,
                "rewind must succeed, not early-return: {:?}",
                response.error
            );

            assert!(
                actor.pending_model_bound_strip.lock().is_empty(),
                "rewind must cancel queued model-bound strip work (mirror of the image-strip rewind cancel)"
            );

            // A terminal arriving after the rewind is a no-op: the entry is gone, so
            // nothing persists against the post-rewind state.
            actor.handle_sampling_event(completed_event(&rid)).await;
            settle().await;
            assert!(
                actor.pending_model_bound_strip.lock().is_empty(),
                "a post-rewind terminal must not resurrect the cancelled strip"
            );
        })
        .await;
}
