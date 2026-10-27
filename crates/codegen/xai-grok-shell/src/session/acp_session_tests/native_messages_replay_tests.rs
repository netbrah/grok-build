use super::support::*;
use super::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use xai_grok_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};
use xai_grok_tools::implementations::grok_build::monitor::types::{
    MonitorEventBuffer, MonitorEventNotification,
};

const K0_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct K0IdleObserver {
    count: std::cell::Cell<usize>,
    notification: tokio::sync::Notify,
}

#[async_trait::async_trait(?Send)]
impl xai_agent_lifecycle::LocalSessionLifecycleContributor for K0IdleObserver {
    async fn on_session_idle(&self, _: &xai_agent_lifecycle::SessionIdleInput) {
        self.count.set(self.count.get() + 1);
        self.notification.notify_one();
    }
}

struct K0Shell {
    actor: Arc<SessionActor>,
    events: Option<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    idle: std::rc::Rc<K0IdleObserver>,
    _workspace: tempfile::TempDir,
    _model_home: tempfile::TempDir,
}

impl Drop for K0Shell {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl K0Shell {
    async fn new(server: &MockInferenceServer, with_memory: bool) -> Self {
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway_task = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        let persistence_task = tokio::task::spawn_local(async move {
            while let Some(message) = persistence_rx.recv().await {
                if let PersistenceMsg::FlushAndAck { respond_to } = message {
                    let _ = respond_to.send(Ok(()));
                }
            }
        });
        let (mut actor, events) =
            create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
        let workspace = tempfile::tempdir().unwrap();
        actor.hook_resolved_workspace_root = workspace.path().to_string_lossy().into_owned();
        actor
            .last_live_orphan_reconcile
            .set(Some(std::time::Instant::now()));
        actor.tool_context.monitor_event_buffer = Some(MonitorEventBuffer::new());
        actor.max_retries = 0;
        actor.mcp_reminder_mode = McpReminderMode::Full;
        let (manager, model_home) = manager_with_entries();
        let mut entry = entry_with_search_flag("test", false);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.info.context_window = std::num::NonZeroU64::new(256_000).unwrap();
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("test-key".into());
        manager.insert_test_entry("test", entry);
        manager.set_current_model_id(acp::ModelId::new("test"));
        actor.models_manager = manager;
        let agent = {
            let old = actor.agent.borrow();
            let mut policy = old.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            policy.wall_clock_budget_secs = 25;
            xai_grok_agent::Agent::new(
                old.definition().clone(),
                old.prompt_context().clone(),
                old.system_prompt().to_string(),
                old.tool_bridge().clone(),
                old.reminder_policy().clone(),
                policy,
                vec![],
                false,
            )
        };
        actor.agent.replace(agent);
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        if with_memory {
            let root = workspace.path().join("memory-v2");
            let storage = crate::session::memory::MemoryStorage::new_for_mode(
                std::path::Path::new("/test/k0-workspace"),
                Some(&root),
                crate::config::MemoryMode::V2,
            );
            storage.ensure_initialized().unwrap();
            actor.memory.configured_mode = Some(crate::config::MemoryMode::V2);
            actor.memory.storage.replace(Some(storage));
        }
        let idle = std::rc::Rc::new(K0IdleObserver::default());
        let mut extensions = xai_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        extensions.session_lifecycle_contributor(idle.clone());
        actor.extension_registry = extensions.build();
        let mut config = actor.chat_state_handle.get_sampling_config().await.unwrap();
        config.base_url = server.url();
        config.model = "test".into();
        config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        config.max_completion_tokens = Some(4096);
        config.max_retries = Some(0);
        actor.chat_state_handle.update_sampling_config(config);
        let mut credentials = actor.chat_state_handle.get_credentials().await;
        credentials.api_key = Some("test-key".into());
        actor.chat_state_handle.update_credentials(credentials);
        let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            xai_grok_sampler::SamplerConfig {
                base_url: server.url(),
                model: "test".into(),
                api_key: Some("test-key".into()),
                api_backend: xai_grok_sampler::ApiBackend::Messages,
                max_completion_tokens: Some(4096),
                max_retries: Some(0),
                ..Default::default()
            },
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            sampler_tx,
        );
        actor
            .workspace_ops
            .bind_local_session(
                &actor.session_id_string(),
                actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(),
                None,
            )
            .unwrap();
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let sampler_task = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                drainer.handle_sampling_event(event).await;
            }
        });
        Self {
            actor,
            events: Some(events),
            tasks: vec![gateway_task, persistence_task, sampler_task],
            idle,
            _workspace: workspace,
            _model_home: model_home,
        }
    }

    fn buffer(&self, task_id: &str, text: &str) {
        self.actor.tool_context.monitor_event_buffer.as_ref().unwrap().push_capped(
            MonitorEventNotification {
                task_id: task_id.into(), event_text: format!("<monitor-event description=\"k0\" task_id=\"{task_id}\">\n{text}\n</monitor-event>"),
                owner_session_id: Some(self.actor.session_id_string()),
            }, 32,
        );
    }

    async fn install_running(&mut self, prompt: &str) {
        let pending = tokio::task::spawn_local(std::future::pending::<()>());
        let epoch = self.actor.turn_report.start_next_turn();
        self.actor.state.lock().await.running_task = Some(AgentTask::new_at_epoch(
            prompt,
            epoch,
            pending.abort_handle(),
        ));
        self.actor
            .state
            .lock()
            .await
            .pending_inputs
            .push_back(user_item(prompt, "k0"));
        *self.actor.current_prompt_id.lock().unwrap() = Some(prompt.into());
        self.actor
            .session_turn_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.tasks.push(pending);
    }

    fn start_loop(&mut self) -> tokio::sync::mpsc::UnboundedSender<SessionCommand> {
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = self.actor.clone();
        let events = self.events.take().expect("one session loop");
        self.tasks.push(tokio::task::spawn_local(async move {
            let _chat_tx = chat_tx;
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                events,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::path::PathBuf::from("/tmp"),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
        commands
    }
}

fn k0_frames(blocks: Vec<Value>, stop_reason: &str) -> Vec<Value> {
    let mut frames = vec![json!({"type":"message_start","message":{
        "id":"msg_k0","type":"message","role":"assistant","model":"test","content":[],
        "stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}
    }})];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(json!({"type":"message_delta","delta":{"stop_reason":stop_reason},"usage":{"input_tokens":10,"output_tokens":5}}));
    frames.push(json!({"type":"message_stop"}));
    frames
}

fn k0_script(blocks: Vec<Value>, stop_reason: &str) -> ScriptedResponse {
    ScriptedResponse::sse(
        k0_frames(blocks, stop_reason)
            .into_iter()
            .map(|frame| SseEvent::data(frame.to_string()))
            .collect(),
    )
}

async fn k0_seed_decoded_discovery(actor: &SessionActor) {
    let raw = futures_util::stream::iter(
        k0_frames(
            vec![json!({
                "type":"server_tool_use","id":"srvtoolu_shell_k0","name":"tool_search_tool_regex",
                "input":{"query":"shell preservation"}
            })],
            "end_turn",
        )
        .into_iter()
        .map(|frame| {
            Ok::<_, xai_grok_sampling_types::SamplingError>(serde_json::from_value(frame).unwrap())
        }),
    )
    .boxed();
    let decoded = xai_grok_sampler::stream_messages(raw, None, "k0-seed".into(), K0_TIMEOUT);
    futures_util::pin_mut!(decoded);
    let mut found = false;
    while let Some(event) = decoded.next().await {
        if let xai_grok_sampler::SamplingEvent::Completed { response, .. } = event {
            for item in response.items {
                if matches!(item, ConversationItem::Discovery { .. }) {
                    found = true;
                }
                actor.chat_state_handle.push_model_output(item);
            }
        }
    }
    assert!(
        found,
        "real Messages decoder supplies the pending Discovery"
    );
    assert!(
        actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| matches!(item, ConversationItem::Discovery { .. }))
    );
}

async fn k0_monitor_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let mut observations = serde_json::Map::new();
    for mode in [
        "completion",
        "user-start",
        "idle",
        "cancel",
        "rewind",
        "drop",
    ] {
        let mut shell = K0Shell::new(&server, false).await;
        shell
            .actor
            .chat_state_handle
            .push_user_message(ConversationItem::user("keep the pending search"));
        k0_seed_decoded_discovery(&shell.actor).await;
        shell.buffer("watch-k0", "monitor payload");
        match mode {
            "completion" => {
                shell.install_running("completion-k0").await;
                shell.actor.state.lock().await.notifications_suppressed = true;
                let epoch = shell.actor.turn_report.epoch();
                let commands = shell.start_loop();
                let (processed, processed_rx) = tokio::sync::oneshot::channel();
                commands
                    .send(SessionCommand::InjectTurnCompletion {
                        prompt_id: "completion-k0".into(),
                        epoch,
                        result: Box::new(crate::session::commands::ok_end_turn(0, None)),
                        elapsed_ms: Some(0),
                        processed,
                    })
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, processed_rx)
                    .await
                    .unwrap()
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, shell.idle.notification.notified())
                    .await
                    .expect("post-drain idle contributor is the completion barrier");
                assert!(shell.idle.count.get() > 0);
            }
            "user-start" => {
                assert!(shell.actor.consume_deferred_completions().await.is_empty());
            }
            "idle" => {
                shell
                    .actor
                    .goal_harness_enabled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                shell
                    .actor
                    .goal_turn_task_ids
                    .lock()
                    .insert("watch-k0".into());
                let (completion_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                shell
                    .actor
                    .clone()
                    .maybe_drain_notifications(completion_tx)
                    .await;
                shell.actor.emit_session_idle_if_idle().await;
                tokio::time::timeout(K0_TIMEOUT, shell.idle.notification.notified())
                    .await
                    .unwrap();
            }
            "cancel" | "rewind" => {
                shell.install_running("cancel-k0").await;
                if mode == "rewind" {
                    shell.actor.state.lock().await.rewindable = true;
                }
                let outcome = shell
                    .actor
                    .cancel_running_task(crate::session::CancelOptions {
                        cancel_subagents: false,
                        kill_background_tasks: false,
                        user_initiated: true,
                        trigger: Some(crate::session::CancelTrigger::CtrlC),
                        history: if mode == "rewind" {
                            crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                                prompt_id: Some("cancel-k0".into()),
                            }
                        } else {
                            crate::session::CancelHistoryDisposition::Keep
                        },
                    })
                    .await;
                assert!(
                    outcome.turn_stopped,
                    "{mode} cancels immediately without a native plan"
                );
            }
            "drop" => {
                shell.actor.state.lock().await.notifications_suppressed = true;
                shell
                    .actor
                    .state
                    .lock()
                    .await
                    .pending_notifications
                    .push(PendingNotification {
                        prompt_id: "monitor-pending-k0".into(),
                        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "pending monitor",
                        ))],
                        priority: NotificationPriority::Next,
                        source: NotificationSource::MonitorEvent {
                            task_id: "watch-k0".into(),
                        },
                    });
                shell
                    .actor
                    .state
                    .lock()
                    .await
                    .pending_notifications
                    .push(PendingNotification {
                        prompt_id: "monitor-completed-k0".into(),
                        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "task completion",
                        ))],
                        priority: NotificationPriority::Later,
                        source: NotificationSource::MonitorCompleted {
                            task_id: "watch-k0".into(),
                        },
                    });
                let commands = shell.start_loop();
                commands
                    .send(SessionCommand::DropMonitorNotifications {
                        task_id: "watch-k0".into(),
                    })
                    .unwrap();
                let (respond_to, response_rx) = tokio::sync::oneshot::channel();
                commands
                    .send(SessionCommand::SnapshotClientHooks { respond_to })
                    .unwrap();
                tokio::time::timeout(K0_TIMEOUT, response_rx)
                    .await
                    .unwrap()
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let state = shell.actor.state.lock().await;
        let pending: Vec<_> = state
            .pending_notifications
            .iter()
            .map(|notification| {
                json!({
                    "source": match &notification.source {
                        NotificationSource::MonitorEvent { .. } => "monitor_event",
                        NotificationSource::MonitorCompleted { .. } => "monitor_completed",
                        NotificationSource::BashTaskCompleted { .. } => "bash_completed",
                    },
                    "task": notification.source.task_id(),
                    "blocks": notification.prompt_blocks,
                })
            })
            .collect();
        let buffer_len = shell
            .actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .len();
        assert_eq!(buffer_len, 0, "{mode} consumes the owned monitor buffer");
        observations.insert(
            mode.into(),
            json!({
                "pending": pending, "buffer_len": buffer_len,
                "running": state.running_task.is_some(),
                "idle_observed": shell.idle.count.get() > 0,
            }),
        );
    }
    assert_eq!(
        server.messages_request_count(),
        0,
        "suppressed or consumed monitors never start an unsolicited turn"
    );
    Value::Object(observations)
}

#[derive(Debug, Clone)]
struct K0ReadTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    admission: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for K0ReadTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Read
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::GrokBuild
    }
    fn description_template(&self) -> &str {
        "Read the offline account fixture"
    }
}

impl xai_tool_runtime::Tool for K0ReadTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("k0_read").unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new("k0_read", "Read the offline account fixture")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        _: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut output = xai_grok_tools::types::output::MCPOutput::okay_output(
            "k0_read".into(),
            "k0".into(),
            "account fixture".into(),
        );
        if call == 0 {
            let (ack, received) = tokio::sync::oneshot::channel();
            self.admission.send(ack).unwrap();
            tokio::time::timeout(K0_TIMEOUT, received)
                .await
                .unwrap()
                .unwrap();
            use base64::Engine as _;
            let image = image::ImageBuffer::from_pixel(32, 32, image::Rgba([80u8, 60, 40, 255]));
            let mut png = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            output
                .extracted_images
                .push(xai_grok_tools::util::base64_images::ExtractedImage {
                    data: base64::engine::general_purpose::STANDARD.encode(png),
                    mime_type: "image/png".into(),
                });
        }
        Ok(xai_grok_tools::types::output::ToolOutput::MCP(output))
    }
}

use xai_grok_tools::implementations::grok_build::task::coordinator as k0_coordinator;
use xai_grok_tools::implementations::grok_build::task::types as k0_task;

type K0Delivery = (
    k0_task::ActiveAgentMessageDelivery,
    tokio::sync::oneshot::Sender<k0_coordinator::ActiveMessageAdmission>,
);

#[derive(Clone)]
struct K0SteerControl(tokio::sync::mpsc::UnboundedSender<K0Delivery>);

impl k0_coordinator::ChildControl for K0SteerControl {
    type ProgressFuture = std::future::Ready<k0_coordinator::SubagentProgress>;
    fn progress(&self) -> Self::ProgressFuture {
        std::future::ready(k0_coordinator::SubagentProgress::default())
    }
    fn send_active_message(
        &self,
        delivery: k0_task::ActiveAgentMessageDelivery,
    ) -> k0_coordinator::SendBoxFuture<k0_coordinator::ActiveMessageAdmission> {
        let (reply, received) = tokio::sync::oneshot::channel();
        self.0.send((delivery, reply)).unwrap();
        Box::pin(async move { received.await.unwrap() })
    }
    fn cancel(&self) {}
}

struct K0SteerRunner {
    control: K0SteerControl,
    started: std::rc::Rc<tokio::sync::Notify>,
}

impl k0_coordinator::ChildRunner for K0SteerRunner {
    type Control = K0SteerControl;
    type CompletionData = ();
    type RunFuture = k0_coordinator::LocalBoxFuture<k0_coordinator::ChildRunOutput<()>>;
    type ValidateFuture = std::future::Pending<k0_task::SubagentValidateTypeOutcome>;
    type DescribeFuture = std::future::Pending<k0_task::SubagentDescribeOutcome>;

    fn run(&self, request: k0_coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let control = self.control.clone();
        let started = self.started.clone();
        Box::pin(async move {
            assert!(
                request
                    .reporter
                    .started(k0_coordinator::StartedChild {
                        child_session_id: "test-actor".into(),
                        persona: None,
                        resumed_from: None,
                        child_cwd: "/test/k0-workspace".into(),
                        worktree_path: None,
                        effective_model_id: "test".into(),
                        definition_background: false,
                        control,
                    })
                    .await
            );
            started.notify_one();
            std::future::pending().await
        })
    }
    fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
        std::future::pending()
    }
    fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
        std::future::pending()
    }
    fn supports_wake(&self) -> bool {
        false
    }
    fn on_completed(
        &self,
        _: k0_coordinator::ChildCompletion<()>,
        published: Box<dyn FnOnce() + Send>,
    ) {
        published();
    }
}

struct K0Abort(tokio::task::AbortHandle);
impl Drop for K0Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn k0_admit_steer(actor: &Arc<SessionActor>) {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackend,
    };
    let (deliveries, mut delivery_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let runner = K0SteerRunner {
        control: K0SteerControl(deliveries),
        started: started.clone(),
    };
    let (commands, receiver) = k0_coordinator::SubagentCoordinator::<K0SteerRunner>::channel();
    let task = tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(receiver, runner, Default::default())
            .run(),
    );
    let _coordinator_guard = K0Abort(task.abort_handle());
    let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
    commands
        .send(k0_task::SubagentEvent::Spawn(
            k0_task::SubagentSpawnRequest {
                request: Box::new(k0_task::SubagentRequest {
                    id: "k0-child".into(),
                    prompt: "unused in-process runner".into(),
                    description: "Steer fixture".into(),
                    subagent_type: "fixture".into(),
                    parent_session_id: "parent-k0".into(),
                    parent_prompt_id: None,
                    resume_from: None,
                    cwd: None,
                    runtime_overrides: Default::default(),
                    run_in_background: true,
                    surface_completion: false,
                    await_to_completion: false,
                    fork_context: false,
                    context: Default::default(),
                    owner: Default::default(),
                    cancel_token: Default::default(),
                    spawn_root: Default::default(),
                }),
                result_tx,
                registered_tx: None,
            },
        ))
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, started.notified())
        .await
        .unwrap();
    let actor = actor.clone();
    let delivery_task = tokio::task::spawn_local(async move {
        let (delivery, respond_to) = delivery_rx.recv().await.unwrap();
        let (receipts, mut receipt_rx) = tokio::sync::mpsc::channel(1);
        let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        actor
            .admit_parent_agent_message(
                delivery,
                receipts,
                xai_grok_telemetry::TelemetryCtx::new(
                    "parent-k0".into(),
                    Arc::new(tokio::sync::Mutex::new(0)),
                ),
                respond_to,
                completion,
            )
            .await;
        receipt_rx
            .recv()
            .await
            .expect("Steer admission reserves its receipt");
    });
    let _delivery_guard = K0Abort(delivery_task.abort_handle());
    let backend = ChannelBackend::for_coordinator_session(commands, "parent-k0");
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        backend.send_active_message(
            k0_task::ActiveAgentMessageRequest::try_new_with_operation(
                "k0-child",
                "parent steer k0",
                k0_task::ActiveAgentMessageOperation::Steer,
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, k0_task::ActiveAgentMessageOutcome::Accepted { .. }),
        "real Steer admission: {outcome:?}"
    );
    delivery_task.await.unwrap();
}

async fn k0_loop_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let mut shell = K0Shell::new(&server, true).await;
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("old account request"));
    k0_seed_decoded_discovery(&shell.actor).await;
    shell.install_running("k0-main").await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (admission, mut admission_rx) = tokio::sync::mpsc::unbounded_channel();
    shell.actor.agent.borrow().tool_bridge().register_mcp_tools(
        "k0_read".into(), K0ReadTool { calls: calls.clone(), admission },
        Some(json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]})),
    ).unwrap();
    let actor = shell.actor.clone();
    shell.tasks.push(tokio::task::spawn_local(async move {
        let ack = admission_rx
            .recv()
            .await
            .expect("real tool dispatch reaches admission barrier");
        actor.pending_interjections.push(PendingInterjection {
            text: "human interjection k0".into(),
            attachments: Vec::new(),
        });
        k0_admit_steer(&actor).await;
        actor
            .pending_skill_reminders
            .lock()
            .push(ConversationItem::system_reminder("skill reminder k0"));
        actor
            .mcp_announcements
            .lock()
            .fingerprints
            .insert("departed-k0".into(), (1, 2, 3));
        actor
            .mcp_reminder_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
        actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .push_capped(
                MonitorEventNotification {
                    task_id: "loop-monitor-k0".into(),
                    event_text: "loop-top monitor k0".into(),
                    owner_session_id: Some(actor.session_id_string()),
                },
                32,
            );
        ack.send(()).unwrap();
    }));
    for ordinal in 0..4 {
        server.enqueue_response("/v1/messages", k0_script(vec![json!({
            "type":"tool_use","id":format!("client-k0-{ordinal}"),"name":"k0_read","input":{"query":"account"}
        })], "tool_use"));
    }
    server.enqueue_response(
        "/v1/messages",
        k0_script(
            vec![json!({"type":"text","text":"k0 complete"})],
            "end_turn",
        ),
    );
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        shell.actor.handle_prompt(
            "k0-main",
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                "continue the account request",
            ))],
            PromptMode::Agent,
            None,
            None,
            None,
            None,
            true,
            false,
            None,
            None,
            None,
        ),
    )
    .await
    .expect("bounded real no-plan shell turn");
    assert!(
        outcome.is_ok(),
        "no-plan tool-round path succeeds: {outcome:?}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        4,
        "tool dispatch: outcome={outcome:?}; requests={:?}; history={:?}",
        server.request_bodies(),
        shell.actor.chat_state_handle.get_conversation().await
    );
    assert_eq!(
        server.messages_request_count(),
        5,
        "stationarity nudge reaches the next request"
    );
    assert!(shell.actor.pending_interjections.is_empty());
    assert!(shell.actor.pending_skill_reminders.lock().is_empty());
    assert_eq!(
        shell
            .actor
            .tool_context
            .monitor_event_buffer
            .as_ref()
            .unwrap()
            .len(),
        0
    );
    assert!(
        !shell
            .actor
            .mcp_reminder_dirty
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let history = shell.actor.chat_state_handle.get_conversation().await;
    let bodies = server.request_bodies();
    let observation = json!({
        "bodies": bodies,
        "tool_calls": calls.load(std::sync::atomic::Ordering::SeqCst),
        "memory_injection_count": shell.actor.memory.injection_count.load(std::sync::atomic::Ordering::Relaxed),
        "memory_context_injected": shell.actor.memory.context_injected.load(std::sync::atomic::Ordering::Relaxed),
        "discovery_survives": history.iter().any(|item| matches!(item, ConversationItem::Discovery { .. })),
        "interjections_empty": shell.actor.pending_interjections.is_empty(),
        "skills_empty": shell.actor.pending_skill_reminders.lock().is_empty(),
        "monitor_empty": shell.actor.tool_context.monitor_event_buffer.as_ref().unwrap().is_empty(),
        "mcp_dirty": shell.actor.mcp_reminder_dirty.load(std::sync::atomic::Ordering::Relaxed),
    });
    k0_normalize_paths(observation, shell._workspace.path())
}

fn k0_normalize_paths(value: Value, workspace: &std::path::Path) -> Value {
    match value {
        Value::String(text) => {
            Value::String(text.replace(&workspace.to_string_lossy().to_string(), "<k0-workspace>"))
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| k0_normalize_paths(value, workspace))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, k0_normalize_paths(value, workspace)))
                .collect(),
        ),
        value => value,
    }
}

// Match the pinned chat-state identity lookup, including its 1024-byte Unix buffer.
fn k0_expected_host_user_id() -> String {
    #[cfg(unix)]
    let identity = {
        // SAFETY: getpwuid_r receives live writable buffers; its C string is copied before they drop.
        let name = unsafe {
            let mut buffer = [0u8; 1024];
            let mut passwd: libc::passwd = std::mem::zeroed();
            let mut result = std::ptr::null_mut();
            let rc = libc::getpwuid_r(
                libc::geteuid(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            );
            if rc == 0 && !passwd.pw_name.is_null() {
                std::ffi::CStr::from_ptr(passwd.pw_name)
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            } else {
                None
            }
        };
        name.or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("LOGNAME").ok())
    };
    #[cfg(not(unix))]
    let identity = std::env::var("USERNAME")
        .ok()
        .map(|name| match std::env::var("USERDOMAIN") {
            Ok(domain) => format!("{domain}\\{name}"),
            Err(_) => name,
        });
    let identity = identity.expect("K0 host identity resolves through passwd or environment");
    use sha2::Digest as _;
    format!(
        "{:x}",
        sha2::Sha256::digest(format!("grok-responses:metadata-user-id:v1:{identity}"))
    )
}

fn k0_normalize_user_id(mut observation: Value, expected_user_id: &str) -> Value {
    for body in observation["loop"]["bodies"]
        .as_array_mut()
        .expect("K0 loop request bodies")
    {
        let user_id = body
            .pointer_mut("/metadata/user_id")
            .expect("K0 request metadata.user_id is present");
        assert_eq!(
            user_id.as_str(),
            Some(expected_user_id),
            "K0 H metadata.user_id matches the account"
        );
        *user_id = json!("<k0-host-user-id>");
    }
    observation
}

async fn k0_compaction_observations() -> Value {
    let server = MockInferenceServer::start().await.unwrap();
    let shell = K0Shell::new(&server, false).await;
    shell
        .actor
        .chat_state_handle
        .replace_conversation(vec![ConversationItem::system(
            "Preserve the account investigation.",
        )]);
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("old account request"));
    k0_seed_decoded_discovery(&shell.actor).await;
    for _ in 0..8 {
        shell
            .actor
            .chat_state_handle
            .push_user_message(ConversationItem::user(
                "Preserve the account query and its constraints. ".repeat(40),
            ));
        shell
            .actor
            .chat_state_handle
            .push_model_output(ConversationItem::assistant(
                "The account investigation remains incomplete. ".repeat(40),
            ));
    }
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("continue after compaction"));
    let before = shell.actor.chat_state_handle.get_conversation().await;
    shell.actor.chat_state_handle.record_token_usage(80_000);
    let mut binding = shell
        .actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    let error = xai_grok_sampler::SamplingErrorInfo {
        kind: xai_grok_sampler::SamplingErrorKind::Api,
        status_code: Some(400),
        message: "prompt is too long".into(),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: Some(xai_grok_sampling_types::ResponseModelMetadata {
            context_window: Some(50_000),
            max_completion_tokens: None,
            models_etag: None,
        }),
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: xai_grok_sampling_types::SentCredential::Sent,
    };
    assert!(shell.actor.should_compact_on_error(&error).await);
    server.set_response(format!(
        "<summary>{}</summary>",
        "Retain the account lookup, user constraints, and pending investigation. ".repeat(20)
    ));
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        shell.actor.handle_sampling_failure(
            error,
            &mut binding,
            0,
            transient_state(0, false),
            false,
            TurnParkState::Fresh,
        ),
    )
    .await
    .expect("bounded no-plan retry compaction");
    assert!(
        matches!(outcome, Ok(SamplerFailureRecovery::CompactAndResubmit)),
        "no-plan compaction keeps its recovery arm: {:?}",
        outcome.as_ref().err()
    );
    let after = shell.actor.chat_state_handle.get_conversation().await;
    assert_ne!(
        before.len(),
        after.len(),
        "real compaction replaces history"
    );
    assert!(
        server.messages_request_count() > 0,
        "real compaction uses the controlled Messages transport"
    );
    json!({
        "recovery": "CompactAndResubmit",
        "compaction_count": shell.actor.compaction.count.load(std::sync::atomic::Ordering::Relaxed),
        "before_items": before.len(), "after_items": after.len(),
        "discovery_before": before.iter().filter(|item| matches!(item, ConversationItem::Discovery { .. })).count(),
        "discovery_after": after.iter().filter(|item| matches!(item, ConversationItem::Discovery { .. })).count(),
        "compaction_requests": server.messages_request_count(),
    })
}

fn k0_isolated_shell_test() -> bool {
    const NAME: &str =
        "session::acp_session::native_messages_replay_tests::messages_no_plan_preserves_pin_shell";
    const CHILD: &str = "GROK_TEST_K0_SHELL_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(NAME) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env(CHILD, NAME)
        .env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG")
        .env_remove("GROK_CONFIG_PATH")
        .env_remove("GROK_CAMPAIGNS")
        .env_remove("GROK_CAMPAIGNS_OVERRIDE")
        .env_remove("XAI_API_KEY")
        .env_remove("APEX_LLM_API_KEY")
        .env_remove("APEX_LLM_PROXY_KEY")
        .args(["--exact", NAME, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated K0 shell failed: stdout={stdout:?}; stderr={stderr:?}"
    );
    assert!(
        stdout.contains("running 1 test") && stdout.contains(&format!("test {NAME} ... ok")),
        "exact isolated selection: {stdout}"
    );
    for line in stdout.lines().filter(|line| line.starts_with("K0_PIN_H ")) {
        println!("{line}");
    }
    true
}

#[tokio::test(flavor = "current_thread")]
async fn messages_no_plan_preserves_pin_shell() {
    if k0_isolated_shell_test() {
        return;
    }
    tokio::task::LocalSet::new().run_until(async {
        let observed = json!({
            "loop": k0_loop_observations().await,
            "monitors": k0_monitor_observations().await,
            "compaction": k0_compaction_observations().await,
        });
        // Frozen from the isolated bb3a3f50 shell observation.
        let expected: Value = serde_json::from_str(r###"{"compaction":{"after_items":4,"before_items":21,"compaction_count":1,"compaction_requests":1,"discovery_after":0,"discovery_before":1,"recovery":"CompactAndResubmit"},"loop":{"bodies":[{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"cache_control":{"type":"ephemeral"},"text":"continue the account request","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"cache_control":{"type":"ephemeral"},"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-2","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-2","type":"tool_result"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]},{"max_tokens":4096,"messages":[{"content":[{"text":"old account request","type":"text"}],"role":"user"},{"content":[{"text":"continue the account request","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-0","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-0","type":"tool_result"},{"text":"[Image extracted from tool result above]","type":"text"},{"source":{"data":"iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAqElEQVR4Ae3AA6AkWZbG8f937o3IzKdyS2Oubdu2bdu2bdu2bWmMnpZKr54yMyLu+Xa3anqmhztr1a++9as/2vz3ofLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/72o/Pei8t+Lyn8vKv+9qPz3ovLfi8p/Lyr/vaj896Ly34vKfy8q/734R5tVAjS+u+QnAAAAAElFTkSuQmCC","media_type":"image/png","type":"base64"},"type":"image"},{"text":"The user sent a message while you were working:\n<user_query>\nhuman interjection k0\n</user_query>\nMake sure to complete any unfinished tasks from previous turns.","type":"text"},{"text":"[Message authored by another agent; not a human request or approval.]","type":"text"},{"text":"parent steer k0","type":"text"},{"text":"skill reminder k0","type":"text"},{"text":"<system-reminder>\n<monitor-event task_id=\"loop-monitor-k0\">\n[event] loop-top monitor k0\n</monitor-event>\n</system-reminder>","type":"text"},{"text":"<system-reminder>\nAll MCP servers have disconnected.\nTo use MCP tools, you MUST call `` first to retrieve the tool's input schema before calling ``. NEVER guess parameter names — always use the exact schema returned by ``.\n</system-reminder>","type":"text"}],"role":"user"},{"content":[{"id":"client-k0-1","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-1","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-2","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"cache_control":{"type":"ephemeral"},"content":"account fixture","tool_use_id":"client-k0-2","type":"tool_result"}],"role":"user"},{"content":[{"id":"client-k0-3","input":{"query":"account"},"name":"k0_read","type":"tool_use"}],"role":"assistant"},{"content":[{"content":"account fixture","tool_use_id":"client-k0-3","type":"tool_result"},{"cache_control":{"type":"ephemeral"},"text":"<system-reminder>\nYou have called the same tool (`k0_read`) with the exact same arguments 4 times in a row — you appear to be stuck in a polling loop. Stop repeating this call. If you are waiting on a long-running job or command, use a background task, or run a single `sleep` and then check once — do not poll in a tight loop. If you cannot make progress, stop and tell the user what you are waiting for. This turn will be halted automatically if the identical call keeps repeating.\n</system-reminder>","type":"text"}],"role":"user"}],"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"},"model":"test","stream":true,"system":[{"cache_control":{"type":"ephemeral"},"text":"<memory-context>\n## Global memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/global`\n\n# Global memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n## Workspace memory manifest\n**Scope root:** `<k0-workspace>/memory-v2/workspaces/k0-workspace-244ae27a`\n\n# Workspace memory index\n\n> Generated by Grok. Do not edit this file directly.\n\nNo memory files have been recorded yet.\n\n</memory-context>","type":"text"}],"tools":[{"description":"Read the offline account fixture","input_schema":{"properties":{"query":{"type":"string"}},"required":["query"],"type":"object"},"name":"k0_read"}]}],"discovery_survives":true,"interjections_empty":true,"mcp_dirty":false,"memory_context_injected":true,"memory_injection_count":2,"monitor_empty":true,"skills_empty":true,"tool_calls":4},"monitors":{"cancel":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"completion":{"buffer_len":0,"idle_observed":true,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"drop":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"task completion","type":"text"}],"source":"monitor_completed","task":"watch-k0"}],"running":false},"idle":{"buffer_len":0,"idle_observed":true,"pending":[],"running":false},"rewind":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false},"user-start":{"buffer_len":0,"idle_observed":false,"pending":[{"blocks":[{"text":"<monitor-event description=\"k0\" task_id=\"watch-k0\">\nmonitor payload\n</monitor-event>","type":"text"}],"source":"monitor_event","task":"watch-k0"}],"running":false}}}"###).unwrap();
        let observed = k0_normalize_user_id(observed, &k0_expected_host_user_id());
        let captured_user_id = expected["loop"]["bodies"][0]["metadata"]["user_id"]
            .as_str()
            .expect("frozen K0 capture account")
            .to_owned();
        let expected = k0_normalize_user_id(expected, &captured_user_id);
        assert_eq!(observed, expected, "K0 H parent shell parity");
    }).await;
}

use xai_grok_sampling_types::conversation::native_search_history::*;
use xai_grok_sampling_types::conversation::projection::{
    Boundary, DiscoveryOrigin, NativeHistoryAction, NativeQualification, NativeReplayDialect,
    NativeReplayDomain, NativeReplayShape, TargetRoute,
};
use xai_grok_sampling_types::request_validation::NativeHistoryRequest;

fn native_content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn k1_prepare(
    client: &SamplingClient,
    mut request: ConversationRequest,
    endpoint: &str,
    streaming: bool,
) -> ConversationRequest {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use xai_grok_sampling_types::ConversationItem as Item;
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    use serde_json::{Value, json};

    let mut blobs = BTreeMap::<String, Arc<[u8]>>::new();
    let mut put = |kind: &str, value: Value| {
        let bytes = serde_json::to_vec(&value).unwrap();
        let reference = NativeBlobRef {
            kind: kind.into(),
            byte_len: bytes.len() as u64,
            digest: native_content_digest(&bytes),
        };
        blobs.insert(reference.digest.clone(), bytes.into());
        reference
    };
    let source_coordinate = |block| NativeSourceCoordinate {
        lineage: "k1-lineage".into(),
        producing_binding: "k1-issued-A".into(),
        turn: 1,
        item: 0,
        response: 0,
        block,
        submember: None,
        occurrence: block,
    };
    let raw = [
        json!({"type":"server_tool_use","id":"srvtoolu_k1","name":"tool_search_tool_regex","input":{"query":"private fixture query"}}),
        json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_k1","content":{"type":"tool_search_tool_search_result","tool_references":[]}}),
    ];
    let offset = request.items.len();
    let mut original = request.items.clone();
    original.extend(raw.iter().map(|value| Item::Discovery {
        item: ToolSearchItem::from_wire(value.clone()).unwrap(),
    }));
    original.push(Item::user("Continue after search."));
    let input_history_bytes: Arc<[u8]> = serde_json::to_vec(&original).unwrap().into();
    let history = NativeHistoryBoundary {
        items: original.len() as u64,
        digest: native_content_digest(&input_history_bytes),
    };
    let origin = put(
        "producing_binding",
        json!({"request":"k1-issued-A","generation":17,"origin":{"status":"Unknown"}}),
    );
    let source = NativeSource {
        schema_version: 2,
        lineage: "k1-lineage".into(),
        source_revision: 3,
        producing_bindings: vec![NativeProducingBinding {
            request: "k1-issued-A".into(),
            generation: Some(17),
            origin: json!({"status":"Unknown"}),
            origin_ref: origin.clone(),
        }],
        responses: vec![NativeResponseBoundary {
            occurrence: 0,
            request: "k1-issued-A".into(),
            lease: 1,
            attempt: 1,
            continuation: 0,
            provider_response_id: Some(json!("msg_k1")),
            source_stop_reason: Some("end_turn".into()),
            block_start: 0,
            block_end: 2,
            stream_complete: true,
            status: NativeResponseStatus::Accepted,
        }],
        ordered_blocks: raw
            .iter()
            .enumerate()
            .map(|(index, value)| NativeRawBlock {
                coordinates: source_coordinate(index as u64),
                wire_index: index as u32,
                arrival_ordinal: index as u64,
                block: put("raw_block", value.clone()),
                partial: None,
                state: NativeBlockState::ClosedFaithful,
                eligible_ref: None,
                eligible_unavailable: None,
            })
            .collect(),
        eligible_snapshots: vec![],
        older_sources: vec![],
    };
    let source_ref = put(
        "source",
        serde_json::to_value(NativeSourceV2::from_parts(source.clone(), vec![])).unwrap(),
    );
    let active_ref = put(
        "active",
        serde_json::to_value(NativeActive::default()).unwrap(),
    );
    let mut ledger = NativeLedger {
        source_digest: source_ref.digest.clone(),
        active_digest: active_ref.digest.clone(),
        groups: vec![NativeGroupRecord {
            key: "k1-completed".into(),
            family: "Messages".into(),
            execution: "server".into(),
            origin_ref: origin.clone(),
            validated_join: Some("srvtoolu_k1".into()),
            keyless_server_fifo: false,
            members: (0..2).map(source_coordinate).collect(),
            duplicate_observations: vec![],
            state: "Completed".into(),
        }],
        accounting: (0..2)
            .map(|index| NativeViewLink {
                source: source_coordinate(index),
                source_digest: source_ref.digest.clone(),
                view_item: Some(offset as u64 + index),
                view_digest: Some(native_content_digest(
                    &serde_json::to_vec(&original[offset + index as usize]).unwrap(),
                )),
                role: "Discovery".into(),
                source_subrange: None,
            })
            .collect(),
        ..Default::default()
    };
    ledger
        .lifecycle
        .groups
        .insert("k1-completed".into(), "Completed".into());
    let ledger_ref = put(
        "ledger",
        serde_json::to_value(NativeLedgerV2::from_parts(ledger.clone(), vec![])).unwrap(),
    );
    let record = NativeSearchHistoryV1 {
        schema_version: 2,
        policy_revision: 1,
        reader_floor: 2,
        limits_revision: 1,
        lineage: NativeLineage {
            id: "k1-lineage".into(),
            source_revision: 3,
            source_digest: source_ref.digest.clone(),
        },
        generation: 5,
        high_water: 7,
        source: source_ref,
        active: active_ref,
        ledger: ledger_ref,
        history_boundary: history.clone(),
        exchange_snapshot: None,
        previous_checkpoint: None,
        capacity: BTreeMap::new(),
    };
    let (normalized, _) = client.finalize_conversation_messages(&request).unwrap();
    request = normalized;
    let model = request.model.clone().unwrap();
    let admission = request
        .search_admission
        .unwrap_or(xai_grok_sampling_types::SearchAdmission {
            supports_search_tool: false,
            has_searchable_tools: false,
        });
    request.search_admission = Some(admission);
    let domain = NativeReplayDomain {
        backend: ApiBackend::Messages,
        dialect: NativeReplayDialect::Messages,
        model: model.clone(),
        row: model.clone(),
        provider: "offline".into(),
        deployment: "offline".into(),
        deployment_revision: "1".into(),
        endpoint: endpoint.into(),
        principal: "fixture".into(),
    };
    let route = TargetRoute::new(ApiBackend::Messages, Boundary::Vertex, admission);
    let selected: Vec<_> = (0..2).map(source_coordinate).collect();
    let identity = NativeIdentityExtension::Revision2 {
        eligible_member_links: vec![],
        eligible_member_accounting: vec![],
    };
    let authorship = (0..2)
        .map(|index| (index, NativeIdAuthorship::Provider))
        .collect();
    let core = prepare_native_history_core(
        &NativePureHistory {
            record: &record,
            source: &source,
            ledger: &ledger,
            identity: &identity,
            blobs: &blobs,
            authorship: &authorship,
        },
        &NativeProjectionInputs {
            target_model_id: &model,
            target: &route,
            tools: &request.tools,
            current: None,
            selected: &selected,
            origin_domains: &Default::default(),
            pending: &Default::default(),
            prior_f: None,
        },
        |_| {
            (
                DiscoveryOrigin {
                    boundary: Boundary::Vertex,
                    owner_model_id: None,
                },
                NativeQualification {
                    source: None,
                    target: Some(domain.clone()),
                    request: Some(domain.clone()),
                    original_admission: admission,
                    shape: NativeReplayShape {
                        family: NativeReplayFamily::MessagesServer,
                        keyless_server: false,
                        output_id_present: false,
                        completed: true,
                        result_error: false,
                        referenced_members: 0,
                    },
                    receipt: None,
                    intact_membership: true,
                    current_backing: false,
                    faithful_identity: true,
                },
            )
        },
    )
    .unwrap();
    assert_eq!(core.decision.groups.len(), 1);
    assert_eq!(core.decision.groups[0].action, NativeHistoryAction::F);
    assert_eq!(core.accounting.qualification, core.decision.qualification);
    assert_eq!(core.accounting.raw_members.len(), 2);
    assert!(core.accounting.references.is_empty());
    assert_eq!(core.decision.members.len(), 2);
    assert_eq!(core.accounting.groups.demoted, 1);
    assert_eq!(core.accounting.members.demoted, 2);
    assert_eq!(
        core.decision.members[0].neutral.as_ref().unwrap().kind,
        NativeNeutralKind::Call
    );
    assert_eq!(
        core.decision.members[1].neutral.as_ref().unwrap().kind,
        NativeNeutralKind::Result
    );
    assert!(core.decision.members.iter().all(|member| {
        member
            .neutral
            .as_ref()
            .is_some_and(|text| text.text.len() <= 128)
    }));
    assert!(
        core.decision
            .members
            .iter()
            .map(|member| member.neutral.as_ref().unwrap().text.len())
            .sum::<usize>()
            <= 256
    );
    let mut replacements = BTreeMap::new();
    for member in &core.decision.members {
        let link = ledger
            .accounting
            .iter()
            .find(|link| link.source == member.source)
            .unwrap();
        assert!(member.raw.is_none());
        let output = member
            .neutral
            .as_ref()
            .map(|neutral| Item::assistant(neutral.text.clone()));
        assert!(
            replacements
                .insert(link.view_item.unwrap() as usize, output)
                .is_none()
        );
    }
    request.items = original
        .into_iter()
        .enumerate()
        .flat_map(|(index, item)| replacements.remove(&index).unwrap_or(Some(item)))
        .collect();
    assert!(replacements.is_empty());
    let (normalized, streaming_carrier) = client.finalize_conversation_messages(&request).unwrap();
    request = normalized;
    let encoded = if streaming {
        streaming_carrier
    } else {
        xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(
            &xai_grok_sampling_types::build_messages_request(&request),
        )
        .unwrap()
    };
    let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
    let declarations: Vec<Value> = carrier
        .declarations()
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .unwrap_or_default();
    let identities = declarations
        .into_iter()
        .enumerate()
        .filter_map(|(index, declaration)| {
            let schema_value = declaration.get("input_schema")?;
            let schema_bytes: Arc<[u8]> = serde_json::to_vec(schema_value).unwrap().into();
            let schema = NativeBlobRef {
                kind: "eligible_schema".into(),
                byte_len: schema_bytes.len() as u64,
                digest: native_content_digest(&schema_bytes),
            };
            let snapshot = NativeEligibleSnapshot {
                version: 1,
                namespace_kind: NativeNamespaceKind::GrokBuild,
                qualified_namespace: "grok_build".into(),
                child_name: declaration["name"].as_str().unwrap().into(),
                schema: schema.clone(),
                binding: origin.clone(),
            };
            let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
            Some(NativeDeclarationIdentity {
                declaration_index: index as u64,
                child_index: None,
                namespace_kind: snapshot.namespace_kind,
                qualified_namespace: snapshot.qualified_namespace,
                child_name: snapshot.child_name,
                declaration,
                schema,
                schema_bytes,
                provenance: NativeBlobRef {
                    kind: "eligible_snapshot".into(),
                    byte_len: provenance_bytes.len() as u64,
                    digest: native_content_digest(&provenance_bytes),
                },
                provenance_bytes,
                binding_bytes: Some(blobs[&origin.digest].clone()),
            })
        })
        .collect();
    let context = NativeHistoryRequestContext::captured(
        NativeHistoryRequirement::Selected,
        NativeHistoryCapture {
            history,
            history_revision: 11,
            lineage: record.lineage.clone(),
            native_generation: 5,
            high_water: 7,
            projection_revision: 0,
            strict_append_sequence: 13,
            binding_generation: 17,
            install_generation: 19,
            issued_request: "k1-issued-A".into(),
            target: domain,
            execution: NativeHistoryExecution::Server,
            original_admission: admission,
            versions: NativeHistoryVersions {
                schema: 2,
                policy: 1,
                projection: 1,
                reader: 2,
                limits: 1,
            },
        },
        NativeHistoryWriteAuthority::SendOnly,
    );
    let plan = NativeHistoryPlan::checked(
        context.clone(),
        NativeHistoryPlanInput {
            ledger_bytes: blobs[&record.ledger.digest].clone(),
            record,
            input_history_bytes,
            lowering_request: &request,
            post_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            carrier,
            identities,
            qualification: core.accounting.qualification.clone(),
            core,
        },
    )
    .unwrap();
    assert_eq!(plan.readiness(), &NativePlanReadiness::Tentative);
    assert_eq!(
        validate_native_history_plan(&context, Some(&plan), plan.carrier()),
        Err(NativePlanRefusal::NotReady)
    );
    let plan = plan.send_only().unwrap();
    assert_eq!(
        validate_native_history_plan(&context, Some(&plan), plan.carrier()),
        Ok(NativeHistoryReady::SendOnly)
    );
    let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
    assert_eq!(body["max_tokens"], request.max_output_tokens.unwrap());
    assert_eq!(body.get("stream"), streaming.then_some(&Value::Bool(true)));
    request.encoded = streaming.then_some(encoded);
    request.native_history = Some(NativeHistoryRequest::new(context, Some(plan)));
    request
}

use xai_grok_sampler::{ApiBackend, SamplingClient, SamplerConfig};

struct K1NativePersistence {
    ordinary: xai_chat_state::NullChatPersistence,
    capture: bool,
}

impl xai_chat_state::ChatPersistence for K1NativePersistence {
    fn persist_message(&mut self, item: &ConversationItem) {
        self.ordinary.persist_message(item);
    }
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::StrictAppendAck, xai_chat_state::StrictAppendError>,
    > {
        self.ordinary.persist_working_directory_switch_and_ack(item)
    }
    fn replace_history(&mut self, items: &[ConversationItem]) {
        self.ordinary.replace_history(items);
    }
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> tokio::sync::oneshot::Receiver<std::io::Result<()>> {
        self.ordinary.replace_history_for_strip_and_ack(items)
    }
    fn flush(&mut self) {
        self.ordinary.flush();
    }
    fn read_native_history(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeRead, NativeUnavailable>,
    > {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(if self.capture {
            Ok(xai_chat_state::persistence::native::NativeRead {
                lineage: "test-actor".into(),
                checkpoint: None,
            })
        } else {
            Err(NativeUnavailable::Persistence)
        });
        rx
    }
}

struct K1Shell {
    actor: Arc<SessionActor>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    evidence: std::rc::Rc<std::cell::RefCell<Vec<(bool, u64)>>>,
    _workspace: tempfile::TempDir,
    _model_home: tempfile::TempDir,
}

impl K1Shell {
    async fn new(base_url: &str, capture: bool) -> Self {
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                    let _ = args.response_tx.send(Ok(()));
                }
            }
        });
        let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        let persistence = tokio::task::spawn_local(async move {
            while let Some(message) = persistence_rx.recv().await {
                if let PersistenceMsg::FlushAndAck { respond_to } = message {
                    let _ = respond_to.send(Ok(()));
                }
            }
        });
        let (mut actor, _events) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence_tx,
            Box::new(K1NativePersistence {
                ordinary: xai_chat_state::NullChatPersistence,
                capture,
            }),
        )
        .await;
        let workspace = tempfile::tempdir().unwrap();
        actor.hook_resolved_workspace_root = workspace.path().to_string_lossy().into_owned();
        actor
            .last_live_orphan_reconcile
            .set(Some(std::time::Instant::now()));
        actor.max_retries = 0;
        actor.transient_retry_enabled = false;
        actor.mcp_reminder_mode = McpReminderMode::Full;
        let (manager, model_home) = manager_with_entries();
        let mut entry = entry_with_search_flag("test", false);
        entry.info.api_backend = ApiBackend::Messages;
        entry.info.context_window = std::num::NonZeroU64::new(256_000).unwrap();
        entry.api_base_url = Some(base_url.into());
        entry.api_key = Some("test-key".into());
        manager.insert_test_entry("test", entry);
        manager.set_current_model_id(acp::ModelId::new("test"));
        actor.models_manager = manager;
        actor.memory.flush_config.enabled = false;
        actor.memory.dream_config.enabled = false;
        actor.memory.save_on_end = false;
        let agent = {
            let old = actor.agent.borrow();
            let mut policy = old.compaction_policy().clone();
            policy.two_pass_enabled = false;
            policy.remote_compaction_v2 = false;
            xai_grok_agent::Agent::new(
                old.definition().clone(),
                old.prompt_context().clone(),
                old.system_prompt().to_string(),
                old.tool_bridge().clone(),
                old.reminder_policy().clone(),
                policy,
                vec![],
                false,
            )
        };
        actor.agent.replace(agent);
        let mut config = actor.chat_state_handle.get_sampling_config().await.unwrap();
        config.base_url = base_url.into();
        config.model = "test".into();
        config.api_backend = ApiBackend::Messages;
        config.max_completion_tokens = Some(4096);
        config.max_retries = Some(0);
        actor.chat_state_handle.update_sampling_config(config);
        let mut credentials = actor.chat_state_handle.get_credentials().await;
        credentials.api_key = Some("test-key".into());
        actor.chat_state_handle.update_credentials(credentials);
        let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            k1_shell_config(base_url),
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            events,
        );
        actor
            .workspace_ops
            .bind_local_session(
                &actor.session_id_string(),
                actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(),
                None,
            )
            .unwrap();
        let actor = Arc::new(actor);
        let drainer = actor.clone();
        let evidence = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = evidence.clone();
        let sampler = tokio::task::spawn_local(async move {
            while let Some(event) = event_rx.recv().await {
                if let xai_grok_sampler::SamplingEvent::NativeAttemptEvidence { evidence, .. } =
                    &event
                {
                    observed.borrow_mut().push(match evidence {
                        NativeEvidenceEvent::Attempt(attempt) => (false, attempt.http_dispatches),
                        NativeEvidenceEvent::End => (true, 0),
                    });
                }
                drainer.handle_sampling_event(event).await;
            }
        });
        Self {
            actor,
            tasks: vec![gateway, persistence, sampler],
            evidence,
            _workspace: workspace,
            _model_home: model_home,
        }
    }

    async fn finish(self) {
        assert_eq!(self.actor.sampler_handle.active_count().await, 0);
        for task in self.tasks {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }
}

fn k1_shell_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        base_url: base_url.into(),
        model: "test".into(),
        api_key: Some("test-key".into()),
        api_backend: ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(0),
        ..Default::default()
    }
}

async fn k1_shell_http() -> (
    String,
    Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    tokio::task::JoinHandle<()>,
) {
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let router = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |body: axum::body::Bytes| {
            let captured = captured.clone();
            async move {
                let value: Value = serde_json::from_slice(&body).unwrap();
                captured.lock().unwrap().push(body.to_vec());
                let (content_type, response) = if value.get("stream") == Some(&Value::Bool(true)) {
                    (
                        "text/event-stream",
                        k0_frames(
                            vec![json!({"type":"text","text":"K1 complete"})],
                            "end_turn",
                        )
                        .into_iter()
                        .map(|event| format!("data: {event}\n\n"))
                        .collect::<String>(),
                    )
                } else {
                    (
                        "application/json",
                        json!({"id":"msg_k1","type":"message","role":"assistant","model":"test",
                    "content":[{"type":"text","text":"K1 complete"}],"stop_reason":"end_turn",
                    "stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":3}})
                        .to_string(),
                    )
                };
                axum::response::Response::builder()
                    .header("content-type", content_type)
                    .body(axum::body::Body::from(response))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, bodies, server)
}
async fn k1_shell_actor_case(
    base_url: &str,
    bodies: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    capture: bool,
    case: &str,
) {
    let shell = K1Shell::new(base_url, capture).await;
    shell
        .actor
        .chat_state_handle
        .push_user_message(ConversationItem::user("K1 bound shell request"));
    let before = bodies.lock().unwrap().len();
    let builds = std::cell::Cell::new(0usize);
    let expected = std::cell::RefCell::new(Vec::new());
    let mut prepare = |request: &mut ConversationRequest,
                       binding: &xai_chat_state::RequestTransportBinding| {
        builds.set(builds.get() + 1);
        assert_eq!(binding.config.base_url, base_url);
        assert_eq!(binding.config.api_backend, ApiBackend::Messages);
        let client = SamplingClient::new(k1_shell_config(&binding.config.base_url)).unwrap();
        *request = k1_prepare(
            &client,
            request.clone(),
            &format!("{base_url}/messages"),
            true,
        );
        *expected.borrow_mut() = request.encoded.as_ref().unwrap().as_bytes().to_vec();
        match case {
            "valid" | "install-B-retain-A" => {}
            "missing-plan" => request.native_history.as_mut().unwrap().invalidate_plan(),
            "stale-request" => request.max_output_tokens = Some(4097),
            "stale-context" => {
                let native = request.native_history.as_ref().unwrap();
                let mut context = native.context().capture().clone();
                context.history_revision += 1;
                request.native_history = Some(NativeHistoryRequest::new(
                    NativeHistoryRequestContext::captured(
                        NativeHistoryRequirement::Selected,
                        context,
                        NativeHistoryWriteAuthority::SendOnly,
                    ),
                    native.plan().cloned(),
                ));
            }
            "wrong-banked-body" => {
                let mut changed = request.clone();
                changed.max_output_tokens = Some(4098);
                request.encoded = Some(client.encode_conversation_messages(&changed).unwrap());
            }
            _ => unreachable!(),
        }
        if case == "install-B-retain-A" {
            let mut next = binding.config.clone();
            next.model = "installed-B".into();
            next.base_url = "http://127.0.0.1:1/not-A".into();
            shell.actor.chat_state_handle.update_sampling_config(next);
        }
        Ok(())
    };
    let mut salvage = super::length_salvage::LengthSalvage::new(None);
    let mut sampling = Default::default();
    let result = tokio::time::timeout(
        K0_TIMEOUT,
        shell
            .actor
            .process_conversation_turn_for_native_history_test(
                "k1-shell-turn",
                None,
                None,
                None,
                &mut salvage,
                &mut sampling,
                &mut prepare,
            ),
    )
    .await
    .expect("K1 shell turn terminates");
    assert_eq!(builds.get(), 1, "one real bound build; no fixture rebuild");
    let success = matches!(case, "valid" | "install-B-retain-A");
    {
        let bodies = bodies.lock().unwrap();
        if success {
            assert!(
                matches!(result, Ok(TurnOutcome::Completed { .. })),
                "K1 shell positive: {:?}",
                result.err()
            );
            assert_eq!(bodies.len(), before + 1);
            assert_eq!(
                bodies[before],
                *expected.borrow(),
                "K1 exact actual shell HTTP bytes"
            );
        } else {
            assert_eq!(
                bodies.len(),
                before,
                "K1 shell forbidden HTTP: capture={capture}/{case}"
            );
            assert!(
                result.is_err(),
                "K1 shell typed refusal surfaces through normal completion"
            );
        }
    }
    let evidence = shell.evidence.borrow();
    assert_eq!(
        evidence.iter().filter(|(end, _)| *end).count(),
        usize::from(capture)
    );
    assert_eq!(
        evidence
            .iter()
            .map(|(_, dispatches)| dispatches)
            .sum::<u64>(),
        u64::from(capture && success)
    );
    println!(
        "K1_SHELL_CASE actor capture={capture} {case} builds={} http_delta={} ends={}",
        builds.get(),
        bodies.lock().unwrap().len() - before,
        evidence.iter().filter(|(end, _)| *end).count()
    );
    drop(evidence);
    shell.finish().await;
}

async fn k1_shell_direct_cases(base_url: &str, bodies: &Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
    let client = SamplingClient::new(k1_shell_config(base_url)).unwrap();
    for route in ["direct", "collect", "non-streaming", "compact-helper"] {
        let prepared = k1_prepare(
            &client,
            ConversationRequest::from_items(vec![ConversationItem::user("K1 direct shell")]),
            &format!("{base_url}/messages"),
            route != "non-streaming",
        );
        let expected = prepared
            .native_history
            .as_ref()
            .unwrap()
            .plan()
            .unwrap()
            .carrier()
            .bytes()
            .to_vec();
        for missing in [false, true] {
            let mut request = prepared.clone();
            request.encoded = None;
            if missing {
                request.native_history.as_mut().unwrap().invalidate_plan();
            }
            let before = bodies.lock().unwrap().len();
            let result: Result<(), String> = tokio::time::timeout(K0_TIMEOUT, async {
                match route {
                    "direct" => {
                        let (mut stream, _) = client.conversation_stream_messages(request).await.map_err(|error| error.to_string())?;
                        while let Some(event) = stream.next().await { event.map_err(|error| error.to_string())?; }
                        Ok(())
                    }
                    "collect" => client.conversation_collect(request).await.map(|_| ()).map_err(|error| error.to_string()),
                    "non-streaming" => client.conversation_messages(request).await.map(|_| ()).map_err(|error| error.to_string()),
                    "compact-helper" => {
                        let (mut stream, _) = crate::session::helpers::session_compact::send_compact_messages_request(
                            &client, request, &tokio_util::sync::CancellationToken::new(),
                        ).await.map_err(|error| format!("{error:?}"))?;
                        while let Some(event) = stream.next().await { event.map_err(|error| error.to_string())?; }
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }).await.expect("K1 shell direct transport terminates");
            let bodies = bodies.lock().unwrap();
            if missing {
                assert_eq!(
                    bodies.len(),
                    before,
                    "K1 shell direct missing proof refuses {route}"
                );
                assert!(result.is_err());
            } else {
                assert!(result.is_ok(), "K1 shell {route}: {result:?}");
                assert_eq!(bodies.len(), before + 1);
                assert_eq!(bodies[before], expected);
            }
            println!(
                "K1_SHELL_CASE {route} missing={missing} http_delta={}",
                bodies.len() - before
            );
        }
    }
}

fn k1_isolated_shell_test() -> bool {
    const NAME: &str = "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends";
    const CHILD: &str = "GROK_TEST_K1_SHELL_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(NAME) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env(CHILD, NAME)
        .env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG")
        .env_remove("GROK_CONFIG_PATH")
        .env_remove("GROK_CAMPAIGNS")
        .env_remove("GROK_CAMPAIGNS_OVERRIDE")
        .env_remove("XAI_API_KEY")
        .env_remove("APEX_LLM_API_KEY")
        .env_remove("APEX_LLM_PROXY_KEY")
        .args(["--exact", NAME, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated K1 shell failed: stdout={stdout}; stderr={stderr}"
    );
    assert!(stdout.contains("running 1 test") && stdout.contains(&format!("test {NAME} ... ok")));
    for line in stdout.lines().filter(|line| line.starts_with("K1_")) {
        println!("{line}");
    }
    true
}

#[tokio::test(flavor = "current_thread")]
async fn messages_replay_plan_checked_on_all_sends() {
    if k1_isolated_shell_test() {
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, bodies, server) = k1_shell_http().await;
            for capture in [false, true] {
                for case in [
                    "valid",
                    "missing-plan",
                    "stale-request",
                    "stale-context",
                    "wrong-banked-body",
                    "install-B-retain-A",
                ] {
                    k1_shell_actor_case(&base_url, &bodies, capture, case).await;
                }
            }
            k1_shell_direct_cases(&base_url, &bodies).await;
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        })
        .await;
}

