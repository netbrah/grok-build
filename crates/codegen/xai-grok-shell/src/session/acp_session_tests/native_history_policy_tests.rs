use super::support::*;
use super::*;
use serde_json::{Value, json};
use xai_grok_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};

const NATIVE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum NativeAckPolicy {
    Pass,
    WriteFailure,
    LoseReply,
    HoldReply,
}

struct NativeHistorySession {
    actor: Arc<SessionActor>,
    persistence: crate::session::persistence::PersistenceHandle,
    directory: tempfile::TempDir,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _models: tempfile::TempDir,
    observed: Arc<std::sync::Mutex<Vec<xai_grok_sampler::SamplingEvent>>>,
    observed_changed: Arc<tokio::sync::Notify>,
    native_end_processed: Arc<tokio::sync::Notify>,
    native_ended: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for NativeHistorySession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl NativeHistorySession {
    async fn new(server: &MockInferenceServer) -> Self {
        Self::with_options(server, 0, NativeAckPolicy::Pass).await
    }
    async fn with_retries(server: &MockInferenceServer, max_attempts: u32) -> Self {
        Self::with_options(server, max_attempts, NativeAckPolicy::Pass).await
    }
    async fn with_ack_policy(server: &MockInferenceServer, policy: NativeAckPolicy) -> Self {
        Self::with_options(server, 0, policy).await
    }
    async fn with_options(
        server: &MockInferenceServer,
        max_attempts: u32,
        policy: NativeAckPolicy,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = xai_grok_sampler::SamplerConfig {
            base_url: server.url(),
            model: "test".into(),
            api_key: Some("test-key".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096),
            max_retries: Some(max_attempts),
            ..Default::default()
        };
        let info = crate::session::info::Info {
            id: acp::SessionId::new(format!(
                "native-history-{}",
                directory.path().file_name().unwrap().to_string_lossy()
            )),
            cwd: "/tmp".into(),
        };
        let persistence = crate::session::persistence::new_with_explicit_dir(
            &info,
            directory.path().to_path_buf(),
            acp::ModelId::new("test"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
            "test".into(),
            crate::session::persistence::ExplicitSessionOpen::New {
                identity: None,
                next_trace_turn: None,
            },
        )
        .await
        .unwrap();
        let mut tasks = Vec::new();
        let chat_tx = if matches!(
            policy,
            NativeAckPolicy::LoseReply | NativeAckPolicy::HoldReply
        ) {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let real = persistence.tx.clone();
            tasks.push(tokio::task::spawn_local(async move {
                while let Some(message) = rx.recv().await {
                    match message {
                        PersistenceMsg::PersistNativeGenerationAndAck { write, respond_to } => {
                            let (disk_tx, disk_rx) = tokio::sync::oneshot::channel();
                            real.send(PersistenceMsg::PersistNativeGenerationAndAck {
                                write,
                                respond_to: disk_tx,
                            })
                            .unwrap();
                            let actual = disk_rx.await.unwrap();
                            assert!(
                                actual.is_ok(),
                                "the real adapter must finish before injecting lost ACK"
                            );
                            if matches!(policy, NativeAckPolicy::HoldReply) {
                                let _held_reply = respond_to;
                                std::future::pending::<()>().await;
                            } else {
                                drop(respond_to);
                            }
                        }
                        message => {
                            real.send(message).unwrap();
                        }
                    }
                }
            }));
            tx
        } else {
            persistence.tx.clone()
        };
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway_task = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (mut actor, _) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence.tx.clone(),
            Box::new(crate::session::chat_persistence::ChannelChatPersistence::new(chat_tx)),
        )
        .await;
        actor.session_info = info;
        actor.max_retries = max_attempts;
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        let (manager, models) = manager_with_entries();
        let mut entry = entry_with_search_flag("test", false);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("test-key".into());
        manager.insert_test_entry("test", entry);
        manager.set_current_model_id(acp::ModelId::new("test"));
        actor.models_manager = manager;
        let mut sampling = actor.chat_state_handle.get_sampling_config().await.unwrap();
        sampling.base_url = server.url();
        sampling.model = "test".into();
        sampling.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        sampling.max_completion_tokens = Some(4096);
        sampling.max_retries = Some(max_attempts);
        actor.chat_state_handle.update_sampling_config(sampling);
        let mut credentials = actor.chat_state_handle.get_credentials().await;
        credentials.api_key = Some("test-key".into());
        actor.chat_state_handle.update_credentials(credentials);
        let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            config,
            xai_grok_sampler::RetryPolicy {
                max_retries: max_attempts,
                ..Default::default()
            },
            sampler_tx,
        );
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = observed.clone();
        let observed_changed = Arc::new(tokio::sync::Notify::new());
        let changed = observed_changed.clone();
        let native_end_processed = Arc::new(tokio::sync::Notify::new());
        let end_processed = native_end_processed.clone();
        let native_ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended = native_ended.clone();
        let sampler_task = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                let end = matches!(&event, xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent::End, ..
                });
                recorded.lock().unwrap().push(event.clone());
                changed.notify_one();
                drainer.handle_sampling_event(event).await;
                if end {
                    ended.store(true, std::sync::atomic::Ordering::SeqCst);
                    end_processed.notify_one();
                }
            }
        });
        Self {
            actor,
            persistence,
            directory,
            tasks: {
                tasks.extend([gateway_task, sampler_task]);
                tasks
            },
            _models: models,
            observed,
            observed_changed,
            native_end_processed,
            native_ended,
        }
    }

    async fn sample_outcome(&self) -> Result<SamplerTurnOutcome, acp::Error> {
        self.actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user("find the account"))
            .await;
        let (mut request, binding) = self
            .actor
            .chat_state_handle
            .build_request_with_binding(
                xai_chat_state::NativeToolCandidates::builtins_only(Vec::new()),
                None,
                false,
                None,
                self.actor.session_id_string(),
                "native-history-observer".into(),
            )
            .await
            .unwrap();
        request.x_grok_turn_idx = Some(self.actor.current_turn_number.get().to_string());
        let mut budget = self.actor.rate_limit_wait_budget(None);
        self.actor
            .run_turn_via_sampler(
                request,
                binding,
                &mut budget,
                sampler_turn::TransientRetryState {
                    step_attempts: 0,
                    prompt_attempts: 0,
                    episode_start: None,
                    enabled: false,
                },
                false,
                TurnParkState::Fresh,
            )
            .await
    }

    async fn sample(&self) {
        let outcome = self.sample_outcome().await.unwrap();
        let SamplerTurnOutcome::Response(response, metrics) = outcome else {
            panic!("expected the real accepted Messages response");
        };
        self.actor
            .record_response_token_usage(&response, Some(metrics.time_to_last_byte_ms));
        self.actor
            .record_response_items(response.items, response.usage.is_some())
            .await;
        self.actor.chat_state_handle.get_conversation().await;
        let (respond_to, ack) = tokio::sync::oneshot::channel();
        self.persistence
            .tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .unwrap();
        tokio::time::timeout(NATIVE_TIMEOUT, ack)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn native_history_frames() -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{
            "id":"msg_native_history","type":"message","role":"assistant","model":"test",
            "content":[],"stop_reason":null,"usage":{"input_tokens":11,"output_tokens":0,
                "cache_read_input_tokens":4,"cache_creation_input_tokens":0,
                "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}}
        }}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"before"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"step"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":" by step"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{
            "type":"server_tool_use","id":"srvtoolu_native_history","name":"tool_search_tool_regex","input":{}}
        }),
        json!({"type":"content_block_delta","index":2,"delta":{
            "type":"input_json_delta","partial_json":"{\"query\":\"account\"}"}
        }),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{
            "type":"tool_search_tool_result","tool_use_id":"srvtoolu_native_history",
            "content":{"type":"tool_search_tool_search_result","tool_references":[
                {"type":"tool_reference","tool_name":"read_file"}]}}
        }),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"content_block_start","index":4,"content_block":{"type":"text","text":"after"}}),
        json!({"type":"content_block_stop","index":4}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{
            "input_tokens":11,"output_tokens":7,"thinking_tokens":2,"cache_read_input_tokens":4}}
        ),
        json!({"type":"message_stop"}),
    ]
}

fn native_history_script(frames: Vec<Value>) -> ScriptedResponse {
    ScriptedResponse::sse(
        frames
            .into_iter()
            .map(|frame| SseEvent::data(frame.to_string()))
            .collect(),
    )
}

fn native_blob(directory: &std::path::Path, reference: &Value) -> Value {
    use sha2::{Digest, Sha256};
    let digest = reference["digest"].as_str().expect("native blob digest");
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    let bytes = std::fs::read(
        directory
            .join("native_history/blobs")
            .join(format!("{digest}.json")),
    )
    .unwrap();
    assert_eq!(bytes.len() as u64, reference["byte_len"].as_u64().unwrap());
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), digest);
    assert!(!bytes.ends_with(b"\n"));
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn persisted_versioned_native_record() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            let ordinary = native_untracked_output(native_history_frames()).await;
            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response(
                "/v1/messages",
                native_history_script(native_history_frames()),
            );
            let session = NativeHistorySession::new(&server).await;
            let outcome = tokio::time::timeout(NATIVE_TIMEOUT, session.sample_outcome())
                .await
                .unwrap();
            let captured_output =
                if let Ok(SamplerTurnOutcome::Response(response, metrics)) = outcome {
                    let output = native_response_observation(&response);
                    assert_eq!(
                        output, ordinary,
                        "R10 F4: capture leaves the ordinary output and usage unchanged"
                    );
                    session
                        .actor
                        .record_response_token_usage(&response, Some(metrics.time_to_last_byte_ms));
                    session
                        .actor
                        .record_response_items(response.items, response.usage.is_some())
                        .await;
                    Some(output)
                } else {
                    None
                };
            session.actor.chat_state_handle.get_conversation().await;
            let (respond_to, ack) = tokio::sync::oneshot::channel();
            session
                .persistence
                .tx
                .send(PersistenceMsg::FlushAndAck { respond_to })
                .unwrap();
            tokio::time::timeout(NATIVE_TIMEOUT, ack)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(server.messages_request_count(), 1);
            let info = crate::session::info::Info {
                id: session.actor.session_info.id.clone(),
                cwd: session.actor.session_info.cwd.clone(),
            };
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                session.directory.path().to_path_buf(),
            );
            let loaded = adapter.load_session(&info).await.unwrap();
            let light = adapter.load_session_without_updates(&info).await.unwrap();
            assert_eq!(
                serde_json::to_value(loaded.chat_history).unwrap(),
                serde_json::to_value(light.chat_history).unwrap()
            );
            let bytes = std::fs::read(session.directory.path().join("native_history/current.json"))
                .unwrap_or_default();
            let record: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            assert_eq!(
                record["schema_version"], 1,
                "W00: real decode -> actor -> disk must retain NativeSearchHistoryV1"
            );
            assert_eq!(
                captured_output,
                Some(ordinary),
                "R10 F4: captured completion equals the ordinary typed control"
            );
            assert_eq!(record["policy_revision"], 1);
            assert_eq!(record["reader_floor"], 1);
            assert_eq!(record["limits_revision"], 1);
            let source = native_blob(session.directory.path(), &record["source"]);
            let active = native_blob(session.directory.path(), &record["active"]);
            let ledger = native_blob(session.directory.path(), &record["ledger"]);
            assert_eq!(source["responses"].as_array().unwrap().len(), 1);
            assert_eq!(source["responses"][0]["status"], "Accepted");
            assert_eq!(source["responses"][0]["stream_complete"], true);
            assert_eq!(
                source["responses"][0]["provider_response_id"],
                "msg_native_history"
            );
            let blocks = source["ordered_blocks"].as_array().unwrap();
            assert_eq!(blocks.len(), 5);
            let raw: Vec<Value> = blocks
                .iter()
                .enumerate()
                .map(|(index, block)| {
                    assert_eq!(block["wire_index"], index);
                    assert_eq!(block["arrival_ordinal"], index);
                    assert_eq!(block["state"], "ClosedFaithful");
                    native_blob(session.directory.path(), &block["block"])
                })
                .collect();
            assert_eq!(raw[0], json!({"type":"text","text":"before"}));
            assert_eq!(
                raw[1],
                json!({"type":"thinking","thinking":"step by step","signature":"signed"})
            );
            assert_eq!(raw[2]["input"], json!({"query":"account"}));
            assert_eq!(raw[3]["tool_use_id"], "srvtoolu_native_history");
            assert!(raw[3].get("id").is_none());
            assert_eq!(raw[4], json!({"type":"text","text":"after"}));
            assert_eq!(active["view_links"].as_array().unwrap().len(), 5);
            assert_eq!(ledger["usage"][0]["observed"]["input_tokens"], 11);
            assert_eq!(ledger["usage"][0]["observed"]["output_tokens"], 7);
            assert_eq!(ledger["usage"][0]["observed"]["thinking_tokens"], 2);
            assert_eq!(
                ledger["usage"][0]["observed"]["cache_creation_input_tokens"],
                0
            );
            assert_eq!(ledger["usage"][0]["http_dispatches"], 1);
            assert!(source["eligible_snapshots"].as_array().unwrap().is_empty());
            assert_eq!(
                source["producing_bindings"][0]["origin"]["status"],
                "Unknown"
            );
            native_history_live_ledger_reload().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn native_history_failed_attempt_survives_disk_reload() {
    let _retry_env = xai_grok_test_support::EnvGuard::unset("GROK_MAX_RETRIES");
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            let mut disk_observations = Vec::new();
            for upstream_error in [true, false] {
                let server = MockInferenceServer::start().await.unwrap();
                let mut frames = native_history_frames()[..10].to_vec();
                if upstream_error {
                    frames.push(json!({"type":"error","error":{
                        "type":"invalid_request_error","message":"native fixture stream failed"}
                    }));
                }
                server.enqueue_response("/v1/messages", native_history_script(frames));
                let session = NativeHistorySession::new(&server).await;
                let outcome = tokio::time::timeout(NATIVE_TIMEOUT, session.sample_outcome())
                    .await
                    .unwrap();
                assert!(
                    outcome.is_err(),
                    "failed attempt must not become an accepted response"
                );
                let observations = session.observed.lock().unwrap();
                let failures: Vec<_> = observations
                    .iter()
                    .filter_map(|event| match event {
                        xai_grok_sampler::SamplingEvent::Failed { error, .. } => Some(error),
                        _ => None,
                    })
                    .collect();
                assert_eq!(failures.len(), 1);
                assert!(!observations.iter().any(|event| matches!(
                    event,
                    xai_grok_sampler::SamplingEvent::Completed { .. }
                )));
                let expected = xai_grok_sampling_types::SamplingError::StreamError {
                    error_type: if upstream_error {
                        "invalid_request_error"
                    } else {
                        "stream_truncated"
                    }
                    .into(),
                    message: if upstream_error {
                        "native fixture stream failed"
                    } else {
                        "stream ended without done (no message_stop)"
                    }
                    .into(),
                    code: None,
                };
                let expected = if upstream_error {
                    expected
                } else {
                    let info = xai_grok_sampler::SamplingErrorInfo::from(&expected);
                    xai_grok_sampling_types::SamplingError::Api {
                        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        message: info.message,
                        model_metadata: info.model_metadata,
                        retry_after_secs: info.retry_after_secs,
                        should_retry: info.should_retry,
                        error_code: info.error_code,
                    }
                };
                assert_eq!(
                    serde_json::to_value(failures[0]).unwrap(),
                    serde_json::to_value(xai_grok_sampler::SamplingErrorInfo::from(&expected))
                        .unwrap(),
                    "native observation must preserve the original rich terminal"
                );
                drop(observations);
                assert_eq!(server.messages_request_count(), 1);
                session.actor.chat_state_handle.get_conversation().await;
                let (respond_to, ack) = tokio::sync::oneshot::channel();
                session
                    .persistence
                    .tx
                    .send(PersistenceMsg::FlushAndAck { respond_to })
                    .unwrap();
                tokio::time::timeout(NATIVE_TIMEOUT, ack)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let info = crate::session::info::Info {
                    id: session.actor.session_info.id.clone(),
                    cwd: session.actor.session_info.cwd.clone(),
                };
                let adapter =
                    crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                        session.directory.path().to_path_buf(),
                    );
                let loaded = adapter.load_session(&info).await.unwrap();
                let light = adapter.load_session_without_updates(&info).await.unwrap();
                assert_eq!(
                    serde_json::to_value(loaded.chat_history).unwrap(),
                    serde_json::to_value(light.chat_history).unwrap()
                );
                let bytes =
                    std::fs::read(session.directory.path().join("native_history/current.json"))
                        .unwrap_or_default();
                let record: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let source = if record.is_object() {
                    native_blob(session.directory.path(), &record["source"])
                } else {
                    Value::Null
                };
                let ledger = if record.is_object() {
                    native_blob(session.directory.path(), &record["ledger"])
                } else {
                    Value::Null
                };
                let partial = source
                    .get("ordered_blocks")
                    .and_then(Value::as_array)
                    .and_then(|blocks| blocks.get(2))
                    .and_then(|block| block.get("partial"))
                    .filter(|reference| reference.is_object())
                    .map(|reference| native_blob(session.directory.path(), reference))
                    .unwrap_or(Value::Null);
                disk_observations.push((record, source, ledger, partial));
            }
            for (record, source, ledger, partial) in disk_observations {
                assert_eq!(
                    record["schema_version"], 1,
                    "R6: failed real HTTP attempt must survive the actor/disk handoff"
                );
                assert_eq!(source["responses"].as_array().unwrap().len(), 1);
                assert!(
                    source["responses"][0]["status"]
                        .get("PartialTerminal")
                        .is_some()
                );
                assert_eq!(source["responses"][0]["stream_complete"], false);
                let blocks = source["ordered_blocks"].as_array().unwrap();
                assert_eq!(blocks.len(), 3);
                assert_eq!(blocks[0]["state"], "ClosedFaithful");
                assert_eq!(blocks[1]["state"], "ClosedFaithful");
                assert!(blocks[2]["state"].get("Unavailable").is_some());
                assert_eq!(
                    partial,
                    native_history_frames()[9]["delta"]["partial_json"],
                    "R6: an open input block must retain its bounded accepted JSON fragment"
                );
                assert_eq!(ledger["usage"].as_array().unwrap().len(), 1);
                assert_eq!(ledger["usage"][0]["observed"]["input_tokens"], 11);
                assert_eq!(ledger["usage"][0]["observed"]["output_tokens"], 0);
                assert_eq!(
                    ledger["usage"][0]["observed"]["cache_creation_input_tokens"],
                    0
                );
                assert!(
                    ledger["usage"][0]["observed"]
                        .get("thinking_tokens")
                        .is_none()
                );
                assert_eq!(ledger["usage"][0]["http_dispatches"], 1);
            }
            native_history_retry_disk_matrix().await;
        })
        .await;
}

async fn native_history_retry_disk_matrix() {
    for last in ["complete", "failed", "http-init", "zero-block"] {
        let server = MockInferenceServer::start().await.unwrap();
        let mut failed = native_history_frames()[..10].to_vec();
        failed.push(json!({"type":"error","error":{"type":"invalid_request_error","message":"native fixture stream failed"}}));
        server.enqueue_response("/v1/messages", native_history_script(failed.clone()));
        server.enqueue_response("/v1/messages", match last {
            "complete" => native_history_script(native_history_frames()),
            "failed" => native_history_script(failed),
            "http-init" => ScriptedResponse::json(503, json!({"error":{"type":"server_error","message":"native fixture initialization failed"}})),
            "zero-block" => native_history_script(vec![
                native_history_frames()[0].clone(),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":11,"output_tokens":7}}),
                json!({"type":"message_stop"}),
            ]),
            _ => unreachable!(),
        });
        let session = NativeHistorySession::with_retries(&server, 2).await;
        let result = tokio::time::timeout(NATIVE_TIMEOUT, session.sample_outcome())
            .await
            .unwrap();
        assert_eq!(result.is_ok(), last == "complete");
        if let Ok(SamplerTurnOutcome::Response(response, metrics)) = result {
            session
                .actor
                .record_response_token_usage(&response, Some(metrics.time_to_last_byte_ms));
            session
                .actor
                .record_response_items(response.items, response.usage.is_some())
                .await;
        }
        session.actor.chat_state_handle.get_conversation().await;
        let (respond_to, ack) = tokio::sync::oneshot::channel();
        session
            .persistence
            .tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .unwrap();
        tokio::time::timeout(NATIVE_TIMEOUT, ack)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            server.messages_request_count(),
            2,
            "one permitted ordinary retry"
        );
        let bytes =
            std::fs::read(session.directory.path().join("native_history/current.json")).unwrap();
        let record: Value = serde_json::from_slice(&bytes).unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        let responses = source["responses"].as_array().unwrap();
        let usage = ledger["usage"].as_array().unwrap();
        assert_eq!(
            usage.len(),
            2,
            "every distinct failed/retried attempt is accounted once"
        );
        assert_eq!(usage[0]["attempt"], 1);
        assert_eq!(usage[1]["attempt"], 2);
        assert_eq!(usage[0]["http_dispatches"], 1);
        assert_eq!(usage[1]["http_dispatches"], 1);
        assert_eq!(usage[0]["observed"]["output_tokens"], 0);
        assert!(responses[0]["status"].get("PartialTerminal").is_some());
        if last == "http-init" {
            assert_eq!(
                responses.len(),
                1,
                "HTTP initialization failure must not invent a provider response"
            );
            assert!(usage[1]["observed"].as_object().unwrap().is_empty());
            assert!(usage[1]["response_occurrence"].is_null());
        } else {
            assert_eq!(responses.len(), 2);
            assert_ne!(responses[0]["occurrence"], responses[1]["occurrence"]);
            assert_eq!(
                responses[0]["provider_response_id"],
                responses[1]["provider_response_id"]
            );
            if last == "complete" {
                assert_eq!(responses[1]["status"], "Accepted");
            } else {
                assert!(responses[1]["status"].get("PartialTerminal").is_some());
            }
            if last == "zero-block" {
                assert_eq!(responses[1]["block_start"], responses[1]["block_end"]);
            }
        }
        for group in ledger["groups"].as_array().unwrap() {
            let members = group["members"].as_array().unwrap();
            if let Some(first) = members.first() {
                assert!(
                    members
                        .iter()
                        .all(|member| member["response"] == first["response"]),
                    "a retry must not close a group with a prior failed attempt's members"
                );
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_cancelled_attempt_keeps_source_only() {
    tokio::task::LocalSet::new().run_until(async {
        use xai_grok_test_support::{InferenceEndpoint, InferenceRequestMatcher};
        let server = MockInferenceServer::start().await.unwrap();
        let mut frames = native_history_frames()[..11].to_vec();
        frames.push(json!({"type":"ping"}));
        let mut blocked = server.expect_response_blocked("native-cancel",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Messages), native_history_script(frames));
        let session = Arc::new(NativeHistorySession::new(&server).await);
        let running = session.clone();
        let collector = tokio::task::spawn_local(async move { running.sample_outcome().await });
        tokio::time::timeout(NATIVE_TIMEOUT, blocked.wait_blocked()).await.unwrap();
        loop {
            let notified = session.observed_changed.notified();
            if session.observed.lock().unwrap().iter().any(|event| matches!(event,
                xai_grok_sampler::SamplingEvent::MessagesToolSearchDisplay { .. })) { break; }
            tokio::time::timeout(NATIVE_TIMEOUT, notified).await.unwrap();
        }
        let original_turn = session.actor.current_turn_number.get();
        session.actor.cancel_active_sampling_requests();
        session.actor.current_turn_number.set(original_turn + 1);
        session.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("next turn")).await;
        collector.abort();
        assert!(matches!(collector.await, Err(error) if error.is_cancelled()), "dropping the real collector must cancel its task");
        loop {
            let notified = session.native_end_processed.notified();
            if session.native_ended.load(std::sync::atomic::Ordering::SeqCst) { break; }
            tokio::time::timeout(NATIVE_TIMEOUT, notified).await.unwrap();
        }
        blocked.release();
        assert_eq!(server.messages_request_count(), 1);
        let observed = session.observed.lock().unwrap();
        assert!(observed.iter().any(|event| matches!(event,
            xai_grok_sampler::SamplingEvent::Failed { error, .. } if error.message == "request cancelled" && !error.is_retryable)));
        assert!(!observed.iter().any(|event| matches!(event,
            xai_grok_sampler::SamplingEvent::Completed { .. } | xai_grok_sampler::SamplingEvent::Retrying { .. })));
        drop(observed);
        let bytes = std::fs::read(session.directory.path().join("native_history/current.json")).unwrap_or_default();
        let record: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(record["schema_version"], 1, "R6: UI revocation must not drop valid old-lease source evidence");
        let source = native_blob(session.directory.path(), &record["source"]);
        let active = native_blob(session.directory.path(), &record["active"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        assert_eq!(source["responses"].as_array().unwrap().len(), 1);
        assert_eq!(source["responses"][0]["status"]["PartialTerminal"], "Cancelled");
        assert_eq!(source["responses"][0]["stream_complete"], false);
        assert_eq!(source["ordered_blocks"][0]["coordinates"]["turn"], original_turn);
        assert!(active["view_links"].as_array().unwrap().is_empty());
        assert_eq!(ledger["usage"].as_array().unwrap().len(), 1);
        assert_eq!(ledger["usage"][0]["observed"]["output_tokens"], 0);
        let native = session.actor.chat_state_handle.read_native_history().await.unwrap();
        let generation = native.checkpoint.as_ref().unwrap().record.generation;
        let event = session.observed.lock().unwrap().iter().find_map(|event| match event {
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { request_id, lease,
                evidence: xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent::Attempt(attempt) } =>
                Some((request_id.clone(), lease.clone(), attempt.clone())),
            _ => None,
        }).unwrap();
        let replay = session.actor.chat_state_handle.append_native_evidence(event.0.as_str().into(), event.1.clone(),
            xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent::Attempt(event.2.clone())).await.unwrap();
        assert_eq!(replay, xai_chat_state::persistence::native::NativeAppendAck::Unchanged);
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let wrong_request = session.actor.chat_state_handle.append_native_evidence("wrong-request".into(), event.1.clone(), NativeEvidenceEvent::Attempt(event.2.clone())).await;
        assert_eq!(wrong_request, Err(NativeUnavailable::StaleHistory));
        for lineage in [event.1.lineage(), "other-lineage"] {
            let forged = NativeCaptureLease::issue(lineage.into(), event.0.as_str().into(), event.1.turn(), event.1.ordinal(), NATIVE_SOURCE_BYTES);
            let wrong_lease = session.actor.chat_state_handle.append_native_evidence(event.0.as_str().into(), forged, NativeEvidenceEvent::Attempt(event.2.clone())).await;
            assert_eq!(wrong_lease, Err(NativeUnavailable::StaleHistory));
        }
        let mut conflicting = event.2.as_ref().clone();
        conflicting.usage.insert("input_tokens".into(), json!(999));
        let conflict = session.actor.chat_state_handle.append_native_evidence(event.0.as_str().into(), event.1, NativeEvidenceEvent::Attempt(Arc::new(conflicting))).await;
        assert_eq!(conflict, Err(NativeUnavailable::ConflictingObservation));

        assert_eq!(session.actor.chat_state_handle.read_native_history().await.unwrap().checkpoint.unwrap().record.generation, generation);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_evidence_ack_failure_preserves_sampling_terminal() {
    tokio::task::LocalSet::new().run_until(async {
        for policy in [NativeAckPolicy::WriteFailure, NativeAckPolicy::LoseReply, NativeAckPolicy::HoldReply] {
            let server = MockInferenceServer::start().await.unwrap();
            let mut frames = native_history_frames()[..10].to_vec();
            frames.push(json!({"type":"error","error":{"type":"invalid_request_error","message":"native fixture stream failed"}}));
            server.enqueue_response("/v1/messages", native_history_script(frames));
            let session = NativeHistorySession::with_ack_policy(&server, policy).await;
            if matches!(policy, NativeAckPolicy::WriteFailure) {
                std::fs::create_dir(session.directory.path().join("native_history")).unwrap();
                std::fs::write(session.directory.path().join("native_history/blobs"), b"not a directory").unwrap();
            }
            let result = tokio::time::timeout(NATIVE_TIMEOUT, session.sample_outcome()).await.unwrap();
            assert!(result.is_err());
            loop {
                let notified = session.native_end_processed.notified();
                if session.native_ended.load(std::sync::atomic::Ordering::SeqCst) { break; }
                tokio::time::timeout(NATIVE_TIMEOUT, notified).await.unwrap();
            }
            let observed = session.observed.lock().unwrap();
            let terminal = observed.iter().find_map(|event| match event {
                xai_grok_sampler::SamplingEvent::Failed { error, .. } => Some(error),
                _ => None,
            }).unwrap();
            let expected = xai_grok_sampling_types::SamplingError::StreamError {
                error_type: "invalid_request_error".into(), message: "native fixture stream failed".into(), code: None,
            };
            assert_eq!(serde_json::to_value(terminal).unwrap(), serde_json::to_value(xai_grok_sampler::SamplingErrorInfo::from(&expected)).unwrap());
            assert!(!observed.iter().any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Completed { .. })));
            drop(observed);
            let acknowledged = session.actor.chat_state_handle.read_native_history().await;
            assert!(acknowledged.is_err(), "R6: failed or absent disk acknowledgement must never publish a ready generation");
            if matches!(policy, NativeAckPolicy::WriteFailure) {
                assert!(!session.directory.path().join("native_history/current.json").exists());
            } else {
                let bytes = std::fs::read(session.directory.path().join("native_history/current.json")).unwrap();
                let record: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(record["generation"], 1, "lost ACK is indeterminate, not proof that disk did not commit");
            }
        }
    }).await;
}

async fn native_history_live_ledger_reload() {
    use crate::session::storage::StorageAdapter;
    let server = MockInferenceServer::start().await.unwrap();
    let mut frames = native_history_frames();
    frames.splice(15..15, [
        json!({"type":"content_block_start","index":5,"content_block":{
            "type":"tool_use","id":"client_waiting","name":"read_file","input":{"path":"not-executed"}}}),
        json!({"type":"content_block_stop","index":5}),
    ]);
    frames[17]["delta"]["stop_reason"] = json!("tool_use");
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let session = NativeHistorySession::new(&server).await;
    tokio::time::timeout(NATIVE_TIMEOUT, session.sample())
        .await
        .unwrap();
    assert_eq!(server.messages_request_count(), 1);
    let info = crate::session::info::Info {
        id: session.actor.session_info.id.clone(),
        cwd: session.actor.session_info.cwd.clone(),
    };
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let root: Value = serde_json::from_slice(
        &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
    )
    .unwrap();
    let ledger = native_blob(session.directory.path(), &root["ledger"]);
    assert_eq!(
        ledger["lifecycle"]["waiting_clients"][0]["client_id"],
        "client_waiting"
    );
    assert_eq!(
        ledger["lifecycle"]["waiting_clients"][0]["delivery"],
        "Waiting"
    );
    assert!(ledger["lifecycle"]["waiting_clients"][0]["result"].is_null());
    assert!(
        ledger["lifecycle"]["terminal"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(
        ledger["lifecycle"]["exchanges"]
            .as_object()
            .unwrap()
            .values()
            .any(|state| state == "LiveBound")
    );
    assert_eq!(ledger["groups"][0]["members"].as_array().unwrap().len(), 2);
    assert_eq!(ledger["groups"][0]["state"], "Completed");
    let path = session
        .directory
        .path()
        .join("native_history/blobs")
        .join(format!(
            "{}.json",
            root["ledger"]["digest"].as_str().unwrap()
        ));
    let original = std::fs::read(&path).unwrap();
    for field in ["client", "delivery", "membership"] {
        let mut changed = ledger.clone();
        match field {
            "client" => {
                changed["lifecycle"]["waiting_clients"][0]["client_id"] = json!("wrong-client")
            }
            "delivery" => {
                changed["lifecycle"]["waiting_clients"][0]["delivery"] = json!("Delivered")
            }
            "membership" => changed["groups"][0]["members"][0]["block"] = json!(99),
            _ => unreachable!(),
        }
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let full = adapter.load_session(&info).await;
        let light = adapter.load_session_without_updates(&info).await;
        std::fs::write(&path, &original).unwrap();
        assert!(
            full.is_err() && light.is_err(),
            "SC2-C1: {field} must be inside the verified ledger partition"
        );
    }
    adapter.load_session(&info).await.unwrap();
    adapter.load_session_without_updates(&info).await.unwrap();
}

async fn u2_metadata_candidate(
    session: &NativeHistorySession,
) -> xai_chat_state::persistence::native::NativeWrite {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let stored = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let (lease, mut observed) = session
        .observed
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                lease,
                evidence: NativeEvidenceEvent::Attempt(attempt),
                ..
            } => Some((lease.clone(), attempt.as_ref().clone())),
            _ => None,
        })
        .unwrap();
    observed.attempt += 1;
    observed.blocks.clear();
    observed.view_roles.clear();
    observed.response_started = false;
    observed.provider_response_id = None;
    observed.source_stop_reason = None;
    observed.stream_complete = false;
    observed.status = NativeResponseStatus::PartialTerminal(NativeUnavailable::Failed);
    let items = session.actor.chat_state_handle.get_conversation().await;
    let candidate = xai_chat_state::persistence::native::NativeCheckpoint::append(
        Some(&stored),
        &lease,
        &[Arc::new(observed)],
        &items,
    )
    .unwrap();
    xai_chat_state::persistence::native::NativeWrite {
        previous_generation: stored.record.generation,
        transaction: format!("u2-metadata-{}", candidate.record.generation),
        checkpoint: Arc::new(candidate),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_disk_recovery_keeps_acknowledged_generation() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::{StorageAdapter, SessionUpdate};
        u2_disk_transaction_matrix().await;
        u2_disk_extra_bytes_refuse_before_copy().await;
        for fault in ["backup", "staging", "file-sync", "directory-sync"] {
            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response("/v1/messages", native_history_script(native_history_frames()));
            let session = NativeHistorySession::new(&server).await;
            session.sample().await;
            let directory = session.directory.path().join("native_history");
            let current = std::fs::read(directory.join("current.json")).unwrap();
            let authority_bytes = std::fs::read(directory.join("authority.json")).unwrap_or_default();
            let authority: Value = serde_json::from_slice(&authority_bytes).unwrap_or(Value::Null);
            assert_eq!(authority["high_water"], 1, "U2: real JSONL acknowledgement requires an independent durable generation authority");
            let info = crate::session::info::Info { id: session.actor.session_info.id.clone(), cwd: session.actor.session_info.cwd.clone() };
            let mut adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
            let write = u2_metadata_candidate(&session).await;
            match fault {
                "backup" => { std::fs::create_dir(directory.join("backup.json")).unwrap(); }
                "staging" => {
                    let stage = directory.join("staging.json");
                    if stage.is_file() { std::fs::remove_file(&stage).unwrap(); }
                    std::fs::create_dir(stage).unwrap();
                }
                "file-sync" => { adapter = adapter.with_file_sync_probe(|| Err(std::io::Error::other("injected native file sync"))); }
                "directory-sync" => { adapter = adapter.with_parent_sync_probe(|| Err(std::io::Error::other("injected native directory sync"))); }
                _ => unreachable!(),
            }
            let result = adapter.persist_native_generation_and_ack(&info, write).await;
            assert!(result.is_err(), "U2: {fault} must not acknowledge a candidate");
            assert_eq!(std::fs::read(directory.join("current.json")).unwrap(), current, "{fault} preserves current generation");
            assert_eq!(std::fs::read(directory.join("authority.json")).unwrap(), authority_bytes, "{fault} preserves authority");
        }

        let server = MockInferenceServer::start().await.unwrap();
        for _ in 0..2 { server.enqueue_response("/v1/messages", native_history_script(native_history_frames())); }
        let session = NativeHistorySession::new(&server).await;
        session.sample().await;
        session.sample().await;
        let info = crate::session::info::Info { id: session.actor.session_info.id.clone(), cwd: session.actor.session_info.cwd.clone() };
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
        let directory = session.directory.path().join("native_history");
        let authority = std::fs::read(directory.join("authority.json")).unwrap();
        let current = std::fs::read(directory.join("current.json")).unwrap();
        let backup = std::fs::read(directory.join("backup.json")).unwrap();
        std::fs::write(directory.join("current.json"), &backup).unwrap();
        let recovered = adapter.read_native_history(&info).await.unwrap().checkpoint.unwrap();
        assert_eq!(recovered.record.generation, 2, "a stale backup must not demote the high-water");
        assert_eq!(recovered.ledger.usage.len(), 2);
        std::fs::write(directory.join("staging.json"), b"{partial staging").unwrap();
        assert_eq!(adapter.read_native_history(&info).await.unwrap().checkpoint.unwrap().record.generation, 2);
        std::fs::write(directory.join("current.json"), &current).unwrap();
        adapter.load_session(&info).await.unwrap();
        adapter.load_session_without_updates(&info).await.unwrap();
        assert_eq!(std::fs::read(directory.join("authority.json")).unwrap(), authority);

        adapter.append_update(&info, &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(info.id.clone(),
            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("rebuild user")))))))).await.unwrap();
        adapter.append_update(&info, &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(info.id.clone(),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new("rebuild assistant")))))))).await.unwrap();
        let chat = session.directory.path().join("chat_history.jsonl");
        std::fs::remove_file(&chat).unwrap();
        std::fs::remove_file(directory.join("authority.json")).unwrap();
        std::fs::remove_file(directory.join("current.json")).unwrap();
        let full = adapter.load_session(&info).await;
        let light = adapter.load_session_without_updates(&info).await;
        assert!(full.is_err() && light.is_err(), "missing required authority is not legacy recovery");
        assert!(!chat.exists(), "N17: ACP reconstruction must not run before native authority validation");
    }).await;
}

async fn u2_disk_transaction_matrix() {
    use crate::session::storage::StorageAdapter;
    use xai_chat_state::persistence::native::{NativeAppendAck, digest};
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    session.sample().await;
    let info = crate::session::info::Info {
        id: session.actor.session_info.id.clone(),
        cwd: session.actor.session_info.cwd.clone(),
    };
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let directory = session.directory.path().join("native_history");
    let write = u2_metadata_candidate(&session).await;
    let old_authority = std::fs::read(directory.join("authority.json")).unwrap();
    let old_current = std::fs::read(directory.join("current.json")).unwrap();
    let chat = session.directory.path().join("chat_history.jsonl");
    let old_chat = std::fs::read(&chat).unwrap();
    let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let changed_probe = changed.clone();
    let raced_chat = chat.clone();
    let race = adapter.clone().with_file_sync_probe(move || {
        if !changed_probe.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let mut bytes = serde_json::to_vec(&ConversationItem::user("changed prefix")).unwrap();
            bytes.push(b'\n');
            std::fs::write(&raced_chat, bytes)?;
        }
        Ok(())
    });
    assert_eq!(
        race.persist_native_generation_and_ack(&info, write.clone())
            .await,
        Err(NativeUnavailable::StaleHistory),
        "U2: a history revision race is checked again before publishing authority"
    );
    assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        std::fs::read(directory.join("authority.json")).unwrap(),
        old_authority
    );
    assert_eq!(
        std::fs::read(directory.join("current.json")).unwrap(),
        old_current
    );
    std::fs::write(&chat, &old_chat).unwrap();

    let (entered, reached) = tokio::sync::oneshot::channel();
    let entered = Arc::new(std::sync::Mutex::new(Some(entered)));
    let (release, gate) = std::sync::mpsc::channel();
    let gate = Arc::new(std::sync::Mutex::new(gate));
    let blocker = adapter.clone().with_file_sync_probe(move || {
        if let Some(entered) = entered.lock().unwrap().take() {
            entered.send(()).unwrap();
            gate.lock().unwrap().recv().unwrap();
        }
        Ok(())
    });
    let competing = adapter.clone();
    let commit_info = info.clone();
    let commit_write = write.clone();
    let first = tokio::task::spawn_local(async move {
        blocker
            .persist_native_generation_and_ack(&commit_info, commit_write)
            .await
    });
    tokio::time::timeout(NATIVE_TIMEOUT, reached)
        .await
        .unwrap()
        .unwrap();
    let second = competing
        .persist_native_generation_and_ack(&info, write.clone())
        .await;
    release.send(()).unwrap();
    assert_eq!(
        second,
        Err(NativeUnavailable::Indeterminate),
        "U2: only one disk transaction may own staging and backup"
    );
    let committed = first.await.unwrap().unwrap();
    assert!(matches!(
        committed,
        NativeAppendAck::Applied { generation: 2, .. }
    ));
    let before_retry: std::collections::BTreeMap<_, _> = std::fs::read_dir(directory.join("blobs"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), std::fs::read(entry.path()).unwrap())
        })
        .collect();
    assert_eq!(
        adapter
            .persist_native_generation_and_ack(&info, write.clone())
            .await
            .unwrap(),
        committed,
        "U2: a lost acknowledgement reconciles the same transaction, not a new occurrence"
    );
    let after_retry: std::collections::BTreeMap<_, _> = std::fs::read_dir(directory.join("blobs"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), std::fs::read(entry.path()).unwrap())
        })
        .collect();
    assert_eq!(
        after_retry, before_retry,
        "U2: retries create no uncharged checkpoint copy"
    );
    let loaded = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(loaded.record.generation, 2);
    assert_eq!(
        loaded.ledger.usage.len(),
        2,
        "U2: metadata-only transactions preserve usage once"
    );
    assert_eq!(loaded.record.source.digest, loaded.ledger.source_digest);
    assert_eq!(loaded.record.active.digest, loaded.ledger.active_digest);
    assert_eq!(loaded.record.high_water, 2);
    let terminals = loaded.ledger.lifecycle.terminal.clone();
    assert!(
        !terminals.is_empty(),
        "U2: the metadata-only failure has a durable terminal tombstone"
    );
    let stale_backup = std::fs::read(directory.join("backup.json")).unwrap();
    std::fs::write(directory.join("current.json"), stale_backup).unwrap();
    let recovered = adapter
        .read_native_history(&info)
        .await
        .expect("U2: stale-backup recovery retains current terminal authority")
        .checkpoint
        .unwrap();
    assert_eq!(recovered.record.high_water, 2);
    assert_eq!(
        recovered.ledger.lifecycle.terminal, terminals,
        "U2: an older backup never forgets terminal tombstones"
    );
    let mut stale = write;
    stale.transaction.push_str("-different");
    assert_eq!(
        adapter
            .persist_native_generation_and_ack(&info, stale)
            .await,
        Err(NativeUnavailable::StaleHistory)
    );
    let authority: Value =
        serde_json::from_slice(&std::fs::read(directory.join("authority.json")).unwrap()).unwrap();
    let raw = std::fs::read(directory.join("blobs").join(format!(
        "{}.json",
        authority["checkpoint"]["digest"].as_str().unwrap()
    )))
    .unwrap();
    assert_eq!(
        digest(&raw),
        authority["checkpoint"]["digest"].as_str().unwrap()
    );
    assert_eq!(
        raw.last().copied(),
        Some(b'}'),
        "U2: compact checkpoint bytes carry no framing newline"
    );

    let next = loaded
        .with_snapshot(
            b"{\"messages\":[],\"tools\":[]}",
            loaded.source.producing_bindings[0].origin_ref.clone(),
        )
        .unwrap();
    let candidate = xai_chat_state::persistence::native::NativeWrite {
        previous_generation: 2,
        transaction: "u2-indeterminate-directory-sync".into(),
        checkpoint: Arc::new(next),
    };
    let authority_barrier = candidate.checkpoint.blobs.len() + 4;
    let barriers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probe = barriers.clone();
    let uncertain = adapter.clone().with_parent_sync_probe(move || {
        if probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == authority_barrier {
            Err(std::io::Error::other("injected authority directory sync"))
        } else {
            Ok(())
        }
    });
    assert_eq!(
        uncertain
            .persist_native_generation_and_ack(&info, candidate.clone())
            .await,
        Err(NativeUnavailable::Indeterminate),
        "U2: authority publication without its directory barrier is indeterminate, not Applied"
    );
    assert_eq!(
        barriers.load(std::sync::atomic::Ordering::SeqCst),
        authority_barrier
    );
    assert_eq!(
        adapter
            .persist_native_generation_and_ack(&info, candidate)
            .await
            .unwrap(),
        NativeAppendAck::Applied {
            generation: 3,
            digest: adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .map(|p| digest(&p.bytes))
                .unwrap(),
        }
    );
}

async fn u2_disk_extra_bytes_refuse_before_copy() {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    for source in [true, false] {
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response(
            "/v1/messages",
            native_history_script(native_history_frames()),
        );
        let session = NativeHistorySession::new(&server).await;
        session.sample().await;
        let info = crate::session::info::Info {
            id: session.actor.session_info.id.clone(),
            cwd: session.actor.session_info.cwd.clone(),
        };
        let directory = session.directory.path().join("native_history");
        let authority = std::fs::read(directory.join("authority.json")).unwrap();
        let current = std::fs::read(directory.join("current.json")).unwrap();
        let limit = if source {
            NATIVE_SOURCE_BYTES
        } else {
            NATIVE_CONTROL_BYTES
        };
        let payload = serde_json::to_vec(&"x".repeat(limit as usize - 1)).unwrap();
        assert_eq!(payload.len() as u64, limit + 1);
        let path = if source {
            directory.join("blobs/orphan-source.json")
        } else {
            directory.join("control-growth.json")
        };
        std::fs::write(path, &payload).unwrap();
        let copies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = copies.clone();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        )
        .with_file_sync_probe(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        });
        let candidate = u2_metadata_candidate(&session).await;
        assert_eq!(
            adapter
                .persist_native_generation_and_ack(&info, candidate)
                .await,
            Err(NativeUnavailable::Capacity),
            "U2: all retained physical bytes, including incomplete staging residues, remain charged"
        );
        assert_eq!(
            copies.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "U2: reject physical over-capacity before making a backup or staging copy"
        );
        assert_eq!(
            std::fs::read(directory.join("authority.json")).unwrap(),
            authority
        );
        assert_eq!(
            std::fs::read(directory.join("current.json")).unwrap(),
            current
        );
    }
}

#[cfg(unix)]
struct NativeLockInheritedChild {
    control: std::os::unix::net::UnixStream,
    worker: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
}

#[cfg(unix)]
impl NativeLockInheritedChild {
    fn hold_before_exec() -> std::io::Result<Self> {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let (control, child) = std::os::unix::net::UnixStream::pair()?;
        control.set_read_timeout(Some(NATIVE_TIMEOUT))?;
        control.set_write_timeout(Some(NATIVE_TIMEOUT))?;
        let parent_fd = control.as_raw_fd();
        let child_fd = child.as_raw_fd();
        let worker = std::thread::spawn(move || {
            let _child = child;
            let mut command = std::process::Command::new("/usr/bin/true");
            // SAFETY: the child uses only async-signal-safe descriptor operations before exec.
            unsafe {
                command.pre_exec(move || {
                    libc::close(parent_fd);
                    let byte = 1u8;
                    loop {
                        if libc::write(child_fd, (&byte as *const u8).cast(), 1) == 1 {
                            break;
                        }
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                    let mut release = 0u8;
                    loop {
                        match libc::read(child_fd, (&mut release as *mut u8).cast(), 1) {
                            0 | 1 => return Ok(()),
                            _ => {
                                let error = std::io::Error::last_os_error();
                                if error.kind() != std::io::ErrorKind::Interrupted {
                                    return Err(error);
                                }
                            }
                        }
                    }
                });
            }
            // status() reaps the child; the guard joins this thread on every exit.
            command.status()
        });
        let mut held = Self {
            control,
            worker: Some(worker),
        };
        held.control.read_exact(&mut [0])?;
        Ok(held)
    }

    fn release_and_wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        use std::io::Write;
        self.control.write_all(&[1])?;
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| std::io::Error::other("inherited-lock child worker panicked"))?
    }
}

#[cfg(unix)]
impl Drop for NativeLockInheritedChild {
    fn drop(&mut self) {
        use std::io::Write;
        if let Some(worker) = self.worker.take() {
            let _ = self.control.write_all(&[1]);
            let _ = worker.join();
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn native_history_retry_releases_inherited_transaction_lock() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            use xai_chat_state::persistence::native::{NativeAppendAck, digest};
            use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;

            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response(
                "/v1/messages",
                native_history_script(native_history_frames()),
            );
            let session = NativeHistorySession::new(&server).await;
            session.sample().await;
            let info = crate::session::info::Info {
                id: session.actor.session_info.id.clone(),
                cwd: session.actor.session_info.cwd.clone(),
            };
            let directory = session.directory.path().join("native_history");
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                session.directory.path().to_path_buf(),
            );
            let candidate = u2_metadata_candidate(&session).await;
            let expected = NativeAppendAck::Applied {
                generation: candidate.checkpoint.record.generation,
                digest: digest(&candidate.checkpoint.bytes),
            };
            let barriers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let probe = barriers.clone();
            let inherited = Arc::new(std::sync::Mutex::new(None));
            let held = inherited.clone();
            let trace = Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed_trace = trace.clone();
            let injected = Arc::new(std::sync::Mutex::new(None));
            let observed_injection = injected.clone();
            let probe_directory = directory.clone();
            let candidate_bytes = candidate.checkpoint.bytes.clone();
            let candidate_digest = digest(&candidate_bytes);
            let probe_digest = candidate_digest.clone();
            let old_authority: Value = serde_json::from_slice(
                &std::fs::read(directory.join("authority.json")).unwrap(),
            )
            .unwrap();
            assert_ne!(old_authority["checkpoint"]["digest"], candidate_digest);
            let uncertain = adapter.clone().with_parent_sync_probe(move || {
                let index = probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let authority: Value = serde_json::from_slice(&std::fs::read(
                    probe_directory.join("authority.json"),
                )?)?;
                // Parent probes run after rename and before the directory durability barrier.
                let at_authority = authority["checkpoint"]["digest"] == probe_digest;
                let operation = if at_authority {
                    "authority-directory-sync"
                } else {
                    "pre-authority-durability-barrier"
                };
                observed_trace.lock().unwrap().push(json!({"index": index, "operation": operation}));
                if at_authority {
                    *observed_injection.lock().unwrap() = Some(json!({
                        "index": index,
                        "operation": operation,
                        "checkpoint_digest": authority["checkpoint"]["digest"],
                        "staging_is_candidate": std::fs::read(probe_directory.join("staging.json"))? == candidate_bytes.as_ref(),
                        "current_is_candidate": std::fs::read(probe_directory.join("current.json"))? == candidate_bytes.as_ref(),
                    }));
                    *held.lock().unwrap() = Some(NativeLockInheritedChild::hold_before_exec()?);
                    Err(std::io::Error::other("injected authority directory sync"))
                } else {
                    Ok(())
                }
            });
            let ambiguous = uncertain
                .persist_native_generation_and_ack(&info, candidate.clone())
                .await;
            let retry = adapter
                .persist_native_generation_and_ack(&info, candidate)
                .await;
            let child = inherited.lock().unwrap().take().unwrap();
            assert!(child.release_and_wait().unwrap().success());
            let trace = trace.lock().unwrap();
            let authority_barrier = trace
                .iter()
                .position(|entry| entry["operation"] == "authority-directory-sync")
                .expect("R11: authority-directory fault must fire")
                + 1;
            let injection = injected.lock().unwrap().take().expect("R11: fault injection recorded");
            eprintln!("R11_OPERATION_TRACE {}", json!({"operations": *trace, "injection": injection}));
            assert_eq!(injection["operation"], "authority-directory-sync");
            assert_eq!(injection["index"], authority_barrier);
            assert_eq!(injection["checkpoint_digest"], candidate_digest);
            assert_eq!(injection["staging_is_candidate"], true);
            assert_eq!(injection["current_is_candidate"], false);
            assert_eq!(trace.iter().filter(|entry| entry["operation"] == "authority-directory-sync").count(), 1);
            assert_eq!(ambiguous, Err(NativeUnavailable::Indeterminate));
            assert_eq!(
                barriers.load(std::sync::atomic::Ordering::SeqCst),
                authority_barrier
            );
            assert_eq!(
                retry,
                Ok(expected),
                "R9: a completed transaction releases the inherited lock before reconciliation"
            );
            let stored = adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            assert_eq!(stored.record.generation, 2);
            assert_eq!(stored.record.high_water, 2);
        })
        .await;
}

fn native_history_parent_pause_frames() -> Vec<serde_json::Value> {
    serde_json::json!([{"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "messages-compatible-model", "stop_reason": null, "usage": {"input_tokens": 10, "output_tokens": 0}}}, {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}, {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial answer"}}, {"type": "content_block_stop", "index": 0}, {"type": "message_delta", "delta": {"stop_reason": "pause_turn"}, "usage": {"output_tokens": 5, "input_tokens": 10}}, {"type": "message_stop"}]).as_array().unwrap().clone()
}

fn native_response_observation(response: &xai_grok_sampling_types::ConversationResponse) -> Value {
    json!({
        "items": response.items,
        "usage": response.usage,
        "stop_reason": response.stop_reason,
        "stop_message": response.stop_message,
        "message_id": response.message_id,
        "raw_stop_reason": response.raw_stop_reason,
        "stop_sequence": response.stop_sequence,
        "cost_usd_ticks": response.cost_usd_ticks,
        "message_chunks_emitted": response.message_chunks_emitted,
    })
}

async fn native_untracked_output(frames: Vec<Value>) -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(0),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    let (events, _received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        events,
    );
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_config(
            xai_grok_sampler::RequestId::random(),
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
        ),
    )
    .await
    .unwrap();
    let (response, _) = result
        .result
        .expect("R10 F4: untracked typed output remains intact");
    assert_eq!(server.messages_request_count(), 1);
    assert_eq!(response.assistant_text(), "before\nafter");
    assert_eq!(response.usage.as_ref().unwrap().completion_tokens, 7);
    eprintln!("R10 F4: ordinary typed output and usage intact before disk capture assertion");
    native_response_observation(&response)
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_parent_pause_refusal_creates_no_record() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::StorageAdapter;
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response("/v1/messages", native_history_script(native_history_parent_pause_frames()));
        let session = NativeHistorySession::new(&server).await;
        assert!(session.sample_outcome().await.is_err());
        while !session.native_ended.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::timeout(NATIVE_TIMEOUT, session.native_end_processed.notified()).await.unwrap();
        }
        assert_eq!(server.messages_request_count(), 1);
        let events = session.observed.lock().unwrap();
        let failures: Vec<_> = events.iter().filter_map(|event| match event {
            xai_grok_sampler::SamplingEvent::Failed { error, .. } => Some(error),
            _ => None,
        }).collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, xai_grok_sampler::SamplingErrorKind::UnsupportedStopControl);
        assert!(!failures[0].is_retryable);
        assert!(failures[0].message.contains("pause_turn"));
        assert!(!events.iter().any(|event| matches!(event,
            xai_grok_sampler::SamplingEvent::Completed { .. }
            | xai_grok_sampler::SamplingEvent::Retrying { .. }
            | xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                evidence: xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent::Attempt(_), ..
            }
        )));
        drop(events);
        assert!(session.actor.chat_state_handle.read_native_history().await.unwrap().checkpoint.is_none());
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
        assert!(adapter.read_native_history(&session.actor.session_info).await.unwrap().checkpoint.is_none());
        assert!(!session.directory.path().join("native_history/current.json").exists());
    }).await;
}

use xai_chat_state as r10_chat_state;

type R10Checkpoint = r10_chat_state::persistence::native::NativeCheckpoint;

fn r10_blob_ref(
    kind: &str,
    bytes: &[u8],
) -> xai_grok_sampling_types::conversation::native_search_history::NativeBlobRef {
    xai_grok_sampling_types::conversation::native_search_history::NativeBlobRef {
        kind: kind.into(),
        byte_len: bytes.len() as u64,
        digest: r10_chat_state::persistence::native::digest(bytes),
    }
}

fn r10_put_blob(
    checkpoint: &mut R10Checkpoint,
    kind: &str,
    bytes: Vec<u8>,
) -> xai_grok_sampling_types::conversation::native_search_history::NativeBlobRef {
    let reference = r10_blob_ref(kind, &bytes);
    checkpoint
        .blob_kinds
        .insert(reference.digest.clone(), kind.into());
    checkpoint
        .blobs
        .insert(reference.digest.clone(), bytes.into());
    reference
}

fn r10_partition_bytes(checkpoint: &R10Checkpoint, partition: &str) -> u64 {
    checkpoint
        .blobs
        .iter()
        .filter(|(key, _)| {
            let kind = checkpoint.blob_kinds[*key].as_str();
            match partition {
                "source" => matches!(
                    kind,
                    "raw_block"
                        | "partial_input_json"
                        | "producing_binding"
                        | "eligible_schema"
                        | "eligible_snapshot"
                        | "source"
                ),
                "snapshot" => matches!(kind, "snapshot" | "snapshot_body"),
                _ => kind == partition,
            }
        })
        .map(|(_, bytes)| bytes.len() as u64)
        .sum()
}

fn r10_snapshot_body(bytes: usize) -> Vec<u8> {
    let mut body = serde_json::json!({"messages":[],"tools":[],"padding":""});
    let overhead = serde_json::to_vec(&body).unwrap().len();
    body["padding"] = serde_json::json!("x".repeat(bytes - overhead));
    let encoded = serde_json::to_vec(&body).unwrap();
    assert_eq!(encoded.len(), bytes);
    encoded
}

fn r10_snapshot_pair(seed: &R10Checkpoint) -> Vec<R10Checkpoint> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let binding = seed.source.producing_bindings[0].origin_ref.clone();
    let big = r10_snapshot_body(NATIVE_SNAPSHOT_PAYLOAD_BYTES as usize);
    let big_descriptor = NativeSnapshot {
        body: r10_blob_ref("snapshot_body", &big),
        tools: Some(NativeToolsRange {
            offset: 0,
            byte_len: 2,
            digest: r10_chat_state::persistence::native::digest(b"[]"),
        }),
        binding: binding.clone(),
    };
    let mut small_len = (NATIVE_SNAPSHOT_BYTES - NATIVE_SNAPSHOT_PAYLOAD_BYTES - 1024) as usize;
    for _ in 0..8 {
        let small = r10_snapshot_body(small_len);
        let first = seed.with_snapshot(&small, binding.clone()).unwrap();
        let mut second_descriptor = big_descriptor.clone();
        let tools_start = big
            .windows(b"\"tools\":[]".len())
            .position(|window| window == b"\"tools\":[]")
            .unwrap()
            + b"\"tools\":".len();
        second_descriptor.tools.as_mut().unwrap().offset = tools_start as u64;
        let predicted = r10_partition_bytes(&first, "snapshot")
            + big.len() as u64
            + serde_json::to_vec(&second_descriptor).unwrap().len() as u64;
        if predicted == NATIVE_SNAPSHOT_BYTES {
            let second = first.with_snapshot(&big, binding).unwrap();
            assert_eq!(
                r10_partition_bytes(&second, "snapshot"),
                NATIVE_SNAPSHOT_BYTES
            );
            return vec![first, second];
        }
        small_len =
            usize::try_from(small_len as i64 + NATIVE_SNAPSHOT_BYTES as i64 - predicted as i64)
                .unwrap();
    }
    panic!("snapshot fixture sizing did not converge");
}

fn r10_expand_source(checkpoint: &mut R10Checkpoint, target: u64) {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let existing = r10_partition_bytes(checkpoint, "source");
    let mut padding = usize::try_from(target - existing - 4096).unwrap();
    for _ in 0..8 {
        let schema = serde_json::to_vec(&serde_json::json!({"type":"object","description":format!("é\n\"\\schema:{}", "x".repeat(padding)),"properties":{}})).unwrap();
        let identity = NativeEligibleSnapshot {
            version: 1,
            namespace_kind: NativeNamespaceKind::Mcp,
            qualified_namespace: "mcp.capacity".into(),
            child_name: "boundary".into(),
            schema: r10_blob_ref("eligible_schema", &schema),
            binding: checkpoint.source.producing_bindings[0].origin_ref.clone(),
        };
        let identity_bytes = serde_json::to_vec(&identity).unwrap();
        let mut source = checkpoint.source.clone();
        source.source_revision += 1;
        source.older_sources.push(checkpoint.record.source.clone());
        source
            .eligible_snapshots
            .push(r10_blob_ref("eligible_snapshot", &identity_bytes));
        let source_bytes = serde_json::to_vec(&source).unwrap();
        let measured = existing
            + schema.len() as u64
            + identity_bytes.len() as u64
            + source_bytes.len() as u64;
        if measured == target {
            r10_put_blob(checkpoint, "eligible_schema", schema);
            r10_put_blob(checkpoint, "eligible_snapshot", identity_bytes);
            checkpoint.record.source = r10_put_blob(checkpoint, "source", source_bytes);
            checkpoint.record.lineage.source_digest = checkpoint.record.source.digest.clone();
            checkpoint.record.lineage.source_revision = source.source_revision;
            checkpoint.source = source;
            return;
        }
        padding = usize::try_from(padding as i64 + target as i64 - measured as i64).unwrap();
    }
    panic!("source fixture sizing did not converge");
}

fn r10_quota_candidate(
    seed: &R10Checkpoint,
    source_bytes: Option<u64>,
    active_bytes: Option<u64>,
    ledger_bytes: Option<u64>,
) -> R10Checkpoint {
    let mut checkpoint = seed.clone();
    checkpoint.record.previous_checkpoint = Some(r10_put_blob(
        &mut checkpoint,
        "checkpoint",
        seed.bytes.to_vec(),
    ));
    checkpoint.record.generation += 1;
    checkpoint.record.high_water = checkpoint.record.generation;
    if let Some(target) = source_bytes {
        r10_expand_source(&mut checkpoint, target);
    }
    if let Some(target) = active_bytes {
        let existing = r10_partition_bytes(&checkpoint, "active");
        checkpoint
            .active
            .loaded_invalidations
            .push("é\n\"\\invalidated:".into());
        let overhead = serde_json::to_vec(&checkpoint.active).unwrap().len() as u64;
        checkpoint
            .active
            .loaded_invalidations
            .last_mut()
            .unwrap()
            .push_str(&"x".repeat((target - existing - overhead) as usize));
        let bytes = serde_json::to_vec(&checkpoint.active).unwrap();
        checkpoint.record.active = r10_put_blob(&mut checkpoint, "active", bytes);
        assert_eq!(r10_partition_bytes(&checkpoint, "active"), target);
    }
    checkpoint.ledger.source_digest = checkpoint.record.source.digest.clone();
    checkpoint.ledger.active_digest = checkpoint.record.active.digest.clone();
    if let Some(target) = ledger_bytes {
        let existing = r10_partition_bytes(&checkpoint, "ledger");
        checkpoint
            .ledger
            .transitions
            .push(serde_json::json!({"reason":"capacity fixture","detail":"é\n\"\\transition:"}));
        let overhead = serde_json::to_vec(&checkpoint.ledger).unwrap().len() as u64;
        checkpoint.ledger.transitions.last_mut().unwrap()["detail"] = serde_json::json!(format!(
            "é\n\"\\transition:{}",
            "x".repeat((target - existing - overhead) as usize)
        ));
    }
    let ledger = serde_json::to_vec(&checkpoint.ledger).unwrap();
    checkpoint.record.ledger = r10_put_blob(&mut checkpoint, "ledger", ledger);
    if let Some(target) = ledger_bytes {
        assert_eq!(r10_partition_bytes(&checkpoint, "ledger"), target);
    }
    checkpoint.record.capacity.clear();
    checkpoint.bytes = serde_json::to_vec(&checkpoint.record).unwrap().into();
    checkpoint
}

fn r10_decode_checkpoint(
    checkpoint: &R10Checkpoint,
) -> Result<
    R10Checkpoint,
    xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
> {
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    R10Checkpoint::decode(checkpoint.bytes.to_vec(), |reference| {
        checkpoint
            .blobs
            .get(&reference.digest)
            .map(|bytes| bytes.to_vec())
            .ok_or(NativeUnavailable::InvalidReference)
    })
}

fn r10_assert_partition_charges(checkpoint: &R10Checkpoint) {
    for name in ["source", "active", "ledger", "snapshot"] {
        assert_eq!(
            checkpoint.record.capacity[&format!("{name}_bytes")],
            r10_partition_bytes(checkpoint, name),
            "R10 F3: production charges the actual serialized {name} partition"
        );
    }
    let actual_role: u64 = ["source", "active", "ledger", "snapshot"]
        .into_iter()
        .map(|name| r10_partition_bytes(checkpoint, name))
        .sum();
    let reserved = checkpoint.reservation().unwrap();
    for (name, role) in [
        ("current", &reserved.current),
        ("staging", &reserved.staging),
        ("backup", &reserved.backup),
    ] {
        assert_eq!(
            role.bytes().unwrap(),
            actual_role,
            "R10 F3: {name} reserves every actual encoded partition"
        );
        assert_eq!(
            checkpoint.record.capacity[&format!("reserved_{name}_bytes")],
            actual_role
        );
    }
    assert_eq!(
        reserved.validate().unwrap(),
        actual_role * 3
            + xai_grok_sampling_types::conversation::native_search_history::NATIVE_CONTROL_BYTES
    );
}

fn r10_seal_reader_fixture(
    checkpoint: &mut R10Checkpoint,
) -> Result<(), xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    for _ in 0..8 {
        let reserved = checkpoint.reservation()?;
        let source = r10_partition_bytes(checkpoint, "source");
        let active = r10_partition_bytes(checkpoint, "active");
        let ledger = r10_partition_bytes(checkpoint, "ledger");
        assert_eq!(reserved.current.active_bytes, active);
        assert_eq!(reserved.current.ledger_bytes, ledger);
        let members = checkpoint.ledger.accounting.len()
            + checkpoint.ledger.usage.len()
            + checkpoint.ledger.observations.len()
            + checkpoint.ledger.transitions.len()
            + checkpoint.ledger.lifecycle.waiting_clients.len()
            + checkpoint.ledger.lifecycle.terminal.len()
            + checkpoint
                .ledger
                .groups
                .iter()
                .map(|group| group.duplicate_observations.len())
                .sum::<usize>();
        let capacity = std::collections::BTreeMap::from([
            ("source_bytes".into(), source),
            (
                "source_responses".into(),
                checkpoint.source.responses.len() as u64,
            ),
            (
                "source_blocks".into(),
                checkpoint.source.ordered_blocks.len() as u64,
            ),
            (
                "source_groups".into(),
                checkpoint.ledger.groups.len() as u64,
            ),
            ("active_bytes".into(), active),
            ("ledger_bytes".into(), ledger),
            ("ledger_members".into(), members as u64),
            (
                "ledger_groups".into(),
                checkpoint.ledger.groups.len() as u64,
            ),
            (
                "snapshot_bytes".into(),
                r10_partition_bytes(checkpoint, "snapshot"),
            ),
            ("terminal_active_bytes".into(), 0),
            ("terminal_ledger_bytes".into(), 0),
            (
                "control_bytes".into(),
                r10_partition_bytes(checkpoint, "checkpoint") + checkpoint.bytes.len() as u64,
            ),
            ("reserved_current_bytes".into(), reserved.current.bytes()?),
            ("reserved_staging_bytes".into(), reserved.staging.bytes()?),
            ("reserved_backup_bytes".into(), reserved.backup.bytes()?),
            ("reserved_control_bytes".into(), reserved.control_bytes),
            ("reserved_lineage_bytes".into(), reserved.validate()?),
        ]);
        if checkpoint.record.capacity == capacity {
            return checkpoint.validate();
        }
        checkpoint.record.capacity = capacity;
        checkpoint.bytes = encode_native(&checkpoint.record, NATIVE_CONTROL_BYTES)?.into();
    }
    Err(NativeUnavailable::Fidelity)
}

async fn r10_disk_partition_boundary(partition: &str) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    session.sample().await;
    let info = session.actor.session_info.clone();
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let seed = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let ceiling = if partition == "active" {
        NATIVE_ACTIVE_BYTES
    } else {
        NATIVE_LEDGER_BYTES
    };
    let build = |extra| {
        r10_quota_candidate(
            &seed,
            None,
            (partition == "active").then_some(ceiling + extra),
            (partition == "ledger").then_some(ceiling + extra),
        )
    };
    let mut fit = build(0);
    r10_seal_reader_fixture(&mut fit).unwrap();
    let fit = r10_decode_checkpoint(&fit).unwrap();
    r10_assert_partition_charges(&fit);
    let over = build(1);
    assert!(matches!(
        r10_decode_checkpoint(&over),
        Err(NativeUnavailable::Capacity)
    ));
    let directory = session.directory.path().join("native_history");
    let authority = std::fs::read(directory.join("authority.json")).unwrap();
    let copies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = copies.clone();
    let checked = adapter.clone().with_file_sync_probe(move || {
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    });
    let make_write = |checkpoint| r10_chat_state::persistence::native::NativeWrite {
        previous_generation: seed.record.generation,
        transaction: format!("r10-{partition}-boundary"),
        checkpoint: Arc::new(checkpoint),
    };
    assert_eq!(
        checked
            .persist_native_generation_and_ack(&info, make_write(over))
            .await,
        Err(NativeUnavailable::Capacity),
        "R10 F3: disk admission refuses the first extra serialized {partition} byte"
    );
    assert_eq!(copies.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(directory.join("authority.json")).unwrap(),
        authority
    );
    assert!(matches!(
        checked
            .persist_native_generation_and_ack(&info, make_write(fit))
            .await
            .unwrap(),
        r10_chat_state::persistence::native::NativeAppendAck::Applied { generation: 2, .. }
    ));
    let loaded = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(r10_partition_bytes(&loaded, partition), ceiling);
    r10_assert_partition_charges(&loaded);
    adapter.load_session(&info).await.unwrap();
    adapter.load_session_without_updates(&info).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_disk_active_partition_boundary() {
    tokio::task::LocalSet::new()
        .run_until(r10_disk_partition_boundary("active"))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_disk_ledger_partition_boundary() {
    tokio::task::LocalSet::new()
        .run_until(r10_disk_partition_boundary("ledger"))
        .await;
}

fn r10_disk_control_reservation(
    directory: &std::path::Path,
    write: &r10_chat_state::persistence::native::NativeWrite,
) -> u64 {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let checkpoint = &write.checkpoint;
    let mut bytes = 0;
    let mut present = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            bytes += entry.metadata().unwrap().len();
        } else if entry.file_name() == "blobs" {
            for blob in std::fs::read_dir(entry.path()).unwrap() {
                let blob = blob.unwrap();
                let hash = blob
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if checkpoint
                    .blob_kinds
                    .get(&hash)
                    .is_some_and(|kind| kind == "checkpoint")
                {
                    bytes += blob.metadata().unwrap().len();
                }
                present.insert(hash);
            }
        }
    }
    for (hash, payload) in &checkpoint.blobs {
        if !present.contains(hash) && checkpoint.blob_kinds[hash] == "checkpoint" {
            bytes += payload.len() as u64;
        }
    }
    let authority = NativeGenerationAuthority {
        schema_version: NATIVE_SCHEMA_VERSION,
        lineage: checkpoint.record.lineage.id.clone(),
        high_water: checkpoint.record.high_water,
        checkpoint: r10_blob_ref("checkpoint", &checkpoint.bytes),
        transaction: write.transaction.clone(),
    };
    bytes
        + checkpoint.bytes.len() as u64 * 4
        + serde_json::to_vec(&authority).unwrap().len() as u64 * 2
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_disk_three_role_reservation_boundary() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            use xai_grok_sampling_types::conversation::native_search_history::*;
            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response(
                "/v1/messages",
                native_history_script(native_history_frames()),
            );
            let session = NativeHistorySession::new(&server).await;
            session.sample().await;
            let info = session.actor.session_info.clone();
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                session.directory.path().to_path_buf(),
            );
            let seed = adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            let snapshots = r10_snapshot_pair(&seed);
            for checkpoint in &snapshots {
                adapter
                    .persist_native_generation_and_ack(
                        &info,
                        r10_chat_state::persistence::native::NativeWrite {
                            previous_generation: checkpoint.record.generation - 1,
                            transaction: format!("r10-snapshot-{}", checkpoint.record.generation),
                            checkpoint: Arc::new(checkpoint.clone()),
                        },
                    )
                    .await
                    .unwrap();
            }
            let mut candidate = r10_quota_candidate(
                snapshots.last().unwrap(),
                Some(NATIVE_SOURCE_BYTES),
                Some(NATIVE_ACTIVE_BYTES),
                Some(NATIVE_LEDGER_BYTES),
            );
            r10_seal_reader_fixture(&mut candidate).unwrap();
            let candidate = r10_decode_checkpoint(&candidate).unwrap();
            r10_assert_partition_charges(&candidate);
            assert_eq!(
                candidate.reservation().unwrap().current.bytes().unwrap(),
                NATIVE_ROLE_BYTES
            );
            assert_eq!(
                candidate.record.capacity["reserved_lineage_bytes"],
                NATIVE_LINEAGE_BYTES
            );
            let write = r10_chat_state::persistence::native::NativeWrite {
                previous_generation: 3,
                transaction: "r10-max-roles".into(),
                checkpoint: Arc::new(candidate),
            };
            let directory = session.directory.path().join("native_history");
            let old_authority = std::fs::read(directory.join("authority.json")).unwrap();
            let occupied = r10_disk_control_reservation(&directory, &write);
            let padding = (NATIVE_CONTROL_BYTES - occupied) as usize;
            let control = serde_json::to_vec(&"x".repeat(padding - 2)).unwrap();
            assert_eq!(control.len(), padding);
            let path = directory.join("r10-control-reserve.json");
            let mut over = control.clone();
            over.push(b' ');
            std::fs::write(&path, over).unwrap();
            assert_eq!(
                r10_disk_control_reservation(&directory, &write),
                NATIVE_CONTROL_BYTES + 1
            );
            let copies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = copies.clone();
            let checked = adapter.clone().with_file_sync_probe(move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            });
            assert_eq!(
                checked
                    .persist_native_generation_and_ack(&info, write.clone())
                    .await,
                Err(NativeUnavailable::Capacity),
                "R10 F3: one extra physical control byte refuses before copying three maximum roles"
            );
            assert_eq!(copies.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                std::fs::read(directory.join("authority.json")).unwrap(),
                old_authority
            );
            std::fs::write(&path, control).unwrap();
            assert_eq!(
                r10_disk_control_reservation(&directory, &write),
                NATIVE_CONTROL_BYTES
            );
            assert!(matches!(
                checked
                    .persist_native_generation_and_ack(&info, write)
                    .await
                    .unwrap(),
                r10_chat_state::persistence::native::NativeAppendAck::Applied { generation: 4, .. }
            ));
            let loaded = adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            r10_assert_partition_charges(&loaded);
            assert_eq!(
                loaded.record.capacity["reserved_lineage_bytes"],
                NATIVE_LINEAGE_BYTES
            );
            for (name, ceiling) in [
                ("source", NATIVE_SOURCE_BYTES),
                ("active", NATIVE_ACTIVE_BYTES),
                ("ledger", NATIVE_LEDGER_BYTES),
                ("snapshot", NATIVE_SNAPSHOT_BYTES),
            ] {
                assert_eq!(r10_partition_bytes(&loaded, name), ceiling);
            }
            for file in ["current.json", "staging.json", "backup.json"] {
                assert!(!std::fs::read(directory.join(file)).unwrap().is_empty());
            }
            adapter.load_session(&info).await.unwrap();
            adapter.load_session_without_updates(&info).await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_retry_reestablishes_blob_durability() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            use xai_chat_state::persistence::native::{NativeAppendAck, digest};
            use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;

            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response(
                "/v1/messages",
                native_history_script(native_history_frames()),
            );
            let session = NativeHistorySession::new(&server).await;
            session.sample().await;
            let info = session.actor.session_info.clone();
            let directory = session.directory.path().join("native_history");
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                session.directory.path().to_path_buf(),
            );
            let candidate = u2_metadata_candidate(&session).await;
            let checkpoint_blob = directory
                .join("blobs")
                .join(format!("{}.json", digest(&candidate.checkpoint.bytes)));
            assert!(!checkpoint_blob.exists());
            let old_authority = std::fs::read(directory.join("authority.json")).unwrap();
            let old_current = std::fs::read(directory.join("current.json")).unwrap();
            let failed = Arc::new(AtomicBool::new(false));
            let observed = failed.clone();
            let probe_path = checkpoint_blob.clone();
            let first = adapter.clone().with_parent_sync_probe(move || {
                if probe_path.exists() && !observed.swap(true, Ordering::SeqCst) {
                    return Err(std::io::Error::other(
                        "last new blob renamed before directory sync",
                    ));
                }
                Ok(())
            });
            assert_eq!(
                first
                    .persist_native_generation_and_ack(&info, candidate.clone())
                    .await,
                Err(NativeUnavailable::Persistence)
            );
            assert!(failed.load(Ordering::SeqCst));
            assert_eq!(
                std::fs::read(&checkpoint_blob).unwrap(),
                candidate.checkpoint.bytes.as_ref()
            );
            assert_eq!(
                std::fs::read(directory.join("authority.json")).unwrap(),
                old_authority
            );
            assert_eq!(
                std::fs::read(directory.join("current.json")).unwrap(),
                old_current
            );
            assert!(candidate.checkpoint.blobs.keys().all(|hash| {
                directory
                    .join("blobs")
                    .join(format!("{hash}.json"))
                    .exists()
            }));

            let reused_checkpoint_barrier = candidate.checkpoint.blobs.len() + 2;
            for fault in ["file", "directory"] {
                let files = Arc::new(AtomicUsize::new(0));
                let parents = Arc::new(AtomicUsize::new(0));
                let file_count = files.clone();
                let parent_count = parents.clone();
                let retry = adapter
                    .clone()
                    .with_file_sync_probe(move || {
                        let at = file_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if fault == "file" && at == reused_checkpoint_barrier {
                            return Err(std::io::Error::other(
                                "reused checkpoint file barrier refused",
                            ));
                        }
                        Ok(())
                    })
                    .with_parent_sync_probe(move || {
                        let at = parent_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if fault == "directory" && at == reused_checkpoint_barrier {
                            return Err(std::io::Error::other(
                                "reused checkpoint directory barrier refused",
                            ));
                        }
                        Ok(())
                    });
                assert_eq!(
                    retry
                        .persist_native_generation_and_ack(&info, candidate.clone())
                        .await,
                    Err(NativeUnavailable::Persistence),
                    "R10 F1: reused blob {fault} durability must be confirmed before Applied"
                );
                assert_eq!(
                    std::fs::read(directory.join("authority.json")).unwrap(),
                    old_authority
                );
                assert_eq!(
                    std::fs::read(directory.join("current.json")).unwrap(),
                    old_current
                );
                assert_eq!(
                    adapter
                        .read_native_history(&info)
                        .await
                        .unwrap()
                        .checkpoint
                        .unwrap()
                        .record
                        .generation,
                    1
                );
            }

            let files = Arc::new(AtomicUsize::new(0));
            let parents = Arc::new(AtomicUsize::new(0));
            let file_count = files.clone();
            let parent_count = parents.clone();
            let retry = adapter
                .clone()
                .with_file_sync_probe(move || {
                    file_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .with_parent_sync_probe(move || {
                    parent_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                });
            let acknowledgement = retry
                .persist_native_generation_and_ack(&info, candidate.clone())
                .await
                .unwrap();
            assert_eq!(
                acknowledgement,
                NativeAppendAck::Applied {
                    generation: 2,
                    digest: digest(&candidate.checkpoint.bytes)
                }
            );
            assert_eq!(
                files.load(Ordering::SeqCst),
                candidate.checkpoint.blobs.len() + 5,
                "R10 F1: every reused blob file crosses a barrier before Applied"
            );
            assert_eq!(
                parents.load(Ordering::SeqCst),
                candidate.checkpoint.blobs.len() + 5,
                "R10 F1: every reused blob directory crosses a barrier before Applied"
            );
            let loaded = adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            assert_eq!(loaded.bytes, candidate.checkpoint.bytes);
            assert_eq!(loaded.record.generation, 2);
        })
        .await;
}

#[derive(Clone, Copy, Debug)]
enum NativeHistoryRewrite {
    Compaction,
    Rewind,
}

async fn native_history_rewrite_survives_reload(rewrite: NativeHistoryRewrite) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;

    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    session.actor.chat_state_handle.replace_conversation(vec![
        ConversationItem::system("native-history rewrite fixture"),
        ConversationItem::user("<user_info>rewrite fixture</user_info>"),
    ]);
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("earlier prompt retained by rewind"))
        .await;
    session.actor.chat_state_handle.increment_prompt_index();
    session.sample().await;
    session.actor.chat_state_handle.increment_prompt_index();
    assert_eq!(session.actor.chat_state_handle.get_prompt_index().await, 2);
    assert_eq!(server.messages_request_count(), 1);
    let info = session.actor.session_info.clone();
    let directory = session.directory.path().join("native_history");
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let checkpoint = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(checkpoint.source.responses.len(), 1);
    let authority = std::fs::read(directory.join("authority.json")).unwrap();
    let current = std::fs::read(directory.join("current.json")).unwrap();
    let retained: Vec<_> = std::fs::read_dir(directory.join("blobs"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let before = session.actor.chat_state_handle.get_conversation().await;
    let live_before = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap();
    assert_eq!(live_before.checkpoint.unwrap().bytes, checkpoint.bytes);
    let rewritten = match rewrite {
        NativeHistoryRewrite::Compaction => {
            let items = vec![ConversationItem::user("compacted ordinary summary")];
            session
                .actor
                .chat_state_handle
                .replace_conversation_for_compaction(items.clone());
            items
        }
        NativeHistoryRewrite::Rewind => {
            let sampled_user = before
                .iter()
                .enumerate()
                .filter(|(_, item)| matches!(item, ConversationItem::User(_)))
                .nth(2)
                .unwrap()
                .0;
            assert!(!crate::session::persistence::session_dir(&info).exists());
            let result = session
                .actor
                .handle_rewind(crate::session::RewindRequest {
                    target_prompt_index: 1,
                    force: true,
                    mode: crate::session::RewindMode::ConversationOnly,
                })
                .await
                .unwrap();
            assert!(
                result.success,
                "ordinary shell rewind must succeed: {result:?}"
            );
            assert_eq!(session.actor.chat_state_handle.get_prompt_index().await, 1);
            before[..sampled_user].to_vec()
        }
    };
    let actual = session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&rewritten).unwrap()
    );
    assert_eq!(
        checkpoint.matches_history(&actual),
        Err(NativeUnavailable::StaleHistory)
    );
    let live = session.actor.chat_state_handle.read_native_history().await;
    assert!(
        matches!(live, Err(NativeUnavailable::StaleHistory)),
        "R13 F1: live native read must reject stale coordinates after {rewrite:?}; error={:?}",
        live.as_ref().err()
    );
    assert_eq!(
        serde_json::to_value(session.actor.chat_state_handle.get_conversation().await).unwrap(),
        serde_json::to_value(&rewritten).unwrap(),
        "R13 F1: native refusal must not change ordinary history"
    );
    assert!(matches!(
        session
            .actor
            .chat_state_handle
            .begin_native_capture(info.id.to_string(), "live-after-rewrite".into(), 2)
            .await,
        Err(NativeUnavailable::StaleHistory)
    ));
    assert!(matches!(
        session.actor.chat_state_handle.read_native_history().await,
        Err(NativeUnavailable::StaleHistory)
    ));
    let (respond_to, ack) = tokio::sync::oneshot::channel();
    session
        .persistence
        .tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(NATIVE_TIMEOUT, ack)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let history_bytes = std::fs::read(session.directory.path().join("chat_history.jsonl")).unwrap();
    assert!(!history_bytes.is_empty());
    let reopened = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let full = reopened.load_session(&info).await;
    let light = reopened.load_session_without_updates(&info).await;
    assert!(
        full.is_ok() && light.is_ok(),
        "R12 F1: legitimate {rewrite:?} must reopen ordinary history through full and light loaders; full={:?}, light={:?}",
        full.as_ref().err(),
        light.as_ref().err()
    );
    let full = full.unwrap();
    let light = light.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&rewritten).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&light.chat_history).unwrap(),
        serde_json::to_value(&rewritten).unwrap()
    );
    assert!(matches!(
        reopened.read_native_history(&info).await,
        Err(NativeUnavailable::StaleHistory)
    ));

    let sampling = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    let (event_tx, _events) = tokio::sync::mpsc::unbounded_channel();
    let cancel = tokio_util::sync::CancellationToken::new();
    let restored = xai_chat_state::ChatStateActor::spawn(
        full.chat_history,
        sampling,
        Box::new(
            crate::session::chat_persistence::ChannelChatPersistence::new(
                session.persistence.tx.clone(),
            ),
        ),
        event_tx,
        cancel.clone(),
    );
    assert!(matches!(
        restored.read_native_history().await,
        Err(NativeUnavailable::StaleHistory)
    ));
    assert_eq!(
        serde_json::to_value(restored.get_conversation().await).unwrap(),
        serde_json::to_value(&rewritten).unwrap()
    );
    assert!(matches!(
        restored
            .begin_native_capture(info.id.to_string(), "after-rewrite".into(), 1)
            .await,
        Err(NativeUnavailable::StaleHistory)
    ));
    cancel.cancel();

    assert_eq!(
        std::fs::read(directory.join("authority.json")).unwrap(),
        authority
    );
    assert_eq!(
        std::fs::read(directory.join("current.json")).unwrap(),
        current
    );
    assert_eq!(
        std::fs::read(session.directory.path().join("chat_history.jsonl")).unwrap(),
        history_bytes
    );
    for (path, bytes) in retained {
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "R12 F1: retained source is immutable after rewrite/reopen"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_compaction_reopens_ordinary_history() {
    tokio::task::LocalSet::new()
        .run_until(native_history_rewrite_survives_reload(
            NativeHistoryRewrite::Compaction,
        ))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_rewind_reopens_ordinary_history() {
    tokio::task::LocalSet::new()
        .run_until(native_history_rewrite_survives_reload(
            NativeHistoryRewrite::Rewind,
        ))
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn native_history_failed_retry_disk_observations() {
    let _retry_env = xai_grok_test_support::EnvGuard::unset("GROK_MAX_RETRIES");
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;

            let frames = native_history_frames();
            let terminal = frames.iter().position(|frame| frame["type"] == "message_delta").unwrap();
            let mut failed = frames[..terminal].to_vec();
            failed.push(json!({"type":"error","error":{
                "type":"invalid_request_error","message":"native fixture stream failed"}
            }));
            let first_usage = frames[0]["message"]["usage"].clone();
            let mut second_usage = first_usage.clone();
            second_usage.as_object_mut().unwrap().extend(
                frames[terminal]["usage"].as_object().unwrap().clone(),
            );
            let ordinary = native_untracked_output(frames.clone()).await;
            let server = MockInferenceServer::start().await.unwrap();
            server.enqueue_response("/v1/messages", native_history_script(failed));
            server.enqueue_response("/v1/messages", native_history_script(frames));
            let session = NativeHistorySession::with_retries(&server, 2).await;
            let outcome = tokio::time::timeout(NATIVE_TIMEOUT, session.sample_outcome()).await.unwrap().unwrap();
            let SamplerTurnOutcome::Response(response, metrics) = outcome else {
                panic!("the second real HTTP response must complete normally");
            };
            assert_eq!(native_response_observation(&response), ordinary);
            session.actor.record_response_token_usage(&response, Some(metrics.time_to_last_byte_ms));
            session.actor.record_response_items(response.items, response.usage.is_some()).await;
            session.actor.chat_state_handle.get_conversation().await;
            let (respond_to, ack) = tokio::sync::oneshot::channel();
            session.persistence.tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
            tokio::time::timeout(NATIVE_TIMEOUT, ack).await.unwrap().unwrap().unwrap();
            assert_eq!(server.messages_request_count(), 2);
            {
                let observed = session.observed.lock().unwrap();
                assert_eq!(observed.iter().filter(|event| matches!(event, xai_grok_sampler::SamplingEvent::Retrying { .. })).count(), 1);
                assert_eq!(observed.iter().filter(|event| matches!(event, xai_grok_sampler::SamplingEvent::Completed { .. })).count(), 1);
                assert!(!observed.iter().any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Failed { .. })));
            }
            let info = session.actor.session_info.clone();
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
            let full = adapter.load_session(&info).await.unwrap();
            let light = adapter.load_session_without_updates(&info).await.unwrap();
            assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&light.chat_history).unwrap());
            let root: Value = serde_json::from_slice(&std::fs::read(session.directory.path().join("native_history/current.json")).unwrap()).unwrap();
            let source = native_blob(session.directory.path(), &root["source"]);
            let active = native_blob(session.directory.path(), &root["active"]);
            let ledger = native_blob(session.directory.path(), &root["ledger"]);
            let responses = source["responses"].as_array().unwrap();
            let usage = ledger["usage"].as_array().unwrap();
            assert_eq!(responses.len(), 2, "R12 F2: failed-attempt disk/retry source record must survive");
            assert_eq!(usage.len(), 2, "R12 F2: failed-attempt observed usage must survive retry and reload");
            assert!(
                responses[0]["status"].get("PartialTerminal").is_some() && responses[0]["stream_complete"] == false,
                "R12 F2: independent reload must reject replay-ready status for the partial occurrence"
            );
            assert_eq!(responses[1]["status"], "Accepted");
            assert_eq!(responses[1]["stream_complete"], true);
            assert_ne!(responses[0]["occurrence"], responses[1]["occurrence"]);
            assert_eq!(responses[0]["provider_response_id"], responses[1]["provider_response_id"]);
            assert!(active["view_links"].as_array().unwrap().iter().all(|link| link["source"]["response"] != responses[0]["occurrence"]));
            assert_eq!(usage[0]["observed"], first_usage, "R12 F2: independent reload must preserve SSE usage presence and categories");
            assert_eq!(usage[1]["observed"], second_usage, "R12 F2: retry usage snapshots replace rather than double-charge categories");
            for (index, entry) in usage.iter().enumerate() {
                assert_eq!(entry["attempt"], index + 1);
                assert_eq!(entry["http_dispatches"], 1);
            }
            let native = adapter.read_native_history(&info).await.unwrap().checkpoint.unwrap();
            assert_eq!(native.record.generation, root["generation"].as_u64().unwrap());
        })
        .await;
}

async fn r14_flush_native_session(session: &NativeHistorySession) {
    session.actor.chat_state_handle.get_conversation().await;
    let (respond_to, ack) = tokio::sync::oneshot::channel();
    session
        .persistence
        .tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(NATIVE_TIMEOUT, ack)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn r14_response_rewrite_invalidates_views(strip: bool) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    session.sample().await;
    let info = session.actor.session_info.clone();
    let directory = session.directory.path().join("native_history");
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let checkpoint = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let count = checkpoint.record.history_boundary.items as usize;
    let authority = std::fs::read(directory.join("authority.json")).unwrap();
    let current = std::fs::read(directory.join("current.json")).unwrap();
    let blobs: Vec<_> = std::fs::read_dir(directory.join("blobs"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("unrelated ordinary suffix"))
        .await;
    r14_flush_native_session(&session).await;
    let before = session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        session
            .actor
            .chat_state_handle
            .read_native_history()
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .bytes,
        checkpoint.bytes
    );
    assert_eq!(
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .bytes,
        checkpoint.bytes
    );
    let mut expected = before.clone();
    if strip {
        let reasoning = expected
            .iter()
            .position(|item| matches!(item, ConversationItem::Reasoning(_)))
            .unwrap();
        assert!(
            reasoning >= count,
            "the strip is confined to the captured response"
        );
        expected.remove(reasoning);
        assert!(matches!(
            session
                .actor
                .chat_state_handle
                .strip_model_bound_history()
                .await,
            xai_chat_state::StripOutcome::Applied { stripped: 1 }
        ));
    } else {
        let assistant = expected
            .iter_mut()
            .enumerate()
            .find_map(|(index, item)| match item {
                ConversationItem::Assistant(assistant) if index >= count => Some(assistant),
                _ => None,
            })
            .unwrap();
        assistant.content = Arc::from("changed captured assistant bytes");
        session
            .actor
            .chat_state_handle
            .replace_conversation_for_compaction(expected.clone());
    }
    r14_flush_native_session(&session).await;
    let actual = session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual[..count]).unwrap(),
        serde_json::to_value(&before[..count]).unwrap()
    );
    let live = session.actor.chat_state_handle.read_native_history().await;
    assert!(
        matches!(live, Err(NativeUnavailable::StaleHistory)),
        "R14 A: unchanged prefix must not validate rewritten response coordinates; strip={strip}, error={:?}",
        live.as_ref().err()
    );
    let full = adapter.load_session(&info).await.unwrap();
    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&light.chat_history).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let reopened = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    assert!(
        matches!(
            reopened.read_native_history(&info).await,
            Err(NativeUnavailable::StaleHistory)
        ),
        "R14 A: the reopened native reader must reject the same stale response view"
    );
    let sampling = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    let (event_tx, _events) = tokio::sync::mpsc::unbounded_channel();
    let cancel = tokio_util::sync::CancellationToken::new();
    let restored = xai_chat_state::ChatStateActor::spawn(
        full.chat_history,
        sampling,
        Box::new(
            crate::session::chat_persistence::ChannelChatPersistence::new(
                session.persistence.tx.clone(),
            ),
        ),
        event_tx,
        cancel.clone(),
    );
    assert!(matches!(
        restored.read_native_history().await,
        Err(NativeUnavailable::StaleHistory)
    ));
    assert_eq!(
        serde_json::to_value(restored.get_conversation().await).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    cancel.cancel();
    assert_eq!(
        std::fs::read(directory.join("authority.json")).unwrap(),
        authority
    );
    assert_eq!(
        std::fs::read(directory.join("current.json")).unwrap(),
        current
    );
    for (path, bytes) in blobs {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(server.messages_request_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_response_strip_invalidates_live_and_disk_views() {
    tokio::task::LocalSet::new()
        .run_until(r14_response_rewrite_invalidates_views(true))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_response_rewrite_invalidates_item_digest() {
    tokio::task::LocalSet::new()
        .run_until(r14_response_rewrite_invalidates_views(false))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_uncommitted_response_has_no_active_view() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::StorageAdapter;
        use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
        use sha2::{Digest, Sha256};
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response("/v1/messages", native_history_script(native_history_frames()));
        let session = NativeHistorySession::new(&server).await;
        let outcome = session.sample_outcome().await.unwrap();
        let SamplerTurnOutcome::Response(response, metrics) = outcome else { panic!("real response expected") };
        let info = session.actor.session_info.clone();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
        let live = session.actor.chat_state_handle.read_native_history().await;
        assert!(matches!(live, Err(NativeUnavailable::StaleHistory)),
            "R14 A: source acknowledgement is not an active view before the actual response commit; error={:?}", live.as_ref().err());
        assert!(matches!(adapter.read_native_history(&info).await, Err(NativeUnavailable::StaleHistory)));
        assert!(adapter.load_session(&info).await.is_ok());
        session.actor.record_response_token_usage(&response, Some(metrics.time_to_last_byte_ms));
        session.actor.record_response_items(response.items, response.usage.is_some()).await;
        r14_flush_native_session(&session).await;
        let live = session.actor.chat_state_handle.read_native_history().await.unwrap().checkpoint.unwrap();
        assert_eq!(adapter.read_native_history(&info).await.unwrap().checkpoint.unwrap().bytes, live.bytes);
        let full = adapter.load_session(&info).await.unwrap();
        let record: Value = serde_json::from_slice(&std::fs::read(session.directory.path().join("native_history/current.json")).unwrap()).unwrap();
        let active = native_blob(session.directory.path(), &record["active"]);
        for link in active["view_links"].as_array().unwrap() {
            let index = link["view_item"].as_u64().unwrap() as usize;
            let bytes = serde_json::to_vec(&std::slice::from_ref(&full.chat_history[index])).unwrap();
            assert_eq!(link["view_digest"], format!("{:x}", Sha256::digest(bytes)),
                "R14 A: each acknowledged link binds the independently serialized committed item");
        }
    }).await;
}

struct R14FirstWrite {
    _source: NativeHistorySession,
    target: tempfile::TempDir,
    info: crate::session::info::Info,
    adapter: crate::session::storage::JsonlStorageAdapter,
    write: xai_chat_state::persistence::native::NativeWrite,
    history: Vec<ConversationItem>,
}

async fn r14_first_write_fixture() -> R14FirstWrite {
    use crate::session::storage::{SessionUpdate, StorageAdapter};
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let source = NativeHistorySession::new(&server).await;
    source.sample().await;
    let info = source.actor.session_info.clone();
    let checkpoint = source
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let history = source.actor.chat_state_handle.get_conversation().await;
    let target = tempfile::tempdir().unwrap();
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        target.path().to_path_buf(),
    );
    adapter
        .init_session(&info, acp::ModelId::new("test"))
        .await
        .unwrap();
    adapter.replace_chat_history(&info, &history).await.unwrap();
    for update in [
        acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("rebuild ordinary user"),
        ))),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("rebuild ordinary assistant"),
        ))),
    ] {
        adapter
            .append_update(
                &info,
                &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                    info.id.clone(),
                    update,
                ))),
            )
            .await
            .unwrap();
    }
    assert!(
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    R14FirstWrite {
        _source: source,
        target,
        info,
        adapter,
        write: xai_chat_state::persistence::native::NativeWrite {
            previous_generation: 0,
            transaction: "r14-first-real-capture".into(),
            checkpoint,
        },
        history,
    }
}

#[derive(Clone, Copy, Debug)]
enum R14FirstFault {
    Blob,
    FileSync,
    DirectorySync,
    StaleHistory,
}

async fn r14_first_failure_recovers(fault: R14FirstFault) {
    use crate::session::storage::StorageAdapter;
    use std::sync::atomic::{AtomicBool, Ordering};
    use xai_chat_state::persistence::native::{NativeAppendAck, digest};
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    let fixture = r14_first_write_fixture().await;
    let R14FirstWrite {
        target,
        info,
        adapter,
        write,
        history,
        ..
    } = &fixture;
    let directory = target.path().join("native_history");
    let chat = target.path().join("chat_history.jsonl");
    let fired = Arc::new(AtomicBool::new(false));
    let probe_fired = fired.clone();
    let faulted = match fault {
        R14FirstFault::Blob => {
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("blobs"), b"first blob directory obstruction").unwrap();
            adapter.clone()
        }
        R14FirstFault::FileSync => adapter.clone().with_file_sync_probe(move || {
            probe_fired.store(true, Ordering::SeqCst);
            Err(std::io::Error::other("R14 injected first blob file sync"))
        }),
        R14FirstFault::DirectorySync => adapter.clone().with_parent_sync_probe(move || {
            probe_fired.store(true, Ordering::SeqCst);
            Err(std::io::Error::other(
                "R14 injected first blob directory sync",
            ))
        }),
        R14FirstFault::StaleHistory => {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&chat)
                .unwrap();
            file.write_all(b"{torn ordinary history\n").unwrap();
            adapter.clone()
        }
    };
    let first = faulted
        .persist_native_generation_and_ack(info, write.clone())
        .await;
    assert!(
        first.is_err(),
        "first {fault:?} must not acknowledge a native generation"
    );
    if matches!(
        fault,
        R14FirstFault::FileSync | R14FirstFault::DirectorySync
    ) {
        assert!(
            fired.load(Ordering::SeqCst),
            "the first blob durability fault must fire"
        );
    }
    assert!(directory.join("transaction.lock").exists());
    assert!(!directory.join("authority.json").exists());
    assert!(!directory.join("current.json").exists());
    let reopened = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        target.path().to_path_buf(),
    );
    let full = reopened.load_session(info).await;
    let light = reopened.load_session_without_updates(info).await;
    assert!(
        full.is_ok() && light.is_ok(),
        "R14 B: unpublished first {fault:?} must not poison ordinary full/light loading; full={:?}, light={:?}",
        full.as_ref().err(),
        light.as_ref().err()
    );
    assert_eq!(
        serde_json::to_value(full.unwrap().chat_history).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    assert_eq!(
        serde_json::to_value(light.unwrap().chat_history).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    assert!(
        reopened
            .read_native_history(info)
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    assert_eq!(
        first,
        Err(if matches!(fault, R14FirstFault::StaleHistory) {
            NativeUnavailable::StaleHistory
        } else {
            NativeUnavailable::Persistence
        })
    );

    std::fs::rename(
        &chat,
        target.path().join("unpublished-ordinary-history.jsonl"),
    )
    .unwrap();
    let rebuilt = reopened.load_session(info).await;
    assert!(
        rebuilt.is_ok(),
        "R14 B: an unpublished first transaction must not block ordinary ACP reconstruction: {:?}",
        rebuilt.as_ref().err()
    );
    assert!(!rebuilt.unwrap().chat_history.is_empty());
    assert!(
        !reopened
            .load_session_without_updates(info)
            .await
            .unwrap()
            .chat_history
            .is_empty()
    );
    assert!(
        reopened
            .read_native_history(info)
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    assert!(
        !directory.join("authority.json").exists(),
        "ordinary reconstruction cannot manufacture native authority"
    );
    adapter.replace_chat_history(info, history).await.unwrap();
    if matches!(fault, R14FirstFault::Blob) {
        std::fs::rename(
            directory.join("blobs"),
            target.path().join("preserved-blob-obstruction"),
        )
        .unwrap();
    }
    let retried = adapter
        .persist_native_generation_and_ack(info, write.clone())
        .await;
    assert_eq!(
        retried,
        Ok(NativeAppendAck::Applied {
            generation: 1,
            digest: digest(&write.checkpoint.bytes)
        }),
        "R14 B: same-transaction retry must recover unpublished first {fault:?}"
    );
    let native = reopened
        .read_native_history(info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(native.bytes, write.checkpoint.bytes);
    assert_eq!(native.ledger.observations.len(), 1);
    let authority: Value =
        serde_json::from_slice(&std::fs::read(directory.join("authority.json")).unwrap()).unwrap();
    assert_eq!(authority["high_water"], 1);
    assert_eq!(authority["transaction"], write.transaction);
    assert_eq!(
        serde_json::to_value(reopened.load_session(info).await.unwrap().chat_history).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            reopened
                .load_session_without_updates(info)
                .await
                .unwrap()
                .chat_history
        )
        .unwrap(),
        serde_json::to_value(history).unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_first_blob_failure_recovers() {
    tokio::task::LocalSet::new()
        .run_until(r14_first_failure_recovers(R14FirstFault::Blob))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_first_file_sync_failure_recovers() {
    tokio::task::LocalSet::new()
        .run_until(r14_first_failure_recovers(R14FirstFault::FileSync))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_first_directory_sync_failure_recovers() {
    tokio::task::LocalSet::new()
        .run_until(r14_first_failure_recovers(R14FirstFault::DirectorySync))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_first_stale_history_preserves_ordinary_load() {
    tokio::task::LocalSet::new()
        .run_until(r14_first_failure_recovers(R14FirstFault::StaleHistory))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_first_published_authority_cannot_be_forgotten() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::StorageAdapter;
        use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
        let fixture = r14_first_write_fixture().await;
        let R14FirstWrite { target, info, adapter, write, .. } = &fixture;
        adapter.persist_native_generation_and_ack(info, write.clone()).await.unwrap();
        let directory = target.path().join("native_history");
        assert!(!directory.join("backup.json").exists());
        for name in ["authority.json", "current.json"] {
            std::fs::rename(directory.join(name), target.path().join(format!("preserved-{name}"))).unwrap();
        }
        let chat = target.path().join("chat_history.jsonl");
        std::fs::rename(&chat, target.path().join("published-ordinary-history.jsonl")).unwrap();
        let native = adapter.read_native_history(info).await;
        assert!(matches!(native, Err(NativeUnavailable::InvalidReference)),
            "R14 B: loss of first published authority must remain unavailable, never legacy; error={:?}", native.as_ref().err());
        assert!(adapter.load_session(info).await.is_err());
        assert!(adapter.load_session_without_updates(info).await.is_err());
        assert!(!chat.exists(), "N17: established native history must never be rebuilt from ACP updates");
        assert!(adapter.persist_native_generation_and_ack(info, write.clone()).await.is_err(),
            "first generation must not reset established authority");
    }).await;
}

async fn r14_capture_with_reserved_allowance(
    precharged: u64,
) -> (
    Value,
    Vec<xai_grok_sampling_types::conversation::native_search_history::NativeAttemptEvidence>,
    u64,
) {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(0),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        events,
    );
    let request_id = xai_grok_sampler::RequestId::random();
    let lease = NativeCaptureLease::issue(
        "r14-cap".into(),
        request_id.as_str().into(),
        1,
        1,
        NATIVE_SOURCE_BYTES,
    );
    lease.reserve(precharged).unwrap();
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_native_capture(
            request_id,
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
            lease.clone(),
        ),
    )
    .await
    .unwrap();
    let (response, _) = result
        .result
        .expect("digest reservation cannot change ordinary completion");
    let mut captured = Vec::new();
    let mut completed = 0;
    loop {
        let event = tokio::time::timeout(NATIVE_TIMEOUT, received.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                evidence: NativeEvidenceEvent::Attempt(attempt),
                ..
            } => captured.push(attempt.as_ref().clone()),
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                evidence: NativeEvidenceEvent::End,
                ..
            } => break,
            xai_grok_sampler::SamplingEvent::Completed { .. } => completed += 1,
            xai_grok_sampler::SamplingEvent::Failed { .. }
            | xai_grok_sampler::SamplingEvent::Retrying { .. } => {
                panic!("passive digest capacity cannot fail or retry ordinary sampling")
            }
            _ => {}
        }
    }
    assert_eq!(completed, 1);
    assert_eq!(server.messages_request_count(), 1);
    (
        native_response_observation(&response),
        captured,
        lease.charged().unwrap() - precharged,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_view_digest_capacity_preserves_partial_evidence() {
    tokio::task::LocalSet::new().run_until(async {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let ordinary = native_untracked_output(native_history_frames()).await;
        let (baseline_output, baseline, charged) = r14_capture_with_reserved_allowance(0).await;
        assert_eq!(baseline_output, ordinary);
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].status, NativeResponseStatus::Accepted);
        assert!(!baseline[0].view_digests.is_empty());
        assert!(charged > 1 && charged < NATIVE_SOURCE_BYTES);
        for (remaining, fits) in [(charged, true), (charged - 1, false)] {
            let (output, captured, _) = r14_capture_with_reserved_allowance(NATIVE_SOURCE_BYTES - remaining).await;
            assert_eq!(output, ordinary);
            assert_eq!(captured.len(), 1,
                "R14 A capacity: digest first-refusal must preserve the admitted attempt rather than drop it");
            assert_eq!(captured[0].blocks.len(), baseline[0].blocks.len());
            assert_eq!(serde_json::to_value(&captured[0].blocks).unwrap(), serde_json::to_value(&baseline[0].blocks).unwrap());
            assert_eq!(captured[0].usage, baseline[0].usage);
            assert_eq!(captured[0].http_dispatches, 1);
            assert!(captured[0].admitted && captured[0].response_started && captured[0].stream_complete);
            if fits {
                assert_eq!(captured[0].status, NativeResponseStatus::Accepted);
                assert_eq!(captured[0].view_digests, baseline[0].view_digests);
            } else {
                assert_eq!(captured[0].status, NativeResponseStatus::PartialTerminal(NativeUnavailable::Capacity));
                assert!(captured[0].view_digests.is_empty());
            }
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_affinity_pin_preserves_views_until_rewrite() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::session::storage::StorageAdapter;
            use sha2::{Digest, Sha256};
            use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;

            const PIN: &str = "history-s0-r15-affinity";
            for strip in [true, false] {
                let server = MockInferenceServer::start().await.unwrap();
                server.enqueue_response(
                    "/v1/messages",
                    native_history_script(native_history_frames()),
                );
                let session = NativeHistorySession::new(&server).await;
                let mut sampling = session.actor.chat_state_handle.get_sampling_config().await.unwrap();
                sampling.extra_headers.insert(
                    xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER.into(),
                    PIN.into(),
                );
                session.actor.chat_state_handle.update_sampling_config(sampling.clone());
                session.sample().await;
                let requests = server.requests();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].path, "/v1/messages");
                assert_eq!(
                    requests[0].header(xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER),
                    Some(PIN),
                );
                let info = session.actor.session_info.clone();
                let reopened = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                    session.directory.path().to_path_buf(),
                );
                let full = reopened.load_session(&info).await.unwrap();
                let light = reopened.load_session_without_updates(&info).await.unwrap();
                assert_eq!(
                    serde_json::to_value(&full.chat_history).unwrap(),
                    serde_json::to_value(&light.chat_history).unwrap(),
                );
                let reasoning = full.chat_history.iter().find_map(|item| match item {
                    ConversationItem::Reasoning(reasoning) => Some(reasoning),
                    _ => None,
                }).unwrap();
                assert_eq!(reasoning.mint_tag.as_deref(), Some(PIN));
                assert!(full.chat_history.iter().any(|item| matches!(item, ConversationItem::Discovery { .. })));
                let live = session.actor.chat_state_handle.read_native_history().await;
                let disk = reopened.read_native_history(&info).await;
                assert!(
                    live.is_ok() && disk.is_ok(),
                    "R15 F1: an untouched pinned response must retain live and reopened native views; live={:?}, disk={:?}",
                    live.as_ref().err(), disk.as_ref().err(),
                );
                let checkpoint = live.unwrap().checkpoint.unwrap();
                assert_eq!(disk.unwrap().checkpoint.unwrap().bytes, checkpoint.bytes);
                let boundary = checkpoint.record.history_boundary.items as usize;
                let directory = session.directory.path().join("native_history");
                let authority = std::fs::read(directory.join("authority.json")).unwrap();
                let current = std::fs::read(directory.join("current.json")).unwrap();
                let record: Value = serde_json::from_slice(&current).unwrap();
                let active = native_blob(session.directory.path(), &record["active"]);
                let source = native_blob(session.directory.path(), &record["source"]);
                assert_eq!(source["responses"][0]["status"], "Accepted");
                let raw_thinking = native_blob(session.directory.path(), &source["ordered_blocks"][1]["block"]);
                assert_eq!(raw_thinking, json!({"type":"thinking","thinking":"step by step","signature":"signed"}));
                for link in active["view_links"].as_array().unwrap() {
                    let index = link["view_item"].as_u64().unwrap() as usize;
                    let bytes = serde_json::to_vec(&std::slice::from_ref(&full.chat_history[index])).unwrap();
                    assert_eq!(link["view_digest"], format!("{:x}", Sha256::digest(bytes)),
                        "R15 F1: native view digests bind the post-stamp stored items");
                }
                let blobs: Vec<_> = std::fs::read_dir(directory.join("blobs")).unwrap()
                    .map(|entry| {
                        let path = entry.unwrap().path();
                        let bytes = std::fs::read(&path).unwrap();
                        (path, bytes)
                    }).collect();
                let (event_tx, _events) = tokio::sync::mpsc::unbounded_channel();
                let cancel = tokio_util::sync::CancellationToken::new();
                let restored = xai_chat_state::ChatStateActor::spawn(
                    full.chat_history,
                    sampling,
                    Box::new(crate::session::chat_persistence::ChannelChatPersistence::new(session.persistence.tx.clone())),
                    event_tx,
                    cancel.clone(),
                );
                assert_eq!(restored.read_native_history().await.unwrap().checkpoint.unwrap().bytes, checkpoint.bytes);
                cancel.cancel();
                session.actor.chat_state_handle.push_user_message_and_ack(
                    ConversationItem::user("unrelated suffix after pinned response"),
                ).await;
                r14_flush_native_session(&session).await;
                assert_eq!(session.actor.chat_state_handle.read_native_history().await.unwrap().checkpoint.unwrap().bytes, checkpoint.bytes);
                assert_eq!(reopened.read_native_history(&info).await.unwrap().checkpoint.unwrap().bytes, checkpoint.bytes);
                let before = session.actor.chat_state_handle.get_conversation().await;
                let mut expected = before.clone();
                if strip {
                    let position = expected.iter().position(|item| matches!(item, ConversationItem::Reasoning(_))).unwrap();
                    assert!(position >= boundary);
                    expected.remove(position);
                    assert!(matches!(
                        session.actor.chat_state_handle.strip_model_bound_history().await,
                        xai_chat_state::StripOutcome::Applied { stripped: 1 },
                    ));
                } else {
                    let assistant = expected.iter_mut().enumerate().find_map(|(index, item)| match item {
                        ConversationItem::Assistant(assistant) if index >= boundary => Some(assistant),
                        _ => None,
                    }).unwrap();
                    assistant.content = Arc::from("rewritten pinned response");
                    session.actor.chat_state_handle.replace_conversation_for_compaction(expected.clone());
                }
                r14_flush_native_session(&session).await;
                let actual = session.actor.chat_state_handle.get_conversation().await;
                assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap());
                assert_eq!(serde_json::to_value(&actual[..boundary]).unwrap(), serde_json::to_value(&before[..boundary]).unwrap());
                assert!(matches!(session.actor.chat_state_handle.read_native_history().await, Err(NativeUnavailable::StaleHistory)),
                    "R15 F1: a later pinned response strip or rewrite must invalidate the live view");
                let reloaded = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
                let full = reloaded.load_session(&info).await.unwrap();
                let light = reloaded.load_session_without_updates(&info).await.unwrap();
                assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&expected).unwrap());
                assert_eq!(serde_json::to_value(&light.chat_history).unwrap(), serde_json::to_value(&expected).unwrap());
                assert!(matches!(reloaded.read_native_history(&info).await, Err(NativeUnavailable::StaleHistory)),
                    "R15 F1: a later pinned response strip or rewrite must invalidate the reopened view");
                assert_eq!(std::fs::read(directory.join("authority.json")).unwrap(), authority);
                assert_eq!(std::fs::read(directory.join("current.json")).unwrap(), current);
                for (path, bytes) in blobs {
                    assert_eq!(std::fs::read(path).unwrap(), bytes);
                }
                assert_eq!(server.messages_request_count(), 1);
            }
        })
        .await;
}

async fn r17_unleased_output(frames: Vec<Value>, pin: Option<&str>) -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let mut config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(0),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    if let Some(pin) = pin {
        config.extra_headers.insert(
            xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER.into(),
            pin.into(),
        );
    }
    let (events, _received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        events,
    );
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_config(
            xai_grok_sampler::RequestId::random(),
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
        ),
    )
    .await
    .unwrap();
    let (response, _) = result.result.expect("unleased ordinary response completes");
    assert_eq!(server.messages_request_count(), 1);
    native_response_observation(&response)
}

async fn r17_run_primary_turn(session: &NativeHistorySession, request_id: &str) -> TurnOutcome {
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("find the account"))
        .await;
    tokio::time::timeout(
        Duration::from_secs(60),
        session.actor.process_conversation_turn_with_recovery(
            request_id,
            None,
            None,
            None,
            &mut length_salvage::LengthSalvage::new(None),
            &mut Default::default(),
        ),
    )
    .await
    .expect("real primary turn deadline")
    .expect("real primary turn completes")
}

fn r17_length_frames() -> Vec<Value> {
    let mut frames = native_history_frames();
    let terminal = frames.len() - 2;
    frames[terminal]["delta"]["stop_reason"] = json!("max_tokens");
    frames.splice(terminal..terminal, [
        json!({"type":"content_block_start","index":5,"content_block":{
            "type":"tool_use","id":"toolu_r17_length","name":"todo_write","input":{
                "merge":false,"todos":[{"id":"r17-length","content":"record completed item","status":"completed"}]}
        }}),
        json!({"type":"content_block_stop","index":5}),
    ]);
    frames
}

async fn r17_length_salvage_observation() {
    use crate::session::storage::StorageAdapter;
    let ordinary = r17_unleased_output(r17_length_frames(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(r17_length_frames()));
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    *session.actor.agent.borrow_mut() = test_grok_build_agent_with_todo().await;
    session
        .actor
        .workspace_ops
        .bind_local_session(
            &session.actor.session_id_string(),
            session.actor.tool_context.cwd.as_path().to_path_buf(),
            session.actor.tool_context.hunk_tracker_handle.clone(),
            session.actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .unwrap();
    let outcome = r17_run_primary_turn(&session, "r17-length").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { ref tools_called, .. } if tools_called == &["todo_write"]),
        "R16 Astra-F1: Length salvage still dispatches the complete ordinary tool once"
    );
    r14_flush_native_session(&session).await;
    assert_eq!(server.messages_request_count(), 2);
    let events = session.observed.lock().unwrap();
    let completions: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            xai_grok_sampler::SamplingEvent::Completed { response, .. } => Some(response.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 2);
    assert_eq!(
        native_response_observation(completions[0]),
        ordinary,
        "R16 Astra-F1: Length response output equals the unleased base"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Failed { .. }))
    );
    drop(events);
    let live = session.actor.chat_state_handle.read_native_history().await;
    assert!(
        live.is_ok(),
        "R16 Astra-F1: the salvage reminder must not invalidate committed native views; error={:?}",
        live.as_ref().err()
    );
    let checkpoint = live.unwrap().checkpoint.unwrap();
    assert_eq!(
        checkpoint.source.responses.len(),
        2,
        "R16 Astra-F1: the next primary sampling iteration captures another native response"
    );
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let info = session.actor.session_info.clone();
    let disk = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    assert_eq!(disk.bytes, checkpoint.bytes);
    let full = adapter.load_session(&info).await.unwrap();
    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    let reminder = full
        .chat_history
        .iter()
        .position(|item| {
            item.text_content()
                .contains(sampler_turn::OUTPUT_TOKEN_LIMIT_REMINDER)
        })
        .unwrap();
    let tool_call = full.chat_history.iter().position(|item| matches!(item, ConversationItem::Assistant(a) if a.tool_calls.iter().any(|tool| tool.id.as_ref() == "toolu_r17_length"))).unwrap();
    assert!(
        tool_call < reminder,
        "R16 Astra-F1: the existing reminder follows the real response commit"
    );
    assert!(
        server
            .requests()
            .last()
            .unwrap()
            .body
            .as_ref()
            .unwrap()
            .to_string()
            .contains("exceeded the output token limit"),
        "the same reminder reaches the next actual request"
    );
    let current: Value = serde_json::from_slice(
        &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
    )
    .unwrap();
    let active = native_blob(session.directory.path(), &current["active"]);
    for link in active["view_links"].as_array().unwrap() {
        let index = link["view_item"].as_u64().unwrap() as usize;
        assert_ne!(
            index, reminder,
            "no source may link to the salvage reminder"
        );
        assert!(matches!(
            &full.chat_history[index],
            ConversationItem::Reasoning(_)
                | ConversationItem::Discovery { .. }
                | ConversationItem::Assistant(_)
        ));
    }
}

#[test]
fn native_history_length_salvage_preserves_committed_views() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r17_length_salvage_observation());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Clone, Copy, Debug)]
enum R18ReasoningShape {
    Redacted,
    Empty,
    Discarded,
    CloseOrder,
}

fn r18_reasoning_frames(shape: R18ReasoningShape) -> Vec<Value> {
    let mut frames = native_history_frames();
    let replacement = match shape {
        R18ReasoningShape::Redacted => vec![
            json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque-data"}}),
            json!({"type":"content_block_stop","index":1}),
        ],
        R18ReasoningShape::Empty => vec![
            json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_stop","index":1}),
        ],
        R18ReasoningShape::Discarded => vec![
            json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"discarded-first","signature":"first-signature"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":8,"content_block":{"type":"thinking","thinking":"retained-last","signature":"last-signature"}}),
            json!({"type":"content_block_stop","index":8}),
            json!({"type":"content_block_start","index":9,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_stop","index":9}),
        ],
        R18ReasoningShape::CloseOrder => vec![
            json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"retained-last","signature":"last-signature"}}),
            json!({"type":"content_block_start","index":8,"content_block":{"type":"thinking","thinking":"discarded-first","signature":"first-signature"}}),
            json!({"type":"content_block_stop","index":8}),
            json!({"type":"content_block_stop","index":1}),
        ],
    };
    frames.splice(4..8, replacement);
    frames
}

async fn r18_reasoning_observation(shape: R18ReasoningShape) {
    use crate::session::storage::StorageAdapter;
    let frames = r18_reasoning_frames(shape);
    let ordinary = r17_unleased_output(frames.clone(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let session = NativeHistorySession::new(&server).await;
    let outcome = r17_run_primary_turn(&session, "r18-reasoning").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { ref tools_called, .. } if tools_called.is_empty())
    );
    r14_flush_native_session(&session).await;
    let events = session.observed.lock().unwrap();
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            xai_grok_sampler::SamplingEvent::Completed { response, .. } => Some(response.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(
        native_response_observation(completed[0]),
        ordinary,
        "R16 Astra-F2: ordinary reasoning output is unchanged for {shape:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Failed { .. }))
    );
    drop(events);
    let live = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let info = session.actor.session_info.clone();
    assert_eq!(
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .bytes,
        live.bytes
    );
    let full = adapter.load_session(&info).await.unwrap();
    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    let record: Value = serde_json::from_slice(
        &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
    )
    .unwrap();
    let source = native_blob(session.directory.path(), &record["source"]);
    let active = native_blob(session.directory.path(), &record["active"]);
    let ledger = native_blob(session.directory.path(), &record["ledger"]);
    assert_eq!(source["responses"].as_array().unwrap().len(), 1);
    assert_eq!(source["responses"][0]["status"], "Accepted");
    let mut reasoning_sources = 0;
    let mut linked_reasoning = 0;
    for block in source["ordered_blocks"].as_array().unwrap() {
        let raw = native_blob(session.directory.path(), &block["block"]);
        if !matches!(raw["type"].as_str(), Some("thinking" | "redacted_thinking")) {
            continue;
        }
        reasoning_sources += 1;
        let retained = raw["thinking"] == "retained-last";
        let coordinate = &block["coordinates"];
        let views: Vec<_> = active["view_links"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|link| &link["source"] == coordinate)
            .collect();
        let accounting: Vec<_> = ledger["accounting"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|link| &link["source"] == coordinate)
            .collect();
        assert_eq!(accounting.len(), 1);
        if retained {
            linked_reasoning += 1;
            assert_eq!(
                views.len(),
                1,
                "the actually committed reasoning must retain one view"
            );
            assert_eq!(views[0]["role"], "Reasoning");
            let item = &full.chat_history[views[0]["view_item"].as_u64().unwrap() as usize];
            assert!(matches!(item, ConversationItem::Reasoning(_)));
            let encoded = serde_json::to_value(item).unwrap().to_string();
            assert!(encoded.contains("retained-last") && !encoded.contains("discarded-first"));
        } else {
            assert!(
                views.is_empty(),
                "R16 Astra-F2: inert or discarded reasoning must be source-only for {shape:?}; raw={raw}"
            );
            assert!(accounting[0]["view_item"].is_null() && accounting[0]["view_digest"].is_null());
            assert_eq!(accounting[0]["role"], "SourceOnly");
        }
    }
    assert_eq!(
        reasoning_sources,
        match shape {
            R18ReasoningShape::Discarded => 3,
            R18ReasoningShape::CloseOrder => 2,
            _ => 1,
        }
    );
    assert_eq!(
        linked_reasoning,
        usize::from(matches!(
            shape,
            R18ReasoningShape::Discarded | R18ReasoningShape::CloseOrder
        ))
    );
    assert_eq!(server.messages_request_count(), 1);
}

#[test]
fn native_history_inert_reasoning_is_source_only() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                for shape in [R18ReasoningShape::Redacted, R18ReasoningShape::Empty] {
                    r18_reasoning_observation(shape).await;
                }
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_history_discarded_reasoning_is_source_only() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                for shape in [R18ReasoningShape::Discarded, R18ReasoningShape::CloseOrder] {
                    r18_reasoning_observation(shape).await;
                }
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

fn r18_terminal_observation(events: &[xai_grok_sampler::SamplingEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            xai_grok_sampler::SamplingEvent::Completed { response, .. } => {
                Some(json!({"completed":native_response_observation(response)}))
            }
            xai_grok_sampler::SamplingEvent::Failed { error, .. } => Some(json!({"failed":error})),
            xai_grok_sampler::SamplingEvent::Retrying { .. } => {
                panic!("ordinary matrix must not retry")
            }
            _ => None,
        })
        .collect()
}

async fn r18_primary_outcome(session: &NativeHistorySession, request_id: &str) -> Value {
    session.actor.attach_non_interactive.set(true);
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("find the account"))
        .await;
    match tokio::time::timeout(
        Duration::from_secs(60),
        session.actor.process_conversation_turn_with_recovery(
            request_id,
            None,
            None,
            None,
            &mut length_salvage::LengthSalvage::new(None),
            &mut Default::default(),
        ),
    )
    .await
    .expect("matrix primary deadline")
    {
        Ok(TurnOutcome::Completed {
            tools_called,
            structured_output,
            stop,
        }) => {
            let stop = match stop {
                CompletedStop::EndTurn => json!("EndTurn"),
                CompletedStop::MaxTokens => json!("MaxTokens"),
                CompletedStop::Refusal(reason) => json!({"Refusal":reason}),
                CompletedStop::ContextWindowExceeded => json!("ContextWindowExceeded"),
            };
            json!({"completed":{"tools":tools_called,"structured_output":structured_output,"stop":stop}})
        }
        Ok(_) => panic!("matrix expected ordinary completion or the base terminal failure"),
        Err(error) => json!({"failed":error}),
    }
}

async fn r18_unleased_terminal(frames: Vec<Value>, pin: Option<&str>) -> Vec<Value> {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let mut config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(0),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    if let Some(pin) = pin {
        config.extra_headers.insert(
            xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER.into(),
            pin.into(),
        );
    }
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        events,
    );
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_config(
            xai_grok_sampler::RequestId::random(),
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
        ),
    )
    .await
    .unwrap();
    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(NATIVE_TIMEOUT, received.recv())
            .await
            .unwrap()
            .unwrap();
        let terminal = matches!(
            event,
            xai_grok_sampler::SamplingEvent::Completed { .. }
                | xai_grok_sampler::SamplingEvent::Failed { .. }
        );
        observed.push(event);
        if terminal {
            break;
        }
    }
    assert_eq!(server.messages_request_count(), 1);
    let terminals = r18_terminal_observation(&observed);
    assert_eq!(terminals.len(), 1);
    assert_eq!(
        result.result.is_ok(),
        terminals[0].get("completed").is_some()
    );
    terminals
}

fn r18_assert_native_links(directory: &std::path::Path, history: &[ConversationItem]) {
    let record: Value = serde_json::from_slice(
        &std::fs::read(directory.join("native_history/current.json")).unwrap(),
    )
    .unwrap();
    let source = native_blob(directory, &record["source"]);
    let active = native_blob(directory, &record["active"]);
    for link in active["view_links"].as_array().unwrap() {
        let block = source["ordered_blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["coordinates"] == link["source"])
            .unwrap();
        let raw = native_blob(directory, &block["block"]);
        let item = &history[link["view_item"].as_u64().unwrap() as usize];
        match raw["type"].as_str().unwrap() {
            "thinking" => {
                assert!(matches!(item, ConversationItem::Reasoning(_)));
                assert_eq!(link["role"], "Reasoning");
            }
            "server_tool_use" | "tool_search_tool_result" => {
                assert!(matches!(item, ConversationItem::Discovery { .. }));
                assert_eq!(link["role"], "Discovery");
            }
            "text" | "tool_use" => assert!(matches!(item, ConversationItem::Assistant(_))),
            kind => panic!("matrix found a false active link for {kind}"),
        }
    }
}

async fn r18_unknown_primary_cell(
    case: &str,
    frames: Vec<Value>,
    expected_error: Option<&str>,
) -> (Value, Vec<Value>, Vec<Value>) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut expected_usage = serde_json::Map::new();
    for frame in &frames {
        let usage = match frame["type"].as_str() {
            Some("message_start") => &frame["message"]["usage"],
            Some("message_delta") => &frame["usage"],
            _ => continue,
        };
        expected_usage.extend(usage.as_object().unwrap().clone());
    }
    let ordinary = r18_unleased_terminal(frames.clone(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(frames));
    let session = NativeHistorySession::new(&server).await;
    let outcome = r18_primary_outcome(&session, case).await;
    assert_eq!(
        outcome.get("failed").is_some(),
        expected_error.is_some(),
        "R19 matrix: primary outcome for {case}"
    );
    loop {
        let notified = session.native_end_processed.notified();
        if session
            .native_ended
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        tokio::time::timeout(NATIVE_TIMEOUT, notified)
            .await
            .unwrap();
    }
    r14_flush_native_session(&session).await;
    {
        let events = session.observed.lock().unwrap();
        assert_eq!(
            r18_terminal_observation(&events),
            ordinary,
            "R19 matrix: ordinary terminal equality for {case}"
        );
    }
    if let Some(error_type) = expected_error {
        let failed = ordinary[0]["failed"]["message"].as_str().unwrap();
        assert!(
            failed.contains(error_type),
            "unchanged ordinary {error_type}: {failed}"
        );
        assert!(!failed.contains("unfaithful_messages_capture"));
        if error_type == "stop_reason_unknown" {
            assert!(failed.contains("future_stop"));
        }
    }
    let directory = session.directory.path();
    let bytes = std::fs::read(directory.join("native_history/current.json")).unwrap_or_default();
    let record: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(
        record["schema_version"], 1,
        "R19: admitted capture error must retain a sealed partial envelope on disk for {case}"
    );
    let source = native_blob(directory, &record["source"]);
    let active = native_blob(directory, &record["active"]);
    let ledger = native_blob(directory, &record["ledger"]);
    let responses = source["responses"].as_array().unwrap();
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0]["status"],
        json!({"PartialTerminal":"Fidelity"})
    );
    assert_eq!(responses[0]["stream_complete"], false);
    assert_eq!(responses[0]["provider_response_id"], "msg_native_history");
    assert!(active["view_links"].as_array().unwrap().is_empty());
    assert_eq!(ledger["usage"].as_array().unwrap().len(), 1);
    assert_eq!(
        ledger["usage"][0]["observed"],
        Value::Object(expected_usage)
    );
    assert_eq!(ledger["usage"][0]["http_dispatches"], 1);
    let mut raw_blocks = Vec::new();
    let mut partials = Vec::new();
    for (ordinal, block) in source["ordered_blocks"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(block["arrival_ordinal"], ordinal);
        raw_blocks.push(native_blob(directory, &block["block"]));
        partials.push(if block["partial"].is_object() {
            native_blob(directory, &block["partial"])
        } else {
            Value::Null
        });
    }
    {
        let events = session.observed.lock().unwrap();
        let attempts: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(position, event)| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: NativeEvidenceEvent::Attempt(evidence),
                    ..
                } => Some((position, evidence)),
                _ => None,
            })
            .collect();
        assert_eq!(
            attempts.len(),
            1,
            "R19: one sealed attempt, never discarded for {case}"
        );
        let (position, evidence) = attempts[0];
        assert!(evidence.admitted);
        assert_eq!(
            evidence.status,
            NativeResponseStatus::PartialTerminal(NativeUnavailable::Fidelity)
        );
        assert!(!evidence.stream_complete);
        assert!(evidence.view_roles.is_empty() && evidence.view_digests.is_empty());
        let terminal = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    xai_grok_sampler::SamplingEvent::Failed { .. }
                        | xai_grok_sampler::SamplingEvent::Completed { .. }
                )
            })
            .unwrap();
        assert!(
            position < terminal,
            "R19: partial evidence precedes the unchanged terminal"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Retrying { .. }))
        );
    }
    let live = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .expect("actor acknowledged the source-only generation");
    assert_eq!(live.bytes.as_ref(), bytes.as_slice());
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        directory.to_path_buf(),
    );
    let info = session.actor.session_info.clone();
    let loaded = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .expect("real adapter reload retains the source-only generation");
    assert_eq!(loaded.bytes, live.bytes);
    let full = adapter.load_session(&info).await.unwrap();
    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    loaded.matches_history(&full.chat_history).unwrap();
    assert_eq!(server.messages_request_count(), 1);
    (source, raw_blocks, partials)
}

async fn r18_pin_primary_cell() {
    use crate::session::storage::StorageAdapter;
    const PIN: &str = "r18-primary-matrix-pin";
    let ordinary = r18_unleased_terminal(native_history_frames(), Some(PIN)).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    let mut config = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    config.extra_headers.insert(
        xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER.into(),
        PIN.into(),
    );
    session
        .actor
        .chat_state_handle
        .update_sampling_config(config);
    assert!(
        r18_primary_outcome(&session, "r18-pin")
            .await
            .get("completed")
            .is_some()
    );
    r14_flush_native_session(&session).await;
    assert_eq!(
        r18_terminal_observation(&session.observed.lock().unwrap()),
        ordinary
    );
    assert_eq!(
        server.requests()[0].header(xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER),
        Some(PIN)
    );
    let live = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let info = session.actor.session_info.clone();
    assert_eq!(
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .bytes,
        live.bytes
    );
    let full = adapter.load_session(&info).await.unwrap();
    let reasoning = full
        .chat_history
        .iter()
        .find_map(|item| match item {
            ConversationItem::Reasoning(reasoning) => Some(reasoning),
            _ => None,
        })
        .unwrap();
    assert_eq!(reasoning.mint_tag.as_deref(), Some(PIN));
    r18_assert_native_links(session.directory.path(), &full.chat_history);
}

async fn r18_first_write_primary_cell() {
    use crate::session::storage::StorageAdapter;
    let ordinary = r18_unleased_terminal(native_history_frames(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    let native = session.directory.path().join("native_history");
    std::fs::create_dir(&native).unwrap();
    std::fs::write(
        native.join("blobs"),
        b"r18 first blob directory obstruction",
    )
    .unwrap();
    assert!(
        r18_primary_outcome(&session, "r18-first-write")
            .await
            .get("completed")
            .is_some()
    );
    r14_flush_native_session(&session).await;
    assert_eq!(
        r18_terminal_observation(&session.observed.lock().unwrap()),
        ordinary
    );
    assert!(
        session
            .actor
            .chat_state_handle
            .read_native_history()
            .await
            .is_err()
    );
    assert!(native.join("transaction.lock").exists());
    assert!(!native.join("authority.json").exists() && !native.join("current.json").exists());
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let info = session.actor.session_info.clone();
    assert!(
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    let full = adapter.load_session(&info).await.unwrap();
    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    assert!(
        full.chat_history
            .iter()
            .any(|item| matches!(item, ConversationItem::Discovery { .. }))
    );
    assert_eq!(server.messages_request_count(), 1);
}

async fn r18_rewrite_primary_cell(rewrite: NativeHistoryRewrite) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    let ordinary = r18_unleased_terminal(native_history_frames(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    for _ in 0..2 {
        server.enqueue_response(
            "/v1/messages",
            native_history_script(native_history_frames()),
        );
    }
    let session = NativeHistorySession::new(&server).await;
    session.actor.chat_state_handle.replace_conversation(vec![
        ConversationItem::system("native-history matrix rewrite"),
        ConversationItem::user("<user_info>matrix fixture</user_info>"),
    ]);
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("earlier prompt retained by rewind"))
        .await;
    session.actor.chat_state_handle.increment_prompt_index();
    assert!(
        r18_primary_outcome(&session, "r18-before-rewrite")
            .await
            .get("completed")
            .is_some()
    );
    session.actor.chat_state_handle.increment_prompt_index();
    assert_eq!(session.actor.chat_state_handle.get_prompt_index().await, 2);
    r14_flush_native_session(&session).await;
    assert_eq!(
        r18_terminal_observation(&session.observed.lock().unwrap()),
        ordinary
    );
    let info = session.actor.session_info.clone();
    let native = session.directory.path().join("native_history");
    let authority = std::fs::read(native.join("authority.json")).unwrap();
    let current = std::fs::read(native.join("current.json")).unwrap();
    let before = session.actor.chat_state_handle.get_conversation().await;
    let checkpoint = session
        .actor
        .chat_state_handle
        .read_native_history()
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    r18_assert_native_links(session.directory.path(), &before);
    let expected = match rewrite {
        NativeHistoryRewrite::Compaction => {
            let items = vec![ConversationItem::user("matrix compacted ordinary summary")];
            session
                .actor
                .chat_state_handle
                .replace_conversation_for_compaction(items.clone());
            items
        }
        NativeHistoryRewrite::Rewind => {
            let sampled_user = before
                .iter()
                .enumerate()
                .filter(|(_, item)| matches!(item, ConversationItem::User(_)))
                .nth(2)
                .unwrap()
                .0;
            let result = session
                .actor
                .handle_rewind(crate::session::RewindRequest {
                    target_prompt_index: 1,
                    force: true,
                    mode: crate::session::RewindMode::ConversationOnly,
                })
                .await
                .unwrap();
            assert!(result.success);
            before[..sampled_user].to_vec()
        }
    };
    r14_flush_native_session(&session).await;
    assert_eq!(
        serde_json::to_value(session.actor.chat_state_handle.get_conversation().await).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        checkpoint.matches_history(&expected),
        Err(NativeUnavailable::StaleHistory)
    );
    assert!(matches!(
        session.actor.chat_state_handle.read_native_history().await,
        Err(NativeUnavailable::StaleHistory)
    ));
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    assert_eq!(
        serde_json::to_value(adapter.load_session(&info).await.unwrap().chat_history).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        serde_json::to_value(
            adapter
                .load_session_without_updates(&info)
                .await
                .unwrap()
                .chat_history
        )
        .unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert!(matches!(
        adapter.read_native_history(&info).await,
        Err(NativeUnavailable::StaleHistory)
    ));
    assert_eq!(
        std::fs::read(native.join("authority.json")).unwrap(),
        authority
    );
    assert_eq!(std::fs::read(native.join("current.json")).unwrap(), current);
    session.observed.lock().unwrap().clear();
    assert!(
        r18_primary_outcome(&session, "r18-after-rewrite")
            .await
            .get("completed")
            .is_some()
    );
    r14_flush_native_session(&session).await;
    assert_eq!(
        r18_terminal_observation(&session.observed.lock().unwrap()),
        ordinary,
        "R18 matrix: a real rewrite must not alter the next ordinary outcome"
    );
    assert_eq!(
        std::fs::read(native.join("authority.json")).unwrap(),
        authority
    );
    assert_eq!(std::fs::read(native.join("current.json")).unwrap(), current);
    assert_eq!(server.messages_request_count(), 2);
}

async fn r18_ordinary_turn_matrix() {
    for (case, position, kind) in [
        ("unknown-block-prefix", 1, "future_block"),
        ("unknown-block-native", 15, "web_search_tool_result"),
    ] {
        let mut frames = native_history_frames();
        frames.splice(position..position, [
            json!({"type":"content_block_start","index":9,"content_block":{"type":kind,"content":[]}}),
            json!({"type":"content_block_stop","index":9}),
        ]);
        r18_unknown_primary_cell(case, frames, None).await;
    }
    let mut frames = native_history_frames();
    frames.insert(1, json!({"type":"future_event","value":"liveness"}));
    r18_unknown_primary_cell("unknown-event", frames, None).await;
    let mut frames = native_history_frames();
    frames.insert(3, json!({"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","url":"https://example.invalid/evidence"}}}));
    r18_unknown_primary_cell("unknown-delta", frames, None).await;
    let mut frames = native_history_frames();
    let terminal = frames.len() - 2;
    frames[terminal]["delta"]["stop_reason"] = json!("future_stop");
    r18_unknown_primary_cell("unknown-stop", frames, Some("stop_reason_unknown")).await;
    r19_malformed_client_input_observation().await;
    r17_length_salvage_observation().await;
    for shape in [
        R18ReasoningShape::Redacted,
        R18ReasoningShape::Empty,
        R18ReasoningShape::Discarded,
        R18ReasoningShape::CloseOrder,
    ] {
        r18_reasoning_observation(shape).await;
    }
    r18_pin_primary_cell().await;
    for rewrite in [
        NativeHistoryRewrite::Compaction,
        NativeHistoryRewrite::Rewind,
    ] {
        r18_rewrite_primary_cell(rewrite).await;
    }
    r18_first_write_primary_cell().await;
}

#[test]
fn native_history_ordinary_turn_matrix() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r18_ordinary_turn_matrix());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn r19_malformed_client_input_observation() {
    let mut frames = native_history_frames()[..15].to_vec();
    frames.extend([
        json!({"type":"content_block_start","index":5,"content_block":{
            "type":"tool_use","id":"toolu_truncated","name":"read_file","input":{}}}),
        json!({"type":"content_block_delta","index":5,"delta":{
            "type":"input_json_delta","partial_json":"{\"path\":\"broken\""}}),
        json!({"type":"content_block_stop","index":5}),
    ]);
    frames.extend(native_history_frames()[15..].iter().cloned());
    let (source, raw, partials) =
        r18_unknown_primary_cell("malformed-client-json", frames, Some("invalid_tool_args")).await;
    let blocks = source["ordered_blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 6);
    assert!(
        blocks[..5]
            .iter()
            .all(|block| block["state"] == "ClosedFaithful")
    );
    assert!(blocks[5]["state"].get("Unavailable").is_some());
    assert_eq!(
        raw[..5],
        [
            json!({"type":"text","text":"before"}),
            json!({"type":"thinking","thinking":"step by step","signature":"signed"}),
            json!({"type":"server_tool_use","id":"srvtoolu_native_history",
            "name":"tool_search_tool_regex","input":{"query":"account"}}),
            native_history_frames()[11]["content_block"].clone(),
            json!({"type":"text","text":"after"}),
        ]
    );
    assert_eq!(
        raw[5],
        json!({"type":"tool_use","id":"toolu_truncated","name":"read_file","input":{}})
    );
    assert!(partials[..5].iter().all(Value::is_null));
    assert_eq!(partials[5], "{\"path\":\"broken\"");
}

#[test]
fn native_history_malformed_tool_input_retains_partial_after_reload() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new()
                .block_on(&runtime, r19_malformed_client_input_observation());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Clone, Copy)]
enum R20CapacityPayload {
    Text,
    ToolInput,
}

fn r20_capacity_value(kind: R20CapacityPayload, index: u32, text: Option<&str>) -> Value {
    match kind {
        R20CapacityPayload::Text => json!({"type":"text","text":text.unwrap_or("")}),
        R20CapacityPayload::ToolInput => json!({"type":"server_tool_use",
            "id":format!("srvtoolu_r20_{index}"),"name":"tool_search_tool_regex",
            "input":text.map_or_else(|| json!({}), |text| json!({"query":text}))}),
    }
}

fn r20_capacity_frames(blocks: u32, kind: R20CapacityPayload) -> Vec<Value> {
    let mut frames = native_history_frames();
    let terminal = frames.split_off(frames.len() - 2);
    for index in 5..5 + blocks {
        let text = "\\".repeat(100_000);
        let delta = match kind {
            R20CapacityPayload::Text => json!({"type":"text_delta","text":text}),
            R20CapacityPayload::ToolInput => json!({"type":"input_json_delta",
                "partial_json":json!({"query":text}).to_string()}),
        };
        frames.extend([
            json!({"type":"content_block_start","index":index,
                "content_block":r20_capacity_value(kind, index, None)}),
            json!({"type":"content_block_delta","index":index,"delta":delta}),
            json!({"type":"content_block_stop","index":index}),
        ]);
    }
    frames.extend(terminal);
    frames
}

async fn r20_capacity_observation() {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    for (kind, count, fits) in [
        (R20CapacityPayload::Text, 2, true),
        (R20CapacityPayload::Text, 3, false),
        (R20CapacityPayload::ToolInput, 2, true),
        (R20CapacityPayload::ToolInput, 3, false),
    ] {
        let frames = r20_capacity_frames(count, kind);
        let ordinary = r17_unleased_output(frames.clone(), None).await;
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response("/v1/messages", native_history_script(frames));
        let session = NativeHistorySession::new(&server).await;
        let outcome = r17_run_primary_turn(&session, "r20-capacity").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { ref tools_called, .. } if tools_called.is_empty())
        );
        r14_flush_native_session(&session).await;
        assert_eq!(server.messages_request_count(), 1);
        let events = session.observed.lock().unwrap();
        let completed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::Completed { response, .. } => Some(response),
                _ => None,
            })
            .collect();
        assert_eq!(completed.len(), 1);
        assert_eq!(native_response_observation(completed[0]), ordinary);
        assert!(!events.iter().any(|event| matches!(
            event,
            xai_grok_sampler::SamplingEvent::Failed { .. }
                | xai_grok_sampler::SamplingEvent::Retrying { .. }
        )));
        let attempts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.clone()),
                _ => None,
            })
            .collect();
        drop(events);
        assert_eq!(attempts.len(), 1);
        let attempt = &attempts[0];
        assert!(
            serde_json::to_vec(attempt.as_ref()).unwrap().len() as u64 <= NATIVE_RESPONSE_BYTES,
            "R20 capacity: sealed envelope must fit the actor response ceiling before handoff"
        );
        let live = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            live.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R20 capacity: admitted fitting or partial evidence must reach the acknowledged checkpoint; error={:?}",
            live.as_ref().err()
        );
        let checkpoint = live.unwrap().checkpoint.unwrap();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let info = session.actor.session_info.clone();
        let disk = adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(disk.bytes, checkpoint.bytes);
        let full = adapter.load_session(&info).await.unwrap();
        let light = adapter.load_session_without_updates(&info).await.unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        let record: Value = serde_json::from_slice(
            &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
        )
        .unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let active = native_blob(session.directory.path(), &record["active"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        assert_eq!(source["responses"].as_array().unwrap().len(), 1);
        assert_eq!(ledger["usage"].as_array().unwrap().len(), 1);
        assert_eq!(ledger["usage"][0]["http_dispatches"], 1);
        assert_eq!(ledger["usage"][0]["observed"]["input_tokens"], 11);
        assert_eq!(ledger["usage"][0]["observed"]["output_tokens"], 7);
        assert_eq!(ledger["usage"][0]["observed"]["cache_read_input_tokens"], 4);
        let retained = source["ordered_blocks"].as_array().unwrap();
        assert_eq!(retained.len(), 5 + count as usize);
        let mut stored_bytes = serde_json::to_vec(&source["responses"][0]).unwrap().len();
        for (index, block) in retained.iter().enumerate() {
            let raw = native_blob(session.directory.path(), &block["block"]);
            let bytes = block["block"]["byte_len"].as_u64().unwrap()
                + serde_json::to_vec(block).unwrap().len() as u64;
            assert!(bytes <= NATIVE_BLOCK_BYTES);
            stored_bytes += bytes as usize;
            if index < 7 || fits {
                assert_eq!(block["state"], "ClosedFaithful");
                if index >= 5 {
                    assert_eq!(
                        raw,
                        r20_capacity_value(kind, index as u32, Some(&"\\".repeat(100_000)))
                    );
                }
            } else {
                assert_eq!(block["state"], json!({"Unavailable":"Capacity"}));
                assert_eq!(raw, r20_capacity_value(kind, index as u32, None));
            }
        }
        assert!(stored_bytes as u64 <= NATIVE_RESPONSE_BYTES);
        if fits {
            assert_eq!(source["responses"][0]["status"], "Accepted");
            assert_eq!(source["responses"][0]["stream_complete"], true);
        } else {
            assert_eq!(
                source["responses"][0]["status"],
                json!({"PartialTerminal":"Capacity"})
            );
            assert_eq!(source["responses"][0]["stream_complete"], false);
            assert!(active["view_links"].as_array().unwrap().is_empty());
            let terminal = ledger["lifecycle"]["terminal"].as_object().unwrap();
            assert_eq!(terminal.len(), 1);
            let marker = terminal.values().next().unwrap();
            assert_eq!(marker["outcome"], "Capacity");
            assert_eq!(marker["failed_block"], 7);
            assert!(marker["observed_bytes"].as_u64().unwrap() > 600_000);
        }
    }
}

#[test]
fn native_history_capacity_retains_bounded_partial_after_reload() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r20_capacity_observation());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn r20a_unleased_messages_reminder() {
    use crate::session::storage::StorageAdapter;
    let ordinary = r17_unleased_output(r17_length_frames(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(r17_length_frames()));
    server.enqueue_response(
        "/v1/messages",
        native_history_script(native_history_frames()),
    );
    let session = NativeHistorySession::new(&server).await;
    *session.actor.agent.borrow_mut() = test_grok_build_agent_with_todo().await;
    session
        .actor
        .workspace_ops
        .bind_local_session(
            &session.actor.session_id_string(),
            session.actor.tool_context.cwd.as_path().to_path_buf(),
            session.actor.tool_context.hunk_tracker_handle.clone(),
            session.actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .unwrap();
    let held = session
        .actor
        .chat_state_handle
        .begin_native_capture(
            session.actor.session_id_string(),
            "r20a-reserved-capacity".into(),
            0,
        )
        .await
        .unwrap();
    let outcome = r17_run_primary_turn(&session, "r20a-unleased-length").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { ref tools_called, .. } if tools_called == &["todo_write"])
    );
    r14_flush_native_session(&session).await;
    assert_eq!(server.messages_request_count(), 2);
    let events = session.observed.lock().unwrap();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { .. }
        )),
        "R20a preservation: both ordinary Messages samples are unleased"
    );
    let responses: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            xai_grok_sampler::SamplingEvent::Completed { response, .. } => Some(response.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(native_response_observation(&responses[0]), ordinary);
    drop(events);
    assert!(
        session
            .actor
            .chat_state_handle
            .read_native_history()
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let full = adapter
        .load_session(&session.actor.session_info)
        .await
        .unwrap();
    let light = adapter
        .load_session_without_updates(&session.actor.session_info)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    let history = &full.chat_history;
    let call = history
        .iter()
        .position(|item| {
            matches!(item, ConversationItem::Assistant(a)
        if a.tool_calls.iter().any(|tool| tool.id.as_ref() == "toolu_r17_length"))
        })
        .unwrap();
    let result = history
        .iter()
        .position(|item| {
            matches!(item, ConversationItem::ToolResult(tool)
        if tool.tool_call_id == "toolu_r17_length")
        })
        .unwrap();
    let reminders: Vec<_> = history
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.text_content()
                .contains(sampler_turn::OUTPUT_TOKEN_LIMIT_REMINDER)
        })
        .collect();
    assert_eq!(reminders.len(), 1);
    let reminder = reminders[0].0;
    assert!(
        call < result && result < reminder,
        "R20a preservation: response commit then tool result then Length reminder"
    );
    let first_start = call + 1 - responses[0].items.len();
    assert_eq!(
        serde_json::to_value(&history[first_start..=call]).unwrap(),
        serde_json::to_value(&responses[0].items).unwrap()
    );
    assert_eq!(result, call + 1);
    assert_eq!(reminder, result + 1);
    let second_start = history.len() - responses[1].items.len();
    assert_eq!(
        serde_json::to_value(&history[second_start..]).unwrap(),
        serde_json::to_value(&responses[1].items).unwrap()
    );
    assert!(session.actor.pending_skill_reminders.lock().is_empty());
    let body = server.requests().last().unwrap().body.clone().unwrap();
    let messages = body["messages"].as_array().unwrap();
    let result_message = messages
        .iter()
        .position(|message| {
            message.to_string().contains("toolu_r17_length") && message["role"] == "user"
        })
        .unwrap();
    let reminder_message = messages
        .iter()
        .position(|message| {
            message
                .to_string()
                .contains("exceeded the output token limit")
        })
        .unwrap();
    assert!(
        result_message <= reminder_message,
        "R20a preservation: reminder is sent only after the tool results"
    );
    assert!(
        !session
            .directory
            .path()
            .join("native_history/current.json")
            .exists()
    );
    held.finish();
}

#[test]
fn native_history_unleased_messages_length_reminder_follows_tool_results() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r20a_unleased_messages_reminder());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn r20_metadata_boundaries() {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    for raw in [
        "{\"nested\\\"key\":[null,true,1.25,\"\\\\\\\"\\n\\u0000\",{\"utf8\":\"é\"}]}",
        "{\"duplicate\":\"\",\"duplicate\":\"\\\\\\\"\"}",
    ] {
        let value: Value = serde_json::from_str(raw).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        let wrapped = serde_json::to_vec(&encoded).unwrap();
        let bytes = wrapped.len() as u64;
        assert_eq!(native_encoded_json_string_len(&value, bytes), Ok(bytes));
        assert_eq!(native_json_string_encoded_len(raw, bytes), Ok(bytes));
        assert_eq!(
            native_json_encoded_len(raw, encoded.len() as u64),
            Ok(encoded.len() as u64)
        );
        assert_eq!(
            native_encoded_json_string_len(&value, bytes - 1),
            Err(NativeUnavailable::Capacity)
        );
        assert_eq!(
            native_json_string_encoded_len(raw, bytes - 1),
            Err(NativeUnavailable::Capacity)
        );
    }
    let shrinking = r#"{"duplicate":"discarded","duplicate":"\\\""}"#;
    let normalized: Value = serde_json::from_str(shrinking).unwrap();
    assert_eq!(
        native_json_encoded_len(
            shrinking,
            serde_json::to_vec(&normalized).unwrap().len() as u64
        ),
        Err(NativeUnavailable::Capacity)
    );
    for (padding, fits) in [(2048, true), (100, false)] {
        let mut frames = native_history_frames();
        let terminal = frames.len() - 2;
        let raw = json!({"type":"text","text":"x".repeat(NATIVE_BLOCK_BYTES as usize - padding)});
        assert!(serde_json::to_vec(&raw).unwrap().len() as u64 <= NATIVE_BLOCK_BYTES);
        frames.splice(
            terminal..terminal,
            [
                json!({"type":"content_block_start","index":5,"content_block":raw}),
                json!({"type":"content_block_stop","index":5}),
            ],
        );
        let ordinary = r17_unleased_output(frames.clone(), None).await;
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response("/v1/messages", native_history_script(frames));
        let session = NativeHistorySession::new(&server).await;
        let outcome = r17_run_primary_turn(&session, "r20-metadata").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { ref tools_called, .. } if tools_called.is_empty())
        );
        r14_flush_native_session(&session).await;
        let events = session.observed.lock().unwrap();
        let completed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::Completed { response, .. } => Some(response),
                _ => None,
            })
            .collect();
        assert_eq!(completed.len(), 1);
        assert_eq!(native_response_observation(completed[0]), ordinary);
        drop(events);
        let read = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            read.as_ref().is_ok_and(|value| value.checkpoint.is_some()),
            "R20 metadata: block metadata first refusal retains the acknowledged prefix, not a rejected whole attempt; error={:?}",
            read.as_ref().err()
        );
        let live = read.unwrap().checkpoint.unwrap();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let disk = adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(live.bytes, disk.bytes);
        assert_eq!(server.messages_request_count(), 1);
        let record: Value = serde_json::from_slice(
            &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
        )
        .unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        let blocks = source["ordered_blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), if fits { 6 } else { 5 });
        assert!(
            blocks
                .iter()
                .all(|block| block["state"] == "ClosedFaithful")
        );
        assert_eq!(ledger["usage"][0]["observed"]["output_tokens"], 7);
        assert_eq!(ledger["usage"][0]["http_dispatches"], 1);
        if fits {
            assert_eq!(
                native_blob(session.directory.path(), &blocks[5]["block"]),
                raw
            );
            assert_eq!(source["responses"][0]["status"], "Accepted");
        } else {
            assert_eq!(
                source["responses"][0]["status"],
                json!({"PartialTerminal":"Capacity"})
            );
            let marker = ledger["lifecycle"]["terminal"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(marker["failed_block"], 5);
        }
    }
}

#[test]
fn native_history_capacity_metadata_boundaries_survive_reload() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r20_metadata_boundaries());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn r21_guard_clock<F: std::future::Future>(
    started: std::time::Instant,
    advanced: &std::cell::Cell<Duration>,
    work: F,
) -> F::Output {
    struct ResumeClock;
    impl Drop for ResumeClock {
        fn drop(&mut self) {
            tokio::time::resume();
        }
    }
    tokio::time::pause();
    let resume = ResumeClock;
    let frozen = tokio::time::Instant::now();
    tokio::pin!(work);
    let output = std::future::poll_fn(|context| {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "R21d: growth acknowledgement deadline"
        );
        let result = std::future::Future::poll(work.as_mut(), context);
        if result.is_pending() {
            // Park without advancing Tokio time while persistence workers make progress.
            std::thread::sleep(Duration::from_millis(1));
            context.waker().wake_by_ref();
        }
        result
    })
    .await;
    assert_eq!(tokio::time::Instant::now(), frozen + advanced.get());
    drop(resume);
    output
}

async fn r21_guard_growth<F: std::future::Future>(
    started: std::time::Instant,
    work: F,
) -> F::Output {
    r21_guard_clock(started, &std::cell::Cell::new(Duration::ZERO), work).await
}

async fn r21_guard_retry<F: std::future::Future>(
    session: &NativeHistorySession,
    start: usize,
    started: std::time::Instant,
    work: F,
) -> F::Output {
    let advanced = std::cell::Cell::new(Duration::ZERO);
    let backoffs = async {
        let mut cursor = start;
        let mut retries = 0;
        loop {
            let changed = session.observed_changed.notified();
            let observed = {
                let events = session.observed.lock().unwrap();
                let count = events[cursor..]
                    .iter()
                    .filter(|event| {
                        matches!(event, xai_grok_sampler::SamplingEvent::Retrying { .. })
                    })
                    .count();
                cursor = events.len();
                count
            };
            for _ in 0..observed {
                retries += 1;
                assert!(retries <= 2);
                // FIFO observation of Retrying follows the prior evidence's disk acknowledgement.
                session.actor.chat_state_handle.get_conversation().await;
                let step = Duration::from_secs(5);
                advanced.set(advanced.get() + step);
                tokio::time::advance(step).await;
            }
            changed.await;
        }
    };
    r21_guard_clock(started, &advanced, async {
        tokio::select! {
            result = work => result,
            () = backoffs => unreachable!(),
        }
    })
    .await
}

async fn r21d_grow_active(
    session: &NativeHistorySession,
    server: &MockInferenceServer,
    frames: &[Value],
    started: std::time::Instant,
) -> Arc<xai_chat_state::persistence::native::NativeCheckpoint> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut growth_frames = frames.to_vec();
    let terminal = growth_frames.len() - 2;
    let extra = (5..NATIVE_RESPONSE_BLOCKS as u32).flat_map(|index| {
        [
            json!({"type":"content_block_start","index":index,
                "content_block":{"type":"text","text":"growth"}}),
            json!({"type":"content_block_stop","index":index}),
        ]
    });
    growth_frames.splice(terminal..terminal, extra);
    let ordinary = r17_unleased_output(growth_frames.clone(), None).await;
    let checkpoint = r21_guard_growth(started, async {
        let mut previous: Option<Arc<xai_chat_state::persistence::native::NativeCheckpoint>> = None;
        for ordinal in 1..=NATIVE_SOURCE_RESPONSES {
            let native_turn = session.actor.current_turn_number.get();
            session
                .native_ended
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let start = session.observed.lock().unwrap().len();
            server.enqueue_response("/v1/messages", native_history_script(growth_frames.clone()));
            assert!(
                matches!(r17_run_primary_turn(session, "r21d-growth").await,
                TurnOutcome::Completed { ref tools_called, .. } if tools_called.is_empty())
            );
            let attempts = {
                let events = session.observed.lock().unwrap();
                assert_eq!(
                    r18_terminal_observation(&events[start..]),
                    vec![json!({"completed":ordinary})],
                    "R21d: setup preserves the unleased primary outcome"
                );
                events[start..]
                    .iter()
                    .filter_map(|event| match event {
                        xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                            evidence: NativeEvidenceEvent::Attempt(attempt),
                            ..
                        } => Some(attempt.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            if attempts.is_empty() {
                r14_flush_native_session(session).await;
                let checkpoint = session
                    .actor
                    .chat_state_handle
                    .read_native_history()
                    .await
                    .unwrap()
                    .checkpoint
                    .unwrap();
                let old = previous.expect("R11: capacity refusal follows an acknowledged capture");
                assert_eq!(checkpoint.record.generation, old.record.generation);
                assert_eq!(checkpoint.bytes, old.bytes, "R11: refusal preserves the checkpoint");
                assert_eq!(
                    std::fs::read(session.directory.path().join("native_history/current.json"))
                        .unwrap(),
                    old.bytes.as_ref()
                );
                let request = session.observed.lock().unwrap()[start..]
                    .iter()
                    .find_map(|event| match event {
                        xai_grok_sampler::SamplingEvent::Completed { request_id, .. } => {
                            Some(request_id.as_str().to_owned())
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    session
                        .actor
                        .chat_state_handle
                        .begin_native_primary_capture(
                            checkpoint.record.lineage.id.clone(),
                            request,
                            native_turn,
                            None,
                        )
                        .await
                        .err(),
                    Some(NativeUnavailable::Capacity),
                    "R11: the same primary admission reports Capacity"
                );
                eprintln!(
                    "R11 growth helper=r21d_grow_active admitted={} refused_ordinal={ordinal} reason=Capacity",
                    ordinal - 1
                );
                return checkpoint;
            }
            r21a_native_ack(session).await;
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].status, NativeResponseStatus::Accepted);
            assert_eq!(attempts[0].blocks.len() as u64, NATIVE_RESPONSE_BLOCKS);
            let read = session.actor.chat_state_handle.read_native_history().await;
            assert!(
                read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
                "R21: every admitted primary generation must survive its physical-copy charge; ordinal={ordinal}, error={:?}",
                read.as_ref().err()
            );
            let checkpoint = read.unwrap().checkpoint.unwrap();
            assert_eq!(checkpoint.source.responses.len() as u64, ordinal);
            assert_eq!(checkpoint.ledger.usage.len() as u64, ordinal);
            assert_eq!(
                checkpoint.source.ordered_blocks.len() as u64,
                ordinal * NATIVE_RESPONSE_BLOCKS
            );
            assert_eq!(server.messages_request_count() as u64, ordinal);
            let remaining = NATIVE_ACTIVE_BYTES - checkpoint.record.capacity["active_bytes"];
            let copy = checkpoint.record.active.byte_len;
            // Leave the last admissible five-block capture to the boundary probe.
            if remaining <= copy * 4 {
                eprintln!("R11 growth helper=r21d_grow_active admitted={ordinal} reason=active_threshold");
                return checkpoint;
            }
            previous = Some(checkpoint);
        }
        panic!("R21d: maximal linked responses must reach the active copy boundary");
    })
    .await;
    eprintln!(
        "R21d growth witness=generation elapsed_ms={} generations={} blocks_per_response={}",
        started.elapsed().as_millis(),
        checkpoint.record.generation,
        NATIVE_RESPONSE_BLOCKS
    );
    checkpoint
}

async fn r21_generation_boundary() {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut frames = native_history_frames();
    frames[9]["delta"]["partial_json"] = json!({"query":"\\\"".repeat(32)}).to_string().into();
    let ordinary = r17_unleased_output(frames.clone(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    let session = NativeHistorySession::new(&server).await;
    let growth_started = std::time::Instant::now();
    let grown = r21d_grow_active(&session, &server, &frames, growth_started).await;
    r21_guard_growth(growth_started, async {
        let growth_admitted = grown.source.responses.len() as u64;
        assert_eq!(
            grown.record.generation,
            growth_admitted * 2,
            "R12: each accepted primary seals source and resolution generations"
        );
        let first_boundary = server.messages_request_count() as u64 + 1;
        let growth_refused = first_boundary == growth_admitted + 2;
        assert!(first_boundary == growth_admitted + 1 || growth_refused);
        let mut previous = Some(grown);
        let mut refused = false;
        let mut last_admitted_ordinal = (!growth_refused).then_some(first_boundary - 1);
        for ordinal in first_boundary..=NATIVE_SOURCE_RESPONSES {
            let native_turn = session.actor.current_turn_number.get();
            session
                .native_ended
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let start = session.observed.lock().unwrap().len();
            server.enqueue_response("/v1/messages", native_history_script(frames.clone()));
            let admitted = growth_admitted + ordinal - first_boundary;
            let work = async {
                assert!(
                    matches!(r17_run_primary_turn(&session, "r21-generation").await,
                    TurnOutcome::Completed { ref tools_called, .. } if tools_called.is_empty())
                );
                let attempts = {
                    let events = session.observed.lock().unwrap();
                    assert_eq!(
                        r18_terminal_observation(&events[start..]),
                        vec![json!({"completed":ordinary})],
                        "R21: passive admission preserves the ordinary primary outcome"
                    );
                    events[start..]
                        .iter()
                        .filter_map(|event| match event {
                            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                                evidence: NativeEvidenceEvent::Attempt(attempt),
                                ..
                            } => Some(attempt.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                if !attempts.is_empty() {
                    loop {
                        let notified = session.native_end_processed.notified();
                        if session
                            .native_ended
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            break;
                        }
                        tokio::time::timeout(NATIVE_TIMEOUT, notified)
                            .await
                            .unwrap();
                    }
                }
                r14_flush_native_session(&session).await;
                attempts
            };
            let attempts = work.await;
            let read = session.actor.chat_state_handle.read_native_history().await;
            assert!(
                read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
                "R21: every admitted primary generation must survive its physical-copy charge; ordinal={ordinal}, error={:?}",
                read.as_ref().err()
            );
            let checkpoint = read.unwrap().checkpoint.unwrap();
            let current =
                std::fs::read(session.directory.path().join("native_history/current.json")).unwrap();
            assert_eq!(current.as_slice(), checkpoint.bytes.as_ref());
            if attempts.is_empty() {
                assert!(
                    last_admitted_ordinal == Some(ordinal - 1)
                        || (ordinal == first_boundary && growth_refused),
                    "R12: refusal follows an acknowledged admission or measured growth refusal"
                );
                let old = previous
                    .as_ref()
                    .expect("a real lineage must precede capacity refusal");
                assert_eq!(old.record.generation, admitted * 2);
                assert_eq!(
                    checkpoint.bytes, old.bytes,
                    "R21: refusal keeps every earlier generation"
                );
                let request = session.observed.lock().unwrap()[start..]
                    .iter()
                    .find_map(|event| match event {
                        xai_grok_sampler::SamplingEvent::Completed { request_id, .. } => {
                            Some(request_id.as_str().to_owned())
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    matches!(
                        session
                            .actor
                            .chat_state_handle
                            .begin_native_primary_capture(
                                checkpoint.record.lineage.id.clone(),
                                request.clone(),
                                native_turn,
                                None,
                            )
                            .await,
                        Err(NativeUnavailable::Capacity)
                    ),
                    "R21: the next lease is refused at admission"
                );
                let legacy = session
                    .actor
                    .chat_state_handle
                    .begin_native_capture(
                        checkpoint.record.lineage.id.clone(),
                        request,
                        native_turn,
                    )
                    .await;
                eprintln!(
                    "R13 legacy admission control={:?}",
                    legacy.as_ref().map(|_| "admitted")
                );
                if let Ok(lease) = legacy {
                    lease.finish();
                }
                let source_seal: NativeSearchHistoryV1 = serde_json::from_value(native_blob(
                    session.directory.path(),
                    &serde_json::to_value(checkpoint.record.previous_checkpoint.as_ref().unwrap())
                        .unwrap(),
                ))
                .unwrap();
                assert_eq!(checkpoint.record.generation, source_seal.generation + 1);
                assert_eq!(checkpoint.record.source, source_seal.source);
                let candidates = [
                    (
                        "source",
                        NATIVE_SOURCE_BYTES
                            .saturating_sub(checkpoint.record.capacity["source_bytes"])
                            .saturating_sub(checkpoint.record.source.byte_len),
                    ),
                    (
                        "active",
                        NATIVE_ACTIVE_BYTES
                            .saturating_sub(checkpoint.record.capacity["active_bytes"])
                            .saturating_sub(source_seal.active.byte_len)
                            .saturating_sub(checkpoint.record.active.byte_len),
                    ),
                    (
                        "ledger",
                        NATIVE_LEDGER_BYTES
                            .saturating_sub(checkpoint.record.capacity["ledger_bytes"])
                            .saturating_sub(source_seal.ledger.byte_len)
                            .saturating_sub(checkpoint.record.ledger.byte_len),
                    ),
                    (
                        "control",
                        NATIVE_CONTROL_BYTES
                            .saturating_sub(checkpoint.record.capacity["control_bytes"]),
                    ),
                ];
                let binding = candidates
                    .into_iter()
                    .min_by_key(|(_, bytes)| *bytes)
                    .unwrap();
                assert_eq!(
                    binding.0, "active",
                    "R21: this lineage binds the active partition"
                );
                eprintln!(
                    "R21 binding={} retained_generations={} remaining={} source={} active={} ledger={} control={}",
                    binding.0,
                    checkpoint.record.generation,
                    binding.1,
                    checkpoint.record.capacity["source_bytes"],
                    checkpoint.record.capacity["active_bytes"],
                    checkpoint.record.capacity["ledger_bytes"],
                    checkpoint.record.capacity["control_bytes"]
                );
                let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                    session.directory.path().to_path_buf(),
                );
                let disk = adapter
                    .read_native_history(&session.actor.session_info)
                    .await
                    .unwrap()
                    .checkpoint
                    .unwrap();
                assert_eq!(disk.bytes, checkpoint.bytes);
                let record: Value = serde_json::from_slice(&current).unwrap();
                let source = native_blob(session.directory.path(), &record["source"]);
                let ledger = native_blob(session.directory.path(), &record["ledger"]);
                assert_eq!(
                    source["responses"].as_array().unwrap().len() as u64,
                    admitted
                );
                assert_eq!(
                    ledger["usage"].as_array().unwrap().len() as u64,
                    admitted
                );
                let last = source["responses"].as_array().unwrap().last().unwrap();
                assert_eq!(last["status"], "Accepted");
                assert_eq!(last["stream_complete"], true);
                assert_eq!(last["source_stop_reason"], "end_turn");
                let usage = ledger["usage"].as_array().unwrap().last().unwrap();
                assert_eq!(usage["observed"]["input_tokens"], 11);
                assert_eq!(usage["observed"]["output_tokens"], 7);
                assert_eq!(usage["http_dispatches"], 1);
                let full = adapter
                    .load_session(&session.actor.session_info)
                    .await
                    .unwrap();
                let light = adapter
                    .load_session_without_updates(&session.actor.session_info)
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&full.chat_history).unwrap(),
                    serde_json::to_value(&light.chat_history).unwrap()
                );
                disk.matches_history(&full.chat_history).unwrap();
                assert_eq!(server.messages_request_count() as u64, ordinal);
                eprintln!(
                    "R12 generation boundary growth_admitted={growth_admitted} boundary_admitted={} checkpoint_generation={} reason=Capacity",
                    ordinal - first_boundary,
                    checkpoint.record.generation
                );
                refused = true;
                break;
            }
            assert_eq!(attempts.len(), 1);
            assert_eq!(
                attempts[0].status,
                NativeResponseStatus::Accepted,
                "R21: the last admissible small capture is retained in full"
            );
            assert_eq!(attempts[0].blocks.len(), 5);
            assert_eq!(checkpoint.source.responses.len() as u64, admitted + 1);
            assert_eq!(checkpoint.ledger.usage.len() as u64, admitted + 1);
            assert_eq!(checkpoint.record.generation, (admitted + 1) * 2);
            last_admitted_ordinal = Some(ordinal);
            previous = Some(checkpoint);
        }
        assert!(
            refused,
            "R21: the real contract byte budget stops this lineage at admission"
        );
    })
    .await;
}

#[test]
fn native_history_next_generation_is_funded_before_admission() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r21_generation_boundary());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn r21a_partial_frames(pairs: u32, padding: usize) -> Vec<Value> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    assert!(u64::from(pairs) * 2 <= NATIVE_RESPONSE_BLOCKS);
    let mut frames = vec![native_history_frames()[0].clone()];
    for pair in 0..pairs {
        let id = format!("srvtoolu_r21a_{pair}_{}", "x".repeat(padding));
        let call = json!({"type":"server_tool_use","id":id,"name":"tool_search_tool_regex","input":{"query":"account"}});
        let result = json!({"type":"tool_search_tool_result","tool_use_id":id,
            "content":{"type":"tool_search_tool_search_result","tool_references":[]}});
        for (index, block) in [(pair * 2, call), (pair * 2 + 1, result)] {
            assert!(native_encoded_len(&block, NATIVE_BLOCK_BYTES).is_ok());
            frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
            frames.push(json!({"type":"content_block_stop","index":index}));
        }
    }
    frames.push(
        json!({"type":"message_delta","delta":{},"usage":{"input_tokens":11,"output_tokens":7}}),
    );
    frames.push(json!({"type":"error","error":{"type":"invalid_request_error","message":"native fixture stream failed"}}));
    frames
}

async fn r21a_native_ack(session: &NativeHistorySession) {
    loop {
        let notified = session.native_end_processed.notified();
        if session
            .native_ended
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        tokio::time::timeout(NATIVE_TIMEOUT, notified)
            .await
            .unwrap();
    }
    r14_flush_native_session(session).await;
}

#[derive(Clone, Copy, Debug)]
enum NativeRetryBoundary {
    Fit,
    Refusal,
}

async fn r21a_grow_ledger(
    session: &NativeHistorySession,
    server: &MockInferenceServer,
    boundary: NativeRetryBoundary,
) -> Arc<xai_chat_state::persistence::native::NativeCheckpoint> {
    let started = std::time::Instant::now();
    let checkpoint =
        r21_guard_growth(started, r21a_grow_ledger_inner(session, server, boundary)).await;
    eprintln!(
        "R21d growth witness=terminal elapsed_ms={} generations={} blocks_per_response={} boundary={boundary:?}",
        started.elapsed().as_millis(),
        checkpoint.record.generation,
        xai_grok_sampling_types::conversation::native_search_history::NATIVE_RESPONSE_BLOCKS
    );
    checkpoint
}

async fn r21a_grow_ledger_inner(
    session: &NativeHistorySession,
    server: &MockInferenceServer,
    boundary: NativeRetryBoundary,
) -> Arc<xai_chat_state::persistence::native::NativeCheckpoint> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut config = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    config.max_retries = Some(0);
    session
        .actor
        .chat_state_handle
        .update_sampling_config(config);
    let frames = r21a_partial_frames((NATIVE_RESPONSE_BLOCKS / 2) as u32, 6451);
    for ordinal in 1..=NATIVE_SOURCE_RESPONSES {
        session
            .native_ended
            .store(false, std::sync::atomic::Ordering::SeqCst);
        server.enqueue_response("/v1/messages", native_history_script(frames.clone()));
        let started = std::time::Instant::now();
        assert!(session.sample_outcome().await.is_err());
        r21a_native_ack(session).await;
        let read = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R21a growth: acknowledged generations remain readable; ordinal={ordinal}, error={:?}",
            read.as_ref().err()
        );
        let checkpoint = read.unwrap().checkpoint.unwrap();
        let remaining = NATIVE_LEDGER_BYTES - checkpoint.record.capacity["ledger_bytes"];
        let copy = checkpoint.record.ledger.byte_len;
        assert_eq!(checkpoint.record.generation, ordinal);
        assert_eq!(checkpoint.source.responses.len() as u64, ordinal);
        assert_eq!(
            checkpoint.source.ordered_blocks.len() as u64,
            ordinal * NATIVE_RESPONSE_BLOCKS
        );
        assert_eq!(
            checkpoint.ledger.groups.len() as u64,
            ordinal * NATIVE_RESPONSE_BLOCKS / 2
        );
        assert!(checkpoint.active.view_links.is_empty());
        assert_eq!(server.messages_request_count() as u64, ordinal);
        eprintln!(
            "R21a growth generation={ordinal} ledger={} copy={copy} remaining={remaining} ack_ms={}",
            checkpoint.record.capacity["ledger_bytes"],
            started.elapsed().as_millis()
        );
        let copies = match boundary {
            NativeRetryBoundary::Fit => 4,
            NativeRetryBoundary::Refusal => 3,
        };
        if remaining < copy * copies {
            return checkpoint;
        }
    }
    panic!("R21a: real append growth must reach the ledger copy boundary");
}

async fn r21h_unleased_retry(first: Vec<Value>, second: Vec<Value>) -> Vec<Value> {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/messages", native_history_script(first));
    server.enqueue_response("/v1/messages", native_history_script(second));
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(2),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 2,
            ..Default::default()
        },
        events,
    );
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_config(
            xai_grok_sampler::RequestId::random(),
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
        ),
    )
    .await
    .unwrap();
    assert!(result.result.is_err());
    let mut observed = Vec::new();
    while let Ok(event) = received.try_recv() {
        assert!(!matches!(
            event,
            xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { .. }
        ));
        observed.push(event);
    }
    assert_eq!(server.messages_request_count(), 2);
    r21e_ordinary_events(&observed)
}

async fn r21_terminal_generation_boundary(usage_only: bool, boundary: NativeRetryBoundary) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let started = std::time::Instant::now();
    let server = MockInferenceServer::start().await.unwrap();
    let session = NativeHistorySession::with_retries(&server, 2).await;
    let before = r21a_grow_ledger(&session, &server, boundary).await;
    let mut config = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    config.max_retries = Some(2);
    session
        .actor
        .chat_state_handle
        .update_sampling_config(config);
    let remaining = NATIVE_LEDGER_BYTES - before.record.capacity["ledger_bytes"];
    let copies = before.record.ledger.byte_len;
    match boundary {
        NativeRetryBoundary::Fit => {
            assert_eq!(before.record.generation, 5);
            assert!(
                remaining > copies * 3 && remaining < copies * 4,
                "R21h: FIT funds two generation copies and their worst-case terminal reserve; remaining={remaining}, copies={copies}"
            );
        }
        NativeRetryBoundary::Refusal => {
            assert_eq!(before.record.generation, 6);
            assert!(
                remaining > copies * 2 && remaining - copies * 2 < copies / 32,
                "R21h: REFUSAL funds the first generation and terminal reserve, not the retry copy; remaining={remaining}, copies={copies}"
            );
        }
    }
    let first = r21a_partial_frames(1, 0);
    let second = if usage_only {
        r21a_partial_frames(0, 0)
    } else {
        r21a_partial_frames(1, (copies / 8).min(NATIVE_BLOCK_BYTES / 2) as usize)
    };
    let ordinary = r21h_unleased_retry(first.clone(), second.clone()).await;
    server.enqueue_response("/v1/messages", native_history_script(first));
    server.enqueue_response("/v1/messages", native_history_script(second));
    session
        .native_ended
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let start = session.observed.lock().unwrap().len();
    r21_guard_retry(&session, start, started, async {
        assert!(
            session.sample_outcome().await.is_err(),
            "R21a: the original final stream error is preserved"
        );
        r21a_native_ack(&session).await;
        let attempts = {
            let events = session.observed.lock().unwrap();
            let current = &events[start..];
            assert_eq!(
                r21e_ordinary_events(current),
                ordinary,
                "R21h: copy admission preserves the unleased retry outcome"
            );
            assert_eq!(
                current
                    .iter()
                    .filter(|event| matches!(event, xai_grok_sampler::SamplingEvent::Retrying { .. }))
                    .count(),
                1
            );
            assert_eq!(
                current
                    .iter()
                    .filter(|event| matches!(event, xai_grok_sampler::SamplingEvent::Failed { .. }))
                    .count(),
                1
            );
            assert!(
                !current
                    .iter()
                    .any(|event| matches!(event, xai_grok_sampler::SamplingEvent::Completed { .. }))
            );
            current
                .iter()
                .filter_map(|event| match event {
                    xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                        request_id,
                        lease,
                        evidence: NativeEvidenceEvent::Attempt(attempt),
                    } => Some((request_id.clone(), lease.clone(), attempt.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let retained = match boundary {
            NativeRetryBoundary::Fit => {
                assert_eq!(
                    attempts.len(),
                    2,
                    "R21a: both retry attempts retain their terminal/usage envelope"
                );
                assert_eq!(
                    attempts[0].0, attempts[1].0,
                    "R21a: retries keep the same request"
                );
                assert!(
                    attempts[0].1.same_lease(&attempts[1].1),
                    "R21a: the second generation consumes the same lease"
                );
                assert_eq!((attempts[0].2.attempt, attempts[1].2.attempt), (1, 2));
                assert!(attempts[1].1.charged().unwrap() >= copies);
                assert!(attempts[1].1.charged().unwrap() <= attempts[1].1.ceiling().unwrap());
                2
            }
            NativeRetryBoundary::Refusal => {
                assert_eq!(
                    attempts.len(),
                    1,
                    "R21h: refusal emits no envelope for the unfunded retry"
                );
                assert_eq!(attempts[0].2.attempt, 1);
                assert_eq!(
                    attempts[0].1.reserve(0),
                    Err(NativeUnavailable::Capacity),
                    "R21h: the refused retry reports capacity unavailability"
                );
                1
            }
        };
        for (_, _, attempt) in &attempts {
            assert!(attempt.admitted);
            assert!(!attempt.stream_complete);
            assert!(matches!(
                attempt.status,
                NativeResponseStatus::PartialTerminal(_)
            ));
            assert_eq!(attempt.http_dispatches, 1);
            assert_eq!(attempt.usage["input_tokens"], 11);
            assert_eq!(attempt.usage["output_tokens"], 7);
        }
        if usage_only && matches!(boundary, NativeRetryBoundary::Fit) {
            assert!(attempts[1].2.blocks.is_empty());
        }
        let current = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            current.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R21a: terminal/usage generation must seal within its copy reservation; remaining={remaining}, copies={copies}, error={:?}",
            current.as_ref().err()
        );
        let checkpoint = current.unwrap().checkpoint.unwrap();
        assert_eq!(
            checkpoint.record.generation,
            before.record.generation + retained as u64
        );
        assert_eq!(
            checkpoint.ledger.usage.len(),
            before.ledger.usage.len() + retained
        );
        assert_eq!(
            checkpoint.source.responses.len(),
            before.source.responses.len() + retained
        );
        assert!(
            checkpoint
                .source
                .ordered_blocks
                .starts_with(&before.source.ordered_blocks)
        );
        assert!(
            checkpoint
                .source
                .responses
                .starts_with(&before.source.responses)
        );
        assert!(checkpoint.ledger.usage.starts_with(&before.ledger.usage));
        assert_eq!(checkpoint.active, before.active);
        assert_eq!(
            server.messages_request_count() as u64,
            before.record.generation + 2
        );
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let disk = adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(disk.bytes, checkpoint.bytes);
        let record: Value = serde_json::from_slice(
            &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
        )
        .unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        let responses = source["responses"].as_array().unwrap();
        let usage = ledger["usage"].as_array().unwrap();
        assert_eq!(responses.len(), before.source.responses.len() + retained);
        assert_eq!(usage.len(), before.ledger.usage.len() + retained);
        for (index, (_, lease, attempt)) in attempts.iter().enumerate() {
            let offset = before.source.responses.len() + index;
            assert_eq!(responses[offset]["lease"], lease.ordinal());
            assert_eq!(responses[offset]["attempt"], attempt.attempt);
            assert!(responses[offset]["status"].get("PartialTerminal").is_some());
            assert_eq!(
                usage[offset]["observed"],
                serde_json::to_value(&attempt.usage).unwrap()
            );
            assert_eq!(usage[offset]["http_dispatches"], 1);
            assert!(
                ledger["lifecycle"]["terminal"]
                    .get(format!("{}:{}", lease.request(), attempt.attempt))
                    .is_some()
            );
        }
        let full = adapter
            .load_session(&session.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&session.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        disk.matches_history(&full.chat_history).unwrap();
        eprintln!(
            "R21h terminal binding=ledger boundary={boundary:?} usage_only={usage_only} before={} after={} remaining={remaining} copies={copies} alone_ms={}",
            before.record.generation,
            checkpoint.record.generation,
            started.elapsed().as_millis()
        );
    })
    .await;
}

#[test]
#[serial_test::serial]
fn native_history_terminal_generation_copies_are_reserved() {
    let _retry_env = xai_grok_test_support::EnvGuard::unset("GROK_MAX_RETRIES");
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                r21_terminal_generation_boundary(true, NativeRetryBoundary::Fit).await;
                r21_terminal_generation_boundary(false, NativeRetryBoundary::Fit).await;
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[serial_test::serial]
fn native_history_terminal_generation_copy_refusal_preserves_retry() {
    let _retry_env = xai_grok_test_support::EnvGuard::unset("GROK_MAX_RETRIES");
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                r21_terminal_generation_boundary(true, NativeRetryBoundary::Refusal).await;
                r21_terminal_generation_boundary(false, NativeRetryBoundary::Refusal).await;
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

fn r21e_ordinary_events(events: &[xai_grok_sampler::SamplingEvent]) -> Vec<Value> {
    events.iter().filter_map(|event| match event {
        xai_grok_sampler::SamplingEvent::Completed { response, .. } =>
            Some(json!({"completed":native_response_observation(response)})),
        xai_grok_sampler::SamplingEvent::Failed { error, .. } => Some(json!({"failed":error})),
        xai_grok_sampler::SamplingEvent::Retrying { attempt, max_retries, kind, reason, .. } =>
            Some(json!({"retry":{"attempt":attempt,"max_retries":max_retries,"kind":kind,"reason":reason}})),
        _ => None,
    }).collect()
}

fn r21e_enqueue_boundary(server: &MockInferenceServer, init_failure: bool) {
    for pairs in [1, 0] {
        server.enqueue_response(
            "/v1/messages",
            native_history_script(r21a_partial_frames(pairs, 0)),
        );
    }
    server.enqueue_response("/v1/messages", if init_failure {
        ScriptedResponse::json(503, json!({"error":{"type":"server_error","message":"native fixture initialization failed"}}))
    } else {
        native_history_script(native_history_frames())
    });
}

async fn r21e_unleased_retry(init_failure: bool) -> Vec<Value> {
    let server = MockInferenceServer::start().await.unwrap();
    r21e_enqueue_boundary(&server, init_failure);
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_retries: Some(3),
        max_completion_tokens: Some(4096),
        ..Default::default()
    };
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let sampler = xai_grok_sampler::SamplerActor::spawn(
        config.clone(),
        xai_grok_sampler::RetryPolicy {
            max_retries: 3,
            ..Default::default()
        },
        events,
    );
    let result = tokio::time::timeout(
        NATIVE_TIMEOUT,
        sampler.submit_and_collect_with_config(
            xai_grok_sampler::RequestId::random(),
            xai_grok_sampling_types::ConversationRequest {
                items: vec![ConversationItem::user("find the account")],
                ..Default::default()
            },
            config,
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.result.is_err(), init_failure);
    let mut observed = Vec::new();
    while let Ok(event) = received.try_recv() {
        observed.push(event);
    }
    assert_eq!(server.messages_request_count(), 3);
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, xai_grok_sampler::SamplingEvent::Retrying { .. }))
            .count(),
        2
    );
    r21e_ordinary_events(&observed)
}

async fn r21e_unfunded_retry_boundary(init_failure: bool) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let started = std::time::Instant::now();
    let ordinary = r21e_unleased_retry(init_failure).await;
    let server = MockInferenceServer::start().await.unwrap();
    let session = NativeHistorySession::with_retries(&server, 3).await;
    let before = r21a_grow_ledger(&session, &server, NativeRetryBoundary::Fit).await;
    let remaining = NATIVE_LEDGER_BYTES - before.record.capacity["ledger_bytes"];
    let copy = before.record.ledger.byte_len;
    assert_eq!(before.record.generation, 5);
    assert!(
        remaining > copy * 3 && remaining < copy * 4,
        "R21h: the real ledger boundary funds two generations and terminal reserve but not a third generation"
    );
    let mut config = session
        .actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .unwrap();
    config.max_retries = Some(3);
    session
        .actor
        .chat_state_handle
        .update_sampling_config(config);
    r21e_enqueue_boundary(&server, init_failure);
    session
        .native_ended
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let start = session.observed.lock().unwrap().len();
    r21_guard_retry(&session, start, started, async {
        let result = session.sample_outcome().await;
        assert_eq!(result.is_err(), init_failure);
        r21a_native_ack(&session).await;
        let events = session.observed.lock().unwrap()[start..].to_vec();
        assert_eq!(
            r21e_ordinary_events(&events),
            ordinary,
            "R21e: capture exhaustion preserves the ordinary retry classification, terminal and output"
        );
        assert_eq!(
            server.messages_request_count() as u64,
            before.record.generation + 3
        );
        let read = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R21e: unfunded retry must not append Capacity or invalidate prior acknowledged history; error={:?}",
            read.as_ref().err()
        );
        let checkpoint = read.unwrap().checkpoint.unwrap();
        assert_eq!(
            checkpoint.record.generation,
            before.record.generation + 2,
            "R21e: the third ordinary attempt creates no native generation"
        );
        let attempts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    lease,
                    evidence: NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some((lease, attempt)),
                _ => None,
            })
            .collect();
        assert_eq!(
            attempts.len(),
            2,
            "R21e: no unfunded envelope enters native persistence"
        );
        assert!(attempts[0].0.same_lease(attempts[1].0));
        assert_eq!((attempts[0].1.attempt, attempts[1].1.attempt), (1, 2));
        assert_eq!(
            attempts[1].0.reserve(0),
            Err(NativeUnavailable::Capacity),
            "R21e: further capture reports its bounded capacity reason"
        );
        assert_eq!(
            checkpoint.source.responses.len(),
            before.source.responses.len() + 2
        );
        assert!(
            checkpoint
                .source
                .responses
                .starts_with(&before.source.responses)
        );
        assert_eq!(checkpoint.ledger.usage.len(), before.ledger.usage.len() + 2);
        assert!(checkpoint.ledger.usage.starts_with(&before.ledger.usage));
        assert_eq!(checkpoint.active, before.active);
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let disk = adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(disk.bytes, checkpoint.bytes);
        let record: Value = serde_json::from_slice(
            &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
        )
        .unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        for (index, (lease, attempt)) in attempts.iter().enumerate() {
            let offset = before.source.responses.len() + index;
            assert_eq!(source["responses"][offset]["attempt"], attempt.attempt);
            assert!(
                source["responses"][offset]["status"]
                    .get("PartialTerminal")
                    .is_some()
            );
            assert_eq!(
                ledger["usage"][offset]["observed"],
                serde_json::to_value(&attempt.usage).unwrap()
            );
            assert_eq!(ledger["usage"][offset]["http_dispatches"], 1);
            assert!(
                ledger["lifecycle"]["terminal"]
                    .get(format!("{}:{}", lease.request(), attempt.attempt))
                    .is_some()
            );
        }
        let full = adapter
            .load_session(&session.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&session.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        disk.matches_history(&full.chat_history).unwrap();
        eprintln!(
            "R21e boundary binding=ledger init_failure={init_failure} before={} after={} remaining={} copy={}",
            before.record.generation, checkpoint.record.generation, remaining, copy
        );
    })
    .await;
}

#[test]
#[serial_test::serial]
fn native_history_unfunded_retry_preserves_acknowledged_generation() {
    let _retry_env = xai_grok_test_support::EnvGuard::unset("GROK_MAX_RETRIES");
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                r21e_unfunded_retry_boundary(false).await;
                r21e_unfunded_retry_boundary(true).await;
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

fn r21f_mixed_frames() -> Vec<Value> {
    let mut frames = native_history_frames();
    let terminal = frames.len() - 2;
    frames[terminal]["delta"]["stop_reason"] = json!("tool_use");
    frames.splice(
        terminal..terminal,
        [
            json!({"type":"content_block_start","index":5,"content_block":{
            "type":"tool_use","id":"toolu_r21f","name":"read_file","input":{"path":"account"}}}),
            json!({"type":"content_block_stop","index":5}),
        ],
    );
    frames
}

async fn r21f_mixed_boundary() {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let frames = r21f_mixed_frames();
    let ordinary = r17_unleased_output(frames.clone(), None).await;
    let growth_ordinary = r17_unleased_output(native_history_frames(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    let session = NativeHistorySession::new(&server).await;
    let started = std::time::Instant::now();
    let grown = r21d_grow_active(&session, &server, &native_history_frames(), started).await;
    let before = r21_guard_growth(started, async {
        let mut checkpoint = grown;
        for ordinal in 1..=NATIVE_SOURCE_RESPONSES {
            let native_turn = session.actor.current_turn_number.get();
            let remaining = NATIVE_ACTIVE_BYTES - checkpoint.record.capacity["active_bytes"];
            if remaining <= checkpoint.record.active.byte_len * 2 {
                eprintln!(
                    "R11 growth helper=r21f admitted_total={} admitted_stage={} reason=active_threshold",
                    checkpoint.source.responses.len(),
                    ordinal - 1
                );
                return checkpoint;
            }
            session
                .native_ended
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let start = session.observed.lock().unwrap().len();
            server.enqueue_response(
                "/v1/messages",
                native_history_script(native_history_frames()),
            );
            session.sample().await;
            let events = session.observed.lock().unwrap()[start..].to_vec();
            assert_eq!(
                r18_terminal_observation(&events),
                vec![json!({"completed":growth_ordinary})],
                "R11: boundary growth preserves the unleased ordinary outcome"
            );
            let evidence = events
                .iter()
                .any(|event| {
                    matches!(
                        event,
                        xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                            evidence: NativeEvidenceEvent::Attempt(_),
                            ..
                        }
                    )
                });
            if evidence {
                r21a_native_ack(&session).await;
            }
            let next = session
                .actor
                .chat_state_handle
                .read_native_history()
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            if !evidence {
                assert_eq!(
                    next.bytes, checkpoint.bytes,
                    "R21f: setup admission refusal keeps its completed lineage"
                );
                assert_eq!(next.record.generation, checkpoint.record.generation);
                assert_eq!(
                    std::fs::read(session.directory.path().join("native_history/current.json"))
                        .unwrap(),
                    checkpoint.bytes.as_ref()
                );
                let request = events
                    .iter()
                    .find_map(|event| match event {
                        xai_grok_sampler::SamplingEvent::Completed { request_id, .. } => {
                            Some(request_id.as_str().to_owned())
                        }
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    session
                        .actor
                        .chat_state_handle
                        .begin_native_capture(
                            checkpoint.record.lineage.id.clone(),
                            request,
                            native_turn,
                        )
                        .await
                        .err(),
                    Some(NativeUnavailable::Capacity),
                    "R11: the same boundary admission reports Capacity"
                );
                eprintln!(
                    "R11 growth helper=r21f admitted_total={} admitted_stage={} refused_ordinal={ordinal} reason=Capacity",
                    checkpoint.source.responses.len(),
                    ordinal - 1
                );
                return checkpoint;
            }
            assert_eq!(next.record.generation, checkpoint.record.generation + 1);
            assert_eq!(
                next.source.responses.last().unwrap().status,
                NativeResponseStatus::Accepted
            );
            checkpoint = next;
        }
        panic!("R21f: completed lineage must reach the terminal-copy boundary");
    })
    .await;
    assert!(before.ledger.lifecycle.waiting_clients.is_empty());
    assert!(
        before
            .ledger
            .lifecycle
            .exchanges
            .values()
            .all(|state| state == "Terminal")
    );
    assert!(
        before
            .ledger
            .groups
            .iter()
            .all(|group| group.state == "Completed")
    );
    eprintln!(
        "R21f growth elapsed_ms={} generations={} active={} copy={}",
        started.elapsed().as_millis(),
        before.record.generation,
        before.record.capacity["active_bytes"],
        before.record.active.byte_len
    );
    r21_guard_growth(started, async {
        let disk_before =
            std::fs::read(session.directory.path().join("native_history/current.json")).unwrap();
        session
            .native_ended
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let start = session.observed.lock().unwrap().len();
        server.enqueue_response("/v1/messages", native_history_script(frames));
        session.sample().await;
        let events = session.observed.lock().unwrap()[start..].to_vec();
        assert_eq!(
            r18_terminal_observation(&events),
            vec![json!({"completed":ordinary})],
            "R21f: native capacity does not change the ordinary mixed handoff"
        );
        let attempts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    lease,
                    evidence: NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some((lease, attempt)),
                _ => None,
            })
            .collect();
        if !attempts.is_empty() {
            r21a_native_ack(&session).await;
        }
        let read = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R21f: mixed handoff must not append Capacity inside an issued lease; error={:?}",
            read.as_ref().err()
        );
        let checkpoint = read.unwrap().checkpoint.unwrap();
        if attempts.is_empty() {
            assert_eq!(
                checkpoint.bytes, before.bytes,
                "R21f: admission refusal retains every acknowledged generation"
            );
            assert_eq!(
                std::fs::read(session.directory.path().join("native_history/current.json"))
                    .unwrap(),
                disk_before
            );
            assert!(
                matches!(
                    session
                        .actor
                        .chat_state_handle
                        .begin_native_capture(
                            checkpoint.record.lineage.id.clone(),
                            "r21f-refused".into(),
                            before.record.generation + 1
                        )
                        .await,
                    Err(NativeUnavailable::Capacity)
                ),
                "R21f: refusal occurs at admission"
            );
        } else {
            assert_eq!(attempts.len(), 1);
            let (lease, attempt) = attempts[0];
            assert_eq!(
                attempt.status,
                NativeResponseStatus::Accepted,
                "R21f: an admitted mixed handoff is retained completely"
            );
            assert!(attempt.stream_complete);
            assert_eq!(attempt.blocks.len(), 6);
            assert_eq!(checkpoint.record.generation, before.record.generation + 1);
            assert_eq!(
                checkpoint.ledger.lifecycle.exchanges[lease.request()],
                "LiveBound"
            );
            assert!(
                checkpoint
                    .ledger
                    .lifecycle
                    .waiting_clients
                    .iter()
                    .any(|waiting| waiting.client_id == "toolu_r21f"
                        && waiting.delivery == "Waiting")
            );
            assert!(checkpoint.record.capacity["terminal_active_bytes"] > 0);
            assert!(checkpoint.record.capacity["terminal_ledger_bytes"] > 0);
        }
        assert!(
            checkpoint
                .source
                .responses
                .starts_with(&before.source.responses)
        );
        assert!(checkpoint.ledger.usage.starts_with(&before.ledger.usage));
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let disk = adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(disk.bytes, checkpoint.bytes);
        let record: Value = serde_json::from_slice(
            &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
        )
        .unwrap();
        let source = native_blob(session.directory.path(), &record["source"]);
        let ledger = native_blob(session.directory.path(), &record["ledger"]);
        assert_eq!(
            source["responses"].as_array().unwrap().len(),
            before.source.responses.len() + attempts.len()
        );
        assert_eq!(
            ledger["usage"].as_array().unwrap().len(),
            before.ledger.usage.len() + attempts.len()
        );
        if let Some((lease, attempt)) = attempts.first() {
            assert_eq!(
                source["responses"].as_array().unwrap().last().unwrap()["status"],
                "Accepted"
            );
            assert_eq!(
                ledger["lifecycle"]["exchanges"][lease.request()],
                "LiveBound"
            );
            assert_eq!(
                ledger["lifecycle"]["waiting_clients"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["client_id"],
                "toolu_r21f"
            );
            assert_eq!(
                ledger["lifecycle"]["waiting_clients"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["delivery"],
                "Waiting"
            );
            assert_eq!(
                ledger["usage"].as_array().unwrap().last().unwrap()["observed"],
                serde_json::to_value(&attempt.usage).unwrap()
            );
        }
        let full = adapter
            .load_session(&session.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&session.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        disk.matches_history(&full.chat_history).unwrap();
        eprintln!(
            "R21f boundary binding=active retained={} admitted={} alone_ms={}",
            before.record.generation,
            attempts.len(),
            started.elapsed().as_millis()
        );
    })
    .await;
}

#[test]
fn native_history_mixed_handoff_funds_terminal_copies() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, r21f_mixed_boundary());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_history_diverged_request_rejects_append_ack() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::StorageAdapter;
        use xai_grok_sampling_types::conversation::native_search_history::*;
        use xai_grok_test_support::{InferenceEndpoint, InferenceRequestMatcher};
        let ordinary = r17_unleased_output(native_history_frames(), None).await;
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response("/v1/messages", native_history_script(native_history_frames()));
        let session = NativeHistorySession::new(&server).await;
        session.sample().await;
        r21a_native_ack(&session).await;
        let before = session.actor.chat_state_handle.read_native_history().await.unwrap().checkpoint.unwrap();
        let directory = session.directory.path().join("native_history");
        let authority = std::fs::read(directory.join("authority.json")).unwrap();
        let current = std::fs::read(directory.join("current.json")).unwrap();
        session.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("second request")).await;
        let (mut request, binding) = session.actor.chat_state_handle.build_request_with_binding(
            xai_chat_state::NativeToolCandidates::builtins_only(Vec::new()), None, false, None,
            session.actor.session_id_string(), "r21f-divergence".into()).await.unwrap();
        request.x_grok_turn_idx = Some(session.actor.current_turn_number.get().to_string());
        let request_id = xai_grok_sampler::RequestId::random();
        let lease = session.actor.chat_state_handle.begin_native_capture(
            before.record.lineage.id.clone(), request_id.as_str().into(), 2).await.unwrap();
        let config = session.actor.turn_sampler_config_from_binding(&binding).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sampler = xai_grok_sampler::SamplerActor::spawn(config.clone(),
            xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, tx);
        let mut blocked = server.expect_response_blocked("r21f-divergence",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Messages),
            native_history_script(native_history_frames()));
        let submitted_lease = lease.clone();
        let submitted_id = request_id.clone();
        let running = tokio::task::spawn_local(async move {
            sampler.submit_and_collect_with_native_capture(submitted_id, request, config, submitted_lease).await
        });
        tokio::time::timeout(NATIVE_TIMEOUT, blocked.wait_blocked()).await.unwrap();
        session.actor.chat_state_handle.replace_conversation(vec![ConversationItem::user("changed while request is in flight")]);
        let changed = session.actor.chat_state_handle.get_conversation().await;
        assert_eq!(before.matches_history(&changed), Err(NativeUnavailable::StaleHistory));
        blocked.release();
        let result = tokio::time::timeout(NATIVE_TIMEOUT, running).await.unwrap().unwrap();
        let (response, _) = result.result.unwrap();
        assert_eq!(native_response_observation(&response), ordinary,
            "R21f M1: stale native history does not replace the ordinary response");
        let mut attempts = 0;
        let mut ended = false;
        while !ended {
            let event = tokio::time::timeout(NATIVE_TIMEOUT, rx.recv()).await.unwrap().unwrap();
            if let xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { request_id: id, lease: delivered, evidence } = event {
                assert_eq!(id, request_id);
                assert!(delivered.same_lease(&lease));
                match evidence {
                    NativeEvidenceEvent::Attempt(attempt) => {
                        attempts += 1;
                        assert!(attempt.admitted && attempt.stream_complete);
                        let ack = session.actor.chat_state_handle.append_native_evidence(
                            id.as_str().into(), delivered, NativeEvidenceEvent::Attempt(attempt)).await;
                        assert_eq!(ack, Err(NativeUnavailable::StaleHistory),
                            "R21f M1: append failure must not acknowledge a discarded generation");
                    }
                    NativeEvidenceEvent::End => {
                        session.actor.chat_state_handle.append_native_evidence(id.as_str().into(), delivered, NativeEvidenceEvent::End).await.unwrap();
                        ended = true;
                    }
                }
            }
        }
        assert_eq!(attempts, 1);
        assert_eq!(server.messages_request_count(), 2);
        r14_flush_native_session(&session).await;
        assert_eq!(std::fs::read(directory.join("authority.json")).unwrap(), authority);
        assert_eq!(std::fs::read(directory.join("current.json")).unwrap(), current);
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
        assert!(matches!(adapter.read_native_history(&session.actor.session_info).await, Err(NativeUnavailable::StaleHistory)));
        assert!(matches!(session.actor.chat_state_handle.read_native_history().await, Err(NativeUnavailable::StaleHistory)));
    }).await;
}

fn r21l_source_frames(ordinal: u64, padding: usize) -> Vec<Value> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut frames = r21a_partial_frames(1, 0);
    let terminal = frames.len() - 2;
    let mut text = Vec::new();
    for index in 2..4 {
        let block =
            json!({"type":"text","text":format!("{ordinal:04}:{index}:{}", "x".repeat(padding))});
        assert!(native_encoded_len(&block, NATIVE_BLOCK_BYTES).is_ok());
        text.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        text.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.splice(terminal..terminal, text);
    frames
}

fn r21l_source_headroom(
    checkpoint: &xai_chat_state::persistence::native::NativeCheckpoint,
) -> [(&'static str, u64); 4] {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    [
        (
            "source",
            NATIVE_SOURCE_BYTES
                .saturating_sub(checkpoint.record.capacity["source_bytes"])
                .saturating_sub(checkpoint.record.source.byte_len),
        ),
        (
            "active",
            NATIVE_ACTIVE_BYTES
                .saturating_sub(checkpoint.record.capacity["active_bytes"])
                .saturating_sub(checkpoint.record.active.byte_len * 2),
        ),
        (
            "ledger",
            NATIVE_LEDGER_BYTES
                .saturating_sub(checkpoint.record.capacity["ledger_bytes"])
                .saturating_sub(checkpoint.record.ledger.byte_len * 2),
        ),
        (
            "control",
            NATIVE_CONTROL_BYTES.saturating_sub(checkpoint.record.capacity["control_bytes"]),
        ),
    ]
}

async fn r21l_grow_source(
    session: &NativeHistorySession,
    server: &MockInferenceServer,
) -> Arc<xai_chat_state::persistence::native::NativeCheckpoint> {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut previous: Option<Arc<xai_chat_state::persistence::native::NativeCheckpoint>> = None;
    for ordinal in 1..=NATIVE_SOURCE_RESPONSES {
        let available = previous.as_ref().map_or(NATIVE_ACTIVE_BYTES, |checkpoint| {
            r21l_source_headroom(checkpoint)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .min()
                .unwrap()
        });
        let padding = (NATIVE_BLOCK_BYTES - NATIVE_BLOCK_BYTES / 32)
            .min(available.saturating_sub(NATIVE_RESPONSE_BYTES / 16) / 8)
            as usize;
        assert!(padding > 0, "R21l: source growth retains a legal payload");
        session
            .native_ended
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let start = session.observed.lock().unwrap().len();
        server.enqueue_response(
            "/v1/messages",
            native_history_script(r21l_source_frames(ordinal, padding)),
        );
        assert!(session.sample_outcome().await.is_err());
        r21a_native_ack(session).await;
        let attempts = session.observed.lock().unwrap()[start..]
            .iter()
            .filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].blocks.len(), 4);
        assert!(
            matches!(attempts[0].status, NativeResponseStatus::PartialTerminal(reason) if reason != NativeUnavailable::Capacity)
        );
        assert!(
            attempts[0]
                .blocks
                .iter()
                .all(|block| block.state == NativeBlockState::ClosedFaithful)
        );
        let read = session.actor.chat_state_handle.read_native_history().await;
        assert!(
            read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
            "R21l: source-copy funding must retain every admitted generation; growth={ordinal}, error={:?}",
            read.as_ref().err()
        );
        let checkpoint = read.unwrap().checkpoint.unwrap();
        assert_eq!(checkpoint.record.generation, ordinal);
        assert_eq!(checkpoint.source.responses.len() as u64, ordinal);
        assert_eq!(checkpoint.source.ordered_blocks.len() as u64, ordinal * 4);
        assert_eq!(checkpoint.ledger.usage.len() as u64, ordinal);
        assert!(checkpoint.active.view_links.is_empty());
        assert_eq!(server.messages_request_count() as u64, ordinal);
        let remaining = NATIVE_SOURCE_BYTES - checkpoint.record.capacity["source_bytes"];
        eprintln!(
            "R21l growth generation={ordinal} source={} copy={} remaining={remaining} padding={padding}",
            checkpoint.record.capacity["source_bytes"], checkpoint.record.source.byte_len
        );
        if remaining <= checkpoint.record.source.byte_len * 4 {
            return checkpoint;
        }
        previous = Some(checkpoint);
    }
    panic!("R21l: legal partial responses must reach the source-copy boundary");
}

async fn r21l_source_boundary(boundary: NativeRetryBoundary) {
    use crate::session::storage::StorageAdapter;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let mut frames = r21a_partial_frames(1, 0);
    let terminal = frames.len() - 2;
    frames[terminal]["delta"]["stop_reason"] = json!("end_turn");
    frames[terminal + 1] = json!({"type":"message_stop"});
    frames.splice(terminal..terminal, [
        json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":"done"}}),
        json!({"type":"content_block_stop","index":2}),
    ]);
    let ordinary = r17_unleased_output(frames.clone(), None).await;
    let server = MockInferenceServer::start().await.unwrap();
    let session = NativeHistorySession::new(&server).await;
    let started = std::time::Instant::now();
    r21_guard_growth(started, async {
        let mut before = r21l_grow_source(&session, &server).await;
        let binding = r21l_source_headroom(&before).into_iter().min_by_key(|(_, bytes)| *bytes).unwrap();
        assert_eq!(binding.0, "source", "R21l: the observer must bind the source partition");
        eprintln!("R21l growth witness=source boundary={boundary:?} elapsed_ms={} generations={} source={} copy={}",
            started.elapsed().as_millis(), before.record.generation,
            before.record.capacity["source_bytes"], before.record.source.byte_len);
        let mut admitted = 0;
        let mut refused = false;
        for _ in 0..NATIVE_SOURCE_RESPONSES {
            let current_path = session.directory.path().join("native_history/current.json");
            let authority_path = session.directory.path().join("native_history/authority.json");
            let current_before = std::fs::read(&current_path).unwrap();
            let authority_before = std::fs::read(&authority_path).unwrap();
            session.native_ended.store(false, std::sync::atomic::Ordering::SeqCst);
            let start = session.observed.lock().unwrap().len();
            server.enqueue_response("/v1/messages", native_history_script(frames.clone()));
            session.sample().await;
            let events = session.observed.lock().unwrap()[start..].to_vec();
            assert_eq!(r18_terminal_observation(&events), vec![json!({"completed":ordinary})],
                "R21l: source capacity leaves the ordinary result unchanged");
            let attempts = events.iter().filter_map(|event| match event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: NativeEvidenceEvent::Attempt(attempt), ..
                } => Some(attempt),
                _ => None,
            }).collect::<Vec<_>>();
            if !attempts.is_empty() {
                r21a_native_ack(&session).await;
            }
            let read = session.actor.chat_state_handle.read_native_history().await;
            assert!(read.as_ref().is_ok_and(|read| read.checkpoint.is_some()),
                "R21l: source-copy funding must retain every admitted generation; boundary={boundary:?}, error={:?}", read.as_ref().err());
            let checkpoint = read.unwrap().checkpoint.unwrap();
            if attempts.is_empty() {
                assert!(matches!(boundary, NativeRetryBoundary::Refusal));
                assert_eq!(checkpoint.bytes, before.bytes, "R21l: source refusal preserves the acknowledged checkpoint");
                assert_eq!(std::fs::read(&current_path).unwrap(), current_before);
                assert_eq!(std::fs::read(&authority_path).unwrap(), authority_before);
                assert!(matches!(session.actor.chat_state_handle.begin_native_capture(
                    checkpoint.record.lineage.id.clone(), "r21l-refused".into(), checkpoint.record.generation + 1).await,
                    Err(NativeUnavailable::Capacity)), "R21l: source exhaustion is refused at admission");
                assert_eq!(r21l_source_headroom(&checkpoint).into_iter().min_by_key(|(_, bytes)| *bytes).unwrap().0, "source");
                refused = true;
            } else {
                assert_eq!(attempts.len(), 1);
                assert_eq!(checkpoint.record.generation, before.record.generation + 1);
                assert_eq!(checkpoint.source.responses.len(), before.source.responses.len() + 1);
                assert_eq!(checkpoint.ledger.usage.len(), before.ledger.usage.len() + 1);
                assert!(checkpoint.source.responses.starts_with(&before.source.responses));
                assert!(checkpoint.ledger.usage.starts_with(&before.ledger.usage));
                if matches!(boundary, NativeRetryBoundary::Fit) {
                    assert_eq!(attempts[0].status, NativeResponseStatus::Accepted, "R21l: source-bound FIT retained in full");
                    assert!(attempts[0].stream_complete);
                    assert_eq!(attempts[0].blocks.len(), 3);
                }
                admitted += 1;
            }
            let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
            let disk = adapter.read_native_history(&session.actor.session_info).await.unwrap().checkpoint.unwrap();
            assert_eq!(disk.bytes, checkpoint.bytes);
            let record: Value = serde_json::from_slice(&std::fs::read(&current_path).unwrap()).unwrap();
            let source = native_blob(session.directory.path(), &record["source"]);
            let ledger = native_blob(session.directory.path(), &record["ledger"]);
            assert_eq!(source["responses"].as_array().unwrap().len(), checkpoint.source.responses.len());
            assert_eq!(ledger["usage"].as_array().unwrap().len(), checkpoint.ledger.usage.len());
            if let Some(attempt) = attempts.first() {
                assert_eq!(ledger["usage"].as_array().unwrap().last().unwrap()["observed"], serde_json::to_value(&attempt.usage).unwrap());
                assert_eq!(ledger["usage"].as_array().unwrap().last().unwrap()["http_dispatches"], 1);
                if matches!(boundary, NativeRetryBoundary::Fit) {
                    assert_eq!(source["responses"].as_array().unwrap().last().unwrap()["status"], "Accepted");
                    for (stored, captured) in source["ordered_blocks"].as_array().unwrap().iter().rev().take(3).zip(attempt.blocks.iter().rev()) {
                        assert_eq!(native_blob(session.directory.path(), &stored["block"]), serde_json::from_str::<Value>(&captured.raw).unwrap());
                    }
                }
            }
            let full = adapter.load_session(&session.actor.session_info).await.unwrap();
            let light = adapter.load_session_without_updates(&session.actor.session_info).await.unwrap();
            assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&light.chat_history).unwrap());
            disk.matches_history(&full.chat_history).unwrap();
            eprintln!("R21l boundary={boundary:?} generation={} source={} copy={} envelopes={} elapsed_ms={}",
                checkpoint.record.generation, checkpoint.record.capacity["source_bytes"], checkpoint.record.source.byte_len,
                attempts.len(), started.elapsed().as_millis());
            if refused || matches!(boundary, NativeRetryBoundary::Fit) {
                break;
            }
            before = checkpoint;
        }
        assert!(admitted > 0, "R21l: the source boundary includes a retained FIT capture");
        assert_eq!(refused, matches!(boundary, NativeRetryBoundary::Refusal));
    }).await;
}

#[test]
fn native_history_source_generation_fits_and_reloads() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new()
                .block_on(&runtime, r21l_source_boundary(NativeRetryBoundary::Fit));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_history_source_generation_refusal_preserves_checkpoint() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new()
                .block_on(&runtime, r21l_source_boundary(NativeRetryBoundary::Refusal));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[derive(Debug, Clone)]
struct U5aClientTool {
    name: &'static str,
    kind: xai_grok_tools::types::tool::ToolKind,
    executions: Arc<std::sync::atomic::AtomicUsize>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for U5aClientTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        self.kind
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Offline primary-resolution tool"
    }
}

impl xai_tool_runtime::Tool for U5aClientTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, "Offline primary-resolution tool")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        _: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(xai_grok_tools::types::output::ToolOutput::Text(
            "offline client result".into(),
        ))
    }
}

struct U5aPrimarySession {
    actor: Arc<SessionActor>,
    persistence: crate::session::persistence::PersistenceHandle,
    directory: tempfile::TempDir,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _models: tempfile::TempDir,
    executions: Arc<std::sync::atomic::AtomicUsize>,
    source_acks: tokio::sync::mpsc::UnboundedReceiver<()>,
    release_decision: Arc<tokio::sync::Semaphore>,
    resolution_writes:
        tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::persistence::native::NativeWrite>,
    resolution_gate: Arc<tokio::sync::Semaphore>,
    gate_enabled: Arc<std::sync::atomic::AtomicBool>,
    resolution_ack_count: Arc<std::sync::atomic::AtomicUsize>,
    lose_resolution_reply: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for U5aPrimarySession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl U5aPrimarySession {
    async fn new(server: &MockInferenceServer) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = xai_grok_sampler::SamplerConfig {
            base_url: server.url(),
            model: "u5a".into(),
            api_key: Some("u5a-loopback".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096),
            max_retries: Some(0),
            ..Default::default()
        };
        let info = crate::session::info::Info {
            id: acp::SessionId::new(format!(
                "u5a-{}",
                directory.path().file_name().unwrap().to_string_lossy()
            )),
            cwd: directory.path().to_string_lossy().into_owned(),
        };
        let persistence = crate::session::persistence::new_with_explicit_dir(
            &info,
            directory.path().to_path_buf(),
            acp::ModelId::new("u5a"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
            "u5a".into(),
            crate::session::persistence::ExplicitSessionOpen::New {
                identity: None,
                next_trace_turn: None,
            },
        )
        .await
        .unwrap();
        let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
        let (resolution_tx, resolution_writes) = tokio::sync::mpsc::unbounded_channel();
        let resolution_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let gate = resolution_gate.clone();
        let gate_enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let enabled = gate_enabled.clone();
        let lose_resolution_reply = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lose_reply = lose_resolution_reply.clone();
        let real = persistence.tx.clone();
        let resolution_ack_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ack_count = resolution_ack_count.clone();
        let disk = tokio::task::spawn_local(async move {
            while let Some(message) = chat_rx.recv().await {
                match message {
                    PersistenceMsg::PersistNativeGenerationAndAck { write, respond_to }
                        if write.transaction.starts_with("primary:")
                            && enabled.load(std::sync::atomic::Ordering::SeqCst) =>
                    {
                        resolution_tx.send(write.clone()).unwrap();
                        gate.acquire().await.unwrap().forget();
                        let (disk_tx, disk_rx) = tokio::sync::oneshot::channel();
                        real.send(PersistenceMsg::PersistNativeGenerationAndAck {
                            write,
                            respond_to: disk_tx,
                        })
                        .unwrap();
                        let result = disk_rx.await.unwrap();
                        if result.is_ok() {
                            ack_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        if lose_reply.load(std::sync::atomic::Ordering::SeqCst) {
                            drop(respond_to);
                        } else {
                            let _ = respond_to.send(result);
                        }
                    }
                    message => {
                        real.send(message).unwrap();
                    }
                }
            }
        });
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (mut actor, _) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence.tx.clone(),
            Box::new(crate::session::chat_persistence::ChannelChatPersistence::new(chat_tx)),
        )
        .await;
        actor.session_info = info;
        actor.attach_non_interactive.set(true);
        actor.max_retries = 0;
        actor.compaction.compaction_mode = xai_chat_state::CompactionMode::Summary;
        {
            let mut slot = actor.agent.borrow_mut();
            let mut policy = slot.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            policy.memory_flush_enabled = false;
            *slot = xai_grok_agent::Agent::new(
                slot.definition().clone(),
                slot.prompt_context().clone(),
                slot.system_prompt().to_string(),
                slot.tool_bridge().clone(),
                slot.reminder_policy().clone(),
                policy,
                Vec::new(),
                true,
            );
        }
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        let (manager, models) = manager_with_entries();
        let mut entry = entry_with_search_flag("u5a", true);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("u5a-loopback".into());
        manager.insert_test_entry("u5a", entry);
        manager.set_current_model_id(acp::ModelId::new("u5a"));
        actor.models_manager = manager;
        let mut binding = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        binding.config.base_url = server.url();
        binding.config.model = "u5a".into();
        binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        binding.config.supports_search_tool = true;
        binding.config.max_completion_tokens = Some(4096);
        binding.config.max_retries = Some(0);
        binding.credentials.api_key = Some("u5a-loopback".into());
        binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
        actor
            .chat_state_handle
            .install_transport_binding(binding)
            .await
            .unwrap();
        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for name in ["alpha__lookup", "zeta__fetch"] {
            actor
                .agent
                .borrow()
                .tool_bridge()
                .register_mcp_tools(
                    name.into(),
                    U5aClientTool {
                        name,
                        kind: xai_grok_tools::types::tool::ToolKind::Other,
                        executions: executions.clone(),
                    },
                    Some(json!({"type":"object","properties":{},"additionalProperties":false})),
                )
                .unwrap();
        }
        let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            config,
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            sampler_tx,
        );
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
        let release_decision = Arc::new(tokio::sync::Semaphore::new(0));
        let resume = release_decision.clone();
        let sampler = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                let accepted = matches!(&event, xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { evidence: xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent::Attempt(attempt), .. } if attempt.status == xai_grok_sampling_types::conversation::native_search_history::NativeResponseStatus::Accepted);
                drainer.handle_sampling_event(event).await;
                if accepted {
                    source_tx.send(()).unwrap();
                    resume.acquire().await.unwrap().forget();
                }
            }
        });
        Self {
            actor,
            persistence,
            directory,
            tasks: vec![gateway, sampler, disk],
            _models: models,
            executions,
            source_acks,
            release_decision,
            resolution_writes,
            resolution_gate,
            gate_enabled,
            resolution_ack_count,
            lose_resolution_reply,
        }
    }
}

fn u5a_frames(stop: &str, tools: usize) -> Vec<Value> {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"msg_u5a","type":"message","role":"assistant","model":"u5a","content":[],"stop_reason":null,"usage":{"input_tokens":11,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_u5a","name":"tool_search_tool_regex","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"lookup\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_search_tool_result","tool_use_id":"srvtoolu_u5a","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"alpha__lookup"},{"type":"tool_reference","tool_name":"zeta__fetch"}]}}}),
        json!({"type":"content_block_stop","index":1}),
    ];
    for index in 0..tools {
        frames.push(json!({"type":"content_block_start","index":index+2,"content_block":{"type":"tool_use","id":format!("toolu_u5a_{index}"),"name":"alpha__lookup","input":{}}}));
        frames.push(json!({"type":"content_block_delta","index":index+2,"delta":{"type":"input_json_delta","partial_json":"{}"}}));
        frames.push(json!({"type":"content_block_stop","index":index+2}));
    }
    frames.extend([
        json!({"type":"content_block_start","index":tools+2,"content_block":{"type":"text","text":"native primary response"}}),
        json!({"type":"content_block_stop","index":tools+2}),
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"input_tokens":11,"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ]);
    frames
}

fn u5a_disk_parts(session: &U5aPrimarySession) -> (Value, Value, Value, Value) {
    let record: Value = serde_json::from_slice(
        &std::fs::read(session.directory.path().join("native_history/current.json")).unwrap(),
    )
    .unwrap();
    let source = native_blob(session.directory.path(), &record["source"]);
    let active = native_blob(session.directory.path(), &record["active"]);
    let ledger = native_blob(session.directory.path(), &record["ledger"]);
    (record, source, active, ledger)
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_native_primary_has_no_active_links_and_next_capture_survives() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for stop in ["refusal", "model_context_window_exceeded"] {
                let server = MockInferenceServer::start().await.unwrap();
                server.enqueue_response("/v1/messages", native_history_script(u5a_frames(stop, 1)));
                let mut session = U5aPrimarySession::new(&server).await;
                session
                    .actor
                    .chat_state_handle
                    .push_user_message_and_ack(ConversationItem::user(
                        "find and use the exact tool",
                    ))
                    .await;
                let actor = session.actor.clone();
                let running = tokio::task::spawn_local(async move {
                    actor
                        .process_conversation_turn_with_recovery(
                            "u5a-reject",
                            None,
                            None,
                            None,
                            &mut length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                        )
                        .await
                });
                tokio::time::timeout(NATIVE_TIMEOUT, session.source_acks.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !running.is_finished(),
                    "primary decision remains held after real source ACK"
                );
                let (_, source, active, ledger) = u5a_disk_parts(&session);
                assert_eq!(source["schema_version"], 2);
                assert!(
                    active["view_links"].as_array().unwrap().is_empty(),
                    "U5a source ACK must not publish active links before the primary decision"
                );
                assert!(
                    ledger["accounting"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|entry| entry["view_item"].is_null()
                            && entry["view_digest"].is_null()
                            && entry["role"] == "SourceOnly")
                );
                session.release_decision.add_permits(1);
                let outcome = tokio::time::timeout(NATIVE_TIMEOUT, running)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert!(matches!(outcome, TurnOutcome::Completed { .. }));
                assert_eq!(
                    session.executions.load(std::sync::atomic::Ordering::SeqCst),
                    0
                );
                let (_, retained, active, ledger) = u5a_disk_parts(&session);
                assert_eq!(
                    retained, source,
                    "primary rejection preserves exact source and A1 identities"
                );
                assert!(active["view_links"].as_array().unwrap().is_empty());
                assert!(
                    ledger["lifecycle"]["waiting_clients"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|client| client["delivery"] == "Unavailable"),
                    "U5a rejected client obligations must be unavailable"
                );
                assert!(
                    ledger["transitions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["primary_resolution"] == "Rejected"),
                    "U5a owns a durable rejected resolution"
                );
                u5a_independent_reader_control(&session).await;
                if stop == "refusal" {
                    server.enqueue_response(
                        "/v1/messages",
                        native_history_script(u5a_frames("end_turn", 0)),
                    );
                    session.release_decision.add_permits(1);
                    session
                        .actor
                        .chat_state_handle
                        .push_user_message_and_ack(ConversationItem::user(
                            "capture the next ordinary turn",
                        ))
                        .await;
                    session
                        .actor
                        .process_conversation_turn_with_recovery(
                            "u5a-next",
                            None,
                            None,
                            None,
                            &mut length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                        )
                        .await
                        .unwrap();
                    use crate::session::storage::StorageAdapter;
                    let adapter =
                        crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
                            session.directory.path().to_path_buf(),
                        );
                    let read = adapter
                        .read_native_history(&session.actor.session_info)
                        .await
                        .unwrap()
                        .checkpoint
                        .unwrap();
                    assert_eq!(
                        read.source.responses.len(),
                        2,
                        "U5a next genuine primary capture is acknowledged"
                    );
                    let full = adapter
                        .load_session(&session.actor.session_info)
                        .await
                        .unwrap();
                    let light = adapter
                        .load_session_without_updates(&session.actor.session_info)
                        .await
                        .unwrap();
                    assert_eq!(
                        serde_json::to_value(&full.chat_history).unwrap(),
                        serde_json::to_value(&light.chat_history).unwrap()
                    );
                    read.matches_history(&full.chat_history).unwrap();
                    assert!(
                        !read.active.view_links.is_empty(),
                        "accepted primary must publish its exact mappings"
                    );
                }
            }
            u5a_disk_controls().await;
            u5a_resolution_failure_controls().await;
            u5a_phase_and_drop_controls().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn media_resampled_native_primary_has_no_active_links_and_next_capture_survives() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let server = MockInferenceServer::start().await.unwrap();
            let session = U5aPrimarySession::new(&server).await;
            let bridge = session.actor.agent.borrow().tool_bridge().clone();
            assert!(bridge.unregister_tool_by_name("alpha__lookup"));
            bridge
                .register_mcp_tools(
                    "alpha__lookup".into(),
                    U5aClientTool {
                        name: "alpha__lookup",
                        kind: xai_grok_tools::types::tool::ToolKind::ImageGen,
                        executions: session.executions.clone(),
                    },
                    Some(json!({"type":"object","properties":{},"additionalProperties":false})),
                )
                .unwrap();
            let count = xai_grok_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN * 2;
            server.enqueue_response(
                "/v1/messages",
                native_history_script(u5a_frames("tool_use", count)),
            );
            server.enqueue_response(
                "/v1/messages",
                native_history_script(u5a_frames("end_turn", 0)),
            );
            let (session, outcome, held) = u5a_observe_primary(session, "u5a-media", 2).await;
            assert!(outcome.is_ok());
            assert_eq!(held.len(), 2);
            assert_eq!(
                server.messages_request_count(),
                2,
                "real media policy performs one bounded resample"
            );
            assert_eq!(
                session.executions.load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            u5a_assert_rejected(&held[0], &u5a_disk_parts(&session));
            u5a_assert_usable(&session, 2).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn length_exhausted_native_primary_has_no_active_links_and_next_capture_survives() {
    tokio::task::LocalSet::new().run_until(async {
        let server = MockInferenceServer::start().await.unwrap();
        let session = U5aPrimarySession::new(&server).await;
        let count = sampler_turn::MAX_OUTPUT_TOKEN_LIMIT_RETRIES as usize + 1;
        for _ in 0..count { server.enqueue_response("/v1/messages", native_history_script(u5a_frames("max_tokens", 1))); }
        let (session, outcome, held) = u5a_observe_primary(session, "u5a-length", count).await;
        assert!(outcome.is_err(), "real Length salvage cap retains its ordinary error");
        assert_eq!(held.len(), count);
        assert_eq!(server.messages_request_count(), count);
        let (_, _, active, ledger) = u5a_disk_parts(&session);
        assert!(active["view_links"].as_array().unwrap().iter().all(|entry| entry["source"]["response"].as_u64().unwrap() < (count - 1) as u64));
        assert!(ledger["transitions"].as_array().unwrap().iter().any(|entry| entry["primary_resolution"] == "Rejected"));
        u5a_assert_rejected(held.last().unwrap(), &u5a_disk_parts(&session));
        server.enqueue_response("/v1/messages", native_history_script(u5a_frames("end_turn", 0)));
        let (session, next, _) = u5a_observe_primary(session, "u5a-after-length", 1).await;
        assert!(next.is_ok());
        u5a_assert_usable(&session, count + 1).await;
    }).await;
}

type U5aParts = (Value, Value, Value, Value);

async fn u5a_observe_primary(
    mut session: U5aPrimarySession,
    request: &'static str,
    count: usize,
) -> (
    U5aPrimarySession,
    Result<TurnOutcome, acp::Error>,
    Vec<U5aParts>,
) {
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("exercise native primary resolution"))
        .await;
    let actor = session.actor.clone();
    let running = tokio::task::spawn_local(async move {
        actor
            .process_conversation_turn_with_recovery(
                request,
                None,
                None,
                None,
                &mut length_salvage::LengthSalvage::new(None),
                &mut Default::default(),
            )
            .await
    });
    let mut held = Vec::new();
    for _ in 0..count {
        tokio::time::timeout(NATIVE_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!running.is_finished());
        let parts = u5a_disk_parts(&session);
        let occurrence = parts.1["responses"].as_array().unwrap().last().unwrap()["occurrence"]
            .as_u64()
            .unwrap();
        assert!(
            parts.2["view_links"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["source"]["response"].as_u64().unwrap() != occurrence),
            "U5a source ACK must not publish active links before the primary decision"
        );
        held.push(parts);
        session.release_decision.add_permits(1);
    }
    let outcome = tokio::time::timeout(NATIVE_TIMEOUT, running)
        .await
        .unwrap()
        .unwrap();
    (session, outcome, held)
}

fn u5a_assert_rejected(before: &U5aParts, after: &U5aParts) {
    let response = before.1["responses"].as_array().unwrap().last().unwrap();
    let occurrence = response["occurrence"].as_u64().unwrap();
    assert_eq!(after.1["responses"][occurrence as usize], *response);
    let prefix = before.1["ordered_blocks"].as_array().unwrap();
    assert_eq!(
        &after.1["ordered_blocks"].as_array().unwrap()[..prefix.len()],
        prefix
    );
    assert!(
        after.2["view_links"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["source"]["response"].as_u64().unwrap() != occurrence)
    );
    let own: Vec<_> = after.3["accounting"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["source"]["response"].as_u64().unwrap() == occurrence)
        .collect();
    assert!(own.iter().all(|entry| entry["role"] == "SourceOnly"
        && entry["view_item"].is_null()
        && entry["view_digest"].is_null()));
    assert!(
        after.3["transitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["occurrence"].as_u64() == Some(occurrence)
                && entry["primary_resolution"] == "Rejected"),
        "U5a rejected occurrence must carry its owned resolution"
    );
    assert!(
        after.3["lifecycle"]["waiting_clients"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["source"]["response"].as_u64() == Some(occurrence))
            .all(|entry| entry["delivery"] == "Unavailable")
    );
    let links = before.1["eligible_member_links"].as_array().unwrap();
    let witnesses = before.3["eligible_member_accounting"].as_array().unwrap();
    assert_eq!(
        &after.1["eligible_member_links"].as_array().unwrap()[..links.len()],
        links
    );
    assert_eq!(
        &after.3["eligible_member_accounting"].as_array().unwrap()[..witnesses.len()],
        witnesses
    );
}

async fn u5a_assert_usable(session: &U5aPrimarySession, responses: usize) {
    use crate::session::storage::StorageAdapter;
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let read = adapter
        .read_native_history(&session.actor.session_info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let full = adapter
        .load_session(&session.actor.session_info)
        .await
        .unwrap();
    let light = adapter
        .load_session_without_updates(&session.actor.session_info)
        .await
        .unwrap();
    assert_eq!(
        read.source.responses.len(),
        responses,
        "next real capture must be acknowledged, not just sent"
    );
    assert_eq!(
        serde_json::to_value(&full.chat_history).unwrap(),
        serde_json::to_value(&light.chat_history).unwrap()
    );
    read.matches_history(&full.chat_history).unwrap();
    assert!(!read.active.view_links.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn context_full_native_primary_compaction_fails_closed_without_stale_links() {
    tokio::task::LocalSet::new().run_until(async {
        use crate::session::storage::StorageAdapter;
        use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
        let server = MockInferenceServer::start().await.unwrap();
        let session = U5aPrimarySession::new(&server).await;
        session.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::system("You are an offline primary-resolution test agent.")).await;
        session.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("retained earlier work ".repeat(3000))).await;
        server.enqueue_response("/v1/messages", native_history_script(u5a_frames("end_turn", 0)));
        let (session, accepted, _) = u5a_observe_primary(session, "u5a-before-context-full", 1).await;
        assert!(accepted.is_ok());
        let before = session.actor.chat_state_handle.get_conversation().await;
        server.enqueue_response("/v1/messages", native_history_script(u5a_frames("model_context_window_exceeded", 1)));
        let (session, rejected, held) = u5a_observe_primary(session, "u5a-context-full", 1).await;
        assert!(matches!(rejected.as_ref().unwrap(), TurnOutcome::Completed { stop: CompletedStop::ContextWindowExceeded, .. }));
        u5a_assert_rejected(&held[0], &u5a_disk_parts(&session));
        let native_before = u5a_disk_parts(&session);
        let requests_before = server.requests().len();
        let ordinary_text = "ordinary continuation after successful context-full compaction";
        let ordinary_usage = xai_grok_sampling_types::TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 7,
            total_tokens: 18,
            ..Default::default()
        };
        let mut expected_prompt = session.actor.chat_state_handle.try_get_prompt_usage().await.unwrap().unwrap();
        let mut expected_session = session.actor.chat_state_handle.try_get_session_usage().await.unwrap();
        for ledger in [&mut expected_prompt, &mut expected_session] {
            ledger.record_main_loop_call("u5a", &ordinary_usage, None, None);
        }
        let count = session.actor.compaction.count.load(std::sync::atomic::Ordering::Relaxed);
        assert!(session.actor.compaction.force_compact.load(std::sync::atomic::Ordering::Relaxed));
        server.enqueue_response("/v1/messages", native_history_script(vec![
            json!({"type":"message_start","message":{"id":"msg_compact_u5a","type":"message","role":"assistant","model":"u5a","content":[],"stop_reason":null,"usage":{"input_tokens":11,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"## Task\nContinue the native primary test.\n## Progress\nThe earlier tool discovery completed; the context-full response was rejected.\n## Next\nAnswer the next request. Preserve the recorded ordinary outcome and do not execute the rejected client call. The two retained typed tool identities remain historical evidence and cannot authorize reuse after this successful history replacement. The summary represents completed earlier work, the source-only rejected occurrence, and the next user request. No file edits, external calls, or additional tasks are needed.\n".repeat(2)}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":11,"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ]));
        let mut ordinary_frames = u5a_frames("end_turn", 0);
        let ordinary_block = ordinary_frames.iter_mut().find(|frame| frame["content_block"]["type"] == "text").unwrap();
        ordinary_block["content_block"]["text"] = json!(ordinary_text);
        server.enqueue_response("/v1/messages", native_history_script(ordinary_frames));
        session.release_decision.add_permits(1);
        session.actor.chat_state_handle.push_user_message_and_ack(ConversationItem::user("continue after the context-full response")).await;
        let result = tokio::time::timeout(Duration::from_secs(60), session.actor.process_conversation_turn_with_recovery("u5a-real-forced-compaction", None, None, None, &mut length_salvage::LengthSalvage::new(None), &mut Default::default())).await.unwrap();
        assert!(result.is_ok(), "ordinary forced compaction and following turn succeed");
        let requests = server.requests();
        let following = &requests[requests_before..];
        assert_eq!(
            following.iter().map(|entry| (entry.method.as_str(), entry.path.as_str())).collect::<Vec<_>>(),
            vec![("POST", "/v1/messages"), ("POST", "/v1/messages")],
            "R15 T5aF: compaction must be followed by exactly one ordinary dispatch"
        );
        let request_text = |body: &Value| {
            body["messages"].as_array().unwrap().iter()
                .flat_map(|message| message["content"].as_array().unwrap())
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>().join("\n")
        };
        let compaction_prompt = crate::session::helpers::session_compact::build_compaction_prompt(None, false);
        let compaction_request = request_text(following[0].body.as_ref().unwrap());
        let ordinary_request = request_text(following[1].body.as_ref().unwrap());
        assert!(compaction_request.ends_with(&compaction_prompt), "first dispatch is the real summarization request");
        assert!(!ordinary_request.contains(&compaction_prompt));
        assert!(ordinary_request.contains("Continue the native primary test."), "next dispatch consumes the installed summary");
        assert!(ordinary_request.contains("continue after the context-full response"));
        assert!(matches!(result.unwrap(), TurnOutcome::Completed {
            stop: CompletedStop::EndTurn, tools_called, structured_output: None,
        } if tools_called.is_empty()), "scripted ordinary terminal and dispatch result are preserved");
        assert_eq!(session.actor.chat_state_handle.get_last_turn_usage().await, Some(ordinary_usage));
        let without_latency = |mut ledger: xai_chat_state::usage::UsageLedger| {
            ledger.totals.api_duration_ms = 0;
            for totals in ledger.by_model.values_mut() {
                totals.api_duration_ms = 0;
            }
            ledger
        };
        assert_eq!(without_latency(session.actor.chat_state_handle.try_get_prompt_usage().await.unwrap().unwrap()), without_latency(expected_prompt), "ordinary prompt accounting matches the scripted usage");
        assert_eq!(without_latency(session.actor.chat_state_handle.try_get_session_usage().await.unwrap()), without_latency(expected_session), "ordinary session accounting matches the scripted usage");
        assert!(matches!(rejected.as_ref().unwrap(), TurnOutcome::Completed {
            stop: CompletedStop::ContextWindowExceeded, tools_called, structured_output: None,
        } if tools_called.is_empty()), "compaction leaves the earlier typed context-full outcome unchanged");
        assert!(!session.actor.compaction.force_compact.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(session.actor.compaction.count.load(std::sync::atomic::Ordering::Relaxed), count + 1, "the real main path successfully compacted");
        let after = session.actor.chat_state_handle.get_conversation().await;
        let ordinary_assistant = after.iter().rev().find_map(|item| match item {
            ConversationItem::Assistant(assistant) => Some(assistant),
            _ => None,
        }).unwrap();
        assert_eq!(ordinary_assistant.content.as_ref(), ordinary_text, "the queued ordinary response was consumed and recorded");
        assert!(ordinary_assistant.tool_calls.is_empty());
        assert_ne!(serde_json::to_value(&after[..before.len().min(after.len())]).unwrap(), serde_json::to_value(&before).unwrap(), "successful compaction rewrites the retained prefix");
        assert_eq!(u5a_disk_parts(&session).0, native_before.0, "the stale native checkpoint stays immutable historical evidence");
        assert!(matches!(session.actor.chat_state_handle.read_native_history().await, Err(NativeUnavailable::StaleHistory)));
        let (respond_to, flushed) = tokio::sync::oneshot::channel();
        session.persistence.tx.send(PersistenceMsg::FlushAndAck { respond_to }).unwrap();
        tokio::time::timeout(NATIVE_TIMEOUT, flushed).await.unwrap().unwrap().unwrap();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(session.directory.path().to_path_buf());
        assert!(matches!(adapter.read_native_history(&session.actor.session_info).await, Err(NativeUnavailable::StaleHistory)));
        let full = adapter.load_session(&session.actor.session_info).await.unwrap();
        let light = adapter.load_session_without_updates(&session.actor.session_info).await.unwrap();
        assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&light.chat_history).unwrap());
        let persisted_assistant = full.chat_history.iter().rev().find_map(|item| match item {
            ConversationItem::Assistant(assistant) => Some(assistant),
            _ => None,
        }).unwrap();
        assert_eq!(serde_json::to_value(persisted_assistant).unwrap(), serde_json::to_value(ordinary_assistant).unwrap(), "the scripted ordinary assistant survives real disk reload");
        assert_eq!(session.executions.load(std::sync::atomic::Ordering::SeqCst), 0);
    }).await;
}

async fn u5a_disk_controls() {
    use xai_chat_state::persistence::native::{NativeAppendAck, NativePrimaryDecision};
    use xai_grok_sampling_types::conversation::native_search_history::{
        NativeCaptureLease, NativeUnavailable,
    };
    {
        use xai_grok_sampling_types::conversation::native_search_history::NativeEvidenceEvent;
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = U5aPrimarySession::new(&server).await;
        let handle = session.actor.chat_state_handle.clone();
        handle
            .push_user_message_and_ack(ConversationItem::user(
                "observe the first primary-resolution reply",
            ))
            .await;
        let lineage = handle.read_native_history().await.unwrap().lineage;
        let request_id = xai_grok_sampler::RequestId::random();
        let lease = handle
            .begin_native_primary_capture(lineage, request_id.as_str().into(), 1, None)
            .await
            .unwrap();
        let config = xai_grok_sampler::SamplerConfig {
            base_url: server.url(),
            model: "u5a".into(),
            api_key: Some("u5a-loopback".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096),
            max_retries: Some(0),
            ..Default::default()
        };
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let sampler = xai_grok_sampler::SamplerActor::spawn(
            config.clone(),
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            events,
        );
        server.enqueue_response(
            "/v1/messages",
            native_history_script(u5a_frames("end_turn", 0)),
        );
        let sampled = tokio::time::timeout(
            NATIVE_TIMEOUT,
            sampler.submit_and_collect_with_native_capture(
                request_id,
                xai_grok_sampling_types::ConversationRequest {
                    items: handle.get_conversation().await,
                    ..Default::default()
                },
                config,
                lease.clone(),
            ),
        )
        .await
        .unwrap();
        assert!(sampled.result.is_ok());
        let mut source_acks = 0;
        loop {
            let event = tokio::time::timeout(NATIVE_TIMEOUT, received.recv())
                .await
                .unwrap()
                .unwrap();
            if let xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                request_id,
                lease: issued,
                evidence,
            } = event
            {
                assert!(issued.same_lease(&lease));
                let end = matches!(&evidence, NativeEvidenceEvent::End);
                let ack = handle
                    .append_native_evidence(request_id.as_str().into(), issued, evidence)
                    .await
                    .unwrap();
                if end {
                    assert_eq!(ack, NativeAppendAck::Released);
                    break;
                }
                assert!(matches!(ack, NativeAppendAck::Applied { .. }));
                source_acks += 1;
            }
        }
        assert_eq!(source_acks, 1);
        let before = u5a_disk_parts(&session);
        assert_eq!(before.3["transitions"][0]["primary_resolution"], "Pending");
        session
            .gate_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut reply = tokio::task::spawn_local(async move {
            handle
                .resolve_native_primary(lease, NativePrimaryDecision::Rejected)
                .await
        });
        let write = tokio::time::timeout(NATIVE_TIMEOUT, session.resolution_writes.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut reply)
                .await
                .is_err(),
            "U5a primary resolution reply cannot precede the held real disk operation"
        );
        assert_eq!(
            session
                .resolution_ack_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(u5a_disk_parts(&session).0, before.0);
        session.resolution_gate.add_permits(1);
        let ack = tokio::time::timeout(NATIVE_TIMEOUT, reply)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(ack, NativeAppendAck::Applied { .. }));
        assert_eq!(
            session
                .resolution_ack_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the real adapter acknowledges exactly one forwarded first resolution"
        );
        assert!(session.resolution_writes.try_recv().is_err());
        assert_eq!(
            write.checkpoint.record.generation,
            u5a_disk_parts(&session).0["generation"].as_u64().unwrap()
        );
    }
    for reject in [false, true] {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = U5aPrimarySession::new(&server).await;
        server.enqueue_response(
            "/v1/messages",
            native_history_script(u5a_frames(
                if reject { "refusal" } else { "end_turn" },
                usize::from(reject),
            )),
        );
        session
            .gate_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        session
            .actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user(
                "hold the real primary-resolution write",
            ))
            .await;
        let actor = session.actor.clone();
        let running = tokio::task::spawn_local(async move {
            actor
                .process_conversation_turn_with_recovery(
                    "u5a-held-disk",
                    None,
                    None,
                    None,
                    &mut length_salvage::LengthSalvage::new(None),
                    &mut Default::default(),
                )
                .await
        });
        tokio::time::timeout(NATIVE_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap()
            .unwrap();
        let before = u5a_disk_parts(&session);
        session.release_decision.add_permits(1);
        let write = tokio::time::timeout(NATIVE_TIMEOUT, session.resolution_writes.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !running.is_finished(),
            "U5a primary success cannot precede the held real disk operation"
        );
        assert_eq!(
            session
                .resolution_ack_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            u5a_disk_parts(&session).0,
            before.0,
            "held operation has not changed disk authority"
        );
        session.resolution_gate.add_permits(1);
        assert!(
            tokio::time::timeout(NATIVE_TIMEOUT, running)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            session
                .resolution_ack_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the real adapter acknowledges exactly one forwarded resolution"
        );
        assert_eq!(
            write.checkpoint.record.generation,
            u5a_disk_parts(&session).0["generation"].as_u64().unwrap()
        );
        let response = &write.checkpoint.source.responses[0];
        let foreign = NativeCaptureLease::issue(
            response.request.clone(),
            response.request.clone(),
            1,
            response.lease,
            1024,
        );
        assert!(
            session
                .actor
                .chat_state_handle
                .resolve_native_primary(
                    foreign,
                    if reject {
                        NativePrimaryDecision::Rejected
                    } else {
                        NativePrimaryDecision::Committed
                    }
                )
                .await
                .is_err(),
            "foreign capability cannot resolve another lease"
        );
        let _ = (NativeAppendAck::Unchanged, NativeUnavailable::StaleHistory);
    }
}

async fn u5a_resolution_failure_controls() {
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    for fail_before_write in [false, true] {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = U5aPrimarySession::new(&server).await;
        session
            .gate_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        session
            .lose_resolution_reply
            .store(!fail_before_write, std::sync::atomic::Ordering::SeqCst);
        server.enqueue_response(
            "/v1/messages",
            native_history_script(u5a_frames("refusal", 1)),
        );
        session
            .actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user(
                "preserve rejection when native disk acknowledgement fails",
            ))
            .await;
        let actor = session.actor.clone();
        let running = tokio::task::spawn_local(async move {
            actor
                .process_conversation_turn_with_recovery(
                    "u5a-failed-ack",
                    None,
                    None,
                    None,
                    &mut length_salvage::LengthSalvage::new(None),
                    &mut Default::default(),
                )
                .await
        });
        tokio::time::timeout(NATIVE_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap()
            .unwrap();
        let before = u5a_disk_parts(&session);
        session.release_decision.add_permits(1);
        tokio::time::timeout(NATIVE_TIMEOUT, session.resolution_writes.recv())
            .await
            .unwrap()
            .unwrap();
        let lock = if fail_before_write {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(
                    session
                        .directory
                        .path()
                        .join("native_history/transaction.lock"),
                )
                .unwrap();
            fs2::FileExt::try_lock_exclusive(&file).unwrap();
            Some(file)
        } else {
            None
        };
        session.resolution_gate.add_permits(1);
        let outcome = tokio::time::timeout(NATIVE_TIMEOUT, running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                outcome,
                TurnOutcome::Completed {
                    stop: CompletedStop::Refusal(_),
                    ..
                }
            ),
            "native resolution failure does not replace the ordinary refusal"
        );
        if let Some(file) = lock {
            fs2::FileExt::unlock(&file).unwrap();
            assert_eq!(u5a_disk_parts(&session).0, before.0);
        }
        assert!(
            matches!(
                session.actor.chat_state_handle.read_native_history().await,
                Err(NativeUnavailable::Indeterminate)
            ),
            "lost or failed native acknowledgement is unavailable, never fabricated success"
        );
        assert_eq!(
            session.executions.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(server.messages_request_count(), 1);
    }
}

async fn u5a_independent_reader_control(session: &U5aPrimarySession) {
    use crate::session::storage::StorageAdapter;
    use xai_chat_state::persistence::native::{NativeCheckpoint, digest};
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    let directory = session.directory.path().join("native_history");
    let original_authority = std::fs::read(directory.join("authority.json")).unwrap();
    let original_record = std::fs::read(directory.join("current.json")).unwrap();
    let mut record: Value = serde_json::from_slice(&original_record).unwrap();
    let mut ledger = native_blob(session.directory.path(), &record["ledger"]);
    assert_eq!(ledger["accounting"][0]["role"], "SourceOnly");
    ledger["accounting"][0]["role"] = json!("sourceOnly");
    let put = |kind: &str, value: &Value| {
        let bytes = serde_json::to_vec(value).unwrap();
        let digest = digest(&bytes);
        std::fs::write(
            directory.join("blobs").join(format!("{digest}.json")),
            &bytes,
        )
        .unwrap();
        json!({"kind":kind,"byte_len":bytes.len(),"digest":digest})
    };
    let reference = put("ledger", &ledger);
    assert_eq!(reference["byte_len"], record["ledger"]["byte_len"]);
    record["ledger"] = reference;
    let checkpoint = put("checkpoint", &record);
    assert_eq!(checkpoint["byte_len"], original_record.len());
    let mut authority: Value = serde_json::from_slice(&original_authority).unwrap();
    authority["checkpoint"] = checkpoint;
    let bytes = serde_json::to_vec(&record).unwrap();
    std::fs::write(directory.join("current.json"), &bytes).unwrap();
    std::fs::write(
        directory.join("authority.json"),
        serde_json::to_vec(&authority).unwrap(),
    )
    .unwrap();
    let decoded = NativeCheckpoint::decode(bytes, |reference| {
        std::fs::read(
            directory
                .join("blobs")
                .join(format!("{}.json", reference.digest)),
        )
        .map_err(|_| NativeUnavailable::InvalidReference)
    });
    let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    );
    let full = adapter.load_session(&session.actor.session_info).await;
    let light = adapter
        .load_session_without_updates(&session.actor.session_info)
        .await;
    std::fs::write(directory.join("authority.json"), original_authority).unwrap();
    std::fs::write(directory.join("current.json"), original_record).unwrap();
    assert!(
        decoded.is_err() && full.is_err() && light.is_err(),
        "non-owned accounting change is rejected by the independent reader"
    );
    assert!(
        adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .is_some()
    );
}

async fn u5a_phase_and_drop_controls() {
    use crate::session::storage::StorageAdapter;
    for drop_primary in [false, true] {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = U5aPrimarySession::new(&server).await;
        server.enqueue_response(
            "/v1/messages",
            native_history_script(u5a_frames("refusal", 1)),
        );
        session
            .actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user(
                "invalidate only the native primary owner",
            ))
            .await;
        let actor = session.actor.clone();
        let running = tokio::task::spawn_local(async move {
            actor
                .process_conversation_turn_with_recovery(
                    "u5a-phase",
                    None,
                    None,
                    None,
                    &mut length_salvage::LengthSalvage::new(None),
                    &mut Default::default(),
                )
                .await
        });
        tokio::time::timeout(NATIVE_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap()
            .unwrap();
        let before = u5a_disk_parts(&session);
        assert!(before.2["view_links"].as_array().unwrap().is_empty());
        if drop_primary {
            running.abort();
            assert!(matches!(running.await, Err(error) if error.is_cancelled()));
            session.release_decision.add_permits(1);
        } else {
            let generation = session.actor.turn_phases.current_generation();
            session.actor.turn_phases.start();
            assert_ne!(session.actor.turn_phases.current_generation(), generation);
            session.release_decision.add_permits(1);
            let outcome = tokio::time::timeout(NATIVE_TIMEOUT, running)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(
                outcome,
                TurnOutcome::Completed {
                    stop: CompletedStop::Refusal(_),
                    ..
                }
            ));
        }
        session.actor.chat_state_handle.get_conversation().await;
        let (respond_to, flushed) = tokio::sync::oneshot::channel();
        session
            .persistence
            .tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .unwrap();
        tokio::time::timeout(NATIVE_TIMEOUT, flushed)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            u5a_disk_parts(&session),
            before,
            "a dropped or stale-phase primary cannot resolve or publish its retained source"
        );
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            session.directory.path().to_path_buf(),
        );
        let read = adapter
            .read_native_history(&session.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert!(read.active.view_links.is_empty());
        let full = adapter
            .load_session(&session.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&session.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        read.matches_history(&full.chat_history).unwrap();
        assert_eq!(
            session.executions.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(server.messages_request_count(), 1);
    }
}
