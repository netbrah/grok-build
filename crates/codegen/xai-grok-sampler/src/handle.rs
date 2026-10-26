use tokio::sync::{mpsc, oneshot};

use xai_grok_sampling_types::{ConversationRequest, ConversationResponse, SamplingError};

use crate::actor::request_task::CompletionResult;
use crate::commands::SamplerCommand;
use crate::config::SamplerConfig;
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// One doom-loop recovery action performed during a request.
pub struct DoomLoopRecoveryAttempt {
    pub triggers: Vec<String>,
    pub aborted_at_chunk: Option<u64>,
}

/// Awaited sampling result plus request-level detector metadata.
pub struct CollectedSamplingResult {
    /// Terminal response and metrics, or the rich sampling error.
    pub result: Result<(ConversationResponse, InferenceLatencyStats), SamplingError>,
    /// Whether a terminal event was successfully queued on the event rail.
    pub terminal_event_queued: bool,
    /// Deduplicated raw labels observed across every attempt in the request.
    pub doom_loop_signals: Vec<String>,
    /// Doom-loop recovery actions performed for this request.
    pub doom_loop_recovery_attempts: Vec<DoomLoopRecoveryAttempt>,
}

/// Cheaply-cloneable handle to the sampler actor.
/// Internally just an `mpsc::UnboundedSender<SamplerCommand>`.
/// All methods are non-blocking (fire-and-forget) except for the `*_async` queries which return a future awaiting an `oneshot::Receiver`.
#[derive(Clone)]
pub struct SamplerHandle {
    cmd_tx: mpsc::UnboundedSender<SamplerCommand>,
}

impl SamplerHandle {
    /// `pub(crate)` because only [`SamplerActor::spawn`](crate::actor::SamplerActor::spawn) produces one of these.
    pub(crate) fn new(cmd_tx: mpsc::UnboundedSender<SamplerCommand>) -> Self {
        Self { cmd_tx }
    }

    /// Create a no-op handle that discards all commands.
    ///
    /// Useful for tests and callers that need a `SamplerHandle` field before the actor is wired up.
    pub fn noop() -> Self {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        // Receiver is dropped immediately; sends will fail but every send-site uses `let _ = ...` so that is fine
        Self { cmd_tx }
    }

    /// Test seam: `notify` fires when the first command is submitted.
    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_on_submit(notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            if cmd_rx.recv().await.is_some() {
                notify.notify_one();
            }
            while cmd_rx.recv().await.is_some() {}
        });
        Self { cmd_tx }
    }

    /// Fire-and-forget: results arrive via the shared event channel.
    pub fn submit(&self, request_id: RequestId, request: ConversationRequest) {
        let _ = self.cmd_tx.send(SamplerCommand::Submit {
            request_id,
            request: Box::new(request),
            config: None,
            completion_tx: None,
        });
    }

    /// Submit a sampling request with an explicit per-request config override (e.g., a different model than the actor's default).
    pub fn submit_with_config(
        &self,
        request_id: RequestId,
        request: ConversationRequest,
        config: SamplerConfig,
    ) {
        let _ = self.cmd_tx.send(SamplerCommand::Submit {
            request_id,
            request: Box::new(request),
            config: Some(Box::new(config)),
            completion_tx: None,
        });
    }

    /// No-op if the request id is unknown (already finished or never submitted).
    pub fn cancel(&self, request_id: RequestId) {
        let _ = self.cmd_tx.send(SamplerCommand::Cancel { request_id });
    }

    /// Update the default sampling config (e.g., after model switch or auth refresh).
    /// The next request submitted without an override will use it.
    pub fn update_config(&self, config: SamplerConfig) {
        let _ = self.cmd_tx.send(SamplerCommand::UpdateConfig {
            config: Box::new(config),
        });
    }

    /// Query whether a request is still in flight.
    /// Returns `false` for unknown / finished / cancelled ids.
    pub async fn is_active(&self, request_id: RequestId) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SamplerCommand::IsActive {
            request_id,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or(false)
    }

    /// Query the number of in-flight requests.
    /// Returns 0 if the actor has been shut down.
    pub async fn active_count(&self) -> usize {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(SamplerCommand::ActiveCount { reply: reply_tx });
        reply_rx.await.unwrap_or(0)
    }

    /// Submit a request and await its completion. Events still flow to the shared channel for live UI updates.
    /// This method also awaits the per-request completion oneshot so the caller gets a clean `Result` without filtering events.
    /// Sequential callers (compaction, summary, `/btw` side questions) use this.
    pub async fn submit_and_collect(
        &self,
        request_id: RequestId,
        request: ConversationRequest,
    ) -> CompletionResult {
        self.submit_and_collect_with_metadata(request_id, request)
            .await
            .result
    }

    /// Submit and also return detector labels observed on discarded attempts.
    pub async fn submit_and_collect_with_metadata(
        &self,
        request_id: RequestId,
        request: ConversationRequest,
    ) -> CollectedSamplingResult {
        self.submit_and_collect_inner(request_id, request, None)
            .await
    }

    /// Submit with a per-request config and return completion and detector metadata.
    pub async fn submit_and_collect_with_config(
        &self,
        request_id: RequestId,
        request: ConversationRequest,
        config: SamplerConfig,
    ) -> CollectedSamplingResult {
        self.submit_and_collect_inner(request_id, request, Some(config))
            .await
    }

    async fn submit_and_collect_inner(
        &self,
        request_id: RequestId,
        request: ConversationRequest,
        config: Option<SamplerConfig>,
    ) -> CollectedSamplingResult {
        // RAII guard: when this future is dropped (cancel, panic, or normal return), tell the sampler actor to cancel the in-flight request_id
        // No-op if the actor already finished and removed it from its active set
        struct CancelOnDrop {
            cmd_tx: mpsc::UnboundedSender<SamplerCommand>,
            request_id: RequestId,
        }
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                let _ = self.cmd_tx.send(SamplerCommand::Cancel {
                    request_id: self.request_id.clone(),
                });
            }
        }

        let (completion_tx, completion_rx) = oneshot::channel();
        let cancel_id = request_id.clone();

        // Only create the guard if Submit actually reached the actor
        let _guard = self
            .cmd_tx
            .send(SamplerCommand::Submit {
                request_id,
                request: Box::new(request),
                config: config.map(Box::new),
                completion_tx: Some(completion_tx),
            })
            .ok()
            .map(|_| CancelOnDrop {
                cmd_tx: self.cmd_tx.clone(),
                request_id: cancel_id,
            });
        completion_rx
            .await
            .unwrap_or_else(|_| CollectedSamplingResult {
                result: Err(SamplingError::auth_unknown(
                    "sampler actor dropped before completion",
                )),
                terminal_event_queued: false,
                doom_loop_signals: Vec::new(),
                doom_loop_recovery_attempts: Vec::new(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::attribution::{Auth401AttributionCallback, SamplingConsumer};
    use crate::config::{AuthScheme, BearerResolver, HeaderInjector, OriginClientInfo};
    use xai_grok_sampling_types::conversation::ToolCacheBreakpoint;
    use xai_grok_sampling_types::{
        ApiBackend, CompactionAtTokens, CompactionsRemaining, ConversationItem,
        DoomLoopRecoveryPolicy, DoomLoopSignal, ReasoningEffort, SentCredential, StopReason,
        TokenUsage,
    };

    #[derive(Debug)]
    struct TransportHooks;

    impl BearerResolver for TransportHooks {
        fn current_bearer(&self) -> Option<String> {
            panic!("queueing a request must not resolve its bearer");
        }
    }

    impl HeaderInjector for TransportHooks {
        fn inject(&self, _: &mut reqwest::header::HeaderMap) {
            panic!("queueing a request must not inject transport headers");
        }
    }

    impl Auth401AttributionCallback for TransportHooks {
        fn record_401(&self, _: SamplingConsumer, _: Option<&str>) {
            panic!("queueing a request must not invoke auth attribution");
        }
    }

    fn explicit_config() -> SamplerConfig {
        SamplerConfig {
            api_key: Some("u2-row-a-key".into()),
            base_url: "https://row-a.invalid/v1".into(),
            mtls_cert_dir: Some("u2-row-a-identity".into()),
            model: "row-a".into(),
            max_completion_tokens: Some(321),
            temperature: Some(0.25),
            top_p: Some(0.75),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            extra_headers: [("x-u2-header".into(), "a-header".into())].into(),
            extra_response_includes: vec!["reasoning.encrypted_content".into()],
            query_params: [("u2-query".into(), "a-query".into())].into(),
            env_http_headers: [("x-u2-env".into(), "U2_TEST_HEADER".into())].into(),
            context_window: 123_456,
            force_http1: true,
            max_retries: Some(2),
            rate_limit_retry_threshold: Some(3),
            stream_tool_calls: true,
            idle_timeout_secs: Some(19),
            reasoning_effort: Some(ReasoningEffort::High),
            model_family: Some("u2-family".into()),
            strict_responses_input: true,
            normalize_content_types: true,
            ultra_wire_effort: Some(ReasoningEffort::High),
            origin_client: Some(OriginClientInfo {
                product: "u2-client".into(),
                version: Some("1".into()),
            }),
            client_identifier: Some("u2-client-id".into()),
            deployment_id: Some("u2-deployment".into()),
            user_id: Some("u2-user".into()),
            conversation_group_id: Some("u2-conversation-group".to_string().into()),
            client_version: Some("u2-version".into()),
            attribution_callback: Some(Arc::new(TransportHooks)),
            bearer_resolver: Some(Arc::new(TransportHooks)),
            supports_backend_search: true,
            compactions_remaining: Some(CompactionsRemaining::Fixed(2)),
            compaction_at_tokens: Some(CompactionAtTokens::Fixed(90_000)),
            cache_ttl: Some("1h".into()),
            thinking_replay: Some("off".into()),
            doom_loop_recovery: Some(DoomLoopRecoveryPolicy {
                max_threshold: 4,
                max_retries: 2,
                window_tokens: 2048,
            }),
            header_injector: Some(Arc::new(TransportHooks)),
            top_k: Some(13),
            stop_sequences: Some(vec!["u2-stop".into()]),
            disable_parallel_tool_use: Some(true),
            tool_cache_breakpoint: Some(ToolCacheBreakpoint::Last),
            server_tools: Some(vec!["web_search_20250305".into()]),
            mcp_servers: Some(vec![xai_grok_sampling_types::messages::McpServerDecl {
                name: "u2-server".into(),
                url: "https://u2-server.invalid".into(),
                authorization_token: Some("u2-server-token".into()),
                tool_configuration: Some(serde_json::json!({"enabled": false})),
            }]),
            mcp_toolset_server: Some("u2-server".into()),
        }
    }

    fn assert_config_eq(actual: &SamplerConfig, expected: &SamplerConfig) {
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "collecting Submit must carry the complete explicit config"
        );
        assert!(Arc::ptr_eq(
            actual.bearer_resolver.as_ref().unwrap(),
            expected.bearer_resolver.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            actual.attribution_callback.as_ref().unwrap(),
            expected.attribution_callback.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            actual.header_injector.as_ref().unwrap(),
            expected.header_injector.as_ref().unwrap()
        ));
    }

    fn request() -> ConversationRequest {
        ConversationRequest {
            items: vec![ConversationItem::user("u2 request")],
            model: Some("row-a".into()),
            ..Default::default()
        }
    }

    fn rich_completion() -> CollectedSamplingResult {
        CollectedSamplingResult {
            result: Ok((
                ConversationResponse {
                    items: vec![ConversationItem::assistant_with_model("u2 reply", "row-a")],
                    stop_reason: Some(StopReason::Stop),
                    usage: Some(TokenUsage {
                        prompt_tokens: 101,
                        completion_tokens: 17,
                        total_tokens: 118,
                        reasoning_tokens: 3,
                        cached_prompt_tokens: 41,
                        cache_creation_prompt_tokens: 13,
                        cache_creation_5m_input_tokens: 5,
                        cache_creation_1h_input_tokens: 8,
                    }),
                    cost_usd_ticks: Some(123),
                    message_chunks_emitted: 3,
                    doom_loop_signals: vec![DoomLoopSignal::parse("tail_repetition:4@thinking")],
                    stop_message: Some("u2 stop detail".into()),
                    message_id: Some("u2-message".into()),
                    raw_stop_reason: Some("end_turn".into()),
                    stop_sequence: Some("u2-stop".into()),
                },
                InferenceLatencyStats {
                    time_to_first_token_ms: Some(7),
                    time_to_last_byte_ms: 31,
                    chunk_count: 3,
                    itl_intervals_ms: vec![5, 11],
                    itl_p50_ms: Some(11),
                    itl_p99_ms: Some(11),
                    itl_max_ms: Some(11),
                    itl_mean_ms: Some(8),
                    attempts: 2,
                },
            )),
            terminal_event_queued: true,
            doom_loop_signals: vec![
                "tail_repetition:4@thinking".into(),
                "low_logprob@response".into(),
            ],
            doom_loop_recovery_attempts: vec![
                DoomLoopRecoveryAttempt {
                    triggers: vec!["tail_repetition:4@thinking".into()],
                    aborted_at_chunk: Some(9),
                },
                DoomLoopRecoveryAttempt {
                    triggers: vec!["tail_repetition:2@thinking".into()],
                    aborted_at_chunk: None,
                },
            ],
        }
    }

    fn rich_failure() -> CollectedSamplingResult {
        let mut result = rich_completion();
        result.result = Err(SamplingError::DoomLoopDetected {
            triggers: vec!["tail_repetition:2@thinking".into()],
            aborted_at_chunk: Some(12),
        });
        result.terminal_event_queued = false;
        result
    }

    fn collected_value(collected: &CollectedSamplingResult) -> serde_json::Value {
        let CollectedSamplingResult {
            result,
            terminal_event_queued,
            doom_loop_signals,
            doom_loop_recovery_attempts,
        } = collected;
        // Terminal types have no PartialEq; their Debug covers the complete response/error and metrics.
        serde_json::json!({
            "result": format!("{result:?}"),
            "terminal_event_queued": terminal_event_queued,
            "doom_loop_signals": doom_loop_signals,
            "doom_loop_recovery_attempts": doom_loop_recovery_attempts.iter().map(|attempt| {
                let DoomLoopRecoveryAttempt { triggers, aborted_at_chunk } = attempt;
                (triggers, aborted_at_chunk)
            }).collect::<Vec<_>>(),
        })
    }

    fn assert_fallback(actual: &CollectedSamplingResult) {
        let expected = CollectedSamplingResult {
            result: Err(SamplingError::Auth {
                message: "sampler actor dropped before completion".into(),
                credential: SentCredential::Unknown,
            }),
            terminal_event_queued: false,
            doom_loop_signals: Vec::new(),
            doom_loop_recovery_attempts: Vec::new(),
        };
        assert_eq!(collected_value(actual), collected_value(&expected));
    }

    fn assert_one_cancel(commands: &mut mpsc::UnboundedReceiver<SamplerCommand>, id: &RequestId) {
        assert!(
            matches!(commands.try_recv(), Ok(SamplerCommand::Cancel { request_id }) if request_id == *id),
            "successful Submit must queue one matching Cancel"
        );
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn collecting_submit_carries_explicit_config() {
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let handle = SamplerHandle::new(commands_tx);
        let default = SamplerConfig {
            api_key: Some("u2-row-b-key".into()),
            base_url: "https://row-b.invalid/v1".into(),
            model: "row-b".into(),
            api_backend: ApiBackend::Responses,
            ..Default::default()
        };
        handle.update_config(default.clone());
        let SamplerCommand::UpdateConfig { config } = commands.try_recv().unwrap() else {
            panic!("expected the queued default B update");
        };
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::to_value(default).unwrap()
        );

        let id = RequestId::from("u2-explicit");
        let config = explicit_config();
        let request = request();
        let mut collecting = Box::pin(handle.submit_and_collect_with_config(
            id.clone(),
            request.clone(),
            config.clone(),
        ));
        assert!(futures_util::poll!(collecting.as_mut()).is_pending());
        let SamplerCommand::Submit {
            request_id,
            request: submitted,
            config: supplied,
            completion_tx,
        } = commands.try_recv().unwrap()
        else {
            panic!("expected the collecting Submit");
        };
        assert_eq!(request_id, id);
        assert_eq!(format!("{submitted:?}"), format!("{request:?}"));
        assert!(
            supplied.is_some(),
            "collecting Submit must carry the complete explicit config"
        );
        assert_config_eq(supplied.as_deref().unwrap(), &config);
        let expected = rich_completion();
        let expected_value = collected_value(&expected);
        assert!(completion_tx.unwrap().send(expected).is_ok());
        assert_eq!(collected_value(&collecting.await), expected_value);
        assert_one_cancel(&mut commands, &id);
    }

    #[tokio::test]
    async fn collecting_submit_preserves_completion_metadata() {
        for explicit in [true, false] {
            for completion in [rich_completion(), rich_failure()] {
                let (commands_tx, mut commands) = mpsc::unbounded_channel();
                let handle = SamplerHandle::new(commands_tx);
                let id = RequestId::from("u2-metadata");
                let mut collecting = Box::pin(async {
                    if explicit {
                        handle
                            .submit_and_collect_with_config(
                                id.clone(),
                                request(),
                                explicit_config(),
                            )
                            .await
                    } else {
                        handle
                            .submit_and_collect_with_metadata(id.clone(), request())
                            .await
                    }
                });
                assert!(futures_util::poll!(collecting.as_mut()).is_pending());
                let SamplerCommand::Submit {
                    request_id,
                    config,
                    completion_tx,
                    ..
                } = commands.try_recv().unwrap()
                else {
                    panic!("expected the collecting Submit");
                };
                assert_eq!(request_id, id);
                if !explicit {
                    assert!(
                        config.is_none(),
                        "legacy metadata collection must retain default config"
                    );
                }
                let expected = collected_value(&completion);
                assert!(completion_tx.unwrap().send(completion).is_ok());
                assert_eq!(
                    collected_value(&collecting.await),
                    expected,
                    "collecting completion must preserve the full result and detector metadata"
                );
                assert_one_cancel(&mut commands, &id);
            }
        }

        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let handle = SamplerHandle::new(commands_tx);
        let id = RequestId::from("u2-legacy-plain");
        let mut collecting = Box::pin(handle.submit_and_collect(id.clone(), request()));
        assert!(futures_util::poll!(collecting.as_mut()).is_pending());
        let SamplerCommand::Submit {
            request_id,
            config,
            completion_tx,
            ..
        } = commands.try_recv().unwrap()
        else {
            panic!("expected the legacy collecting Submit");
        };
        assert_eq!(request_id, id);
        assert!(config.is_none());
        let completion = rich_completion();
        let expected = format!("{:?}", completion.result);
        assert!(completion_tx.unwrap().send(completion).is_ok());
        assert_eq!(format!("{:?}", collecting.await), expected);
        assert_one_cancel(&mut commands, &id);
    }

    #[tokio::test]
    async fn collecting_submit_cancels_only_after_successful_send() {
        let closed = SamplerHandle::noop();
        assert_fallback(
            &closed
                .submit_and_collect_with_config(
                    "u2-closed-explicit".into(),
                    request(),
                    explicit_config(),
                )
                .await,
        );
        assert_fallback(
            &closed
                .submit_and_collect_with_metadata("u2-closed-legacy".into(), request())
                .await,
        );

        for explicit in [true, false] {
            let (commands_tx, mut commands) = mpsc::unbounded_channel();
            let handle = SamplerHandle::new(commands_tx);
            let id = RequestId::from("u2-completion-loss");
            let mut collecting = Box::pin(async {
                if explicit {
                    handle
                        .submit_and_collect_with_config(id.clone(), request(), explicit_config())
                        .await
                } else {
                    handle
                        .submit_and_collect_with_metadata(id.clone(), request())
                        .await
                }
            });
            assert!(futures_util::poll!(collecting.as_mut()).is_pending());
            let SamplerCommand::Submit {
                request_id,
                completion_tx,
                ..
            } = commands.try_recv().unwrap()
            else {
                panic!("expected the collecting Submit");
            };
            assert_eq!(request_id, id);
            drop(completion_tx.unwrap());
            assert_fallback(&collecting.await);
            assert_one_cancel(&mut commands, &id);
        }
    }

    #[tokio::test]
    async fn collecting_submit_precedes_cancel_on_open_channel() {
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let handle = SamplerHandle::new(commands_tx);
        let id = RequestId::from("u2-in-flight");
        let config = explicit_config();
        let request = request();
        let mut collecting = Box::pin(handle.submit_and_collect_with_config(
            id.clone(),
            request.clone(),
            config.clone(),
        ));
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(futures_util::poll!(collecting.as_mut()).is_pending());
        let first = commands.try_recv().unwrap();
        assert!(
            matches!(&first, SamplerCommand::Submit { .. }),
            "first queued command must be Submit"
        );
        let SamplerCommand::Submit {
            request_id,
            request: submitted,
            config: supplied,
            completion_tx,
        } = first
        else {
            unreachable!();
        };
        assert_eq!(request_id, id);
        assert_eq!(format!("{submitted:?}"), format!("{request:?}"));
        assert!(
            supplied.is_some(),
            "collecting Submit must carry the complete explicit config"
        );
        assert_config_eq(supplied.as_deref().unwrap(), &config);
        let completion_tx = completion_tx.unwrap();
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(collecting);
        assert_one_cancel(&mut commands, &id);
        assert!(completion_tx.is_closed());
        drop(completion_tx);
    }
}
