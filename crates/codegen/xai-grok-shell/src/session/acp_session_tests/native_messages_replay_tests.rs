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
    if k2lg_selector_prelude().await {
        return;
    }
    const CHILD_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends";
    if std::env::var("GROK_TEST_K1_SHELL_CHILD").as_deref() != Ok(CHILD_SELECTOR) {
        k2lg_pending_isolated().await;
    }
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


use xai_grok_sampling_types::conversation::native_search_history as k2_history;
use xai_grok_sampling_types::conversation::projection as k2_replay;

type K2Checkpoint = xai_chat_state::persistence::native::NativeCheckpoint;

#[derive(Clone, Debug)]
struct K2ClientTool {
    name: &'static str,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    release: Arc<tokio::sync::Semaphore>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for K2ClientTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Bounded offline K2 client tool"
    }
}

impl xai_tool_runtime::Tool for K2ClientTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, "Bounded offline K2 client tool")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        arguments: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        self.executions.lock().unwrap().push(self.name.into());
        self.entered.send(self.name.into()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Ok(xai_grok_tools::types::output::ToolOutput::Text(format!(
            "{} actual result: {}",
            self.name, arguments["value"]
        ).into()))
    }
}

struct K2ReplaySession {
    actor: Arc<SessionActor>,
    persistence: crate::session::persistence::PersistenceHandle,
    directory: tempfile::TempDir,
    _models: tempfile::TempDir,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedReceiver<String>,
    releases: [Arc<tokio::sync::Semaphore>; 2],
    source_acks: tokio::sync::mpsc::UnboundedReceiver<k2_history::NativeResponseStatus>,
}

impl Drop for K2ReplaySession {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl K2ReplaySession {
    async fn new(server: &MockInferenceServer) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = xai_grok_sampler::SamplerConfig {
            base_url: server.url(),
            model: "k2-replay".into(),
            api_key: Some("nonsecret-k2-key".into()),
            api_backend: xai_grok_sampler::ApiBackend::Messages,
            max_completion_tokens: Some(4096),
            max_retries: Some(0),
            ..Default::default()
        };
        let info = crate::session::info::Info {
            id: acp::SessionId::new(format!(
                "k2-{}",
                directory.path().file_name().unwrap().to_string_lossy()
            )),
            cwd: directory.path().to_string_lossy().into_owned(),
        };
        let persistence = crate::session::persistence::new_with_explicit_dir(
            &info,
            directory.path().to_path_buf(),
            acp::ModelId::new("k2-replay"),
            xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
            "k2-replay".into(),
            crate::session::persistence::ExplicitSessionOpen::New {
                identity: None,
                next_trace_turn: None,
            },
        )
        .await
        .unwrap();
        let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = tokio::task::spawn_local(async move {
            while let Some(message) = gateway_rx.recv().await {
                if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                    let _ = arguments.response_tx.send(Ok(()));
                }
            }
        });
        let (mut actor, _) = create_test_actor_with_chat_persistence(
            0,
            256_000,
            85,
            gateway_tx,
            persistence.tx.clone(),
            Box::new(
                crate::session::chat_persistence::ChannelChatPersistence::new(
                    persistence.tx.clone(),
                ),
            ),
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
        let mut entry = entry_with_search_flag("k2-replay", true);
        entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        entry.api_base_url = Some(server.url());
        entry.api_key = Some("nonsecret-k2-key".into());
        manager.insert_test_entry("k2-replay", entry);
        manager.set_current_model_id(acp::ModelId::new("k2-replay"));
        actor.models_manager = manager;
        let mut binding = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        binding.config.base_url = server.url();
        binding.config.model = "k2-replay".into();
        binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        binding.config.supports_search_tool = true;
        binding.config.max_completion_tokens = Some(4096);
        binding.config.max_retries = Some(0);
        binding.credentials.api_key = Some("nonsecret-k2-key".into());
        binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
        actor
            .chat_state_handle
            .install_transport_binding(binding)
            .await
            .unwrap();
        let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
        let releases = [
            Arc::new(tokio::sync::Semaphore::new(16)),
            Arc::new(tokio::sync::Semaphore::new(16)),
        ];
        for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
            actor.agent.borrow().tool_bridge().register_mcp_tools(name.into(), K2ClientTool { name, executions: executions.clone(), entered: entered_tx.clone(), release: releases[index].clone() }, Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}))).unwrap();
        }
        {
            let mut mcp = actor.mcp_state.lock().await;
            mcp.finish_init();
            mcp.complete_init();
        }
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
        let sampler = tokio::task::spawn_local(async move {
            while let Some(event) = sampler_rx.recv().await {
                let status = match &event {
                    xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                        evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                        ..
                    } => Some(attempt.status),
                    _ => None,
                };
                drainer.handle_sampling_event(event).await;
                if let Some(status) = status {
                    source_tx.send(status).unwrap();
                }
            }
        });
        Self {
            actor,
            persistence,
            directory,
            _models: models,
            tasks: vec![gateway, sampler],
            executions,
            entered,
            releases,
            source_acks,
        }
    }

    fn domain(&self, server: &MockInferenceServer) -> k2_replay::NativeReplayDomain {
        k2_replay::NativeReplayDomain {
            backend: xai_grok_sampling_types::ApiBackend::Messages,
            dialect: k2_replay::NativeReplayDialect::Messages,
            model: "k2-replay".into(),
            row: "k2-replay".into(),
            provider: "offline".into(),
            deployment: "loopback".into(),
            deployment_revision: "1".into(),
            endpoint: format!("{}/messages", server.url()),
            principal: "nonsecret-fixture".into(),
        }
    }

    async fn turn(
        &self,
        requirement: k2_history::NativeHistoryRequirement,
        server: &MockInferenceServer,
        label: &str,
    ) -> Result<TurnOutcome, acp::Error> {
        self.actor
            .chat_state_handle
            .push_user_message_and_ack(ConversationItem::user(label))
            .await;
        let selected = requirement == k2_history::NativeHistoryRequirement::Selected;
        self.actor
            .session_turn_active
            .store(selected, std::sync::atomic::Ordering::SeqCst);
        let result = super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                self.domain(server),
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    requirement,
                    self.actor.process_conversation_turn_with_recovery(
                        label,
                        None,
                        None,
                        None,
                        &mut super::length_salvage::LengthSalvage::new(None),
                        &mut Default::default(),
                    ),
                ),
            )
            .await;
        self.actor
            .session_turn_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    async fn reload(&self) -> (K2Checkpoint, Vec<ConversationItem>) {
        use crate::session::storage::StorageAdapter;
        let (respond_to, flushed) = tokio::sync::oneshot::channel();
        self.persistence
            .tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .unwrap();
        flushed.await.unwrap().unwrap();
        let adapter = crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
            self.directory.path().to_path_buf(),
        );
        let full = adapter
            .load_session(&self.actor.session_info)
            .await
            .unwrap();
        let light = adapter
            .load_session_without_updates(&self.actor.session_info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap(),
            "K2 full/light disk readers agree"
        );
        let disk = adapter
            .read_native_history(&self.actor.session_info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        let live = self
            .actor
            .chat_state_handle
            .read_native_history()
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(
            disk.source, live.source,
            "K2 actor source is the reloaded source"
        );
        assert_eq!(
            disk.ledger, live.ledger,
            "K2 actor ledger is the reloaded ledger"
        );
        assert_eq!(
            disk.bytes.as_ref(),
            std::fs::read(self.directory.path().join("native_history/current.json")).unwrap()
        );
        disk.matches_history(&full.chat_history).unwrap();
        ((*disk).clone(), full.chat_history)
    }
}

fn k2_script(frames: Vec<Value>) -> ScriptedResponse {
    ScriptedResponse::sse(
        frames
            .into_iter()
            .map(|frame| SseEvent::data(frame.to_string()))
            .collect(),
    )
}

fn k2_frames(id: &str, blocks: Vec<(Value, Vec<Value>)>, stop: &str) -> Vec<Value> {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":"k2-replay","content":[],"stop_reason":null,"usage":{"input_tokens":17,"output_tokens":0}}}),
    ];
    for (index, (block, deltas)) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        for delta in deltas {
            frames.push(json!({"type":"content_block_delta","index":index,"delta":delta}));
        }
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.extend([json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"input_tokens":17,"output_tokens":7}}),json!({"type":"message_stop"})]);
    frames
}

fn k2_call(id: &str, caller: Option<Value>) -> (Value, Vec<Value>) {
    let mut block =
        json!({"type":"server_tool_use","id":id,"name":"tool_search_tool_regex","input":{}});
    if let Some(caller) = caller {
        block["caller"] = caller;
    }
    (
        block,
        vec![json!({"type":"input_json_delta","partial_json":"{\"query\":\"exact K2 search\"}"})],
    )
}

fn k2_result(id: &str, name: &str) -> (Value, Vec<Value>) {
    (
        json!({"type":"tool_search_tool_result","tool_use_id":id,"content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":name}]}}),
        Vec::new(),
    )
}

async fn k2_seed(session: &mut K2ReplaySession, server: &MockInferenceServer) {
    server.enqueue_response(
        "/v1/messages",
        k2_script(k2_frames(
            "msg_k2_seed",
            vec![
                k2_call("srvtoolu_k2_seed", None),
                k2_result("srvtoolu_k2_seed", "alpha__lookup"),
                (json!({"type":"text","text":"K2 seed complete"}), Vec::new()),
            ],
            "end_turn",
        )),
    );
    session
        .turn(
            k2_history::NativeHistoryRequirement::Inactive,
            server,
            "seed exactly one completed native group",
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(k2_history::NativeResponseStatus::Accepted)
    );
    assert!(session.source_acks.try_recv().is_err());
    let (checkpoint, _) = session.reload().await;
    assert_eq!(
        checkpoint
            .ledger
            .groups
            .iter()
            .map(|group| group.state.as_str())
            .collect::<Vec<_>>(),
        vec!["Completed"],
        "K2 Selected scenarios start after the H-S3F-R6 Inactive seed"
    );
    assert_eq!(server.messages_request_count(), 1);
}

fn k2_citation(text: &str, start: u64) -> Value {
    json!({"type":"char_location","cited_text":text,"document_index":0,"document_title":null,"file_id":null,"start_char_index":start,"end_char_index":start+1})
}

fn k2_ordered_frames() -> Vec<Value> {
    k2_frames(
        "msg_k2_ordered",
        vec![
            (
                json!({"type":"text","text":"missing citations"}),
                Vec::new(),
            ),
            (
                json!({"type":"text","text":"null citations","citations":null}),
                Vec::new(),
            ),
            (
                json!({"type":"text","text":"ordered citations","citations":[]}),
                vec![
                    json!({"type":"citations_delta","citation":k2_citation("first",0)}),
                    json!({"type":"citations_delta","citation":k2_citation("second",1)}),
                ],
            ),
            k2_call("srvtoolu_k2_s1", None),
            (
                json!({"type":"thinking","thinking":""}),
                vec![
                    json!({"type":"thinking_delta","thinking":"ordered reasoning"}),
                    json!({"type":"signature_delta","signature":"opaque-k2-signature"}),
                ],
            ),
            k2_call(
                "srvtoolu_k2_s2",
                Some(json!({"type":"direct","retained":"K2 caller"})),
            ),
            k2_result("srvtoolu_k2_s2", "zeta__fetch"),
            (json!({"type":"text","text":"between results"}), Vec::new()),
            k2_result("srvtoolu_k2_s1", "alpha__lookup"),
            (json!({"type":"text","text":"ordered suffix"}), Vec::new()),
        ],
        "end_turn",
    )
}

#[derive(Clone, Copy)]
enum K2Receipt {
    Matching,
    None,
    WrongShape,
    Foreign,
}

fn k2_core(
    checkpoint: &K2Checkpoint,
    request: &ConversationRequest,
    domain: &k2_replay::NativeReplayDomain,
    receipt: K2Receipt,
) -> Result<k2_history::NativeHistoryCore, k2_history::NativeHistoryRefusal> {
    use k2_history::*;
    use k2_replay::*;
    let selected: Vec<_> = checkpoint
        .source
        .ordered_blocks
        .iter()
        .map(|block| block.coordinates.clone())
        .collect();
    let authorship = checkpoint
        .source
        .ordered_blocks
        .iter()
        .map(|block| (block.coordinates.occurrence, NativeIdAuthorship::Provider))
        .collect();
    let target = TargetRoute::new(
        xai_grok_sampling_types::ApiBackend::Messages,
        Boundary::Vertex,
        request.search_admission.unwrap(),
    );
    let origins = checkpoint
        .source
        .producing_bindings
        .iter()
        .map(|binding| (binding.origin_ref.digest.clone(), domain.clone()))
        .collect();
    prepare_native_history_core(
        &NativePureHistory {
            record: &checkpoint.record,
            source: &checkpoint.source,
            ledger: &checkpoint.ledger,
            identity: &checkpoint.identity,
            blobs: &checkpoint.blobs,
            authorship: &authorship,
        },
        &NativeProjectionInputs {
            target_model_id: "k2-replay",
            target: &target,
            tools: &request.tools,
            current: request.native_eligible_inputs.as_deref(),
            selected: &selected,
            origin_domains: &origins,
            pending: &Default::default(),
            prior_f: None,
        },
        |group| {
            let result = group
                .members
                .iter()
                .filter_map(|coordinate| {
                    checkpoint
                        .source
                        .ordered_blocks
                        .iter()
                        .find(|block| &block.coordinates == coordinate)
                })
                .map(|block| {
                    serde_json::from_slice::<Value>(&checkpoint.blobs[&block.block.digest]).unwrap()
                })
                .find(|raw| raw["type"] == "tool_search_tool_result")
                .unwrap();
            let shape = NativeReplayShape {
                family: NativeReplayFamily::MessagesServer,
                keyless_server: false,
                output_id_present: false,
                completed: true,
                result_error: false,
                referenced_members: native_candidate_references(&result).unwrap().len() as u64,
            };
            let mut receipt_shape = shape.clone();
            let mut receipt_target = domain.clone();
            if matches!(receipt, K2Receipt::WrongShape) {
                receipt_shape.referenced_members += 1;
            }
            if matches!(receipt, K2Receipt::Foreign) {
                receipt_target.row = "foreign-K2-row".into();
            }
            (
                DiscoveryOrigin {
                    boundary: Boundary::Vertex,
                    owner_model_id: Some("k2-replay".into()),
                },
                NativeQualification {
                    source: Some(domain.clone()),
                    target: Some(domain.clone()),
                    request: Some(domain.clone()),
                    original_admission: request.search_admission.unwrap(),
                    shape,
                    receipt: (!matches!(receipt, K2Receipt::None)).then_some(NativeShapeReceipt {
                        target: receipt_target,
                        shape: receipt_shape,
                        receipt_id: "K2-synthetic-offline-receipt".into(),
                    }),
                    intact_membership: true,
                    current_backing: true,
                    faithful_identity: true,
                },
            )
        },
    )
}

async fn k2_shared_request(
    session: &K2ReplaySession,
    server: &MockInferenceServer,
    checkpoint: &K2Checkpoint,
    history: Vec<ConversationItem>,
) -> (ConversationRequest, k2_history::NativeHistoryRequestContext) {
    use k2_history::*;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let owner = session
        .actor
        .chat_state_handle
        .register_native_history_owner(
            session.actor.session_id_string(),
            "K2-shared-boundary".into(),
            session.domain(server),
            NativeProjectionOwner::OwningPrimary,
            session.actor.session_turn_active.clone(),
        )
        .await
        .unwrap();
    let (prepared, _) = session.actor.prepare_native_tool_candidates_timed().await;
    let bundle = session.actor.prepared_tool_bundle(&prepared);
    let projection = super::sampler_turn::BridgeDeclarationProjection::prepared(
        bundle,
        prepared
            .mcp_specs
            .iter()
            .map(|spec| spec.name.clone())
            .collect(),
    );
    let candidates = projection
        .candidates()
        .with_history_entry(xai_chat_state::NativeHistoryEntry::captured(
            NativeHistoryRequirement::Selected,
            xai_chat_state::NativeHistoryEntryIntent::OwningPrimary(owner.token()),
        ))
        .unwrap();
    let (mut request, _) = session
        .actor
        .chat_state_handle
        .build_request_with_binding(
            candidates,
            None,
            false,
            None,
            session.actor.session_id_string(),
            "K2-shared-boundary".into(),
        )
        .await
        .unwrap();
    let capture = request
        .native_bound_preparation
        .as_ref()
        .unwrap()
        .context()
        .unwrap()
        .capture()
        .clone();
    request.items = history;
    checkpoint.matches_history(&request.items).unwrap();
    let context = NativeHistoryRequestContext::captured(
        NativeHistoryRequirement::Selected,
        capture,
        NativeHistoryWriteAuthority::SendOnly,
    );
    drop(owner);
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    (request, context)
}

fn k2_projected_request(
    checkpoint: &K2Checkpoint,
    mut request: ConversationRequest,
    context: &k2_history::NativeHistoryRequestContext,
    core: &k2_history::NativeHistoryCore,
) -> ConversationRequest {
    let mut replacements = std::collections::BTreeMap::new();
    for member in &core.decision.members {
        let link = checkpoint
            .ledger
            .accounting
            .iter()
            .find(|link| link.source == member.source)
            .unwrap();
        if let Some(index) = link.view_item {
            let item = member.raw.as_ref().map(|raw| ConversationItem::Discovery { item: xai_grok_sampling_types::conversation::tool_search::ToolSearchItem::from_wire(raw.clone()).unwrap() })
                .or_else(|| member.neutral.as_ref().map(|neutral| ConversationItem::assistant(neutral.text.clone())));
            replacements.insert(index as usize, item);
        }
    }
    request.items = request
        .items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| replacements.get(&index).cloned().unwrap_or(Some(item)))
        .collect();
    request.native_history = Some(
        xai_grok_sampling_types::request_validation::NativeHistoryRequest::new(
            context.clone(),
            None,
        ),
    );
    request
}

fn k2_shared_plan(
    checkpoint: &K2Checkpoint,
    request: &ConversationRequest,
    context: k2_history::NativeHistoryRequestContext,
    core: k2_history::NativeHistoryCore,
    carrier: k2_history::NativeFinalCarrier,
) -> Result<k2_history::NativeHistoryPlan, k2_history::NativePlanRefusal> {
    use k2_history::*;
    use xai_chat_state::persistence::native::digest;
    let binding: Arc<[u8]> = serde_json::to_vec(&json!({"request":context.capture().issued_request,"generation":context.capture().binding_generation,"origin":{"status":"Captured"}})).unwrap().into();
    let binding_ref = NativeBlobRef {
        kind: "producing_binding".into(),
        byte_len: binding.len() as u64,
        digest: digest(&binding),
    };
    let declarations: Vec<Value> = carrier
        .declarations()
        .map(|bytes| serde_json::from_slice(bytes).unwrap())
        .unwrap_or_default();
    let identities = declarations
        .iter()
        .enumerate()
        .filter_map(|(index, declaration)| {
            let schema = declaration.get("input_schema")?;
            let name = declaration["name"].as_str().unwrap();
            let entry = request.native_eligible_inputs.as_ref().and_then(|inputs| {
                inputs
                    .entries()
                    .iter()
                    .find(|entry| entry.declaration().name == name)
            });
            let (kind, namespace, child) = if let Some(entry) = entry {
                (
                    entry.namespace_kind(),
                    entry.qualified_namespace().to_owned(),
                    entry.child_name().to_owned(),
                )
            } else {
                assert_eq!(name, "DeferredToolPlaceholder");
                (
                    NativeNamespaceKind::GrokBuild,
                    "grok_build".into(),
                    name.into(),
                )
            };
            let schema_bytes: Arc<[u8]> = serde_json::to_vec(schema).unwrap().into();
            let schema_ref = NativeBlobRef {
                kind: "eligible_schema".into(),
                byte_len: schema_bytes.len() as u64,
                digest: digest(&schema_bytes),
            };
            let snapshot = NativeEligibleSnapshot {
                version: 1,
                namespace_kind: kind,
                qualified_namespace: namespace.clone(),
                child_name: child.clone(),
                schema: schema_ref.clone(),
                binding: binding_ref.clone(),
            };
            let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
            Some(NativeDeclarationIdentity {
                declaration_index: index as u64,
                child_index: None,
                namespace_kind: kind,
                qualified_namespace: namespace,
                child_name: child,
                declaration: declaration.clone(),
                schema: schema_ref,
                schema_bytes,
                provenance: NativeBlobRef {
                    kind: "eligible_snapshot".into(),
                    byte_len: provenance_bytes.len() as u64,
                    digest: digest(&provenance_bytes),
                },
                provenance_bytes,
                binding_bytes: Some(binding.clone()),
            })
        })
        .collect();
    let finalization =
        NativeFinalizationEvidence::checked_native_messages(request, &core, carrier.clone())?;
    NativeHistoryPlan::checked_finalized(
        context,
        NativeHistoryPlanInput {
            record: checkpoint.record.clone(),
            input_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            ledger_bytes: checkpoint.blobs[&checkpoint.record.ledger.digest].clone(),
            lowering_request: request,
            post_history_bytes: serde_json::to_vec(&request.items).unwrap().into(),
            carrier,
            identities,
            qualification: core.accounting.qualification.clone(),
            core,
        },
        finalization,
    )?
    .send_only()
}

fn k2_source_blocks(checkpoint: &K2Checkpoint, response_id: &str) -> Vec<Value> {
    let response = checkpoint
        .source
        .responses
        .iter()
        .find(|response| {
            response
                .provider_response_id
                .as_ref()
                .and_then(Value::as_str)
                == Some(response_id)
        })
        .unwrap();
    checkpoint.source.ordered_blocks[response.block_start as usize..response.block_end as usize]
        .iter()
        .map(|block| serde_json::from_slice(&checkpoint.blobs[&block.block.digest]).unwrap())
        .collect()
}

fn k2_body_blocks(body: &Value) -> Vec<Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .cloned()
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn messages_ordered_replay_survives_checkpoint() {
    use k2_history::*;
    tokio::task::LocalSet::new().run_until(async {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = K2ReplaySession::new(&server).await;
        k2_seed(&mut session, &server).await;
        server.enqueue_response("/v1/messages", k2_script(k2_ordered_frames()));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 ordered real decode and checkpoint").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        assert!(session.source_acks.try_recv().is_err());
        assert_eq!(server.messages_request_count(), 2);
        let (checkpoint, history) = session.reload().await;
        let mut s1 = k2_call("srvtoolu_k2_s1", None).0;
        s1["input"] = json!({"query":"exact K2 search"});
        let mut s2 = k2_call("srvtoolu_k2_s2", Some(json!({"type":"direct","retained":"K2 caller"}))).0;
        s2["input"] = json!({"query":"exact K2 search"});
        let expected = vec![
            json!({"type":"text","text":"missing citations"}),
            json!({"type":"text","text":"null citations","citations":null}),
            json!({"type":"text","text":"ordered citations","citations":[k2_citation("first",0),k2_citation("second",1)]}),
            s1.clone(), json!({"type":"thinking","thinking":"ordered reasoning","signature":"opaque-k2-signature"}),
            s2.clone(), k2_result("srvtoolu_k2_s2", "zeta__fetch").0,
            json!({"type":"text","text":"between results"}), k2_result("srvtoolu_k2_s1", "alpha__lookup").0,
            json!({"type":"text","text":"ordered suffix"}),
        ];
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_ordered"), expected, "K2 real delta assembly and ordered raw fields survive actor/disk/reload unchanged");
        assert_eq!(checkpoint.source.responses.len(), 2);
        assert_eq!(checkpoint.ledger.usage.len(), 2, "K2 retains usage per response occurrence, never sums it");
        assert!(matches!(checkpoint.messages_metadata, NativeMessagesMetadataState::Revision3 { .. }));
        let (request, context) = k2_shared_request(&session, &server, &checkpoint, history).await;
        let core = k2_core(&checkpoint, &request, &session.domain(&server), K2Receipt::Matching).unwrap();
        assert_eq!(core.decision.groups.iter().filter(|group| group.action == k2_replay::NativeHistoryAction::Keep).count(), 2);
        let request = k2_projected_request(&checkpoint, request, &context, &core);
        let lowered = xai_grok_sampling_types::conversation::build_messages_request(&request);
        let encoded = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered).unwrap();
        let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
        let blocks = k2_body_blocks(&body);
        let native: Vec<_> = blocks.iter().filter(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).cloned().collect();
        assert_eq!(native, vec![s1, s2, k2_result("srvtoolu_k2_s2", "zeta__fetch").0, k2_result("srvtoolu_k2_s1", "alpha__lookup").0], "K2 shared-boundary native emission preserves order and caller once");
        let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
        let plan = k2_shared_plan(&checkpoint, &request, context.clone(), core.clone(), carrier.clone()).unwrap();
        assert_eq!(xai_grok_sampling_types::request_validation::validate_native_messages_send(&context, Some(&plan), &carrier, Some(&carrier)), Ok(NativeHistoryReady::SendOnly));
        let citation_blocks: Vec<_> = blocks.iter().filter(|block| block["text"].as_str().is_some_and(|text| text.contains("citations"))).cloned().collect();
        let ordered_message = body["messages"].as_array().unwrap().iter().find(|message| {
            message["content"].as_array().unwrap().iter().any(|block| block["type"] == "server_tool_use" && block["id"] == "srvtoolu_k2_s1")
        }).unwrap();
        let ordered_content = ordered_message["content"].as_array().unwrap();
        let ordinary_blocks: Vec<_> = ordered_content.iter().filter(|block| !matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result" | "thinking"))).cloned().collect();
        let ordinary_text = expected.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n");
        let block_types: Vec<_> = ordered_content.iter().map(|block| block["type"].as_str().unwrap()).collect();
        println!("{}", json!({"K2_ordered_checkpoint":true,"source_response_count":checkpoint.source.responses.len(),"raw_text_blocks":&expected[..3],"lowered_citation_blocks":citation_blocks,"block_types":block_types,"ordinary_replay":"PARENT_PINNED_STAGED_NOT_CREDITED","ready":"SendOnly","owning_primary_KEEP":"STAGED_NOT_CREDITED"}));
        assert_eq!(ordinary_blocks, vec![json!({"type":"text","text":ordinary_text})], "PARENT BEHAVIOUR, not K2 fidelity: one flattened text block, citations missing; ordinary ordered-content replay fidelity is STAGED (K2-U4 erratum section 4)");
        let text_position = ordered_content.iter().position(|block| block["type"] == "text").unwrap();
        let last_native_position = ordered_content.iter().rposition(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).unwrap();
        assert!(text_position > last_native_position, "PARENT BEHAVIOUR, not K2 fidelity: flattened text follows the native blocks; ordinary ordered-content replay fidelity is STAGED");
        for receipt in [K2Receipt::None, K2Receipt::WrongShape, K2Receipt::Foreign] {
            let twin = k2_core(&checkpoint, &request, &session.domain(&server), receipt).unwrap();
            assert!(twin.decision.groups.iter().all(|group| group.action != k2_replay::NativeHistoryAction::Keep), "K2 missing/mismatched receipt twin demotes for qualification, not checkpoint refusal");
        }
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_no_input_twin", vec![(json!({"type":"text","text":"no-input twin complete"}), Vec::new())], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 no-input owning-primary twin").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        let (after, _) = session.reload().await;
        for group in &checkpoint.ledger.groups {
            assert_eq!(after.active.dispositions.get(&group.key).map(String::as_str), Some("F"), "K2 parent no-input owning-primary demotes completed groups");
        }
        assert!(session.executions.lock().unwrap().is_empty());

        for fault in ["completed-signature-missing", "unknown-start", "wrong-index", "singular-citation"] {
            let server = MockInferenceServer::start().await.unwrap();
            let mut session = K2ReplaySession::new(&server).await;
            k2_seed(&mut session, &server).await;
            let (seed_checkpoint, _) = session.reload().await;
            let prior = seed_checkpoint.source.responses;
            let mut frames = k2_ordered_frames();
            match fault {
                "completed-signature-missing" => frames.retain(|frame| frame.pointer("/delta/type").and_then(Value::as_str) != Some("signature_delta")),
                "unknown-start" => frames[1]["content_block"]["future_default"] = json!(""),
                "wrong-index" => {
                    let frame = frames.iter_mut().find(|frame| frame.pointer("/delta/type").and_then(Value::as_str) == Some("citations_delta")).unwrap();
                    frame["index"] = json!(99);
                }
                "singular-citation" => {
                    let frame = frames.iter_mut().find(|frame| frame.pointer("/delta/type").and_then(Value::as_str) == Some("citations_delta")).unwrap();
                    frame["delta"]["type"] = json!("citation_delta");
                }
                _ => unreachable!(),
            }
            server.enqueue_response("/v1/messages", k2_script(frames));
            let outcome = session.turn(NativeHistoryRequirement::Selected, &server, fault).await;
            let status = tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap().unwrap();
            assert!(outcome.is_err() || status != NativeResponseStatus::Accepted, "K2 completed-only mismatch and malformed events cannot become faithful KEEP: {fault}");
            let (checkpoint, _) = session.reload().await;
            assert!(checkpoint.source.responses.len() >= prior.len(), "K2 pre-turn native responses cannot be removed: {fault}");
            assert_eq!(&checkpoint.source.responses[..prior.len()], prior.as_slice(), "K2 pre-turn native responses remain unchanged after malformed attempt: {fault}");
            let appended = &checkpoint.source.responses[prior.len()..];
            let branch = if appended.is_empty() { "ABSENT" } else { "PRESENT" };
            let statuses: Vec<_> = appended.iter().map(|response| response.status).collect();
            println!("{}", json!({"K2_malformed_checkpoint":true,"fault":fault,"branch":branch,"statuses":statuses,"prior_responses":prior.len(),"pre_turn_responses_unchanged":true}));
            assert!(appended.iter().all(|response| response.status != NativeResponseStatus::Accepted), "K2 malformed completed capture remains incomplete after disk reload: {fault}");
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_split_server_pair_joins_across_responses() {
    use k2_history::*;
    tokio::task::LocalSet::new().run_until(async {
        let server = MockInferenceServer::start().await.unwrap();
        let mut session = K2ReplaySession::new(&server).await;
        k2_seed(&mut session, &server).await;
        let client = json!({"type":"tool_use","id":"client_k2_split","name":"alpha__lookup","input":{"value":"split actual"}});
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_1", vec![
            k2_call("srvtoolu_k2_split_s2", Some(json!({"type":"direct","retained":"split caller"}))),
            k2_call("srvtoolu_k2_split_s1", None), (client.clone(), Vec::new()),
        ], "tool_use")));
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_2", vec![
            k2_result("srvtoolu_k2_split_s1", "alpha__lookup"),
            k2_result("srvtoolu_k2_split_s2", "zeta__fetch"),
            (json!({"type":"text","text":"split suffix"}), Vec::new()),
        ], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 two live split calls with an actual client result").await.unwrap();
        for _ in 0..2 {
            assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        }
        assert!(session.source_acks.try_recv().is_err());
        assert_eq!(server.messages_request_count(), 3, "K2 split has one seed and exactly two real Selected requests");
        assert_eq!(*session.executions.lock().unwrap(), vec!["alpha__lookup"]);
        let (checkpoint, history) = session.reload().await;
        let mut s2 = k2_call("srvtoolu_k2_split_s2", Some(json!({"type":"direct","retained":"split caller"}))).0;
        s2["input"] = json!({"query":"exact K2 search"});
        let mut s1 = k2_call("srvtoolu_k2_split_s1", None).0;
        s1["input"] = json!({"query":"exact K2 search"});
        let r1 = k2_result("srvtoolu_k2_split_s1", "alpha__lookup").0;
        let r2 = k2_result("srvtoolu_k2_split_s2", "zeta__fetch").0;
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_split_1"), vec![s2.clone(), s1.clone(), client]);
        assert_eq!(k2_source_blocks(&checkpoint, "msg_k2_split_2"), vec![r1.clone(), r2.clone(), json!({"type":"text","text":"split suffix"})]);
        assert_eq!(checkpoint.source.responses.len(), 3);
        assert_eq!(checkpoint.ledger.usage.len(), 3, "K2 split retains per-response usage occurrence linkage");
        assert_eq!(checkpoint.source.responses.iter().map(|response| response.source_stop_reason.as_deref()).collect::<Vec<_>>(), vec![Some("end_turn"), Some("tool_use"), Some("end_turn")]);
        assert!(matches!(checkpoint.messages_metadata, NativeMessagesMetadataState::Revision3 { .. }));
        assert_eq!(checkpoint.ledger.groups.len(), 3, "K2 split has the seed and two joined groups, no result-only group");
        let mut joined_keys = Vec::new();
        for id in ["srvtoolu_k2_split_s1", "srvtoolu_k2_split_s2"] {
            let groups: Vec<_> = checkpoint.ledger.groups.iter().filter(|group| {
                group.members.iter().any(|member| {
                    let raw: Value = serde_json::from_slice(&checkpoint.blobs[&checkpoint.source.ordered_blocks[member.occurrence as usize].block.digest]).unwrap();
                    raw["type"] == "server_tool_use" && raw["id"] == id
                })
            }).collect();
            assert_eq!(groups.len(), 1, "K2 split joins each real server id into one group");
            let group = groups[0];
            assert_eq!(group.state, "Completed");
            assert_eq!(checkpoint.ledger.lifecycle.groups.get(&group.key).map(String::as_str), Some("Completed"));
            assert_eq!(group.members.len(), 2);
            assert_ne!(group.members[0].response, group.members[1].response, "K2 split joined members span both responses");
            assert_eq!(group.validated_join.as_deref(), Some(id));
            assert!(!checkpoint.active.dispositions.contains_key(&group.key), "K2 both live groups survive the S3F continuation without premature F");
            joined_keys.push(group.key.clone());
        }
        let requests: Vec<_> = server.requests().into_iter().filter(|request| request.path == "/v1/messages").collect();
        let continuation = requests[2].body.as_ref().unwrap();
        let actual_result = json!({"type":"tool_result","tool_use_id":"client_k2_split","content":"alpha__lookup actual result: \"split actual\"","cache_control":{"type":"ephemeral"}});
        assert_eq!(continuation["messages"].as_array().unwrap().last().unwrap(), &json!({"role":"user","content":[actual_result.clone()]}), "K2 split continuation is the actual results-only client message");
        let (request, context) = k2_shared_request(&session, &server, &checkpoint, history).await;
        let core = k2_core(&checkpoint, &request, &session.domain(&server), K2Receipt::Matching).unwrap();
        let kept: Vec<_> = core.decision.groups.iter().filter(|group| group.action == k2_replay::NativeHistoryAction::Keep).collect();
        assert_eq!(kept.len(), 2, "K2 matching split core keeps both joined groups");
        let request = k2_projected_request(&checkpoint, request, &context, &core);
        let lowered = xai_grok_sampling_types::conversation::build_messages_request(&request);
        let encoded = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered).unwrap();
        let body: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
        let blocks = k2_body_blocks(&body);
        let native: Vec<_> = blocks.iter().filter(|block| matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result"))).cloned().collect();
        assert_eq!(native, vec![s2, s1, r1, r2], "K2 split item-3 native equality preserves both id joins, raw fields and native-relative order once");
        let replay_results: Vec<_> = blocks.iter().filter(|block| block["type"] == "tool_result" && block["tool_use_id"] == "client_k2_split").cloned().collect();
        assert_eq!(replay_results, vec![actual_result], "K2 split leaves the intervening actual client result unmoved and unchanged");
        let call_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["id"] == "srvtoolu_k2_split_s1")).unwrap();
        let client_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["type"] == "tool_result" && block["tool_use_id"] == "client_k2_split")).unwrap();
        let result_message = body["messages"].as_array().unwrap().iter().position(|message| message["content"].as_array().unwrap().iter().any(|block| block["type"] == "tool_search_tool_result" && block["tool_use_id"] == "srvtoolu_k2_split_s1")).unwrap();
        assert!(call_message < client_message && client_message < result_message, "K2 split preserves the intervening user result between source responses");
        let result_content = body["messages"][result_message]["content"].as_array().unwrap();
        let ordinary: Vec<_> = result_content.iter().filter(|block| !matches!(block["type"].as_str(), Some("server_tool_use" | "tool_search_tool_result" | "thinking"))).cloned().collect();
        assert_eq!(ordinary, vec![json!({"type":"text","text":"split suffix"})], "PARENT BEHAVIOUR, not K2 fidelity: split ordinary replay follows the existing Assistant arm");
        let text_position = result_content.iter().position(|block| block["type"] == "text").unwrap();
        let last_native = result_content.iter().rposition(|block| block["type"] == "tool_search_tool_result").unwrap();
        assert!(text_position > last_native, "PARENT BEHAVIOUR, not K2 fidelity: split flattened text follows native blocks");
        let carrier = NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
        let plan = k2_shared_plan(&checkpoint, &request, context.clone(), core.clone(), carrier.clone()).unwrap();
        assert_eq!(xai_grok_sampling_types::request_validation::validate_native_messages_send(&context, Some(&plan), &carrier, Some(&carrier)), Ok(NativeHistoryReady::SendOnly));
        for receipt in [K2Receipt::None, K2Receipt::WrongShape, K2Receipt::Foreign] {
            let twin = k2_core(&checkpoint, &request, &session.domain(&server), receipt).unwrap();
            assert!(twin.decision.groups.iter().all(|group| group.action != k2_replay::NativeHistoryAction::Keep), "K2 split receipt twin demotes for qualification on the same valid joined checkpoint");
        }
        println!("{}", json!({"K2_split_checkpoint":true,"joined_keys":joined_keys,"responses":checkpoint.source.responses,"groups":checkpoint.ledger.groups,"lifecycle":checkpoint.ledger.lifecycle,"native_replay":native,"ready":"SendOnly","ordinary_replay":"PARENT_PINNED_STAGED_NOT_CREDITED","owning_primary_KEEP":"STAGED_NOT_CREDITED"}));
        server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_split_no_input", vec![(json!({"type":"text","text":"split no-input twin"}), Vec::new())], "end_turn")));
        session.turn(NativeHistoryRequirement::Selected, &server, "K2 split no-input owning-primary twin").await.unwrap();
        assert_eq!(tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv()).await.unwrap(), Some(NativeResponseStatus::Accepted));
        let (after, _) = session.reload().await;
        for group in &checkpoint.ledger.groups {
            assert_eq!(after.active.dispositions.get(&group.key).map(String::as_str), Some("F"), "K2 parent split no-input owning-primary demotes completed groups");
        }
        assert_eq!(*session.executions.lock().unwrap(), vec!["alpha__lookup"]);

        let invalid_server = MockInferenceServer::start().await.unwrap();
        let mut invalid = K2ReplaySession::new(&invalid_server).await;
        k2_seed(&mut invalid, &invalid_server).await;
        invalid_server.enqueue_response("/v1/messages", k2_script(k2_frames("msg_k2_result_before_call", vec![
            k2_result("srvtoolu_k2_result_before_call", "alpha__lookup"),
            k2_call("srvtoolu_k2_result_before_call", None),
        ], "end_turn")));
        let invalid_outcome = invalid.turn(NativeHistoryRequirement::Selected, &invalid_server, "K2 result before its own server call").await;
        let invalid_status = tokio::time::timeout(K0_TIMEOUT, invalid.source_acks.recv()).await.unwrap().unwrap();
        let (invalid_checkpoint, _) = invalid.reload().await;
        let request_count = invalid_server.messages_request_count();
        if invalid_outcome.is_ok() && invalid_status == NativeResponseStatus::Accepted {
            let next = invalid.turn(NativeHistoryRequirement::Selected, &invalid_server, "K2 refuses invalid retained result-before-call").await;
            assert!(next.is_err(), "K2 result-before-own-call is invalid at the real subsequent core");
            assert_eq!(invalid_server.messages_request_count(), request_count, "K2 invalid result-before-own-call cannot reach continuation HTTP");
        }
        println!("{}", json!({"K2_result_before_call":true,"turn_refused":invalid_outcome.is_err(),"capture_status":invalid_status,"persisted_responses":invalid_checkpoint.source.responses.len(),"subsequent_HTTP":invalid_server.messages_request_count()-request_count}));
    }).await;
}

#[derive(Default)]
struct K2U5Trace {
    check_waiting: bool,
    check_tools: bool,
    builds: Vec<(ConversationRequest, xai_chat_state::RequestTransportBinding)>,
    submitted: Vec<(
        ConversationRequest,
        xai_chat_state::RequestTransportBinding,
        k2_history::NativeFinalCarrier,
    )>,
}

tokio::task_local! {
    static K2_U5_TRACE: std::rc::Rc<std::cell::RefCell<K2U5Trace>>;
}

pub(super) fn k2_u5_observe_bound(
    request: &ConversationRequest,
    binding: &xai_chat_state::RequestTransportBinding,
) {
    let _ = K2_U5_TRACE.try_with(|trace| {
        let mut trace = trace.borrow_mut();
        assert!(trace.builds.len() < 2, "K2 mixed handoff has exactly two bound builds");
        if let Some((original, original_binding)) = trace.builds.first()
            && trace.check_tools
        {
            assert_eq!(serde_json::to_value(&request.tools).unwrap(), serde_json::to_value(&original.tools).unwrap(), "K2 pending retains the whole original bound tool array, including schemas, order and deferred entries");
            assert_eq!(request.model, original.model, "K2 pending never rebuilds its model from the selected row");
            assert_eq!(request.search_admission, original.search_admission, "K2 pending retains original admission");
            assert_eq!(serde_json::to_value(&binding.config).unwrap(), serde_json::to_value(&original_binding.config).unwrap(), "K2 pending retains the original transport binding");
            assert!(binding.credentials == original_binding.credentials, "K2 pending retains original credential authority without printing it");
        }
        let pending = request.items.iter().any(|item| matches!(item, ConversationItem::Discovery { item }
            if item.kind() == xai_grok_sampling_types::conversation::tool_search::ToolSearchKind::ServerToolUse
                && item.raw()["id"] == "srvtoolu_k2_pending"));
        if pending {
            if trace.check_waiting {
                for missing in ["client_k2_c1", "client_k2_c2"] {
                    let mut partial = request.clone();
                    partial.items.retain(|item| !matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == missing));
                    let lowered = xai_grok_sampling_types::conversation::build_messages_request(&partial);
                    let result = xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&lowered);
                    assert!(matches!(result, Err(xai_grok_sampling_types::request_validation::RequestValidationError::UnansweredToolUse { ref id }) if id == missing), "K2 waiting set is checked before cleanup: a missing actual result cannot erase its obligated client id: {missing}");
                }
                let mut duplicate = request.clone();
                let result = duplicate.items.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
                duplicate.items.push(result);
                assert!(xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&xai_grok_sampling_types::conversation::build_messages_request(&duplicate)).is_err(), "K2 duplicate actual client result cannot be a continuation");
                let mut foreign = request.clone();
                let result = foreign.items.iter_mut().find_map(|item| match item { ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2" => Some(result), _ => None }).unwrap();
                result.tool_call_id = "client_k2_foreign".into();
                assert!(xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(&xai_grok_sampling_types::conversation::build_messages_request(&foreign)).is_err(), "K2 foreign result cannot fill a waiting hole");
            }
            let body = serde_json::to_value(xai_grok_sampling_types::conversation::build_messages_request(request)).unwrap();
            let message = body["messages"].as_array().unwrap().iter().find(|message| message["content"].as_array().unwrap().iter().any(|block| block["id"] == "srvtoolu_k2_pending")).expect("K2 pending native server call survives the real bound lowerer");
            assert_eq!(message["content"], json!(k2_u5_expected_blocks()), "K2 pending native work and both real client calls replay unchanged, not cleaned or summarized");
        }
        trace.builds.push((request.clone(), binding.clone()));
    });
}

fn k2_u5_expected_blocks() -> Vec<Value> {
    vec![
        json!({"type":"server_tool_use","id":"srvtoolu_k2_pending","name":"tool_search_tool_regex","input":{"query":"exact K2 search"},"caller":{"type":"direct","retained":"pending caller"}}),
        json!({"type":"tool_use","id":"client_k2_c1","name":"alpha__lookup","input":{"value":"first actual"}}),
        json!({"type":"tool_use","id":"client_k2_c2","name":"zeta__fetch","input":{"value":"second actual"}}),
    ]
}

fn k2_u5_frames() -> Vec<Value> {
    let blocks = k2_u5_expected_blocks();
    k2_frames(
        "msg_k2_pending",
        vec![
            k2_call(
                "srvtoolu_k2_pending",
                Some(json!({"type":"direct","retained":"pending caller"})),
            ),
            (blocks[1].clone(), Vec::new()),
            (blocks[2].clone(), Vec::new()),
        ],
        "tool_use",
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum K2U5Change {
    None,
    SelectedRow,
    InstalledRow,
    RevokedDefinition,
}

struct K2U5Handoff {
    session: K2ReplaySession,
    server: MockInferenceServer,
    other: MockInferenceServer,
    checkpoint: K2Checkpoint,
    before_result: Vec<ConversationItem>,
    trace: std::rc::Rc<std::cell::RefCell<K2U5Trace>>,
    outcome: Result<TurnOutcome, acp::Error>,
}

async fn k2_u5_handoff(check_waiting: bool, check_tools: bool, change: K2U5Change) -> K2U5Handoff {
    use k2_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    let other = MockInferenceServer::start().await.unwrap();
    let mut session = K2ReplaySession::new(&server).await;
    k2_seed(&mut session, &server).await;
    session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    server.enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    server.enqueue_response(
        "/v1/messages",
        k2_script(k2_frames(
            "msg_k2_pending_done",
            vec![(
                json!({"type":"text","text":"K2 continuation complete"}),
                Vec::new(),
            )],
            "end_turn",
        )),
    );
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("K2 real mixed pending handoff"))
        .await;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let trace = std::rc::Rc::new(std::cell::RefCell::new(K2U5Trace {
        check_waiting,
        check_tools,
        ..Default::default()
    }));
    let run_trace = trace.clone();
    let actor = session.actor.clone();
    let domain = session.domain(&server);
    let mut running = tokio::task::spawn_local(async move {
        let capture = run_trace.clone();
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                let encoded = request
                    .encoded
                    .as_ref()
                    .expect("K2 Selected submit has the real finalized carrier");
                let carrier =
                    NativeFinalCarrier::capture(encoded.as_bytes().to_vec().into()).unwrap();
                let mut trace = capture.borrow_mut();
                assert!(
                    trace.submitted.len() < 2,
                    "K2 real mixed turn submits at most two requests"
                );
                trace
                    .submitted
                    .push((request.clone(), binding.clone(), carrier));
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    K2_U5_TRACE.scope(
                        run_trace,
                        actor.process_conversation_turn_for_native_history_test(
                            "K2-live-pending",
                            None,
                            None,
                            None,
                            &mut super::length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                            &mut prepare,
                        ),
                    ),
                ),
            )
            .await
    });
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted),
        "K2 mixed response is actually decoded and committed before client execution"
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(
        entered,
        vec!["alpha__lookup", "zeta__fetch"],
        "K2 executes every real waiting client exactly once"
    );
    let before_result = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) {
                break items;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!before_result.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2")), "K2 second actual result is held at its deterministic barrier");
    assert_eq!(
        server.messages_request_count(),
        2,
        "K2 has zero continuation HTTP while one real waiting result is missing"
    );
    assert!(
        !running.is_finished(),
        "K2 cannot settle or continue before every waiting result"
    );
    let (checkpoint, _) = session.reload().await;
    assert_eq!(
        k2_source_blocks(&checkpoint, "msg_k2_pending"),
        k2_u5_expected_blocks(),
        "K2 mixed source is retained before cleanup"
    );
    let live = checkpoint
        .ledger
        .groups
        .iter()
        .find(|group| {
            group.members.iter().any(|member| {
                let raw: Value = serde_json::from_slice(
                    &checkpoint.blobs[&checkpoint.source.ordered_blocks
                        [member.occurrence as usize]
                        .block
                        .digest],
                )
                .unwrap();
                raw["id"] == "srvtoolu_k2_pending"
            })
        })
        .unwrap();
    assert_eq!(live.state, "LivePending");
    assert_eq!(
        checkpoint
            .ledger
            .lifecycle
            .groups
            .get(&live.key)
            .map(String::as_str),
        Some("LivePending")
    );
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    let mut waiting: Vec<_> = checkpoint
        .ledger
        .lifecycle
        .waiting_clients
        .iter()
        .filter(|client| client.source.producing_binding == origin.request)
        .map(|client| (client.client_id.as_str(), client.delivery.as_str()))
        .collect();
    waiting.sort();
    assert_eq!(
        waiting,
        vec![("client_k2_c1", "Waiting"), ("client_k2_c2", "Waiting")],
        "K2 captures the entire persisted waiting map before item or block cleanup"
    );
    assert!(
        !native_results_continuation(
            &checkpoint.source,
            &checkpoint.ledger,
            &before_result,
            origin
        ),
        "K2 owner refuses the actual partial result snapshot"
    );
    if change != K2U5Change::None {
        let mut row = entry_with_search_flag("k2-selected-b", false);
        row.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
        row.api_base_url = Some(other.url());
        row.api_key = Some("nonsecret-k2-b-key".into());
        session
            .actor
            .models_manager
            .insert_test_entry("k2-selected-b", row);
        session
            .actor
            .models_manager
            .set_current_model_id(acp::ModelId::new("k2-selected-b"));
        assert_eq!(
            session.actor.models_manager.current_model_id(),
            acp::ModelId::new("k2-selected-b")
        );
        if change == K2U5Change::InstalledRow {
            let mut binding = session
                .actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap();
            binding.config.model = "k2-selected-b".into();
            binding.config.base_url = other.url();
            binding.config.supports_search_tool = false;
            binding.credentials.api_key = Some("nonsecret-k2-b-key".into());
            session
                .actor
                .chat_state_handle
                .install_transport_binding(binding)
                .await
                .unwrap();
        }
        if change == K2U5Change::RevokedDefinition {
            assert!(
                session
                    .actor
                    .agent
                    .borrow()
                    .tool_bridge()
                    .unregister_tool_by_name("zeta__fetch")
            );
        }
    }
    session.releases[1].add_permits(1);
    let outcome = match tokio::time::timeout(K0_TIMEOUT, &mut running).await {
        Ok(result) => result.unwrap(),
        Err(error) => {
            running.abort();
            panic!("K2 released real result must finish the bounded handoff: {error}");
        }
    };
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        other.messages_request_count(),
        0,
        "K2 never reconstructs or submits the pending exchange on B"
    );
    K2U5Handoff {
        session,
        server,
        other,
        checkpoint,
        before_result,
        trace,
        outcome,
    }
}

fn k2_u5_assert_continuation(handoff: &K2U5Handoff) {
    assert!(
        handoff.outcome.is_ok(),
        "K2 all real waiting results permit exactly one continuation"
    );
    assert_eq!(
        handoff.server.messages_request_count(),
        3,
        "K2 final actual result permits exactly one continuation HTTP"
    );
    assert_eq!(handoff.other.messages_request_count(), 0);
    let executions = handoff.session.executions.lock().unwrap().clone();
    let mut names = executions.clone();
    names.sort();
    assert_eq!(
        names,
        vec!["alpha__lookup", "zeta__fetch"],
        "K2 client execution is once-only, never re-executed or fabricated"
    );
    let requests: Vec<_> = handoff
        .server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .collect();
    let continuation = requests[2].body.as_ref().unwrap();
    let expected = json!({"role":"user","content":[
        {"type":"tool_result","tool_use_id":"client_k2_c1","content":"alpha__lookup actual result: \"first actual\""},
        {"type":"tool_result","tool_use_id":"client_k2_c2","content":"zeta__fetch actual result: \"second actual\"","cache_control":{"type":"ephemeral"}}
    ]});
    assert_eq!(
        continuation["messages"].as_array().unwrap().last().unwrap(),
        &expected,
        "K2 exactly one nonempty results-only user message contains one actual result per waiting id and no synthetic tail"
    );
    let pending = continuation["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["id"] == "srvtoolu_k2_pending")
        })
        .expect("K2 actual HTTP retains the pending native assistant work");
    assert_eq!(
        pending["content"],
        json!(k2_u5_expected_blocks()),
        "K2 actual continuation replays the entire mixed native/client assistant unchanged"
    );
    let trace = handoff.trace.borrow();
    assert_eq!(trace.builds.len(), 2);
    assert_eq!(trace.submitted.len(), 2);
    let (_, _, carrier) = &trace.submitted[1];
    assert_eq!(
        serde_json::from_slice::<Value>(carrier.bytes()).unwrap(),
        *continuation,
        "K2 captured finalized bytes are the actual HTTP body"
    );
    let typed: xai_grok_sampling_types::messages::Message =
        serde_json::from_value(expected).unwrap();
    let bytes = serde_json::to_vec(&typed).unwrap();
    let ranges: Vec<_> = carrier
        .bytes()
        .windows(bytes.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == bytes).then_some(offset))
        .collect();
    assert_eq!(
        ranges.len(),
        1,
        "K2 actual results-only body range is unique"
    );
    println!(
        "{}",
        json!({"K2_U5_submitted":true,"waiting_before_cleanup":["client_k2_c1","client_k2_c2"],"zero_early_continuation_http":true,"continuation_http":1,"executions":executions,"body_digest":carrier.digest(),"body_bytes":carrier.bytes().len(),"tools_range":carrier.tools(),"results_only_range":{"offset":ranges[0],"bytes":bytes.len(),"digest":native_content_digest(&bytes)},"pre_client_hook":"SessionActor::execute_tool_calls_with_native_recovery","owning_primary_KEEP":"STAGED_NOT_CREDITED"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_search_survives_client_results() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let handoff = k2_u5_handoff(false, false, K2U5Change::None).await;
            k2_u5_assert_continuation(&handoff);
            let server = MockInferenceServer::start().await.unwrap();
            let mut session = K2ReplaySession::new(&server).await;
            k2_seed(&mut session, &server).await;
            let mut frames = k2_u5_frames();
            frames.insert(
                2,
                json!({"type":"k2_unknown_semantic_event","payload":"unclassified"}),
            );
            server.enqueue_response("/v1/messages", k2_script(frames));
            let outcome = session
                .turn(
                    k2_history::NativeHistoryRequirement::Selected,
                    &server,
                    "K2 unknown semantic event must fail explicitly",
                )
                .await;
            assert!(
                outcome.is_err(),
                "K2 unknown semantic events end incomplete, never an implicit successful handoff"
            );
            assert_eq!(
                server.messages_request_count(),
                2,
                "K2 unknown semantic event cannot authorize a continuation"
            );
            assert!(
                session.executions.lock().unwrap().is_empty(),
                "K2 unknown semantic event cannot authorize client execution"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_live_handoff_requires_all_waiting_results() {
    tokio::task::LocalSet::new().run_until(async {
        let handoff = k2_u5_handoff(true, false, K2U5Change::None).await;
        k2_u5_assert_continuation(&handoff);
        let trace = handoff.trace.borrow();
        let actual = &trace.builds[1].0.items;
        let origin = handoff.checkpoint.source.producing_bindings.last().unwrap();
        assert!(k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, actual, origin), "K2 committed owner accepts the exact actual result snapshot");
        let mut text_tail = actual.clone();
        text_tail.push(ConversationItem::user("extra non-result tail"));
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &text_tail, origin), "K2 results-only grammar excludes text, reminders and synthetic tails");
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &handoff.before_result, origin), "K2 cannot forget the withheld actual result");
        let mut duplicate = actual.clone();
        let first = duplicate.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
        duplicate.push(first);
        assert!(!k2_history::native_results_continuation(&handoff.checkpoint.source, &handoff.checkpoint.ledger, &duplicate, origin), "K2 actual result duplication never satisfies a waiting obligation twice");
        println!("{}", json!({"K2_U5_waiting":true,"waiting_ids":["client_k2_c1","client_k2_c2"],"source_delivery":"Waiting","actual_result_before_release":"client_k2_c1","withheld_actual_result":"client_k2_c2","raw_snapshot_checked_before_cleanup":true,"exact_results_only_messages":1}));
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_retains_bound_tool_array() {
    tokio::task::LocalSet::new().run_until(async {
        let handoff = k2_u5_handoff(false, true, K2U5Change::SelectedRow).await;
        k2_u5_assert_continuation(&handoff);
        {
            let trace = handoff.trace.borrow();
            let original = &trace.submitted[0];
            let continuation = &trace.submitted[1];
            let first: Value = serde_json::from_slice(original.2.bytes()).unwrap();
            let second: Value = serde_json::from_slice(continuation.2.bytes()).unwrap();
            assert_eq!(first["tools"], second["tools"], "K2 complete original wire tools array survives a selected-row change");
            assert_eq!(original.2.declarations(), continuation.2.declarations(), "K2 original declaration bytes, not merely names, remain identical");
            for name in ["alpha__lookup", "zeta__fetch"] {
                let tool = first["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == name).unwrap();
                assert_eq!(tool["defer_loading"], json!(true), "K2 full-array proof includes the original deferred definitions");
                assert!(tool["input_schema"].is_object());
            }
            assert!(original.1.credentials == continuation.1.credentials, "K2 bound credentials do not change or print their values");
            assert_eq!(original.1.config.model, "k2-replay");
            assert_eq!(continuation.1.config.base_url, handoff.server.url());
            assert_eq!(handoff.session.actor.models_manager.current_model_id(), acp::ModelId::new("k2-selected-b"));
            println!("{}", json!({"K2_U5_binding":true,"selected_row":"k2-selected-b","submitted_bound_row":"k2-replay","selection_changed_without_installing_B":true,"original_authority_still_installed":true,"original_tools_digest":original.2.tools().unwrap().digest,"continuation_tools_digest":continuation.2.tools().unwrap().digest,"whole_array_equal":true,"deferred_entries_retained":true,"body_and_transport_binding":"A"}));
        }
        for change in [K2U5Change::InstalledRow, K2U5Change::RevokedDefinition] {
            let refused = k2_u5_handoff(false, false, change).await;
            let outcome_kind = match &refused.outcome {
                Err(_) => "ERROR",
                Ok(TurnOutcome::Completed { .. }) => "COMPLETED",
                Ok(TurnOutcome::Cancelled { .. }) => "CANCELLED",
                Ok(TurnOutcome::MaxTurnsReached { .. }) => "MAX_TURNS",
                Ok(TurnOutcome::StationarityEnded) => "STATIONARITY",
            };
            println!("{}", json!({"K2_U5_refusal_outcome":true,"outcome":outcome_kind,
                "installed_B":change == K2U5Change::InstalledRow,"revoked_definition":change == K2U5Change::RevokedDefinition,
                "messages_HTTP_A":refused.server.messages_request_count(),"messages_HTTP_B":refused.other.messages_request_count()}));
            assert!(refused.outcome.is_err(), "K2 unavailable or revoked original authority ends incomplete, never rebuilds the pending exchange on B");
            assert_eq!(refused.server.messages_request_count(), 2, "K2 revoked authority has zero continuation HTTP on A");
            assert_eq!(refused.other.messages_request_count(), 0, "K2 revoked authority has zero continuation HTTP on B");
            println!("{}", json!({"K2_U5_unavailable":true,"installed_B":change == K2U5Change::InstalledRow,"revoked_definition":change == K2U5Change::RevokedDefinition,"incomplete":true,"continuation_http_A":0,"continuation_http_B":0}));
        }
    }).await;
}

fn k2lg_compiled_sources() -> Value {
    json!({
        "sampler": native_content_digest(include_bytes!("../../../../xai-grok-sampler/src/client.rs")),
        "validation": native_content_digest(include_bytes!("../../../../xai-grok-sampling-types/src/request_validation.rs")),
        "fixture": native_content_digest(include_bytes!("native_messages_replay_tests.rs")),
    })
}

async fn k2lg_selector_prelude() -> bool {
    if std::env::var_os("K2LG_SOURCE_PROBE").is_some() {
        println!("K2LG_BUILD {}", k2lg_compiled_sources());
        return true;
    }
    if std::env::var_os("K2LG_PENDING_CHILD").is_some() {
        tokio::task::LocalSet::new().run_until(k2lg_v_cases()).await;
        return true;
    }
    false
}

async fn k2lg_pending_isolated() {
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env("K2LG_PENDING_CHILD", "1").env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG").env_remove("GROK_CONFIG_PATH")
        .args(["--exact", "session::acp_session::native_messages_replay_tests::messages_replay_plan_checked_on_all_sends", "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stdout.lines().filter(|line| line.starts_with("K2LG_")) {
        println!("{line}");
    }
    let assertion: Vec<_> = stderr
        .lines()
        .filter(|line| line.contains("K2LG boundary"))
        .collect();
    assert!(
        output.status.success(),
        "K2LG boundary child failed: {}",
        assertion.join("; ")
    );
    assert!(stdout.contains("running 1 test"));
}

fn k2lg_start_command_loop(
    session: &mut K2ReplaySession,
) -> tokio::sync::mpsc::UnboundedSender<SessionCommand> {
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = session.actor.clone();
    session.tasks.push(tokio::task::spawn_local(async move {
        let _keep_channels = (chat_tx, event_tx);
        super::run_session(
            actor,
            command_rx,
            chat_rx,
            event_rx,
            None,
            Arc::new(parking_lot::Mutex::new(
                xai_grok_workspace::file_system::CodebaseIndexManager::new(),
            )),
            std::env::temp_dir(),
            crate::session::fs_watch::FsWatchCapabilities::none(),
        )
        .await;
    }));
    commands
}

fn k2lg_v_corrupt(
    request: &mut ConversationRequest,
    binding: &xai_chat_state::RequestTransportBinding,
    case: &str,
) {
    use k2_history::*;
    match case {
        "valid" | "live-defensive" | "W1" | "W3" => {}
        "missing-native" => {
            assert_eq!(request.native_bound_preparation.as_ref().unwrap().requirement(), NativeHistoryRequirement::Selected);
            request.native_history = None;
        }
        "missing-plan" => request.native_history.as_mut().unwrap().invalidate_plan(),
        "stale-context" => {
            let native = request.native_history.as_ref().unwrap();
            let mut capture = native.context().capture().clone();
            capture.history_revision += 1;
            request.native_history = Some(NativeHistoryRequest::new(NativeHistoryRequestContext::captured(
                NativeHistoryRequirement::Selected, capture, native.context().authority().clone()), native.plan().cloned()));
        }
        "stale-carrier" => {
            let mut changed = request.clone();
            changed.max_output_tokens = Some(4097);
            let client = SamplingClient::new(xai_grok_sampler::SamplerConfig {
                base_url: binding.config.base_url.clone(), model: binding.config.model.clone(),
                api_backend: xai_grok_sampler::ApiBackend::Messages, max_completion_tokens: Some(4096),
                ..Default::default()
            }).unwrap();
            request.encoded = Some(client.encode_conversation_messages(&changed).unwrap());
        }
        "tools" => request.tools.reverse(),
        "schema" => request.tools[0].parameters = json!({"type":"object","properties":{"changed":{"type":"number"}}}),
        "text" => request.items.push(ConversationItem::user("K2LG extra non-result text")),
        "missing-result" => request.items.retain(|item| !matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2")),
        "duplicate-result" => {
            let item = request.items.iter().find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")).unwrap().clone();
            request.items.push(item);
        }
        "foreign-result" => {
            let result = request.items.iter_mut().find_map(|item| match item { ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2" => Some(result), _ => None }).unwrap();
            result.tool_call_id = "client_k2_foreign".into();
        }
        _ => unreachable!(),
    }
}

async fn k2lg_v_drive(case: &'static str) -> K2U5Handoff {
    use k2_history::*;
    let server = MockInferenceServer::start().await.unwrap();
    let other = MockInferenceServer::start().await.unwrap();
    let mut session = K2ReplaySession::new(&server).await;
    k2_seed(&mut session, &server).await;
    session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    server.enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    if !matches!(case, "live-defensive" | "W3" | "W1") {
        server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                "msg_k2lg_done",
                vec![(json!({"type":"text","text":"K2LG done"}), Vec::new())],
                "end_turn",
            )),
        );
    }
    session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("K2LG real pending producer"))
        .await;
    session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let trace = std::rc::Rc::new(std::cell::RefCell::new(K2U5Trace {
        check_tools: true,
        ..Default::default()
    }));
    let run_trace = trace.clone();
    let actor = session.actor.clone();
    let domain = session.domain(&server);
    let mut running = tokio::task::spawn_local(async move {
        let capture = run_trace.clone();
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                let carrier = NativeFinalCarrier::capture(
                    request.encoded.as_ref().unwrap().as_bytes().to_vec().into(),
                )
                .unwrap();
                let pending = capture.borrow().submitted.len() == 1;
                capture
                    .borrow_mut()
                    .submitted
                    .push((request.clone(), binding.clone(), carrier));
                if pending {
                    k2lg_v_corrupt(request, binding, case);
                }
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    K2_U5_TRACE.scope(
                        run_trace,
                        actor.process_conversation_turn_for_native_history_test(
                            "K2LG-live-pending",
                            None,
                            None,
                            None,
                            &mut super::length_salvage::LengthSalvage::new(None),
                            &mut Default::default(),
                            &mut prepare,
                        ),
                    ),
                ),
            )
            .await
    });
    let epoch = session.actor.turn_report.start_next_turn();
    {
        let mut state = session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(
            "K2LG-live-pending",
            epoch,
            running.abort_handle(),
        ));
        state
            .pending_inputs
            .push_front(user_item("K2LG-live-pending", "K2LG"));
    }
    *session.actor.current_prompt_id.lock().unwrap() = Some("K2LG-live-pending".into());
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted)
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"]);
    let before_result = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) { break items; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let (checkpoint, _) = session.reload().await;
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    assert_eq!(
        checkpoint
            .ledger
            .lifecycle
            .exchanges
            .get(&origin.request)
            .map(String::as_str),
        Some("LiveBound")
    );
    assert!(
        checkpoint
            .ledger
            .groups
            .iter()
            .any(|group| group.state == "LivePending" && group.origin_ref == origin.origin_ref)
    );
    assert_eq!(server.messages_request_count(), 2);
    if case == "W1" {
        let commands = k2lg_start_command_loop(&mut session);
        commands
            .send(SessionCommand::Interject {
                text: "K2LG W1 real late user".into(),
                id: None,
                images: Vec::new(),
            })
            .unwrap();
        tokio::time::timeout(K0_TIMEOUT, async {
            while session.actor.pending_interjections.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let mut defensive_failures = Vec::new();
    let mut defensive_delta = 0;
    let mut held = matches!(case, "live-defensive" | "W3" | "W1").then(|| {
        server.expect_response_blocked(
            "K2LG live pending continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames(
                "msg_k2lg_done",
                vec![(json!({"type":"text","text":"K2LG done"}), Vec::new())],
                "end_turn",
            )),
        )
    });
    session.releases[1].add_permits(1);
    if let Some(expectation) = held.as_mut() {
        if case == "W1" {
            let continued = tokio::time::timeout(K0_TIMEOUT, async {
                tokio::select! {
                    result = &mut running => { let _ = result.unwrap(); false },
                    _ = expectation.wait_blocked() => true,
                }
            }).await.expect("W1_REQUALIFIED_BOUNDED_OUTCOME_RACE");
            let history = session.actor.chat_state_handle.get_conversation().await;
            assert!(!history.iter().any(|item| item.text_content().contains("K2LG W1 real late user")),
                "W1_REQUALIFIED_HOLD_ASSERTION");
            assert!(continued, "W1_REQUALIFIED_CONTINUATION_SENT");
            assert!(session.actor.pending_interjections.is_empty(), "W1_REQUALIFIED_NOT_RESIDENT");
            {
                let state = session.actor.state.lock().await;
                assert_eq!(state.message_delivery.held_input.entries.iter().filter(|entry| {
                    matches!(&entry.body, super::interjection::HeldBody::Human(raw)
                        if raw.text == "K2LG W1 real late user")
                }).count(), 1, "W1_REQUALIFIED_IN_U6_CUSTODY");
            }
            let (saved, _, carrier) = trace.borrow().submitted[1].clone();
            let native = saved.native_history.as_ref().unwrap();
            assert!(validate_native_history_plan(native.context(), native.plan(), &carrier).is_ok(),
                "W1_REQUALIFIED_LIVE_BOUND_AUTHORITY");
            let bodies: Vec<_> = server.requests().into_iter().filter(|request| request.path == "/v1/messages")
                .map(|request| request.body.unwrap()).collect();
            assert_eq!(bodies.len(), 3, "W1_REQUALIFIED_ONE_CONTINUATION");
            u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
            assert!(bodies.iter().all(|body| !u6_body_text(body).contains("K2LG W1 real late user")),
                "W1_REQUALIFIED_INPUT_NOT_TRANSMITTED");
            assert_eq!(trace.borrow().builds.len(), 2, "W1_REQUALIFIED_ORIGINAL_BUILDS");
            assert_eq!(trace.borrow().submitted.len(), 2, "W1_REQUALIFIED_ORIGINAL_SUBMITS");
            assert_eq!(session.executions.lock().unwrap().len(), 2, "W1_REQUALIFIED_CLIENTS_ONCE");
            assert_eq!(other.messages_request_count(), 0, "W1_REQUALIFIED_NO_REBUILD_B");
            let (after, _) = session.reload().await;
            assert_eq!(k2_source_blocks(&after, "msg_k2_pending"), k2_source_blocks(&checkpoint, "msg_k2_pending"),
                "W1_REQUALIFIED_NATIVE_SOURCE_UNCHANGED");
            running.abort();
            assert!(matches!(tokio::time::timeout(K0_TIMEOUT, &mut running).await.unwrap(),
                Err(error) if error.is_cancelled()), "W1_REQUALIFIED_CONTROLLED_STOP");
            expectation.release();
            session.actor.session_turn_active.store(false, std::sync::atomic::Ordering::SeqCst);
            println!("K2LG_V {}", json!({"route":"V2","case":"W1","window":"W1",
                "status":"OBSERVED_REQUALIFIED","time_bound":"accepted capture through continuation HTTP while exchange open",
                "continuation_http":1,"input_in_history":false,"input_in_custody":true,
                "input_resident":false,"input_transmitted":false,"source_unchanged":true,
                "client_executions":2,"controlled_test_stop":true,"post_closure_credit":false,
                "K2LG_kill":false,"L16_credit":false,"body_digest":carrier.digest()}));
            return K2U5Handoff { session, server, other, checkpoint, before_result, trace,
                outcome: Err(crate::sampling::error::local_error("k2lg_w1_controlled_stop",
                    "test stopped at the requalified open-exchange continuation")) };
        }
        tokio::time::timeout(K0_TIMEOUT, expectation.wait_blocked())
            .await
            .unwrap();
        assert!(
            !running.is_finished(),
            "K2LG setup producer remains active at the terminal barrier"
        );
        let (saved, _, carrier) = trace.borrow().submitted[1].clone();
        let native = saved.native_history.as_ref().unwrap();
        assert!(
            validate_native_history_plan(native.context(), native.plan(), &carrier).is_ok(),
            "K2LG setup direct routes require the original still-live bound authority"
        );
        if case == "W3" {
            let commands = k2lg_start_command_loop(&mut session);
            commands
                .send(SessionCommand::Interject {
                    text: "K2LG W3 real late user".into(),
                    id: None,
                    images: Vec::new(),
                })
                .unwrap();
            tokio::time::timeout(K0_TIMEOUT, async {
                while !session
                    .actor
                    .pending_interjections
                    .snapshot()
                    .iter()
                    .any(|entry| entry.text == "K2LG W3 real late user")
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let requests = server.requests();
            let submitted = requests
                .iter()
                .filter(|request| request.path == "/v1/messages")
                .nth(2)
                .unwrap()
                .body
                .as_ref()
                .unwrap();
            assert_eq!(
                server.messages_request_count(),
                3,
                "K2LG boundary W3 continuation sends exactly once"
            );
            assert_eq!(
                *submitted,
                serde_json::from_slice::<Value>(carrier.bytes()).unwrap(),
                "K2LG boundary W3 actual carrier is the original bound body"
            );
            let origin = checkpoint.source.producing_bindings.last().unwrap();
            let mut waiting: Vec<_> = checkpoint
                .ledger
                .lifecycle
                .waiting_clients
                .iter()
                .filter(|client| {
                    client.source.producing_binding == origin.request
                        && client.delivery == "Waiting"
                })
                .map(|client| client.client_id.as_str())
                .collect();
            waiting.sort();
            let blocks = submitted["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_array()
                .unwrap();
            let mut delivered: Vec<_> = blocks
                .iter()
                .map(|block| {
                    assert_eq!(
                        block["type"], "tool_result",
                        "K2LG boundary W3 results-only user tail"
                    );
                    block["tool_use_id"].as_str().unwrap()
                })
                .collect();
            delivered.sort();
            assert!(
                !waiting.is_empty(),
                "K2LG boundary W3 waiting set is real and nonempty"
            );
            assert_eq!(
                delivered, waiting,
                "K2LG boundary W3 contains all and only real waiting IDs"
            );
            assert_eq!(
                trace.borrow().builds.len(),
                2,
                "K2LG boundary W3 uses only the original and continuation builds"
            );
            assert_eq!(
                trace.borrow().submitted.len(),
                2,
                "K2LG boundary W3 captured exactly two bound requests"
            );
            assert_eq!(
                session.executions.lock().unwrap().len(),
                2,
                "K2LG boundary W3 clients execute once-only"
            );
            assert!(
                !saved
                    .items
                    .iter()
                    .any(|item| item.text_content().contains("K2LG W3 real late user")),
                "K2LG boundary W3 input is queued, not in the bound send"
            );
            assert_eq!(
                other.messages_request_count(),
                0,
                "K2LG boundary W3 never rebuilds on B"
            );
            running.abort();
            assert!(
                matches!(running.await, Err(error) if error.is_cancelled()),
                "K2LG setup W3 bounded test stop aborts its own turn"
            );
            expectation.release();
            session
                .actor
                .session_turn_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            assert!(
                session
                    .actor
                    .pending_interjections
                    .snapshot()
                    .iter()
                    .any(|entry| entry.text == "K2LG W3 real late user"),
                "K2LG boundary W3 undelivered ordinary input remains queued after the bounded stop"
            );
            let (after, _) = session.reload().await;
            assert_eq!(
                k2_source_blocks(&after, "msg_k2_pending"),
                k2_source_blocks(&checkpoint, "msg_k2_pending"),
                "K2LG boundary W3 immutable native source survives"
            );
            println!(
                "K2LG_V {}",
                json!({"route":"V2","case":"W3","window":"W3",
                "producer":"SessionCommand::Interject through real run loop after continuation HTTP",
                "continuation_http":1,"input_queued":true,"input_transmitted":false,
                "source_unchanged":true,"client_executions":2,"controlled_test_stop":true,
                "turn_completion_credit":false,"body_digest":carrier.digest()})
            );
            let outcome = Err(crate::sampling::error::local_error(
                "k2lg_w3_controlled_stop",
                "test stopped after observing the live W3 boundary",
            ));
            return K2U5Handoff {
                session,
                server,
                other,
                checkpoint,
                before_result,
                trace,
                outcome,
            };
        }
        let before = server.messages_request_count();
        for route in ["V3", "V4"] {
            for corruption in [
                "valid",
                "missing-plan",
                "missing-native",
                "stale-context",
                "tools",
                "schema",
                "text",
                "missing-result",
                "duplicate-result",
                "foreign-result",
            ] {
                let label = format!("{route}/{corruption}");
                if !k2lg_v_arm(
                    &label,
                    k2lg_v_transport(&session, &server, &trace, route, corruption),
                )
                .await
                {
                    defensive_failures.push(label);
                }
            }
        }
        defensive_delta = server.messages_request_count() - before;
        expectation.release();
    }
    let outcome = match tokio::time::timeout(K0_TIMEOUT, &mut running).await {
        Ok(result) => result.unwrap(),
        Err(error) => {
            running.abort();
            panic!("K2LG boundary bounded handoff: {error}");
        }
    };
    if let Some(expectation) = held.as_mut() {
        tokio::time::timeout(K0_TIMEOUT, expectation.wait_satisfied())
            .await
            .unwrap();
        expectation.assert_satisfied();
    }
    session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let requests = server.requests();
    let continuation = requests
        .iter()
        .filter(|request| request.path == "/v1/messages")
        .nth(2);
    println!(
        "K2LG_V {}",
        json!({"route":"V1","case":case,"producer":"K2 decoded mixed handoff","builds":trace.borrow().builds.len(),"continuation_http":usize::from(continuation.is_some()),"executions":session.executions.lock().unwrap().clone(),"body_digest":continuation.and_then(|request| request.body.as_ref()).map(|body| native_content_digest(&serde_json::to_vec(body).unwrap()))})
    );
    let (after, _) = session.reload().await;
    assert_eq!(
        k2_source_blocks(&after, "msg_k2_pending"),
        k2_source_blocks(&checkpoint, "msg_k2_pending"),
        "K2LG boundary native source stays immutable after every valid or refused send"
    );
    assert_eq!(
        other.messages_request_count(),
        0,
        "K2LG boundary never reconstructs on B"
    );
    let usage: Vec<_> = after
        .ledger
        .usage
        .iter()
        .map(|item| {
            json!({
                "request":item.request,"response_occurrence":item.response_occurrence,
                "outcome":item.outcome,"http_dispatches":item.http_dispatches,
            })
        })
        .collect();
    println!(
        "K2LG_V_SOURCE {}",
        json!({"case":case,"source_unchanged":true,"HTTP_B":0,
        "native_usage":usage,"defensive_http_delta":defensive_delta})
    );
    let success = matches!(case, "valid" | "live-defensive" | "W3");
    assert_eq!(
        server.messages_request_count(),
        if success { 3 + defensive_delta } else { 2 },
        "K2LG boundary V1 invalid pending continuation must send zero HTTP: {case}"
    );
    assert_eq!(outcome.is_ok(), success, "K2LG boundary V1 outcome: {case}");
    assert_eq!(
        session.executions.lock().unwrap().len(),
        2,
        "K2LG boundary client execution stays once-only"
    );
    assert!(
        defensive_failures.is_empty(),
        "K2LG boundary failed live defensive arms: {defensive_failures:?}"
    );
    K2U5Handoff {
        session,
        server,
        other,
        checkpoint,
        before_result,
        trace,
        outcome,
    }
}

async fn k2lg_v_transport(
    session: &K2ReplaySession,
    server: &MockInferenceServer,
    trace: &std::rc::Rc<std::cell::RefCell<K2U5Trace>>,
    route: &'static str,
    case: &'static str,
) {
    let (saved, mut binding, original) = trace.borrow().submitted[1].clone();
    let mut request = saved;
    request.x_grok_req_id = Some(format!("K2LG-{route}-{case}"));
    request.encoded = None;
    k2lg_v_corrupt(&mut request, &binding, case);
    let client = session
        .actor
        .prepare_chat_completion_from_binding(&mut binding, false)
        .await
        .unwrap();
    let before = server.messages_request_count();
    let result: Result<(), String> = tokio::time::timeout(K0_TIMEOUT, async {
        match route {
            "V3" => {
                let (mut stream, _) = client
                    .conversation_stream_messages(request)
                    .await
                    .map_err(|error| error.to_string())?;
                while let Some(event) = stream.next().await {
                    event.map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            "V4" => client
                .conversation_collect(request)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string()),
            _ => unreachable!(),
        }
    })
    .await
    .unwrap();
    let delta = server.messages_request_count() - before;
    println!(
        "K2LG_V {}",
        json!({"route":route,"case":case,"producer":"saved real K2 continuation","credit":"DEFENSIVE_TRANSPORT","http_delta":delta,"expected_digest":original.digest(),"client_executions":session.executions.lock().unwrap().len()})
    );
    if case == "valid" {
        assert!(
            result.is_ok(),
            "K2LG boundary direct positive {route}: {result:?}"
        );
        assert_eq!(delta, 1, "K2LG boundary direct positive sends once {route}");
        let requests = server.requests();
        let sent = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .last()
            .unwrap()
            .body
            .as_ref()
            .unwrap();
        assert_eq!(
            *sent,
            serde_json::from_slice::<Value>(original.bytes()).unwrap(),
            "K2LG boundary direct actual body equals the original bound carrier"
        );
        let primary = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .nth(2)
            .unwrap();
        let direct = requests
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .last()
            .unwrap();
        for header in [
            "authorization",
            "x-api-key",
            "anthropic-version",
            "anthropic-beta",
        ] {
            assert!(
                primary.header(header) == direct.header(header),
                "K2LG boundary defensive transport preserves captured headers without printing values: {header}"
            );
        }
        println!(
            "K2LG_V_TRANSPORT {}",
            json!({"route":route,"case":case,
            "bound_client_preparation":true,"captured_headers_equal":true,"credential_values_printed":false})
        );
    } else {
        assert_eq!(
            delta, 0,
            "K2LG boundary direct invalid proof sends zero {route}/{case}"
        );
        assert!(
            result.is_err(),
            "K2LG boundary direct invalid proof refuses {route}/{case}"
        );
    }
    assert_eq!(
        session.executions.lock().unwrap().len(),
        2,
        "K2LG boundary defensive transport never re-executes clients"
    );
}

async fn k2lg_v_arm(label: &str, arm: impl std::future::Future<Output = ()>) -> bool {
    use futures_util::FutureExt;
    match std::panic::AssertUnwindSafe(arm).catch_unwind().await {
        Ok(()) => {
            println!("\nK2LG_ARM {}", json!({"label":label,"passed":true}));
            true
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("");
            if !message.contains("K2LG boundary") {
                std::panic::resume_unwind(payload);
            }
            println!(
                "\nK2LG_ARM {}",
                json!({"label":label,"passed":false,"assertion":message})
            );
            false
        }
    }
}

async fn k2lg_v_cases() {
    println!("K2LG_BUILD {}", k2lg_compiled_sources());
    let valid = k2lg_v_drive("valid").await;
    k2_u5_assert_continuation(&valid);
    let mut failures = Vec::new();
    if !k2lg_v_arm("V3-V4/live-binding", async {
        k2lg_v_drive("live-defensive").await;
    })
    .await
    {
        failures.push("V3-V4/live-binding".into());
    }
    for case in [
        "missing-plan",
        "missing-native",
        "stale-context",
        "stale-carrier",
        "tools",
        "schema",
        "text",
        "missing-result",
        "duplicate-result",
        "foreign-result",
        "W1",
        "W3",
    ] {
        let label = format!("V1/{case}");
        if !k2lg_v_arm(&label, async {
            let handoff = k2lg_v_drive(case).await;
            if case == "W3" {
                assert!(
                    handoff.outcome.is_err(),
                    "K2LG setup W3 is an explicitly bounded live snapshot, not a completed turn"
                );
            }
        })
        .await
        {
            failures.push(label);
        }
    }
    println!(
        "K2LG_V {}",
        json!({"route":"V2","window":"W2","status":"UNSCHEDULABLE_GAP","reason":"no committed rendezvous between tool_calls.rs:539 and turn.rs:2844; no new product hook","K2LG_kill":false,"L16_credit":false})
    );
    assert!(
        failures.is_empty(),
        "K2LG boundary failed pending arms: {failures:?}"
    );
}

const K2LG_N_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_native_agent_grammar_uses_real_mailbox_producer";
const K2LG_N_ROOT: &str = "k2lg-root";
const K2LG_N_WORKER: &str = "k2lg-worker";
const K2LG_N_PROMPT: &str = "K2LG-N-live";
const K2LG_N_TARGET: &str = "/root/worker";

type K2lgNLog = Arc<std::sync::Mutex<Vec<(String, Value)>>>;

#[derive(Clone, Copy, Default)]
struct K2lgNOptions {
    retries: u32,
    max_turns: Option<usize>,
}

/// Forwards every chat-state persistence call to the real channel writer and records the
/// item-level appends and replacements the native arms observe.
struct K2lgNPersistence {
    inner: crate::session::chat_persistence::ChannelChatPersistence,
    log: K2lgNLog,
}

impl xai_chat_state::ChatPersistence for K2lgNPersistence {
    fn persist_message(&mut self, item: &ConversationItem) {
        self.log
            .lock()
            .unwrap()
            .push(("persist".into(), serde_json::to_value(item).unwrap()));
        xai_chat_state::ChatPersistence::persist_message(&mut self.inner, item);
    }
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::StrictAppendAck, xai_chat_state::StrictAppendError>,
    > {
        xai_chat_state::ChatPersistence::persist_working_directory_switch_and_ack(
            &mut self.inner,
            item,
        )
    }
    fn replace_history(&mut self, items: &[ConversationItem]) {
        self.log
            .lock()
            .unwrap()
            .push(("replace".into(), serde_json::to_value(items).unwrap()));
        xai_chat_state::ChatPersistence::replace_history(&mut self.inner, items);
    }
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> tokio::sync::oneshot::Receiver<std::io::Result<()>> {
        self.log
            .lock()
            .unwrap()
            .push(("strip".into(), serde_json::to_value(items).unwrap()));
        xai_chat_state::ChatPersistence::replace_history_for_strip_and_ack(&mut self.inner, items)
    }
    fn flush(&mut self) {
        xai_chat_state::ChatPersistence::flush(&mut self.inner);
    }
    fn read_native_history(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeRead, k2_history::NativeUnavailable>,
    > {
        xai_chat_state::ChatPersistence::read_native_history(&mut self.inner)
    }
    fn persist_native_generation_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeAppendAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("generation")));
        xai_chat_state::ChatPersistence::persist_native_generation_and_ack(&mut self.inner, write)
    }
    fn persist_native_projection_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeProjectionWrite,
    ) -> tokio::sync::oneshot::Receiver<
        Result<k2_history::NativeCompoundAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("projection")));
        xai_chat_state::ChatPersistence::persist_native_projection_and_ack(&mut self.inner, write)
    }
    fn persist_native_checkpoint_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
        pending: xai_chat_state::persistence::native::NativePendingProtection,
    ) -> tokio::sync::oneshot::Receiver<
        Result<xai_chat_state::persistence::native::NativeAppendAck, k2_history::NativeUnavailable>,
    > {
        self.log
            .lock()
            .unwrap()
            .push(("native".into(), json!("checkpoint")));
        xai_chat_state::ChatPersistence::persist_native_checkpoint_and_ack(
            &mut self.inner,
            write,
            pending,
        )
    }
}

/// Registers the receiver session as an active same-team child whose control is the
/// production `ShellChildRuntime`, so coordinator delivery uses the real follow-up seam.
struct K2lgNRunner {
    commands: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    started: std::rc::Rc<tokio::sync::Notify>,
}

impl k0_coordinator::ChildRunner for K2lgNRunner {
    type Control = crate::agent::subagent::ShellChildRuntime;
    type CompletionData = ();
    type RunFuture = k0_coordinator::LocalBoxFuture<k0_coordinator::ChildRunOutput<()>>;
    type ValidateFuture = std::future::Pending<k0_task::SubagentValidateTypeOutcome>;
    type DescribeFuture = std::future::Pending<k0_task::SubagentDescribeOutcome>;

    fn run(&self, request: k0_coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let commands = self.commands.clone();
        let started = self.started.clone();
        Box::pin(async move {
            let id = request.request.id.clone();
            let (receipt_sink, _receipts) = tokio::sync::mpsc::channel(1);
            assert!(
                request
                    .reporter
                    .started(k0_coordinator::StartedChild {
                        child_session_id: id.clone(),
                        persona: None,
                        resumed_from: None,
                        child_cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                        worktree_path: None,
                        effective_model_id: "k2-replay".into(),
                        definition_background: false,
                        control: crate::agent::subagent::ShellChildRuntime {
                            message_delivery:
                                crate::session::message_delivery::MessageDeliveryHandle::new(
                                    commands.clone(),
                                    id.clone(),
                                ),
                            child_cmd_tx: commands,
                            active_message_target_session_id: id,
                            child_signals: SessionSignalsHandle::new(),
                            _child_thread: None,
                            receipt_sink,
                            force_queue_envelope: false,
                            active_message_parent_session_id: K2LG_N_ROOT.into(),
                            active_message_parent_prompt_index: Arc::new(
                                std::sync::atomic::AtomicUsize::new(0),
                            ),
                        },
                    })
                    .await,
                "K2LG setup worker registers as the active same-team child"
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

struct K2lgN {
    session: K2ReplaySession,
    server: MockInferenceServer,
    root: xai_grok_agent::Agent,
    loop_tx: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    loop_rx: Option<tokio::sync::mpsc::UnboundedReceiver<SessionCommand>>,
    log: K2lgNLog,
    chat_token: tokio_util::sync::CancellationToken,
    agent_wide: Arc<std::sync::atomic::AtomicBool>,
    calls: usize,
}

struct K2lgNTurn {
    running: tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    epoch: TurnEpoch,
    identity: std::rc::Rc<()>,
    submits: std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
    turn_start_ops: usize,
}

struct K2lgNObserved {
    fx: K2lgN,
    barrier: Vec<ConversationItem>,
    barrier_ops: usize,
    turn_start_ops: usize,
    submits: std::rc::Rc<std::cell::RefCell<Vec<Value>>>,
}

/// K2's replay session with three declared differences: the agent-wide turn flag is shared
/// with the sender, the chat-state actor runs on a retained cancellation token with the
/// recording persistence, and retries/max-turns come from `options`.
async fn k2lg_n_session(
    server: &MockInferenceServer,
    options: K2lgNOptions,
    agent_wide: &Arc<std::sync::atomic::AtomicBool>,
    chat_token: &tokio_util::sync::CancellationToken,
    log: &K2lgNLog,
) -> K2ReplaySession {
    let directory = tempfile::tempdir().unwrap();
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "k2-replay".into(),
        api_key: Some("nonsecret-k2-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(options.retries),
        ..Default::default()
    };
    let info = crate::session::info::Info {
        id: acp::SessionId::new(format!(
            "k2lg-n-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        )),
        cwd: directory.path().to_string_lossy().into_owned(),
    };
    let persistence = crate::session::persistence::new_with_explicit_dir(
        &info,
        directory.path().to_path_buf(),
        acp::ModelId::new("k2-replay"),
        xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
        "k2-replay".into(),
        crate::session::persistence::ExplicitSessionOpen::New {
            identity: None,
            next_trace_turn: None,
        },
    )
    .await
    .unwrap();
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                let _ = arguments.response_tx.send(Ok(()));
            }
        }
    });
    let (mut actor, _) = create_test_actor_with_chat_persistence(
        0,
        256_000,
        85,
        gateway_tx,
        persistence.tx.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
    )
    .await;
    let sampling = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("K2LG setup chat-state sampling config");
    let (chat_event_tx, _chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        sampling,
        Box::new(K2lgNPersistence {
            inner: crate::session::chat_persistence::ChannelChatPersistence::new(
                persistence.tx.clone(),
            ),
            log: log.clone(),
        }),
        chat_event_tx,
        chat_token.clone(),
    );
    actor.tool_context.is_turn_active = Some(agent_wide.clone());
    actor.max_turns = options.max_turns;
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
    let mut entry = entry_with_search_flag("k2-replay", true);
    entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    entry.api_base_url = Some(server.url());
    entry.api_key = Some("nonsecret-k2-key".into());
    manager.insert_test_entry("k2-replay", entry);
    manager.set_current_model_id(acp::ModelId::new("k2-replay"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.base_url = server.url();
    binding.config.model = "k2-replay".into();
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.supports_search_tool = true;
    binding.config.max_completion_tokens = Some(4096);
    binding.config.max_retries = Some(options.retries);
    binding.credentials.api_key = Some("nonsecret-k2-key".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let releases = [
        Arc::new(tokio::sync::Semaphore::new(16)),
        Arc::new(tokio::sync::Semaphore::new(16)),
    ];
    for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
        actor
            .agent
            .borrow()
            .tool_bridge()
            .register_mcp_tools(
                name.into(),
                K2ClientTool {
                    name,
                    executions: executions.clone(),
                    entered: entered_tx.clone(),
                    release: releases[index].clone(),
                },
                Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})),
            )
            .unwrap();
    }
    {
        let mut mcp = actor.mcp_state.lock().await;
        mcp.finish_init();
        mcp.complete_init();
    }
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
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        config,
        xai_grok_sampler::RetryPolicy {
            max_retries: options.retries,
            ..Default::default()
        },
        sampler_tx,
    );
    let actor = Arc::new(actor);
    let drainer = actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    let sampler = tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    });
    K2ReplaySession {
        actor,
        persistence,
        directory,
        _models: models,
        tasks: vec![gateway, sampler],
        executions,
        entered,
        releases,
        source_acks,
    }
}

/// Builds the sending root through the production `AgentRebuildSpec`, so the normal
/// `native_agents_enabled_for` gate decides `NativeAgentsEnabled` from declared offline
/// inputs (subagents on, v2 feature tier on, own Messages row `multi_agent_v2 = true`).
async fn k2lg_n_root(
    server: &MockInferenceServer,
    sender: xai_grok_tools::implementations::grok_build::task::backend::SubagentCoordinatorSender,
) -> xai_grok_agent::Agent {
    let mut spec = crate::session::agent_rebuild::test_rebuild_spec_default();
    {
        let spec = Arc::get_mut(&mut spec).expect("K2LG setup root rebuild spec uniquely owned");
        spec.subagents_enabled = true;
        spec.multi_agent_v2_feature = true;
        spec.subagent_event_tx = Some(sender.event_sender().0);
        spec.subagent_coordinator_sender = Some(sender);
        spec.session_id_str = K2LG_N_ROOT.into();
        spec.team_scope_id = K2LG_N_ROOT.into();
    }
    let mut row = entry_with_search_flag("k2lg-root-row", true);
    row.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    row.info.multi_agent_v2 = Some(true);
    row.api_base_url = Some(server.url());
    row.api_key = Some("nonsecret-k2lg-root-key".into());
    spec.models_manager.insert_test_entry("k2lg-root-row", row);
    *spec
        .session_model_id
        .write()
        .expect("K2LG setup session model lock") = acp::ModelId::new("k2lg-root-row");
    let root = spec
        .build_agent(AgentDefinition::default_grok_build())
        .await
        .expect("K2LG setup root agent builds through the normal host gate");
    let gate = root
        .tool_bridge()
        .toolset()
        .resources
        .lock()
        .await
        .get::<k0_task::NativeAgentsEnabled>()
        .map(|gate| gate.0);
    assert_eq!(
        gate,
        Some(true),
        "K2LG setup native_agents_enabled_for arms the root on its declared Messages v2 row"
    );
    assert!(
        root.tool_bridge()
            .toolset()
            .get_tool_metadata("send_message")
            .is_some(),
        "K2LG setup the armed host gate adds the real send_message tool"
    );
    println!(
        "\nK2LG_N {}",
        json!({"producer":"host_gate","native_agents_enabled_for":{"subagents_enabled":true,"subagent_depth":0,"multi_agent_v2_feature":true,"model_row":true},"NativeAgentsEnabled":true,"send_message_tool":true,"row":"k2lg-root-row","api_backend":"messages"})
    );
    root
}

async fn k2lg_n_register_worker(root: &xai_grok_agent::Agent, started: &tokio::sync::Notify) {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        SubagentBackend, SubagentBackendResource,
    };
    let backend = root
        .tool_bridge()
        .read_resource::<SubagentBackendResource>()
        .await
        .expect("K2LG setup root carries its real coordinator backend");
    let spawned = tokio::time::timeout(
        K0_TIMEOUT,
        backend.backend().spawn(
            k0_task::SubagentRequest {
                id: K2LG_N_WORKER.into(),
                prompt: "K2LG receiver worker".into(),
                description: "worker".into(),
                subagent_type: "general-purpose".into(),
                parent_session_id: K2LG_N_ROOT.into(),
                parent_prompt_id: None,
                resume_from: None,
                cwd: None,
                runtime_overrides: k0_task::SubagentRuntimeOverrides {
                    native_agent: Some(k0_task::NativeAgentSpawn {
                        task_name: "worker".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                run_in_background: true,
                surface_completion: false,
                await_to_completion: false,
                fork_context: false,
                context: Default::default(),
                owner: Default::default(),
                cancel_token: Default::default(),
                spawn_root: Default::default(),
            },
            None,
        ),
    )
    .await
    .expect("K2LG setup worker spawn returns")
    .expect("K2LG setup worker spawn accepted");
    assert!(
        spawned.success,
        "K2LG setup named worker spawn succeeds: {:?}",
        spawned.error
    );
    tokio::time::timeout(K0_TIMEOUT, started.notified())
        .await
        .expect("K2LG setup worker becomes active");
}

async fn k2lg_n_fixture(options: K2lgNOptions) -> K2lgN {
    let server = MockInferenceServer::start().await.unwrap();
    let agent_wide = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chat_token = tokio_util::sync::CancellationToken::new();
    let log = K2lgNLog::default();
    let mut session = k2lg_n_session(&server, options, &agent_wide, &chat_token, &log).await;
    let (loop_tx, loop_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<K2lgNRunner>::channel();
    session.tasks.push(tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(
            receiver,
            K2lgNRunner {
                commands: loop_tx.clone(),
                started: started.clone(),
            },
            Default::default(),
        )
        .run(),
    ));
    let root = k2lg_n_root(&server, sender).await;
    k2lg_n_register_worker(&root, &started).await;
    K2lgN {
        session,
        server,
        root,
        loop_tx,
        loop_rx: Some(loop_rx),
        log,
        chat_token,
        agent_wide,
        calls: 0,
    }
}

/// R5-1 harness setup: the turn under test heads the queue with `current_prompt_id` pinned
/// and `session_turn_active` stored; the receiver run loop starts on the first install.
async fn k2lg_n_running(
    fx: &mut K2lgN,
    handle: tokio::task::AbortHandle,
) -> (TurnEpoch, std::rc::Rc<()>) {
    let epoch = fx.session.actor.turn_report.start_next_turn();
    let identity = {
        let mut state = fx.session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(K2LG_N_PROMPT, epoch, handle));
        state
            .pending_inputs
            .push_front(user_item(K2LG_N_PROMPT, "K2LGN"));
        state.running_task.as_ref().unwrap().identity.clone()
    };
    *fx.session.actor.current_prompt_id.lock().unwrap() = Some(K2LG_N_PROMPT.into());
    fx.session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(command_rx) = fx.loop_rx.take() {
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep_channels = (chat_tx, event_tx);
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    (epoch, identity)
}

async fn k2lg_n_stub_running(fx: &mut K2lgN) -> (TurnEpoch, std::rc::Rc<()>) {
    let stub = tokio::task::spawn_local(std::future::pending::<()>());
    let handle = stub.abort_handle();
    fx.session.tasks.push(stub);
    k2lg_n_running(fx, handle).await
}

fn k2lg_n_native_text(body: &str) -> String {
    format!("Untrusted message from agent /root to {K2LG_N_TARGET} (not user consent):\n{body}")
}

fn k2lg_n_is_native(item: &ConversationItem) -> bool {
    matches!(item, ConversationItem::User(user)
        if user.synthetic_reason == Some(xai_grok_sampling_types::SyntheticReason::SystemReminder))
        && item
            .text_content()
            .starts_with("Untrusted message from agent /root to /root/worker (not user consent):")
}

fn k2lg_n_natives(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .filter(|item| k2lg_n_is_native(item))
        .map(ConversationItem::text_content)
        .collect()
}

fn k2lg_n_values(items: &[ConversationItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect()
}

fn k2lg_n_resident(session: &K2ReplaySession) -> Vec<ConversationItem> {
    session
        .actor
        .pending_native_agent_messages
        .lock()
        .unwrap()
        .clone()
}

fn k2lg_n_ops(fx: &K2lgN) -> Vec<(String, Value)> {
    fx.log.lock().unwrap().clone()
}

fn k2lg_n_native_persists(ops: &[(String, Value)]) -> usize {
    ops.iter()
        .filter(|(op, value)| {
            op == "persist"
                && serde_json::from_value::<ConversationItem>(value.clone())
                    .is_ok_and(|item| k2lg_n_is_native(&item))
        })
        .count()
}

fn k2lg_n_bodies(server: &MockInferenceServer) -> Vec<Value> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .filter_map(|request| request.body)
        .collect()
}

fn k2lg_n_text_count(body: &Value, native_body: &str) -> usize {
    let needle = serde_json::to_string(&k2lg_n_native_text(native_body)).unwrap();
    serde_json::to_string(body)
        .unwrap()
        .matches(&needle[1..needle.len() - 1])
        .count()
}

fn k2lg_n_pending_group(checkpoint: &K2Checkpoint) -> Option<String> {
    checkpoint
        .ledger
        .groups
        .iter()
        .find(|group| {
            group.members.iter().any(|member| {
                let raw: Value = serde_json::from_slice(
                    &checkpoint.blobs[&checkpoint.source.ordered_blocks
                        [member.occurrence as usize]
                        .block
                        .digest],
                )
                .unwrap();
                raw["id"] == "srvtoolu_k2_pending"
            })
        })
        .map(|group| group.state.clone())
}

fn k2lg_n_live(checkpoint: &K2Checkpoint) -> bool {
    checkpoint
        .source
        .producing_bindings
        .last()
        .is_some_and(|origin| {
            checkpoint
                .ledger
                .lifecycle
                .exchanges
                .get(&origin.request)
                .map(String::as_str)
                == Some("LiveBound")
                && checkpoint.ledger.groups.iter().any(|group| {
                    group.state == "LivePending" && group.origin_ref == origin.origin_ref
                })
        })
}

async fn k2lg_n_disk(session: &K2ReplaySession) -> Vec<ConversationItem> {
    use crate::session::storage::StorageAdapter;
    let (respond_to, flushed) = tokio::sync::oneshot::channel();
    session
        .persistence
        .tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    flushed.await.unwrap().unwrap();
    crate::session::storage::JsonlStorageAdapter::with_explicit_session_dir(
        session.directory.path().to_path_buf(),
    )
    .load_session(&session.actor.session_info)
    .await
    .unwrap()
    .chat_history
}

async fn k2lg_n_selected<F: std::future::Future>(fx: &K2lgN, future: F) -> F::Output {
    super::switch_projection::HISTORY_S3_TEST_DOMAIN
        .scope(
            fx.session.domain(&fx.server),
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT
                .scope(k2_history::NativeHistoryRequirement::Selected, future),
        )
        .await
}

async fn k2lg_n_close_mailbox(fx: &K2lgN) {
    fx.chat_token.cancel();
    tokio::time::timeout(K0_TIMEOUT, async {
        while fx
            .session
            .actor
            .chat_state_handle
            .get_sampling_config()
            .await
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG setup the real chat-state receiver closes");
}

/// One real `send_message` from the gated root: tool -> coordinator -> `ShellChildRuntime`
/// -> `SessionCommand::AgentMessage` -> `message_item` -> resident buffer.
async fn k2lg_n_send(fx: &mut K2lgN, body: &str) {
    fx.calls += 1;
    let before = k2lg_n_resident(&fx.session).len();
    let result = fx
        .root
        .tool_bridge()
        .call(
            "send_message",
            json!({"target": K2LG_N_TARGET, "message": body}),
            &format!("k2lg_n_send_{}", fx.calls),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("K2LG boundary real send_message tool call failed: {error}")
        });
    let xai_grok_tools::types::output::ToolOutput::Dynamic(output) = &result.output else {
        panic!("K2LG boundary send_message returns its coordinator receipt");
    };
    assert_eq!(
        output.value["status"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("delivered"),
        "K2LG boundary the real coordinator delivers live to the worker: {}",
        output.value
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        while k2lg_n_resident(&fx.session).len() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG boundary the receiver run loop buffers the real native message");
    println!(
        "\nK2LG_N {}",
        json!({"producer":"send_message->coordinator->ShellChildRuntime->AgentMessage->message_item->resident","call":fx.calls,"status":output.value["status"],"resident":k2lg_n_resident(&fx.session).len()})
    );
}

fn k2lg_n_closure_frames() -> Vec<Value> {
    k2_frames(
        "msg_k2lg_n_closed",
        vec![
            k2_result("srvtoolu_k2_pending", "alpha__lookup"),
            (
                json!({"type":"text","text":"K2LG N continuation closes the pending search"}),
                Vec::new(),
            ),
        ],
        "end_turn",
    )
}

fn k2lg_n_after_frames(id: &str) -> Vec<Value> {
    k2_frames(
        id,
        vec![(
            json!({"type":"text","text":"K2LG N after the native release"}),
            Vec::new(),
        )],
        "end_turn",
    )
}

fn k2lg_n_results_tail() -> Value {
    json!({"role":"user","content":[
        {"type":"tool_result","tool_use_id":"client_k2_c1","content":"alpha__lookup actual result: \"first actual\""},
        {"type":"tool_result","tool_use_id":"client_k2_c2","content":"zeta__fetch actual result: \"second actual\"","cache_control":{"type":"ephemeral"}}
    ]})
}

/// Seeds K2's completed group, then drives K2's real mixed pending handoff under Selected
/// with the second client held, the turn installed per R5-1 and the receiver loop running.
async fn k2lg_n_handoff(fx: &mut K2lgN, continuation: Option<ScriptedResponse>) -> K2lgNTurn {
    use k2_history::*;
    k2_seed(&mut fx.session, &fx.server).await;
    fx.session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    fx.server
        .enqueue_response("/v1/messages", k2_script(k2_u5_frames()));
    if let Some(continuation) = continuation {
        fx.server.enqueue_response("/v1/messages", continuation);
    }
    fx.session.actor.chat_state_handle.begin_turn_capture();
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let turn_start_ops = fx.log.lock().unwrap().len();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(
            "K2LG N real native producer during the live handoff",
        ))
        .await;
    fx.session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let log = fx.log.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let running = tokio::task::spawn_local(async move {
        let mut prepare =
            move |request: &mut ConversationRequest,
                  _: &xai_chat_state::RequestTransportBinding| {
                record.borrow_mut().push(json!({
                    "items": serde_json::to_value(&request.items).unwrap(),
                    "history_revision": request
                        .native_bound_preparation
                        .as_ref()
                        .and_then(|preparation| preparation.history_revision()),
                    "persistence_ops": log.lock().unwrap().len(),
                }));
                Ok(())
            };
        super::switch_projection::HISTORY_S3_TEST_DOMAIN
            .scope(
                domain,
                super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                    NativeHistoryRequirement::Selected,
                    actor.process_conversation_turn_for_native_history_test(
                        K2LG_N_PROMPT,
                        None,
                        None,
                        None,
                        &mut super::length_salvage::LengthSalvage::new(None),
                        &mut Default::default(),
                        &mut prepare,
                    ),
                ),
            )
            .await
    });
    let (epoch, identity) = k2lg_n_running(fx, running.abort_handle()).await;
    assert_eq!(
        tokio::time::timeout(K0_TIMEOUT, fx.session.source_acks.recv())
            .await
            .unwrap(),
        Some(NativeResponseStatus::Accepted),
        "K2LG setup K2's mixed response is decoded and committed before client execution"
    );
    let mut entered = Vec::new();
    for _ in 0..2 {
        entered.push(
            tokio::time::timeout(K0_TIMEOUT, fx.session.entered.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    entered.sort();
    assert_eq!(entered, vec!["alpha__lookup", "zeta__fetch"]);
    tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            let items = fx.session.actor.chat_state_handle.get_conversation().await;
            if items.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fx.server.messages_request_count(), 2);
    let (checkpoint, _) = fx.session.reload().await;
    assert!(
        k2lg_n_live(&checkpoint),
        "K2LG setup K2's exchange is LiveBound with a LivePending group while clients execute"
    );
    K2lgNTurn {
        running,
        epoch,
        identity,
        submits,
        turn_start_ops,
    }
}

async fn k2lg_n_n1() -> K2lgNObserved {
    const BODY: &str = "K2LG N native body during the live handoff";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    let sender_turn = super::turn_task::TurnActiveGuard::activate(Some(&fx.agent_wide));
    assert!(
        fx.agent_wide.load(std::sync::atomic::Ordering::SeqCst),
        "K2LG setup the sender session's turn is in flight on the agent-wide flag"
    );
    k2lg_n_send(&mut fx, BODY).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 the real plaintext reminder is resident while K2's continuation is owed"
    );
    assert!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await).is_empty(),
        "K2LG boundary N1 no native item reaches history while pending"
    );
    drop(sender_turn);
    assert!(
        !fx.agent_wide.load(std::sync::atomic::Ordering::SeqCst),
        "K2LG setup the sender session's turn ended and cleared the agent-wide flag"
    );
    let mut held = fx.server.expect_response_blocked(
        "K2LG N continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N1 the legitimate results-only continuation reaches HTTP while the native prefix stays resident");
    let barrier = fx.session.actor.chat_state_handle.get_conversation().await;
    let barrier_ops = fx.log.lock().unwrap().len();
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 per-session rule keeps the receiver's prefix resident after the sender's turn ended"
    );
    assert!(
        k2lg_n_natives(&barrier).is_empty(),
        "K2LG boundary N1 history holds no native item before closure"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2]["messages"].as_array().unwrap().last().unwrap(),
        &k2lg_n_results_tail(),
        "K2LG boundary N1 the continuation is the exact results-only message"
    );
    assert_eq!(
        k2lg_n_text_count(&bodies[2], BODY),
        0,
        "K2LG boundary N1 the native reminder is not transmitted before closure"
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N1 the turn finishes after closure")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N1 closure release completes the same turn: {:?}",
        outcome.err()
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N1 the unchanged native prefix is released exactly once after closure"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N1 the released prefix leaves the resident buffer"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        4,
        "K2LG boundary N1 exactly one same-turn model round follows the release"
    );
    assert_eq!(
        k2lg_n_text_count(&bodies[3], BODY),
        1,
        "K2LG boundary N1 the model sees the native reminder once, only after closure"
    );
    let (checkpoint, _) = fx.session.reload().await;
    assert_eq!(
        k2lg_n_pending_group(&checkpoint).as_deref(),
        Some("Completed"),
        "K2LG boundary N1 the matching server closure was decoded before the release"
    );
    let prompts: Vec<String> = fx
        .session
        .actor
        .state
        .lock()
        .await
        .pending_inputs
        .iter()
        .map(|input| input.prompt_id.clone())
        .collect();
    assert_eq!(
        prompts,
        vec![K2LG_N_PROMPT.to_string()],
        "K2LG boundary N1 the release opens no new prompt"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N1","closure":"Completed","continuation_http":1,"post_release_rounds":1,"native_occurrences":1,"per_session":{"agent_wide_flag_after_sender_turn":false,"receiver_prefix_resident":true},"W_TE":"U6-owned, not asserted"})
    );
    K2lgNObserved {
        fx,
        barrier,
        barrier_ops,
        turn_start_ops: turn.turn_start_ops,
        submits: turn.submits,
    }
}

async fn k2lg_n_n6_i(observed: &K2lgNObserved) {
    let ops = k2lg_n_ops(&observed.fx);
    let natives: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (op, value))| {
            op == "persist"
                && serde_json::from_value::<ConversationItem>(value.clone())
                    .is_ok_and(|item| k2lg_n_is_native(&item))
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        natives.len(),
        1,
        "K2LG boundary N6/i one persistence append for the one occurrence"
    );
    let release = natives[0];
    assert!(
        release > observed.barrier_ops,
        "K2LG boundary N6/i the native append follows closure"
    );
    // The release is one chat-state command: a repair inside it would replace history just
    // before the append, a prune just after it.
    assert_ne!(
        ops[release - 1].0,
        "replace",
        "K2LG boundary N6/i no integrity repair precedes the native append"
    );
    assert!(
        ops.get(release + 1).is_none_or(|(op, _)| op != "replace"),
        "K2LG boundary N6/i no prune replacement follows the native append"
    );
    let appended: ConversationItem = serde_json::from_value(ops[release].1.clone()).unwrap();
    assert_eq!(
        k2lg_n_natives(std::slice::from_ref(&appended)),
        vec![k2lg_n_native_text(
            "K2LG N native body during the live handoff"
        )],
        "K2LG boundary N6/i the native prefix is appended unchanged"
    );
    let between = &ops[observed.barrier_ops..release];
    assert!(
        between
            .iter()
            .all(|(op, _)| op == "persist" || op == "native"),
        "K2LG boundary N6/i no history replacement between closure and the release"
    );
    let closure_appends = between.iter().filter(|(op, _)| op == "persist").count();
    let live = observed
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    assert_eq!(
        live.iter().position(k2lg_n_is_native),
        Some(observed.barrier.len() + closure_appends),
        "K2LG boundary N6/i the native item lands exactly where a plain append lands"
    );
    let names: Vec<&str> = ops[observed.barrier_ops..]
        .iter()
        .map(|(op, _)| op.as_str())
        .collect();
    let revisions: Vec<Value> = observed
        .submits
        .borrow()
        .iter()
        .map(|submit| json!({"history_revision":submit["history_revision"],"persistence_ops":submit["persistence_ops"]}))
        .collect();
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/i","release_op_index":release,"closure_appends":closure_appends,"ops_after_closure":names,"plain_append_equivalent":true,"native_persists":1,"repair_before":false,"prune_after":false,"submit_revisions":revisions})
    );
}

async fn k2lg_n_n6_iv(observed: &K2lgNObserved) {
    let appended: Vec<Value> = k2lg_n_ops(&observed.fx)[observed.turn_start_ops..]
        .iter()
        .filter(|(op, _)| op == "persist")
        .map(|(_, value)| value.clone())
        .collect();
    let capture = observed
        .fx
        .session
        .actor
        .chat_state_handle
        .take_turn_messages()
        .await
        .expect("K2LG boundary N6/iv the active turn capture is present");
    assert!(
        !capture.compaction_occurred,
        "K2LG boundary N6/iv no compaction occurred in the turn"
    );
    assert_eq!(
        k2lg_n_values(&capture.messages),
        appended,
        "K2LG boundary N6/iv pre_replacement_messages plus tail equal the turn's ordered content"
    );
    assert_eq!(
        k2lg_n_natives(&capture.messages).len(),
        1,
        "K2LG boundary N6/iv the capture holds the native occurrence once"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/iv","observer":"TakeTurnMessages (consuming, last read)","captured_items":capture.messages.len(),"turn_appends":appended.len()})
    );
}

async fn k2lg_n_turn_end() {
    const FIRST: &str = "K2LG N turn-end first";
    const SECOND: &str = "K2LG N turn-end second";
    let mut fx = k2lg_n_fixture(K2lgNOptions {
        max_turns: Some(1),
        ..Default::default()
    })
    .await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, FIRST).await;
    k2lg_n_send(&mut fx, SECOND).await;
    fx.session.releases[1].add_permits(1);
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N1 turn-end leg the turn returns at its max-turns limit")
        .unwrap();
    assert!(
        matches!(outcome, Ok(TurnOutcome::MaxTurnsReached { .. })),
        "K2LG boundary N1 turn-end leg the turn ends with K2's continuation still owed"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(FIRST), k2lg_n_native_text(SECOND)],
        "K2LG boundary N1 turn-end leg the prefix is resident when the turn returns"
    );
    assert!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await).is_empty(),
        "K2LG boundary N1 turn-end leg no native item reached history during the turn"
    );
    assert_eq!(fx.server.messages_request_count(), 2);
    let (checkpoint, _) = fx.session.reload().await;
    assert!(
        k2lg_n_live(&checkpoint),
        "K2LG boundary N1 turn-end leg the exchange is still LiveBound with a LivePending group"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let ops_before = k2lg_n_ops(&fx).len();
    let owned = k2lg_n_selected(
        &fx,
        fx.session.actor.handle_completion(
            K2LG_N_PROMPT.into(),
            turn.epoch,
            &turn.identity,
            Ok(PromptTurnOk {
                stop_reason: acp::StopReason::MaxTurnRequests,
                total_tokens: 0,
                turn_snapshot: None,
                completion_kind: PromptCompletionKind::MaxTurnsReached { limit: 1 },
                structured_output: None,
                usage: None,
                tool_overrides: None,
            }),
            Some(0),
        ),
    )
    .await;
    assert!(owned, "K2LG setup the turn-end completion is owned");
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(FIRST), k2lg_n_native_text(SECOND)],
        "K2LG boundary N1 turn-end leg the turn-end flush releases the prefix in resident order"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N1 turn-end leg the released prefix leaves the resident buffer"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)[ops_before..]),
        2,
        "K2LG boundary N1 turn-end leg one persistence append per occurrence"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N1/turn-end","site":"turn_end.rs:220 handle_completion","session_turn_active":false,"exchange_live_at_release":true,"released":2})
    );
}

async fn k2lg_n_n2() {
    const BODY: &str = "K2LG N equal body";
    const USER: &str = "K2LG N ordinary user after native";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, BODY).await;
    k2lg_n_send(&mut fx, BODY).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY), k2lg_n_native_text(BODY)],
        "K2LG boundary N2 two equal-body occurrences stay two resident items"
    );
    let mut held = fx.server.expect_response_blocked(
        "K2LG N2 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N2 the continuation reaches HTTP with both occurrences resident");
    assert_eq!(k2lg_n_resident(&fx.session).len(), 2);
    fx.loop_tx
        .send(SessionCommand::Interject {
            text: USER.into(),
            id: None,
            images: Vec::new(),
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !fx
            .session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|entry| entry.text == USER)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("K2LG setup the ordinary user input is queued through the real run loop");
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n2_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N2 the turn finishes after closure")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N2 the release completes the same turn: {:?}",
        outcome.err()
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_natives(&after),
        vec![k2lg_n_native_text(BODY), k2lg_n_native_text(BODY)],
        "K2LG boundary N2 two logical occurrences, no text dedup and no repeat"
    );
    let first = after.iter().position(k2lg_n_is_native).unwrap();
    let user = after
        .iter()
        .position(|item| {
            matches!(item, ConversationItem::User(_)) && item.text_content().contains(USER)
        })
        .expect("K2LG boundary N2 the ordinary user input is delivered");
    assert!(
        k2lg_n_is_native(&after[first + 1]) && first + 2 == user,
        "K2LG boundary N2 the native prefix is released in resident order before the ordinary user batch"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)),
        2,
        "K2LG boundary N2 one persistence append per logical occurrence"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4);
    assert_eq!(
        k2lg_n_text_count(&bodies[3], BODY),
        2,
        "K2LG boundary N2 the next model round carries both occurrences once each"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N2","native_occurrences":2,"order":"native,native,user","persist_appends":2})
    );
}

async fn k2lg_n_n3() {
    const BODY: &str = "K2LG N closed mailbox";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let disk_before = k2lg_n_values(&k2lg_n_disk(&fx.session).await);
    let ops_before = k2lg_n_ops(&fx).len();
    k2lg_n_close_mailbox(&fx).await;
    let read = fx
        .session
        .actor
        .chat_state_handle
        .read_native_history()
        .await;
    assert!(
        matches!(read, Err(k2_history::NativeUnavailable::Persistence)),
        "K2LG setup the closed receiver is first met at read_native_history"
    );
    for attempt in 0..2 {
        let drained =
            k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
        assert!(
            !drained,
            "K2LG boundary N3 a failed fallible enqueue reports no delivery (attempt {attempt})"
        );
        assert_eq!(
            k2lg_n_natives(&k2lg_n_resident(&fx.session)),
            vec![k2lg_n_native_text(BODY)],
            "K2LG boundary N3 the original prefix stays resident after a closed-mailbox failure (attempt {attempt})"
        );
    }
    k2lg_n_selected(&fx, fx.session.actor.flush_pending_skill_reminders()).await;
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N3 the second flush site is lossless too"
    );
    assert_eq!(
        k2lg_n_ops(&fx).len(),
        ops_before,
        "K2LG boundary N3 no persistence append on a closed mailbox"
    );
    assert_eq!(
        k2lg_n_values(&k2lg_n_disk(&fx.session).await),
        disk_before,
        "K2LG boundary N3 no native history reaches disk"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N3","first_met":"read_native_history Err(Persistence)","enqueue":"Err(ChatStateMailboxClosed)","resident_after_failures":1,"delivery_reported":false})
    );
}

async fn k2lg_n_n4() {
    const FIRST: &str = "K2LG N first prefix";
    const LATER: &str = "K2LG N later suffix";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, FIRST).await;
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        drained,
        "K2LG boundary N4 the first prefix is enqueued successfully"
    );
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "K2LG boundary N4 the successfully enqueued prefix is removed once"
    );
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(FIRST)]
    );
    k2lg_n_send(&mut fx, LATER).await;
    k2lg_n_close_mailbox(&fx).await;
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        !drained,
        "K2LG boundary N4 the later release failure reports no delivery"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_resident(&fx.session)),
        vec![k2lg_n_native_text(LATER)],
        "K2LG boundary N4 only the unsent suffix remains eligible; the accepted prefix never reappears"
    );
    assert_eq!(
        k2lg_n_natives(&k2lg_n_disk(&fx.session).await),
        vec![k2lg_n_native_text(FIRST)],
        "K2LG boundary N4 the accepted prefix is persisted exactly once"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N4","accepted_prefix":1,"later_failure":"closed mailbox","resident_after":"unsent suffix only","constructed_defensive":false})
    );
}

async fn k2lg_n_n5() {
    const BODY: &str = "K2LG N retried transport";
    let mut fx = k2lg_n_fixture(K2lgNOptions {
        retries: 2,
        ..Default::default()
    })
    .await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    k2lg_n_send(&mut fx, BODY).await;
    let mut held = fx.server.expect_response_blocked(
        "K2LG N5 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N5 the continuation reaches HTTP with the native prefix resident");
    let mut throttled = ScriptedResponse::json(
        429,
        json!({"type":"error","error":{"type":"rate_limit_error","message":"K2LG N transient rate limit"}}),
    );
    throttled.headers.push(("retry-after".into(), "1".into()));
    fx.server.enqueue_response("/v1/messages", throttled);
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_k2lg_n5_after")),
    );
    held.release();
    let outcome = tokio::time::timeout(Duration::from_secs(90), &mut turn.running)
        .await
        .expect("K2LG boundary N5 the turn finishes after the transport retry")
        .unwrap();
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        outcome.is_ok(),
        "K2LG boundary N5 the retried round completes: {:?}",
        outcome.err()
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        5,
        "K2LG boundary N5 the post-release request is retried once at the transport"
    );
    assert_eq!(
        bodies[3], bodies[4],
        "K2LG boundary N5 the retry resends identical request bytes"
    );
    assert_eq!(k2lg_n_text_count(&bodies[4], BODY), 1);
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(BODY)],
        "K2LG boundary N5 the retry never readmits or appends the native message again"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(&fx)),
        1,
        "K2LG boundary N5 one persistence append despite the transport retry"
    );
    assert!(k2lg_n_resident(&fx.session).is_empty());
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N5","http_attempts_for_round":2,"identical_bytes":true,"native_appends":1,"native_persists":1})
    );
}

/// Shared tail of the N6/ii routes: the first subsequent ordinary push, then the live
/// history and the disk read-back.
async fn k2lg_n_ordinary_push(
    fx: &K2lgN,
    text: &str,
) -> (Vec<ConversationItem>, Vec<ConversationItem>) {
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(text))
        .await;
    let live = fx.session.actor.chat_state_handle.get_conversation().await;
    (live, k2lg_n_disk(&fx.session).await)
}

type K2lgNRoute = (
    Vec<ConversationItem>,
    Vec<ConversationItem>,
    Vec<(String, Value)>,
);

fn k2lg_n_compare(native: &[ConversationItem], control: &[ConversationItem], route: &str) -> usize {
    let positions: Vec<usize> = native
        .iter()
        .enumerate()
        .filter(|(_, item)| k2lg_n_is_native(item))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "K2LG boundary N6/ii {route}: one native occurrence"
    );
    let mut stripped = native.to_vec();
    stripped.remove(positions[0]);
    assert_eq!(
        k2lg_n_values(&stripped),
        k2lg_n_values(control),
        "K2LG boundary N6/ii {route}: the native release adds no repair, prune, rewrite or duplicate relative to the matched control"
    );
    assert_eq!(
        positions[0],
        control.len() - 1,
        "K2LG boundary N6/ii {route}: the native prefix sits immediately before the first subsequent ordinary push"
    );
    positions[0]
}

async fn k2lg_n_cancel_client(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N cancel during client execution";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    let ops_before = k2lg_n_ops(&fx).len();
    let _cancel = k2lg_n_selected(
        &fx,
        fx.session
            .actor
            .cancel_running_task(crate::session::CancelOptions::default()),
    )
    .await;
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii command-Cancel ends the turn under test");
    assert!(
        matches!(joined, Err(ref error) if error.is_cancelled()),
        "K2LG boundary N6/ii command-Cancel aborts the turn under test"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii the command-Cancel flush releases the resident prefix"
        );
    }
    let (live, disk) = k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after cancel").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_cancel_results(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N cancel after genuine results";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(&mut fx, None).await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    let mut held = fx.server.expect_response_blocked(
        "K2LG N6 cancel-after-results continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    tokio::time::timeout(K0_TIMEOUT, held.wait_blocked())
        .await
        .expect("K2LG boundary N6/ii cancel-after-results the continuation reaches HTTP with the genuine results in history");
    if native {
        assert_eq!(
            k2lg_n_resident(&fx.session).len(),
            1,
            "K2LG boundary N6/ii cancel-after-results the prefix is still resident"
        );
    }
    let ops_before = k2lg_n_ops(&fx).len();
    let _cancel = k2lg_n_selected(
        &fx,
        fx.session
            .actor
            .cancel_running_task(crate::session::CancelOptions::default()),
    )
    .await;
    held.release();
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii command-Cancel ends the turn under test");
    assert!(
        matches!(joined, Err(ref error) if error.is_cancelled()),
        "K2LG boundary N6/ii command-Cancel aborts the turn under test"
    );
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii the command-Cancel flush releases the resident prefix"
        );
    }
    let (live, disk) = k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after cancel").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_error(native: bool) -> K2lgNRoute {
    const BODY: &str = "K2LG N error route";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    let mut turn = k2lg_n_handoff(
        &mut fx,
        Some(ScriptedResponse::json(
            400,
            json!({"type":"error","error":{"type":"invalid_request_error","message":"K2LG N offline continuation failure"}}),
        )),
    )
    .await;
    if native {
        k2lg_n_send(&mut fx, BODY).await;
    }
    fx.session.releases[1].add_permits(1);
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("K2LG boundary N6/ii error route the turn returns")
        .unwrap();
    let error = outcome.err().expect(
        "K2LG boundary N6/ii error route the continuation failure ends the turn with an error",
    );
    assert_eq!(
        fx.server.messages_request_count(),
        3,
        "K2LG boundary N6/ii error route the ordinary continuation attempt is unchanged by the native message"
    );
    if native {
        assert_eq!(
            k2lg_n_resident(&fx.session).len(),
            1,
            "K2LG boundary N6/ii error route the prefix is still resident when the turn ends"
        );
    }
    fx.session
        .actor
        .session_turn_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let ops_before = k2lg_n_ops(&fx).len();
    let owned = k2lg_n_selected(
        &fx,
        fx.session.actor.handle_completion(
            K2LG_N_PROMPT.into(),
            turn.epoch,
            &turn.identity,
            Err(error),
            Some(0),
        ),
    )
    .await;
    assert!(owned, "K2LG setup the error completion is owned");
    if native {
        assert!(
            k2lg_n_resident(&fx.session).is_empty(),
            "K2LG boundary N6/ii error route the turn-end flush releases the prefix"
        );
    }
    let (live, disk) =
        k2lg_n_ordinary_push(&fx, "K2LG N first ordinary push after the error").await;
    (live, disk, k2lg_n_ops(&fx)[ops_before..].to_vec())
}

async fn k2lg_n_n6_cancel_client() {
    let (native, native_disk, ops) = k2lg_n_cancel_client(true).await;
    let (control, control_disk, _) = k2lg_n_cancel_client(false).await;
    let position = k2lg_n_compare(&native, &control, "command-Cancel during client execution");
    k2lg_n_compare(
        &native_disk,
        &control_disk,
        "command-Cancel during client execution (disk)",
    );
    assert!(
        matches!(&native[position - 1], ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c2"),
        "K2LG boundary N6/ii the cancel's eager repair of the dangling waiting call precedes the native release"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-cancel-during-client","site":"cancel.rs:799 after eager repair cancel.rs:718-726","matched_control_equal_after_first_ordinary_push":true,"ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_cancel_results() {
    let (native, native_disk, ops) = k2lg_n_cancel_results(true).await;
    let (control, control_disk, _) = k2lg_n_cancel_results(false).await;
    let position = k2lg_n_compare(&native, &control, "command-Cancel after genuine results");
    k2lg_n_compare(
        &native_disk,
        &control_disk,
        "command-Cancel after genuine results (disk)",
    );
    assert!(
        matches!(&native[position - 1], ConversationItem::ToolResult(result)
            if result.tool_call_id == "client_k2_c2" && result.content.contains("actual result")),
        "K2LG boundary N6/ii a cancel after the genuine results leaves nothing to repair"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-cancel-after-results","site":"cancel.rs:799","repair":"none needed","matched_control_equal_after_first_ordinary_push":true,"ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_error() {
    let (native, native_disk, ops) = k2lg_n_error(true).await;
    let (control, control_disk, control_ops) = k2lg_n_error(false).await;
    k2lg_n_compare(&native, &control, "error route");
    k2lg_n_compare(&native_disk, &control_disk, "error route (disk)");
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/ii-error","site":"turn_end.rs:220 after a continuation error","matched_control_equal_after_first_ordinary_push":true,"native_ops":ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(),"control_ops":control_ops.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>()})
    );
}

async fn k2lg_n_n6_prune() {
    const BODY: &str = "K2LG N prune eligibility";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    for index in 0..13 {
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                &format!("msg_k2lg_n_ordinary_{index}"),
                vec![(
                    json!({"type":"tool_use","id":format!("k2lg_n_ordinary_{index}"),"name":"alpha__lookup","input":{"value":format!("ordinary {index}")}}),
                    Vec::new(),
                )],
                "tool_use",
            )),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2_frames(
                &format!("msg_k2lg_n_ordinary_{index}_done"),
                vec![(
                    json!({"type":"text","text":"ordinary turn done"}),
                    Vec::new(),
                )],
                "end_turn",
            )),
        );
        fx.session.actor.chat_state_handle.increment_prompt_index();
        fx.session
            .turn(
                k2_history::NativeHistoryRequirement::Inactive,
                &fx.server,
                &format!("K2LG N ordinary prompt {index}"),
            )
            .await
            .expect("K2LG setup ordinary seeded turn");
    }
    let cleared = |items: &[ConversationItem]| -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                ConversationItem::ToolResult(result)
                    if result.tool_call_id.starts_with("k2lg_n_ordinary_")
                        && !result.content.contains("actual result") =>
                {
                    Some(result.tool_call_id.clone())
                }
                _ => None,
            })
            .collect()
    };
    let seeded = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !cleared(&seeded).is_empty(),
        "K2LG setup the seeded history reached real prune eligibility"
    );
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let before = fx.session.actor.chat_state_handle.get_conversation().await;
    let ops_before = k2lg_n_ops(&fx).len();
    let drained = k2lg_n_selected(&fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    assert!(
        drained,
        "K2LG boundary N6/iii the native prefix is released"
    );
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        k2lg_n_values(&after[..before.len()]),
        k2lg_n_values(&before),
        "K2LG boundary N6/iii the native release prunes or rewrites nothing"
    );
    assert_eq!(after.len(), before.len() + 1);
    assert!(k2lg_n_is_native(&after[before.len()]));
    let ops: Vec<(String, Value)> = k2lg_n_ops(&fx)[ops_before..]
        .iter()
        .filter(|(op, _)| op != "native")
        .cloned()
        .collect();
    assert_eq!(
        (ops.len(), k2lg_n_native_persists(&ops)),
        (1, 1),
        "K2LG boundary N6/iii the release is exactly one append with no prune replacement"
    );
    fx.session.actor.chat_state_handle.increment_prompt_index();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user(
            "K2LG N first ordinary push after native",
        ))
        .await;
    let pushed = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        cleared(&pushed).len() > cleared(&after).len(),
        "K2LG setup the boundary result was eligible for the next real prompt"
    );
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/iii","seeded_turns":13,"cleared_before":cleared(&seeded).len(),"cleared_after_native":cleared(&after).len(),"cleared_after_next_prompt":cleared(&pushed).len()})
    );
}

async fn k2lg_n_inactive() {
    const BODY: &str = "K2LG N inactive plain flush";
    let mut fx = k2lg_n_fixture(K2lgNOptions::default()).await;
    k2lg_n_stub_running(&mut fx).await;
    k2lg_n_send(&mut fx, BODY).await;
    let ops_before = k2lg_n_ops(&fx).len();
    let drained = fx.session.actor.drain_interjections_at_safe_point().await;
    assert!(
        drained,
        "K2LG boundary N6 Inactive keeps the plain flush and reports native activity"
    );
    assert!(k2lg_n_resident(&fx.session).is_empty());
    assert_eq!(
        k2lg_n_natives(&fx.session.actor.chat_state_handle.get_conversation().await),
        vec![k2lg_n_native_text(BODY)]
    );
    assert_eq!(k2lg_n_native_persists(&k2lg_n_ops(&fx)[ops_before..]), 1);
    println!(
        "\nK2LG_N {}",
        json!({"arm":"N6/inactive-no-plan","path":"take + push_tool_result","native_appends":1})
    );
}

async fn k2lg_n_arm(label: &str, arm: impl std::future::Future<Output = ()>) -> bool {
    use futures_util::FutureExt;
    match std::panic::AssertUnwindSafe(arm).catch_unwind().await {
        Ok(()) => {
            println!("\nK2LG_N_ARM {}", json!({"label":label,"passed":true}));
            true
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("");
            if !message.contains("K2LG boundary") {
                std::panic::resume_unwind(payload);
            }
            println!(
                "\nK2LG_N_ARM {}",
                json!({"label":label,"passed":false,"assertion":message})
            );
            false
        }
    }
}

fn k2lg_n_compiled_sources() -> Value {
    json!({
        "session_setup": native_content_digest(include_bytes!("../acp_session_impl/session_setup.rs")),
        "interjection": native_content_digest(include_bytes!("../acp_session_impl/interjection.rs")),
        "fixture": native_content_digest(include_bytes!("native_messages_replay_tests.rs")),
    })
}

async fn k2lg_n_cases() {
    println!("\nK2LG_N_BUILD {}", k2lg_n_compiled_sources());
    let mut failures = Vec::new();
    let mut n1 = None;
    if !k2lg_n_arm("N1/live-retain-per-session-closure-release", async {
        n1 = Some(k2lg_n_n1().await);
    })
    .await
    {
        failures.push("N1/live-retain-per-session-closure-release");
    }
    if !k2lg_n_arm("N6/i-post-closure-mid-turn", async {
        k2lg_n_n6_i(
            n1.as_ref()
                .expect("K2LG boundary N6/i needs the completed N1 run"),
        )
        .await;
    })
    .await
    {
        failures.push("N6/i-post-closure-mid-turn");
    }
    if !k2lg_n_arm("N6/iv-active-turn-capture", async {
        k2lg_n_n6_iv(
            n1.as_ref()
                .expect("K2LG boundary N6/iv needs the completed N1 run"),
        )
        .await;
    })
    .await
    {
        failures.push("N6/iv-active-turn-capture");
    }
    drop(n1);
    if !k2lg_n_arm("N1/turn-end-leg", k2lg_n_turn_end()).await {
        failures.push("N1/turn-end-leg");
    }
    if !k2lg_n_arm("N2/equal-body-order", k2lg_n_n2()).await {
        failures.push("N2/equal-body-order");
    }
    if !k2lg_n_arm("N3/closed-mailbox", k2lg_n_n3()).await {
        failures.push("N3/closed-mailbox");
    }
    if !k2lg_n_arm("N4/success-prefix-then-failure", k2lg_n_n4()).await {
        failures.push("N4/success-prefix-then-failure");
    }
    if !k2lg_n_arm("N5/transport-retry", k2lg_n_n5()).await {
        failures.push("N5/transport-retry");
    }
    if !k2lg_n_arm(
        "N6/ii-command-cancel-during-client",
        k2lg_n_n6_cancel_client(),
    )
    .await
    {
        failures.push("N6/ii-command-cancel-during-client");
    }
    if !k2lg_n_arm(
        "N6/ii-command-cancel-after-results",
        k2lg_n_n6_cancel_results(),
    )
    .await
    {
        failures.push("N6/ii-command-cancel-after-results");
    }
    if !k2lg_n_arm("N6/ii-error-route", k2lg_n_n6_error()).await {
        failures.push("N6/ii-error-route");
    }
    if !k2lg_n_arm("N6/iii-prune-eligibility", k2lg_n_n6_prune()).await {
        failures.push("N6/iii-prune-eligibility");
    }
    if !k2lg_n_arm("N6/inactive-no-plan", k2lg_n_inactive()).await {
        failures.push("N6/inactive-no-plan");
    }
    assert!(
        failures.is_empty(),
        "K2LG boundary failed native arms: {failures:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_native_agent_grammar_uses_real_mailbox_producer() {
    if std::env::var_os("K2LG_N_CHILD").is_some() {
        tokio::task::LocalSet::new().run_until(k2lg_n_cases()).await;
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env("K2LG_N_CHILD", "1")
        .env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG")
        .env_remove("GROK_CONFIG_PATH")
        .args(["--exact", K2LG_N_SELECTOR, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stdout.lines().filter(|line| line.starts_with("K2LG_")) {
        println!("{line}");
    }
    let assertion: Vec<_> = stderr
        .lines()
        .filter(|line| line.contains("K2LG boundary"))
        .collect();
    assert!(
        output.status.success(),
        "K2LG boundary native child failed: {}",
        assertion.join("; ")
    );
    assert!(stdout.contains("running 1 test"));
}


const KOXD_U6_SELECTOR: &str = "session::acp_session::native_messages_replay_tests::messages_pending_holds_harness_user_content";
const KOXD_U6_CHILD: &str = "KOXD_U6A_CHILD";
const KOXD_U6_PROMPT: &str = "KOXD-U6-live";

fn u6_png(side: u32) -> String {
    use base64::Engine;
    let image = image::RgbImage::from_fn(side, side, |x, y| {
        let value = x
            .wrapping_mul(0x45d9f3b)
            .wrapping_add(y.wrapping_mul(0x119de1f3));
        image::Rgb([value as u8, (value >> 8) as u8, (value >> 16) as u8])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    )
}

#[derive(Clone, Debug)]
struct U6ClientTool {
    name: &'static str,
    executions: Arc<std::sync::Mutex<Vec<String>>>,
    entered: tokio::sync::mpsc::UnboundedSender<String>,
    release: Arc<tokio::sync::Semaphore>,
    image: Option<Arc<str>>,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for U6ClientTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Bounded offline U6 client tool"
    }
}

impl xai_tool_runtime::Tool for U6ClientTool {
    type Args = Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, "Bounded offline U6 client tool")
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        arguments: Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        self.executions.lock().unwrap().push(self.name.into());
        self.entered.send(self.name.into()).unwrap();
        self.release.acquire().await.unwrap().forget();
        let text = format!("{} actual result: {}", self.name, arguments["value"]);
        if let Some(image) = &self.image {
            let mut output = xai_grok_tools::types::output::MCPOutput::okay_output(
                self.name.into(),
                "u6".into(),
                text,
            );
            output
                .extracted_images
                .push(xai_grok_tools::util::base64_images::ExtractedImage {
                    data: image
                        .strip_prefix("data:image/png;base64,")
                        .unwrap()
                        .to_owned(),
                    mime_type: "image/png".into(),
                });
            Ok(xai_grok_tools::types::output::ToolOutput::MCP(output))
        } else {
            Ok(xai_grok_tools::types::output::ToolOutput::Text(text.into()))
        }
    }
}

#[derive(Clone, Default)]
struct U6PersistenceControl {
    updates: Arc<std::sync::Mutex<Vec<Value>>>,
    cleanup: Arc<tokio::sync::Notify>,
    cleanup_count: Arc<std::sync::atomic::AtomicUsize>,
    bookkeeping_feedback: bool,
    media_resample: bool,
    barriers: Arc<std::sync::atomic::AtomicUsize>,
    hold_barriers: Arc<std::sync::atomic::AtomicBool>,
    held_barriers: Arc<std::sync::Mutex<Vec<PersistenceMsg>>>,
    barrier_held: Arc<tokio::sync::Notify>,
    user_chunks: Arc<std::sync::Mutex<Vec<Value>>>,
    announcement_updates: Arc<std::sync::atomic::AtomicUsize>,
    plan_updates: Arc<std::sync::atomic::AtomicUsize>,
    plan_tools: bool,
}

struct U6Fixture {
    fx: K2lgN,
    persistence_control: U6PersistenceControl,
}

async fn u6_install_running(
    fx: &mut K2lgN,
    handle: tokio::task::AbortHandle,
) -> (TurnEpoch, std::rc::Rc<()>) {
    let epoch = fx.session.actor.turn_report.start_next_turn();
    let identity = {
        let mut state = fx.session.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(KOXD_U6_PROMPT, epoch, handle));
        state
            .pending_inputs
            .push_front(user_item(KOXD_U6_PROMPT, "U6"));
        state.running_task.as_ref().unwrap().identity.clone()
    };
    *fx.session.actor.current_prompt_id.lock().unwrap() = Some(KOXD_U6_PROMPT.into());
    fx.session
        .actor
        .session_turn_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(command_rx) = fx.loop_rx.take() {
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let actor = fx.session.actor.clone();
        fx.session.tasks.push(tokio::task::spawn_local(async move {
            let _keep = (chat_tx, event_tx);
            super::run_session(
                actor,
                command_rx,
                chat_rx,
                event_rx,
                None,
                Arc::new(parking_lot::Mutex::new(
                    xai_grok_workspace::file_system::CodebaseIndexManager::new(),
                )),
                std::env::temp_dir(),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            )
            .await;
        }));
    }
    (epoch, identity)
}

async fn u6_monitor(fx: &K2lgN, task: &str, text: &str) {
    let buffer = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap();
    let before = buffer.len();
    fx.loop_tx
        .send(SessionCommand::InjectNotification {
            prompt_id: format!("u6-monitor-{task}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
                "<monitor-event description=\"u6\" task_id=\"{task}\">\n{text}\n</monitor-event>"
            )))],
            priority: NotificationPriority::Next,
            source: NotificationSource::MonitorEvent {
                task_id: task.into(),
            },
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while buffer.len() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_MONITOR_REAL_INGRESS");
}

fn u6_body_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|part| part["text"].as_str())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn u6_assert_results_only(checkpoint: &K2Checkpoint, initial: &Value, continuation: &Value) {
    let origin = checkpoint.source.producing_bindings.last().unwrap();
    let mut waiting: Vec<_> = checkpoint
        .ledger
        .lifecycle
        .waiting_clients
        .iter()
        .filter(|client| {
            client.source.producing_binding == origin.request && client.delivery == "Waiting"
        })
        .map(|client| client.client_id.as_str())
        .collect();
    waiting.sort();
    let messages = continuation["messages"].as_array().unwrap();
    let tail = messages.last().unwrap();
    let blocks = tail["content"].as_array().unwrap();
    assert!(
        tail["role"] == "user" && !blocks.is_empty(),
        "H6_RESULTS_ONLY_NONEMPTY_USER"
    );
    assert!(
        blocks.iter().all(|block| block["type"] == "tool_result"),
        "H6_RESULTS_ONLY_NO_USER_CONTENT"
    );
    let mut actual: Vec<_> = blocks
        .iter()
        .map(|block| block["tool_use_id"].as_str().unwrap())
        .collect();
    actual.sort();
    assert!(
        actual == waiting && !waiting.is_empty(),
        "H6_RESULTS_ONLY_REAL_WAITING_IDS"
    );
    assert!(
        initial["tools"] == continuation["tools"],
        "H6_ORIGINAL_FULL_TOOLS"
    );
    assert!(
        initial["model"] == continuation["model"],
        "H6_ORIGINAL_BOUND_MODEL"
    );
}

async fn u6_session(
    server: &MockInferenceServer,
    options: K2lgNOptions,
    agent_wide: &Arc<std::sync::atomic::AtomicBool>,
    chat_token: &tokio_util::sync::CancellationToken,
    log: &K2lgNLog,
    control: &U6PersistenceControl,
    image: Option<Arc<str>>,
) -> K2ReplaySession {
    let directory = tempfile::tempdir().unwrap();
    let config = xai_grok_sampler::SamplerConfig {
        base_url: server.url(),
        model: "k2-replay".into(),
        api_key: Some("nonsecret-k2-key".into()),
        api_backend: xai_grok_sampler::ApiBackend::Messages,
        max_completion_tokens: Some(4096),
        max_retries: Some(options.retries),
        ..Default::default()
    };
    let info = crate::session::info::Info {
        id: acp::SessionId::new(format!(
            "k2lg-n-{}",
            directory.path().file_name().unwrap().to_string_lossy()
        )),
        cwd: directory.path().to_string_lossy().into_owned(),
    };
    let persistence = crate::session::persistence::new_with_explicit_dir(
        &info,
        directory.path().to_path_buf(),
        acp::ModelId::new("k2-replay"),
        xai_grok_sampler::SamplingClient::new(config.clone()).unwrap(),
        "k2-replay".into(),
        crate::session::persistence::ExplicitSessionOpen::New {
            identity: None,
            next_trace_turn: None,
        },
    )
    .await
    .unwrap();
    let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let cleanup_control = control.clone();
    let gateway = tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(arguments) = message {
                if matches!(&arguments.request.update, acp::SessionUpdate::Plan(plan)
                    if plan.entries.iter().any(|entry| entry.content == "U6 bookkeeping observer")
                        && plan.entries.iter().all(|entry| entry.status == acp::PlanEntryStatus::Completed))
                {
                    cleanup_control
                        .cleanup_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    cleanup_control.cleanup.notify_one();
                }
                let _ = arguments.response_tx.send(Ok(()));
            }
        }
    });
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let real_persistence = persistence.tx.clone();
    let updates = control.updates.clone();
    let barriers = control.barriers.clone();
    let observation_control = control.clone();
    let persistence_proxy = tokio::task::spawn_local(async move {
        while let Some(message) = persistence_rx.recv().await {
            if matches!(&message, PersistenceMsg::FlushAndAck { .. }) {
                barriers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if observation_control
                    .hold_barriers
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    observation_control
                        .held_barriers
                        .lock()
                        .unwrap()
                        .push(message);
                    observation_control.barrier_held.notify_one();
                    continue;
                }
            }
            if matches!(&message, PersistenceMsg::AnnouncementState(_)) {
                observation_control
                    .announcement_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if matches!(&message, PersistenceMsg::PlanModeState(_)) {
                observation_control
                    .plan_updates
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if let PersistenceMsg::Update(SessionUpdate::Acp(notification)) = &message {
                if matches!(
                    &notification.update,
                    acp::SessionUpdate::UserMessageChunk(_)
                ) {
                    observation_control
                        .user_chunks
                        .lock()
                        .unwrap()
                        .push(serde_json::to_value(notification).unwrap());
                }
                updates
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(notification).unwrap());
            }
            if real_persistence.send(message).is_err() {
                break;
            }
        }
    });
    let (mut actor, _) = create_test_actor_with_chat_persistence(
        0,
        256_000,
        85,
        gateway_tx,
        persistence_tx.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
    )
    .await;
    let sampling = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("K2LG setup chat-state sampling config");
    let (chat_event_tx, _chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        sampling,
        Box::new(K2lgNPersistence {
            inner: crate::session::chat_persistence::ChannelChatPersistence::new(
                persistence_tx.clone(),
            ),
            log: log.clone(),
        }),
        chat_event_tx,
        chat_token.clone(),
    );
    actor.tool_context.is_turn_active = Some(agent_wide.clone());
    actor.tool_context.monitor_event_buffer = Some(MonitorEventBuffer::new());
    actor.max_turns = options.max_turns;
    actor.session_info = info;
    actor.attach_non_interactive.set(true);
    if control.media_resample {
        *actor.agent.borrow_mut() = super::support::test_agent_with_tools(vec![
            xai_grok_tools::registry::types::ToolConfig::for_tool::<
                xai_grok_tools::implementations::grok_build::image_gen::ImageGenTool,
            >(),
        ])
        .await;
    }
    if control.plan_tools {
        *actor.agent.borrow_mut() = super::support::test_agent_with_plan_tools().await;
        *actor.plan_mode.lock() =
            crate::session::plan_mode::PlanModeTracker::new(directory.path().to_path_buf());
    }
    if control.bookkeeping_feedback {
        actor.feedback_manager = Arc::new(crate::session::feedback_manager::FeedbackManager::new(
            "u6-bookkeeping",
            None,
            crate::session::feedback_manager::FeedbackManagerConfig {
                feedback_enabled: true,
                ..Default::default()
            },
        ));
        actor
            .notifications
            .gateway_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
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
    let mut entry = entry_with_search_flag("k2-replay", true);
    entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    entry.api_base_url = Some(server.url());
    entry.api_key = Some("nonsecret-k2-key".into());
    manager.insert_test_entry("k2-replay", entry);
    manager.set_current_model_id(acp::ModelId::new("k2-replay"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.base_url = server.url();
    binding.config.model = "k2-replay".into();
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.supports_search_tool = true;
    binding.config.max_completion_tokens = Some(4096);
    binding.config.max_retries = Some(options.retries);
    binding.credentials.api_key = Some("nonsecret-k2-key".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    let executions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (entered_tx, entered) = tokio::sync::mpsc::unbounded_channel();
    let releases = [
        Arc::new(tokio::sync::Semaphore::new(16)),
        Arc::new(tokio::sync::Semaphore::new(16)),
    ];
    for (index, name) in ["alpha__lookup", "zeta__fetch"].into_iter().enumerate() {
        actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(
            name.into(),
            U6ClientTool {
                name,
                executions: executions.clone(),
                entered: entered_tx.clone(),
                release: releases[index].clone(),
                image: if index == 1 { image.clone() } else { None },
            },
            Some(json!({"type":"object","description":"Exact K2 original schema","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})),
        )
        .unwrap();
    }
    {
        let mut mcp = actor.mcp_state.lock().await;
        mcp.finish_init();
        mcp.complete_init();
    }
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
    let (sampler_tx, mut sampler_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        config,
        xai_grok_sampler::RetryPolicy {
            max_retries: options.retries,
            ..Default::default()
        },
        sampler_tx,
    );
    let actor = Arc::new(actor);
    let drainer = actor.clone();
    let (source_tx, source_acks) = tokio::sync::mpsc::unbounded_channel();
    let sampler = tokio::task::spawn_local(async move {
        while let Some(event) = sampler_rx.recv().await {
            let status = match &event {
                xai_grok_sampler::SamplingEvent::NativeAttemptEvidence {
                    evidence: k2_history::NativeEvidenceEvent::Attempt(attempt),
                    ..
                } => Some(attempt.status),
                _ => None,
            };
            drainer.handle_sampling_event(event).await;
            if let Some(status) = status {
                let _ = source_tx.send(status);
            }
        }
    });
    K2ReplaySession {
        actor,
        persistence,
        directory,
        _models: models,
        tasks: vec![gateway, persistence_proxy, sampler],
        executions,
        entered,
        releases,
        source_acks,
    }
}

async fn u6_fixture_with_bookkeeping(
    image: Option<Arc<str>>,
    options: K2lgNOptions,
    bookkeeping_feedback: bool,
    media_resample: bool,
) -> U6Fixture {
    u6_fixture_controlled(
        image,
        options,
        U6PersistenceControl {
            bookkeeping_feedback,
            media_resample,
            ..Default::default()
        },
    )
    .await
}

async fn u6_fixture_controlled(
    image: Option<Arc<str>>,
    options: K2lgNOptions,
    persistence_control: U6PersistenceControl,
) -> U6Fixture {
    let server = MockInferenceServer::start().await.unwrap();
    let agent_wide = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chat_token = tokio_util::sync::CancellationToken::new();
    let log = K2lgNLog::default();
    let mut session = u6_session(
        &server,
        options,
        &agent_wide,
        &chat_token,
        &log,
        &persistence_control,
        image,
    )
    .await;
    let (loop_tx, loop_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::rc::Rc::new(tokio::sync::Notify::new());
    let (sender, receiver) = k0_coordinator::SubagentCoordinator::<K2lgNRunner>::channel();
    session.tasks.push(tokio::task::spawn_local(
        k0_coordinator::SubagentCoordinator::from_channel(
            receiver,
            K2lgNRunner {
                commands: loop_tx.clone(),
                started: started.clone(),
            },
            Default::default(),
        )
        .run(),
    ));
    let root = k2lg_n_root(&server, sender).await;
    k2lg_n_register_worker(&root, &started).await;
    U6Fixture {
        fx: K2lgN {
            session,
            server,
            root,
            loop_tx,
            loop_rx: Some(loop_rx),
            log,
            chat_token,
            agent_wide,
            calls: 0,
        },
        persistence_control,
    }
}

async fn u6_handoff_with_salvage(
    fx: &mut K2lgN,
    budget: Option<u32>,
    schema: Option<Value>,
    wait_timeout: Option<u64>,
    stepper: Option<U6TurnStepper>,
) -> K2lgNTurn {
    let stepper = Some(stepper.unwrap_or_default());
    U6_ARM_STEPPERS.with(|steppers| {
        steppers
            .borrow_mut()
            .push(stepper.as_ref().unwrap().clone())
    });
    k2_seed(&mut fx.session, &fx.server).await;
    fx.session.releases[1]
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap()
        .forget();
    let frames = wait_timeout.map_or_else(k2_u5_frames, |timeout| {
        u6_wait_frames(k2_u5_frames(), "client_u6_wait", timeout)
    });
    fx.server
        .enqueue_response("/v1/messages", k2_script(frames));
    fx.session.actor.chat_state_handle.begin_turn_capture();
    let _ = fx.session.actor.chat_state_handle.get_conversation().await;
    let turn_start_ops = fx.log.lock().unwrap().len();
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("U6 real mixed handoff"))
        .await;
    let submits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = submits.clone();
    let actor = fx.session.actor.clone();
    let domain = fx.session.domain(&fx.server);
    let observations = U6_ARM_OBSERVATIONS.with(Clone::clone);
    let mut running = tokio::task::spawn_local(async move {
        let mut prepare =
            move |request: &mut ConversationRequest,
                  binding: &xai_chat_state::RequestTransportBinding| {
                assert!(record.borrow().len() < 16, "H6_REQUEST_BUDGET");
                record.borrow_mut().push(json!({
            "items": serde_json::to_value(&request.items).unwrap(),
            "tools": serde_json::to_value(&request.tools).unwrap(),
            "model": request.model,
            "bound_model": binding.config.model,
            "body_digest": request.encoded.as_ref().map(|body| native_content_digest(body.as_bytes())),
        }));
                Ok(())
            };
        let mut salvage = super::length_salvage::LengthSalvage::new(budget);
        let mut sampling = Default::default();
        let future = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
            domain,
            super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
                k2_history::NativeHistoryRequirement::Selected,
                actor.process_conversation_turn_for_native_history_test(
                    KOXD_U6_PROMPT,
                    None,
                    None,
                    schema,
                    &mut salvage,
                    &mut sampling,
                    &mut prepare,
                ),
            ),
        );
        U6_ARM_OBSERVATIONS.scope(observations, U6SteppedTurn {
            future: Box::pin(future),
            stepper,
        }).await
    });
    let (epoch, identity) = u6_install_running(fx, running.abort_handle()).await;
    let captured = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"real handoff setup rejected before native capture",
                    "error_code":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("code")).and_then(Value::as_str),
                    "local_message":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("message")).and_then(Value::as_str)
                        .map(|message| message.chars().take(256).collect::<String>()),
                    "error_message_digest":outcome.as_ref().err().map(|error| native_content_digest(error.message.as_bytes())),
                    "http_count":fx.server.messages_request_count()}));
                false
            },
            status = fx.session.source_acks.recv() => status == Some(k2_history::NativeResponseStatus::Accepted),
        }
    }).await.expect("H6_HANDOFF_CAPTURE_OUTCOME_RACE");
    assert!(captured, "H6_DECODED_NATIVE_HANDOFF");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered,
        &mut running,
        json!("Accepted"),
        "real handoff client dispatch",
        "H6_HANDOFF_DISPATCH_OUTCOME_RACE",
        "H6_HANDOFF_TURN_ENDED_BEFORE_DISPATCH",
    )
    .await;
    entered.sort();
    assert!(
        entered == vec!["alpha__lookup", "zeta__fetch"],
        "H6_REAL_CLIENT_EXECUTION"
    );
    let conversation = &fx.session.actor.chat_state_handle;
    u6_await_turn_progress(
        &mut running,
        "real handoff first actual client result",
        "H6_FIRST_ACTUAL_RESULT",
        "H6_FIRST_ACTUAL_RESULT_TURN_ENDED",
        || async move {
            conversation.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == "client_k2_c1")
            })
        },
    )
    .await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_HANDOFF_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_REAL_LIVE_PENDING");
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_ZERO_EARLY_CONTINUATION"
    );
    K2lgNTurn {
        running,
        epoch,
        identity,
        submits,
        turn_start_ops,
    }
}

async fn u6_l1() -> Value {
    let mut fixture = u6_fixture(Some(Arc::<str>::from(u6_png(32))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _turn_guard = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L1_RELOAD").await;
    k0_admit_steer(&fx.session.actor).await;
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L1 results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l1_after")),
    );
    fx.session.releases[1].add_permits(1);
    let (continued, finished_outcome) = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => (false, Some(result.expect("H6_L1_TURN_TASK"))),
            _ = continuation.wait_blocked() => (true, None),
        }
    })
    .await
    .expect("H6_L1_BOUNDED_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let held = !history.iter().any(|item| {
        item.text_content().contains("parent steer k0")
            || item
                .text_content()
                .contains("Image extracted from tool result")
    });
    u6_observe(
        json!({"stage":"L1 hold before closure","continuation_observed":continued,"input_in_history":!held,
    "turn_finished":finished_outcome.is_some(),"turn_refused":finished_outcome.as_ref().is_some_and(Result::is_err),
    "error_type":finished_outcome.as_ref().and_then(|result| result.as_ref().err()).and_then(|error| error.data.as_ref()).and_then(|data| data.get("error_type")).and_then(Value::as_str),
    "http_count":fx.server.messages_request_count()}),
    );
    if !continued {
        continuation.release();
    }
    assert!(held, "H6_L1_HOLD_ASSERTION");
    assert!(continued, "H6_L1_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_monitor(fx, "u6-a", "U6 first monitor").await;
    u6_monitor(fx, "u6-b", "U6 second monitor").await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains("U6 first monitor")),
        "H6_L1_MONITOR_HOLD"
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L1_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L1_CLOSED_EXCHANGE_PROGRESS")
        .unwrap();
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for needle in [
        "parent steer k0",
        "Image extracted from tool result",
        "U6 first monitor",
        "U6 second monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(needle))
                .count(),
            1,
            "H6_L1_ONCE_ONLY_RELEASE"
        );
    }
    assert!(outcome.is_ok(), "H6_L1_CLOSED_EXCHANGE_OUTCOME");
    continuation.assert_satisfied();
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L1_FIRST_RELEASE_WITHOUT_PROMPT");
    let tail = bodies[3]["messages"].as_array().unwrap().last().unwrap();
    let blocks = tail["content"].as_array().unwrap();
    let text = blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let steer = text.find("parent steer k0").expect("H6_L1_PARENT_RELEASE");
    let followup = text
        .find("Image extracted from tool result")
        .expect("H6_L1_REAL_FOLLOWUP_RELEASE");
    let monitor = text
        .find("U6 first monitor")
        .expect("H6_L1_MONITOR_RELEASE");
    assert!(
        steer < followup && followup < monitor,
        "H6_L1_GLOBAL_OCCURRENCE_ORDER"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block["type"] == "image")
            .count(),
        1,
        "H6_L1_FOLLOWUP_IMAGE"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|block| block["text"].as_str().is_some_and(|text| text
                .contains("U6 first monitor")
                && text.contains("U6 second monitor")))
            .count(),
        1,
        "H6_L1_MAXIMAL_MONITOR_RUN"
    );
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        2,
        "H6_L1_ONCE_ONLY_CLIENTS"
    );
    let (after, _) = u6_quiescent_reload(fx, "H6_R20_L1_AFTER_RELOAD").await;
    assert!(
        k2_source_blocks(&after, "msg_k2_pending")
            == k2_source_blocks(&checkpoint, "msg_k2_pending"),
        "H6_L1_NATIVE_SOURCE_UNCHANGED"
    );
    json!({"continuation_http":1,"release_http":1,"client_executions":2,"first_closed_safe_point":"turn.rs:3913","continuation_body_digest":native_content_digest(&serde_json::to_vec(&bodies[2]).unwrap()),"release_body_digest":native_content_digest(&serde_json::to_vec(&bodies[3]).unwrap()),"monitor_run":2})
}

tokio::task_local! {
    static U6_ARM_OBSERVATIONS: std::rc::Rc<std::cell::RefCell<Vec<Value>>>;
}

fn u6_observe(value: Value) {
    U6_ARM_OBSERVATIONS.with(|observations| observations.borrow_mut().push(value));
}

fn u6_assertion(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("");
    text.split_whitespace()
        .find_map(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            word.starts_with("H6_").then(|| word.to_owned())
        })
        .unwrap_or_else(|| "H6_UNEXPECTED_PANIC".into())
}

async fn u6_arm<F>(records: &mut Vec<Value>, label: &str, future: F)
where
    F: std::future::Future<Output = Value>,
{
    use futures_util::FutureExt;
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let panics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_panics = panics.clone();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        captured_panics.lock().unwrap().push(json!({
            "file":info.location().map(|location| location.file()),
            "line":info.location().map(|location| location.line()),
            "message_digest":native_content_digest(text.as_bytes())}));
    }));
    let steppers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let result = U6_ARM_OBSERVATIONS
        .scope(
            observations.clone(),
            U6_ARM_STEPPERS.scope(
                steppers,
                std::panic::AssertUnwindSafe(future).catch_unwind(),
            ),
        )
        .await;
    std::panic::set_hook(previous_hook);
    let (passed, assertion, result) = match result {
        Ok(value) => (true, None, value),
        Err(error) => (false, Some(u6_assertion(error.as_ref())), Value::Null),
    };
    records.push(json!({"label":label,"passed":passed,"assertion":assertion,"observations":observations.borrow().clone(),"panics":panics.lock().unwrap().clone(),"result":result}));
}

fn u6_write_proof(records: &[Value]) {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("k3u6a-h6-proof-")
        .suffix(".json")
        .tempfile_in(std::env::temp_dir())
        .unwrap();
    file.write_all(
        &serde_json::to_vec(&json!({"selector":KOXD_U6_SELECTOR,"arms":records})).unwrap(),
    )
    .unwrap();
    file.flush().unwrap();
    file.keep().unwrap();
}

async fn u6_isolated() {
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "DEVELOPER_DIR",
        "SDKROOT",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "__CF_USER_TEXT_ENCODING",
        "CARGO_TARGET_DIR",
        "TMPDIR",
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "RUST_MIN_STACK",
        "GROK_SHELL_BUNDLE_RG_PATH",
        "GROK_TOOLS_BUNDLE_RG_PATH",
        "CARGO_NET_OFFLINE",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env(KOXD_U6_CHILD, "1")
        .env("GROK_HOME", home.path())
        .args(["--exact", KOXD_U6_SELECTOR, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout
        .lines()
        .filter_map(|line| line.strip_prefix("KOXD_U6_ARM "))
    {
        let value: Value = serde_json::from_str(line).expect("H6_CHILD_ARM_RECORD");
        println!(
            "KOXD_U6_ARM {}",
            json!({"label":value["label"],"passed":value["passed"],"assertion":value["assertion"]})
        );
    }
    assert!(
        output.status.success(),
        "H6_CHILD_FAILED status={:?} stdout_digest={} stderr_digest={}",
        output.status.code(),
        native_content_digest(&output.stdout),
        native_content_digest(&output.stderr)
    );
    assert!(
        stdout.contains("running 1 test"),
        "H6_CHILD_EXACT_SELECTION"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn messages_pending_holds_harness_user_content() {
    if std::env::var_os(KOXD_U6_CHILD).is_none() {
        u6_isolated().await;
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut records = Vec::new();
            u6_arm(&mut records, "L1", u6_l1()).await;
            u6_arm(&mut records, "L1-N", u6_l1_native()).await;
            u6_arm(&mut records, "L1-unavailable-view", u6_unavailable_view_hold()).await;
            u6_arm(&mut records, "L2-pre-completion", u6_l2("pre-completion")).await;
            u6_arm(&mut records, "L2-tool-batch", u6_l2("tool-batch")).await;
            u6_arm(&mut records, "L2-empty-continuation", u6_l2("empty-continuation")).await;
            records.push(json!({"label":"L2-loop-top","passed":null,"credited":false,
                "status":"NOT_RUN","owed_unit":"koxd-U6.5","authority":"H-K3U6a-R8 R8-3",
                "reason":"TodoGate discipline predicate is always false; the media resample diagnostic ends without a post-release HTTP"}));
            u6_arm(&mut records, "L2-new-late-bookkeeping", u6_l2_late_bookkeeping()).await;
            u6_arm(&mut records, "L3-another-pending", u6_l3(false)).await;
            u6_arm(&mut records, "L3-rearm", u6_l3(true)).await;
            u6_arm(&mut records, "L4", u6_l4()).await;
            u6_arm(&mut records, "L12", u6_l12()).await;
            u6_arm(&mut records, "TW-1-resident-arrival", u6_tw1(true)).await;
            u6_arm(&mut records, "TW-1-already-custody", u6_tw1(false)).await;
            u6_arm(&mut records, "L10-records", u6_l10("records")).await;
            u6_arm(&mut records, "L10-aggregate", u6_l10("aggregate")).await;
            u6_arm(&mut records, "L10-entry", u6_l10("entry")).await;
            u6_arm(&mut records, "L10-metadata", u6_l10("metadata")).await;
            u6_arm(&mut records, "L10-parent-stamps", u6_l10("parent-stamps")).await;
            u6_arm(&mut records, "L10-overlength-id", u6_l10_overlength_id()).await;
            u6_arm(&mut records, "L10-terminal-reserve", u6_l10_reserve()).await;
            u6_arm(&mut records, "L10-deferred-image", u6_l10_deferred_image()).await;
            u6_arm(&mut records, "L10b", u6_l10b()).await;
            u6_arm(&mut records, "L14", u6_l14()).await;
            u6_arm(&mut records, "L14c-base", u6_l14c(false)).await;
            u6_arm(&mut records, "L14c-straddling", u6_l14c(true)).await;
            u6_arm(&mut records, "L14b-closed", u6_l14b_closed("matching")).await;
            u6_arm(&mut records, "L14b-latch-absent", u6_l14b_closed("absent")).await;
            u6_arm(&mut records, "L14b-latch-inert", u6_l14b_closed("inert")).await;
            u6_arm(&mut records, "L14b-persistence-recovery", u6_l14b_persistence_recovery()).await;
            u6_arm(&mut records, "L14b-command-Cancel-new-owner", u6_l14b_command_cancel()).await;
            u6_arm(&mut records, "L14b-constructed-partial-loop", u6_l14b_constructed_partial_loop()).await;
            u6_write_proof(&records);
            for record in &records {
                println!("KOXD_U6_ARM {}", record);
            }
            assert!(
                records.iter().all(|record| record["passed"] == true ||
                    (record["label"] == "L2-loop-top" && record["status"] == "NOT_RUN"
                        && record["authority"] == "H-K3U6a-R8 R8-3")),
                "H6_ALLOCATED_ARM_FAILURE"
            );
        })
        .await;
}

async fn u6_interject(fx: &K2lgN, text: &str, images: Vec<acp::ImageContent>) {
    fx.loop_tx
        .send(SessionCommand::Interject {
            text: text.to_owned(),
            id: None,
            images,
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while !fx
            .session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|item| item.text == text)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_REAL_HUMAN_INGRESS");
}

async fn u6_l1_native() -> Value {
    const NATIVE: &str = "U6 L1-N native prefix";
    const HUMAN: &str = "U6 L1-N ordinary human";
    let mut fixture = u6_fixture(Some(Arc::<str>::from(u6_png(32))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L1N_RELOAD").await;
    k0_admit_steer(&fx.session.actor).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    k2lg_n_send(fx, NATIVE).await;
    k2lg_n_send(fx, NATIVE).await;
    let resident = k2lg_n_values(&k2lg_n_resident(&fx.session));
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L1-N continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l1n_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L1N_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(HUMAN)
                || item.text_content().contains(NATIVE)
                || item.text_content().contains("parent steer k0")
                || item
                    .text_content()
                    .contains("Image extracted from tool result")),
        "H6_L1N_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L1N_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    assert!(
        k2lg_n_values(&k2lg_n_resident(&fx.session)) == resident,
        "H6_L1N_RESIDENT_PREFIX_UNCHANGED"
    );
    u6_monitor(fx, "u6-l1n", "U6 L1-N monitor").await;
    k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L1N_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L1N_FIRST_RELEASE_PROGRESS")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L1N_OUTCOME");
    continuation.assert_satisfied();
    let after = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        k2lg_n_natives(&after) == vec![k2lg_n_native_text(NATIVE), k2lg_n_native_text(NATIVE)],
        "H6_L1N_NATIVE_PREFIX_ONCE"
    );
    let first = after
        .iter()
        .position(k2lg_n_is_native)
        .expect("H6_L1N_NATIVE_FIRST");
    let parent = after
        .iter()
        .position(|item| item.text_content().contains("parent steer k0"))
        .expect("H6_L1N_PARENT_RELEASE");
    assert!(
        k2lg_n_is_native(&after[first + 1]) && first + 2 == parent,
        "H6_L1N_NATIVE_BEFORE_MERGED_BATCH"
    );
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L1-N monitor",
    ] {
        assert_eq!(
            after
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L1N_ORDINARY_ONCE"
        );
    }
    assert!(
        k2lg_n_resident(&fx.session).is_empty(),
        "H6_L1N_NO_DOUBLE_HOLD"
    );
    assert_eq!(
        k2lg_n_native_persists(&k2lg_n_ops(fx)),
        2,
        "H6_L1N_NATIVE_PERSIST_ONCE"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 4, "H6_L1N_SAME_TURN_RELEASE");
    assert_eq!(
        k2lg_n_text_count(&bodies[3], NATIVE),
        2,
        "H6_L1N_TRANSMITTED_PREFIX_ONCE"
    );
    json!({"producer":"send_message->coordinator->AgentMessage->message_item->resident",
        "native_occurrences":2,"order":"native,native,merged ordinary batch",
        "first_closed_safe_point":"turn.rs:3913","native_persists":2,"waiting_client_content":false})
}

async fn u6_handoff(fx: &mut K2lgN) -> K2lgNTurn {
    u6_handoff_with_salvage(fx, None, None, None, None).await
}

async fn u6_l2(site: &str) -> Value {
    const HUMAN: &str = "U6 L2 held human";
    let mut fixture =
        u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), false, site == "loop-top").await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff_with_salvage(
        fx,
        (site == "empty-continuation").then_some(1),
        None,
        None,
        None,
    )
    .await;
    let _stop = K0Abort(turn.running.abort_handle());
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L2_RELOAD").await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut blocks = vec![
        k2_result("srvtoolu_k2_pending", "alpha__lookup"),
        (
            json!({"type":"text","text":"U6 L2 closes the real server exchange"}),
            Vec::new(),
        ),
    ];
    if site == "tool-batch" {
        blocks.push((json!({"type":"tool_use","id":"client_u6_l2","name":"alpha__lookup","input":{"value":"L2"}}), Vec::new()));
    }
    if site == "loop-top" {
        for index in 0..(2 * xai_grok_tools::media_gen_limits::DEFAULT_MAX_PARALLEL_IMAGE_GEN) {
            blocks.push((
                json!({"type":"tool_use","id":format!("client_u6_l2_media_{index}"),
                "name":"image_gen","input":{"prompt":"U6 loop-top resample; never executed"}}),
                Vec::new(),
            ));
        }
    }
    let stop = if site == "empty-continuation" {
        "max_tokens"
    } else {
        "end_turn"
    };
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L2 pending continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames("msg_u6_l2_closed", blocks, stop)),
    );
    let mut empty = (site == "empty-continuation").then(|| {
        fx.server.expect_response_blocked(
            "U6 L2 real empty continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames("msg_u6_l2_empty", Vec::new(), "max_tokens")),
        )
    });
    let mut released = fx.server.expect_response_blocked(
        "U6 L2 first release request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l2_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L2_CONTINUATION_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L2_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L2_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L2_CLOSED_CHECKPOINT",
    )
    .await;
    if let Some(empty) = empty.as_mut() {
        let observed = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = empty.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_L2_EMPTY_CONTINUATION_RACE");
        let read = fx
            .session
            .actor
            .chat_state_handle
            .read_native_history()
            .await;
        u6_observe(json!({"stage":"L2 empty continuation before its terminal",
            "empty_http_observed":observed,"read_ok":read.is_ok(),
            "groups":read.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).map(|checkpoint|
                checkpoint.ledger.groups.iter().map(|group| group.state.clone()).collect::<Vec<_>>())}));
        assert!(observed, "H6_L2_EMPTY_CONTINUATION_REACHED");
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &empty,
            false,
            "H6_R15_L2_EMPTY_CHECKPOINT",
        )
        .await;
    }
    let release_observed = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"L2 first eligible caller", "site":site,
                    "turn_finished":true,"turn_ok":outcome.is_ok(),
                    "error_type":outcome.as_ref().err().and_then(|error| error.data.as_ref())
                        .and_then(|data| data.get("error_type")).and_then(Value::as_str)}));
                false
            },
            _ = released.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L2_FIRST_RELEASE_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let count = history
        .iter()
        .filter(|item| item.text_content().contains(HUMAN))
        .count();
    if site == "loop-top" {
        let read = fx
            .session
            .actor
            .chat_state_handle
            .read_native_history()
            .await;
        u6_observe(
            json!({"stage":"R8 media resample route", "release_http_observed":release_observed,
            "live_pending_after_resample":read.as_ref().ok().and_then(|read| read.checkpoint.as_ref()).is_some_and(|checkpoint| k2lg_n_live(checkpoint)),
            "read_ok":read.is_ok(),"declarations_unchanged":bodies[1]["tools"] == bodies[2]["tools"],
            "first_candidate_caller":"turn.rs:2844", "media_calls_executed":0}),
        );
    }
    u6_observe(
        json!({"stage":"L2 first release", "site":site,"release_observed":release_observed,
        "human_in_history":count,"http_count":fx.server.messages_request_count()}),
    );
    assert!(
        release_observed && count == 1,
        "H6_L2_FIRST_CLOSED_SAFE_POINT_RELEASE"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert!(
        u6_body_text(bodies.last().unwrap()).contains(HUMAN),
        "H6_L2_ACTUAL_RELEASE_BODY"
    );
    assert_eq!(
        bodies.len(),
        if site == "empty-continuation" { 5 } else { 4 },
        "H6_L2_NO_EXTRA_PROMPT"
    );
    released.release();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L2_FINISH")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L2_OUTCOME");
    continuation.assert_satisfied();
    released.assert_satisfied();
    if let Some(empty) = empty {
        empty.assert_satisfied();
    }
    json!({"site":site,"first_release":1,"human_occurrences":count,"http":bodies.len(),
        "new_prompt":false,"caller":match site { "tool-batch" => "tool_calls.rs:539",
            "empty-continuation" => "turn.rs:3139", "loop-top" => "turn.rs:2844", _ => "turn.rs:3913" }})
}

async fn u6_l4() -> Value {
    const HUMAN: &str = "U6 L4 retained across round boundary";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut salvage = super::length_salvage::LengthSalvage::new(Some(1));
    let _ = salvage.on_length_stop();
    salvage.response_arrived();
    salvage.round_boundary();
    assert!(
        !salvage.awaiting_continuation(),
        "H6_L4_REAL_ROUND_BOUNDARY"
    );
    let delivered = k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_L4_RELOAD").await;
    assert!(k2lg_n_live(&checkpoint), "H6_L4_NATIVE_EXCHANGE_STILL_LIVE");
    assert!(
        !delivered
            && !history
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
        "H6_L4_NO_BOOKKEEPING_CLOSURE"
    );
    assert_eq!(fx.server.messages_request_count(), 2, "H6_L4_NO_EARLY_HTTP");
    json!({"round_boundary":true,"native_pending":true,"delivery":false,
        "bookkeeping_observation":"round-boundary policy exercised, not server closure"})
}

fn u6_next_pending(close_original: bool) -> Vec<Value> {
    let mut blocks = Vec::new();
    if close_original {
        blocks.push(k2_result("srvtoolu_k2_pending", "alpha__lookup"));
    }
    blocks.extend([k2_call("srvtoolu_u6_next", None),
        (json!({"type":"tool_use","id":"client_u6_next_a","name":"alpha__lookup","input":{"value":"next A"}}), Vec::new()),
        (json!({"type":"tool_use","id":"client_u6_next_b","name":"zeta__fetch","input":{"value":"next B"}}), Vec::new())]);
    k2_frames("msg_u6_next_pending", blocks, "tool_use")
}

async fn u6_l3(rearm: bool) -> Value {
    const FIRST: &str = "U6 L3 first held human";
    const SECOND: &str = "U6 L3 second held human";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_interject(fx, FIRST, Vec::new()).await;
    let frames = if rearm {
        k2lg_n_closure_frames()
    } else {
        u6_next_pending(true)
    };
    let mut first_continuation = fx.server.expect_response_blocked(
        "U6 L3 first continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(frames),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = first_continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L3_FIRST_OUTCOME_RACE");
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains(FIRST)),
        "H6_L3_INITIAL_HOLD"
    );
    assert!(continued, "H6_L3_FIRST_CONTINUATION");
    let mut first_release = rearm.then(|| {
        fx.server.expect_response_blocked(
            "U6 L3 first release before re-arm",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(u6_next_pending(false)),
        )
    });
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &first_continuation,
        !rearm,
        "H6_R15_L3_FIRST_RELEASE_CHECKPOINT",
    )
    .await;
    if let Some(released) = first_release.as_mut() {
        let (released_http, finished) = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                _ = released.wait_blocked() => (true, None),
                result = &mut turn.running => (false, Some(result.unwrap())),
            }
        })
        .await
        .expect("H6_L3_FIRST_RELEASE_OUTCOME_RACE");
        let custody = u6_mixed_keys(fx).await;
        let history = fx.session.actor.chat_state_handle.get_conversation().await;
        let history_has_first = history
            .iter()
            .any(|item| item.text_content().contains(FIRST));
        u6_observe(
            json!({"stage":"L3 first release race", "release_http":released_http,
            "turn_finished":finished.is_some(), "turn_refused":finished.as_ref().is_some_and(Result::is_err),
            "custody_present":!custody.is_empty(), "history_has_first":history_has_first}),
        );
        assert!(
            released_http && finished.is_none() && history_has_first,
            "H6_L3_FIRST_RELEASE"
        );
        assert!(custody.is_empty(), "H6_L3_FIRST_RELEASE_CUSTODY_COMMITTED");
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(FIRST))
                .count(),
            1,
            "H6_L3_RELEASE_COMPLETED_BEFORE_REARM"
        );
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &released,
            true,
            "H6_R15_L3_NEXT_HANDOFF_CHECKPOINT",
        )
        .await;
    }
    let next_capture = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            status = fx.session.source_acks.recv() => Ok(status),
            result = &mut turn.running => Err(result),
        }
    })
    .await;
    let next_status = match next_capture {
        Ok(Ok(status)) => status,
        Ok(Err(result)) => u6_turn_ended(
            result,
            "L3 next handoff native capture",
            "H6_L3_NEXT_TURN_ENDED_BEFORE_CAPTURE",
            json!({"capture":null}),
        ),
        Err(_) => u6_bound_expired(
            &turn.running,
            "L3 next handoff native capture",
            "H6_L3_NEXT_CAPTURE_OUTCOME_RACE",
            json!({"capture":null}),
        ),
    };
    assert!(next_status.is_some(), "H6_L3_REAL_NEXT_NATIVE_CAPTURE");
    let mut entered = u6_client_dispatch(
        &mut fx.session.entered,
        &mut turn.running,
        json!(next_status),
        "L3 next handoff client dispatch",
        "H6_L3_NEXT_DISPATCH_OUTCOME_RACE",
        "H6_L3_NEXT_TURN_ENDED_BEFORE_DISPATCH",
    )
    .await;
    entered.sort();
    assert_eq!(
        entered,
        vec!["alpha__lookup", "zeta__fetch"],
        "H6_L3_NEXT_REAL_CLIENTS"
    );
    let (next, _) = u6_quiescent_reload(fx, "H6_R20_L3_NEXT_RELOAD").await;
    assert!(k2lg_n_live(&next), "H6_L3_ANOTHER_PENDING_CALL");
    if !rearm {
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(FIRST)),
            "H6_L3_RETAIN_WHILE_ANOTHER_PENDING"
        );
    }
    u6_interject(fx, SECOND, Vec::new()).await;
    k0_admit_steer(&fx.session.actor).await;
    let mut next_continuation = fx.server.expect_response_blocked(
        "U6 L3 next results-only continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2_frames(
            "msg_u6_next_closed",
            vec![
                k2_result("srvtoolu_u6_next", "alpha__lookup"),
                (
                    json!({"type":"text","text":"U6 L3 closes remaining call"}),
                    Vec::new(),
                ),
            ],
            "end_turn",
        )),
    );
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l3_after")),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => {
                let outcome = result.unwrap();
                u6_observe(json!({"stage":"L3 next handoff","turn_finished":true,
                    "turn_refused":outcome.is_err(),"http_count":fx.server.messages_request_count()}));
                false
            },
            _ = next_continuation.wait_blocked() => true,
        }
    }).await.expect("H6_L3_NEXT_OUTCOME_RACE");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains(SECOND)
                || item.text_content().contains("parent steer k0")),
        "H6_L3_REARM_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L3_NEXT_RESULTS_ONLY_CONTINUATION");
    let bodies = k2lg_n_bodies(&fx.server);
    let initial = if rearm { 3 } else { 2 };
    u6_assert_results_only(&next, &bodies[initial], &bodies[initial + 1]);
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &next_continuation,
        false,
        "H6_R15_L3_FINAL_CLOSED_CHECKPOINT",
    )
    .await;
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L3_FINAL_RELEASE")
        .unwrap();
    assert!(outcome.is_ok(), "H6_L3_OUTCOME");
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [FIRST, SECOND, "parent steer k0"] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L3_ONCE_ONLY_RELEASE"
        );
    }
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        4,
        "H6_L3_CLIENTS_ONCE"
    );
    first_continuation.assert_satisfied();
    next_continuation.assert_satisfied();
    if let Some(released) = first_release {
        released.assert_satisfied();
    }
    json!({"rearm_after_completed_release":rearm,"another_pending_retained":!rearm,
        "client_executions":4,"new_prompt":false,"second_hold":true})
}

async fn u6_l12() -> Value {
    const HUMAN: &str = "U6 L12 once-only human";
    let mut fixture = u6_fixture(
        Some(Arc::<str>::from(u6_png(32))),
        K2lgNOptions {
            retries: 2,
            max_turns: None,
        },
    )
    .await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    k0_admit_steer(&fx.session.actor).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L12 continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    let continued = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = continuation.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_OUTCOME_RACE");
    assert!(
        !fx.session
            .actor
            .chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L12_HOLD_ASSERTION"
    );
    assert!(continued, "H6_L12_CONTINUATION");
    u6_monitor(fx, "u6-l12", "U6 L12 monitor").await;
    k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let mut throttled = ScriptedResponse::json(
        429,
        json!({"type":"error","error":{"type":"rate_limit_error","message":"bounded U6 retry"}}),
    );
    throttled.headers.push(("retry-after".into(), "1".into()));
    let mut throttled = fx.server.expect_response_blocked(
        "U6 L12 first post-release transport attempt",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        throttled,
    );
    let mut retry = fx.server.expect_response_blocked(
        "U6 L12 actual post-release transport retry",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l12_after")),
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L12_CLOSED_CHECKPOINT",
    )
    .await;
    let first_observed = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = throttled.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_FIRST_RELEASE_OUTCOME_RACE");
    let before_retry = fx.session.actor.chat_state_handle.get_conversation().await;
    u6_observe(
        json!({"stage":"L12 first post-release HTTP cardinality", "http_observed":first_observed,
        "human":before_retry.iter().filter(|item| item.text_content().contains(HUMAN)).count(),
        "parent":before_retry.iter().filter(|item| item.text_content().contains("parent steer k0")).count(),
        "followup":before_retry.iter().filter(|item| item.text_content().contains("Image extracted from tool result")).count(),
        "monitor":before_retry.iter().filter(|item| item.text_content().contains("U6 L12 monitor")).count()}),
    );
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L12 monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L12_LOGICAL_DELIVERY_ONCE"
        );
    }
    assert!(first_observed, "H6_L12_FIRST_POST_RELEASE_HTTP");
    throttled.release();
    let retry_observed = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::select! {
            result = &mut turn.running => { let _ = result.unwrap(); false },
            _ = retry.wait_blocked() => true,
        }
    })
    .await
    .expect("H6_L12_ACTUAL_RETRY_OUTCOME_RACE");
    assert!(retry_observed, "H6_L12_ACTUAL_RETRY_HTTP");
    retry.release();
    let outcome = tokio::time::timeout(Duration::from_secs(90), &mut turn.running)
        .await
        .expect("H6_L12_RETRY_FINISH")
        .unwrap();
    u6_observe(json!({"stage":"L12 transport outcome","ok":outcome.is_ok(),
        "error_type":outcome.as_ref().err().and_then(|error| error.data.as_ref())
            .and_then(|data| data.get("error_type")).and_then(Value::as_str),
        "http_count":fx.server.messages_request_count()}));
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    for text in [
        HUMAN,
        "parent steer k0",
        "Image extracted from tool result",
        "U6 L12 monitor",
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L12_LOGICAL_DELIVERY_ONCE"
        );
    }
    assert!(outcome.is_ok(), "H6_L12_OUTCOME");
    let requests: Vec<_> = fx
        .server
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/messages")
        .collect();
    assert_eq!(requests.len(), 5, "H6_L12_TWO_POST_RELEASE_ATTEMPTS");
    assert!(
        requests[3].body == requests[4].body,
        "H6_L12_ACTUAL_RETRY_BODY_EQUAL"
    );
    let updates = fixture.persistence_control.updates.lock().unwrap();
    assert_eq!(
        updates
            .iter()
            .filter(|item| item.to_string().contains(HUMAN))
            .count(),
        1,
        "H6_L12_HUMAN_PERSIST_ONCE"
    );
    assert_eq!(
        fx.session.executions.lock().unwrap().len(),
        2,
        "H6_L12_CLIENT_EXECUTION_ONCE"
    );
    continuation.assert_satisfied();
    throttled.assert_satisfied();
    retry.assert_satisfied();
    json!({"post_release_transport_attempts":2,"equal_actual_submitted_bodies":true,
        "raw_request_bytes":"not recorded by this server", "logical_occurrences_per_source":1,
        "human_persistence":1,"client_executions":2})
}

async fn u6_fixture(image: Option<Arc<str>>, options: K2lgNOptions) -> U6Fixture {
    u6_fixture_with_bookkeeping(image, options, false, false).await
}

async fn u6_l2_late_bookkeeping() -> Value {
    const HUMAN: &str = "U6 L2 new late-bookkeeping admission";
    use crate::tools::todo::{TodoItem, TodoPriority, TodoState, TodoStatus};
    use xai_grok_tools::types::resources::State as ResourceState;
    let mut fixture = u6_fixture_with_bookkeeping(None, K2lgNOptions::default(), true, false).await;
    let fx = &mut fixture.fx;
    k2_seed(&mut fx.session, &fx.server).await;
    let bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    let resources = bridge.shared_resources().await;
    let mut todos = TodoState::default();
    todos.push(
        "u6-bookkeeping".into(),
        TodoItem {
            content: "U6 bookkeeping observer".into(),
            priority: TodoPriority::Medium,
            status: TodoStatus::InProgress,
            meta: None,
        },
    );
    resources.lock().await.insert(ResourceState(todos));
    let dummy = tokio::task::spawn_local(std::future::pending::<()>());
    let _stop = K0Abort(dummy.abort_handle());
    u6_install_running(fx, dummy.abort_handle()).await;
    fx.session
        .actor
        .chat_state_handle
        .push_user_message_and_ack(ConversationItem::user("U6 real late-bookkeeping turn"))
        .await;
    let mut first = fx.server.expect_response_blocked(
        "U6 L2 before real bookkeeping",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_late_first")),
    );
    let mut after = fx.server.expect_response_blocked(
        "U6 L2 after new late admission",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_late_after")),
    );
    let actor = fx.session.actor.clone();
    let mut prepare =
        |_: &mut ConversationRequest, _: &xai_chat_state::RequestTransportBinding| Ok(());
    let mut salvage = super::length_salvage::LengthSalvage::new(None);
    let mut sampling = Default::default();
    let turn = super::switch_projection::HISTORY_S3_TEST_DOMAIN.scope(
        fx.session.domain(&fx.server),
        super::switch_projection::HISTORY_S3_TEST_REQUIREMENT.scope(
            k2_history::NativeHistoryRequirement::Selected,
            actor.process_conversation_turn_for_native_history_test(
                KOXD_U6_PROMPT,
                None,
                None,
                None,
                &mut salvage,
                &mut sampling,
                &mut prepare,
            ),
        ),
    );
    tokio::pin!(turn);
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = first.wait_blocked() => (),
            _ = &mut turn => panic!("H6_L2_LATE_EARLY_END"),
        }
    })
    .await
    .expect("H6_L2_LATE_FIRST_HTTP");
    let before = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !before
            .iter()
            .any(|item| item.text_content().contains(HUMAN)),
        "H6_L2_LATE_NO_EARLY_ADMISSION"
    );
    first.release();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = fixture.persistence_control.cleanup.notified() => (),
            _ = &mut turn => panic!("H6_L2_LATE_BOOKKEEPING_AWAIT_NOT_REACHED"),
        }
    })
    .await
    .expect("H6_L2_LATE_REAL_BOOKKEEPING_NOTIFICATION");
    assert_eq!(
        fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "H6_L2_LATE_CLEANUP_OBSERVED"
    );
    u6_interject(fx, HUMAN, Vec::new()).await;
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_L2_LATE_NOT_NEXT_PROMPT"
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = after.wait_blocked() => (),
            _ = &mut turn => panic!("H6_L2_LATE_NOT_RELEASED"),
        }
    })
    .await
    .expect("H6_L2_LATE_POST_BOOKKEEPING_RELEASE");
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(bodies.len(), 3, "H6_L2_LATE_ONCE_ONLY_HTTP");
    assert!(
        u6_body_text(&bodies[2]).contains(HUMAN),
        "H6_L2_LATE_ACTUAL_POST_BOOKKEEPING_BODY"
    );
    after.release();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn)
            .await
            .unwrap()
            .is_ok(),
        "H6_L2_LATE_OUTCOME"
    );
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        history
            .iter()
            .filter(|item| item.text_content().contains(HUMAN))
            .count(),
        1,
        "H6_L2_LATE_ADMISSION_ONCE"
    );
    first.assert_satisfied();
    after.assert_satisfied();
    json!({"caller":"turn.rs:3924","new_late_admission":true,"older_held_first_release_credit":false,
        "barrier":"real transient Plan cleanup, then feedback-manager snapshot await",
        "producer":"SessionCommand::Interject through real run loop","new_prompt":false,"delivery":1})
}

async fn u6_wait_fixture() -> U6Fixture {
    use xai_grok_tools::implementations::grok_build::task::backend::SubagentBackendResource;
    let fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let bridge = fixture
        .fx
        .session
        .actor
        .agent
        .borrow()
        .tool_bridge()
        .clone();
    bridge
        .register_mcp_tools(
            "get_task_output".into(),
            xai_grok_tools::implementations::grok_build::task_output::TaskOutputTool,
            None,
        )
        .unwrap();
    let backend = fixture
        .fx
        .root
        .tool_bridge()
        .read_resource::<SubagentBackendResource>()
        .await
        .expect("H6_TW1_REAL_COORDINATOR_BACKEND");
    bridge.shared_resources().await.lock().await.insert(backend);
    fixture
}

fn u6_wait_frames(mut frames: Vec<Value>, id: &str, timeout_ms: u64) -> Vec<Value> {
    let terminal = frames
        .iter()
        .position(|frame| frame["type"] == "message_delta")
        .expect("H6_TW1_REAL_FRAME_TERMINAL");
    let index = frames
        .iter()
        .filter(|frame| frame["type"] == "content_block_start")
        .count();
    frames.splice(
        terminal..terminal,
        [
            json!({"type":"content_block_start","index":index,"content_block":{
            "type":"tool_use","id":id,"name":"get_task_output",
            "input":{"task_ids":[K2LG_N_WORKER],"timeout_ms":timeout_ms}}}),
            json!({"type":"content_block_stop","index":index}),
        ],
    );
    frames
}

async fn u6_wait_started(fx: &K2lgN, running: &mut U6Running) {
    u6_await_turn_progress(
        running,
        "TW-1 real wait in flight",
        "H6_TW1_REAL_WAIT_IN_FLIGHT",
        "H6_TW1_REAL_WAIT_IN_FLIGHT_TURN_ENDED",
        || async move { fx.session.actor.tool_context.blocking_wait_depth.depth() != 0 },
    )
    .await;
}

async fn u6_wait_result(fx: &K2lgN, running: &mut U6Running, id: &str) {
    u6_await_turn_progress(
        running,
        "TW-1 real wait outcome",
        "H6_TW1_REAL_WAIT_OUTCOME",
        "H6_TW1_REAL_WAIT_OUTCOME_TURN_ENDED",
        || async move {
            fx.session.actor.chat_state_handle.get_conversation().await.iter().any(|item| {
                matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == id)
            })
        },
    )
    .await;
}

async fn u6_tw1(resident_arrival: bool) -> Value {
    const HUMAN: &str = "U6 TW-1 resident versus custody";
    let mut fixture = u6_wait_fixture().await;
    let fx = &mut fixture.fx;
    let mut turn =
        u6_handoff_with_salvage(fx, None, None, resident_arrival.then_some(60_000), None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    if resident_arrival {
        u6_wait_started(fx, &mut turn.running).await;
        assert!(
            fx.session
                .actor
                .state
                .lock()
                .await
                .message_delivery
                .held_input
                .latch
                .is_some(),
            "H6_TW1_REAL_HOLD_ENGAGED"
        );
        u6_interject(fx, HUMAN, Vec::new()).await;
        u6_wait_result(fx, &mut turn.running, "client_u6_wait").await;
        assert_eq!(
            fx.session
                .actor
                .tool_context
                .blocking_wait_depth
                .interrupted_wait_ids(),
            Some(vec![K2LG_N_WORKER.to_owned()]),
            "H6_TW1_RESIDENT_INTERRUPTS_REAL_WAIT"
        );
        let mut continuation = fx.server.expect_response_blocked(
            "U6 TW-1 interrupted wait continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2lg_n_closure_frames()),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2lg_n_after_frames("msg_u6_tw1_arrival_after")),
        );
        u6_readable_checkpoint(
            fx,
            &mut turn.running,
            true,
            "H6_R15_TW1_READABLE_PENDING_CHECKPOINT",
        )
        .await;
        let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_TW1_ARRIVAL_RELOAD").await;
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = continuation.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_ARRIVAL_CONTINUATION_RACE");
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
            "H6_TW1_ARRIVAL_ADMITTED_TO_CUSTODY"
        );
        assert!(continued, "H6_TW1_ARRIVAL_RESULTS_ONLY_CONTINUATION");
        let bodies = k2lg_n_bodies(&fx.server);
        u6_assert_results_only(&checkpoint, &bodies[1], &bodies[2]);
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &continuation,
            false,
            "H6_R15_TW1_RESIDENT_CLOSED_CHECKPOINT",
        )
        .await;
        assert!(
            tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
                .await
                .unwrap()
                .unwrap()
                .is_ok(),
            "H6_TW1_ARRIVAL_FIRST_CLOSED_RELEASE"
        );
        continuation.assert_satisfied();
    } else {
        u6_interject(fx, HUMAN, Vec::new()).await;
        assert!(
            !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
            "H6_TW1_CUSTODY_ADMISSION_WHILE_PENDING"
        );
        assert!(
            fx.session.actor.pending_interjections.is_empty(),
            "H6_TW1_CUSTODY_NOT_RESIDENT"
        );
        let mut continuation = fx.server.expect_response_blocked(
            "U6 TW-1 next pending handoff",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(u6_wait_frames(
                u6_next_pending(true),
                "client_u6_wait_next",
                1_000,
            )),
        );
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = continuation.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_CUSTODY_FIRST_CONTINUATION_RACE");
        assert!(continued, "H6_TW1_CUSTODY_FIRST_CONTINUATION");
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &continuation,
            true,
            "H6_R15_TW1_CUSTODY_NEXT_CHECKPOINT",
        )
        .await;
        u6_wait_started(fx, &mut turn.running).await;
        assert!(
            fx.session.actor.pending_interjections.is_empty(),
            "H6_TW1_NO_CUSTODY_WAKE"
        );
        u6_wait_result(fx, &mut turn.running, "client_u6_wait_next").await;
        assert!(
            fx.session
                .actor
                .tool_context
                .blocking_wait_depth
                .interrupted_wait_ids()
                .is_none(),
            "H6_TW1_CUSTODY_DOES_NOT_RETRIGGER"
        );
        u6_readable_checkpoint(
            fx,
            &mut turn.running,
            true,
            "H6_R15_TW1_READABLE_PENDING_CHECKPOINT",
        )
        .await;
        let (checkpoint, _) = u6_quiescent_reload(fx, "H6_R20_TW1_CUSTODY_RELOAD").await;
        assert!(k2lg_n_live(&checkpoint), "H6_TW1_REAL_NEXT_PENDING_CALL");
        let mut closed = fx.server.expect_response_blocked(
            "U6 TW-1 custody wait continuation",
            xai_grok_test_support::InferenceRequestMatcher::foreground(
                xai_grok_test_support::InferenceEndpoint::Messages,
            ),
            k2_script(k2_frames(
                "msg_u6_tw1_closed",
                vec![
                    k2_result("srvtoolu_u6_next", "alpha__lookup"),
                    (
                        json!({"type":"text","text":"U6 TW-1 closes the exchange"}),
                        Vec::new(),
                    ),
                ],
                "end_turn",
            )),
        );
        fx.server.enqueue_response(
            "/v1/messages",
            k2_script(k2lg_n_after_frames("msg_u6_tw1_custody_after")),
        );
        fx.session.releases[1].add_permits(1);
        let continued = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                result = &mut turn.running => { let _ = result.unwrap(); false },
                _ = closed.wait_blocked() => true,
            }
        })
        .await
        .expect("H6_TW1_CUSTODY_NEXT_CONTINUATION_RACE");
        assert!(
            !fx.session
                .actor
                .chat_state_handle
                .get_conversation()
                .await
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
            "H6_TW1_CUSTODY_HOLD_ASSERTION"
        );
        assert!(continued, "H6_TW1_CUSTODY_RESULTS_ONLY_CONTINUATION");
        let bodies = k2lg_n_bodies(&fx.server);
        u6_assert_results_only(&checkpoint, &bodies[2], &bodies[3]);
        u6_release_after_checkpoint(
            fx,
            &mut turn.running,
            &closed,
            false,
            "H6_R15_TW1_CUSTODY_CLOSED_CHECKPOINT",
        )
        .await;
        assert!(
            tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
                .await
                .unwrap()
                .unwrap()
                .is_ok(),
            "H6_TW1_CUSTODY_FIRST_CLOSED_RELEASE"
        );
        closed.assert_satisfied();
        continuation.assert_satisfied();
    }
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert_eq!(
        history
            .iter()
            .filter(|item| item.text_content().contains(HUMAN))
            .count(),
        1,
        "H6_TW1_ONCE_ONLY_RELEASE"
    );
    json!({"resident_arrival":resident_arrival,
        "tool":"get_task_output", "backend":"existing real SubagentCoordinator with scripted worker",
        "outcome":if resident_arrival { "interrupted by resident arrival" } else { "own bounded query timeout" },
        "custody_retriggers":false,"first_closed_release":1,"provider_or_model_calls":0})
}

#[derive(Clone, Default)]
struct U6TurnStepper(std::rc::Rc<U6StepState>);

#[derive(Default)]
struct U6StepState {
    paused: std::cell::Cell<bool>,
    permits: std::cell::Cell<u32>,
    finished: std::cell::Cell<bool>,
    wake: Arc<U6StepWake>,
    advanced: tokio::sync::Notify,
}

struct U6StepWake {
    ready: std::sync::atomic::AtomicBool,
    target: std::sync::Mutex<Option<std::task::Waker>>,
}

impl Default for U6StepWake {
    fn default() -> Self {
        Self {
            ready: std::sync::atomic::AtomicBool::new(true),
            target: Default::default(),
        }
    }
}

impl std::task::Wake for U6StepWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, std::sync::atomic::Ordering::SeqCst);
        let target = self.target.lock().unwrap().clone();
        if let Some(target) = target {
            target.wake();
        }
    }
}

impl U6TurnStepper {
    fn wake_task(&self) {
        let target = self.0.wake.target.lock().unwrap().clone();
        if let Some(target) = target {
            target.wake();
        }
    }
    fn pause(&self) {
        self.0.paused.set(true);
    }
    fn resume(&self) {
        self.0.paused.set(false);
        self.wake_task();
    }
    async fn step(&self) -> bool {
        if self.0.finished.get() {
            return false;
        }
        self.0.permits.set(self.0.permits.get() + 1);
        self.wake_task();
        tokio::time::timeout(K0_TIMEOUT, self.0.advanced.notified())
            .await
            .expect("H6_U63_REAL_TURN_POLL_ADVANCES");
        !self.0.finished.get()
    }
}

struct U6SteppedTurn<F> {
    future: std::pin::Pin<Box<F>>,
    stepper: Option<U6TurnStepper>,
}
impl<F: std::future::Future> std::future::Future for U6SteppedTurn<F> {
    type Output = F::Output;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let stepper = self.stepper.clone();
        let result = if let Some(stepper) = &stepper {
            *stepper.0.wake.target.lock().unwrap() = Some(cx.waker().clone());
            if stepper.0.paused.get() {
                if stepper.0.permits.get() == 0
                    || !stepper
                        .0
                        .wake
                        .ready
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return std::task::Poll::Pending;
                }
                stepper.0.permits.set(stepper.0.permits.get() - 1);
            }
            stepper
                .0
                .wake
                .ready
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let waker = std::task::Waker::from(stepper.0.wake.clone());
            let mut inner = std::task::Context::from_waker(&waker);
            self.future.as_mut().poll(&mut inner)
        } else {
            self.future.as_mut().poll(cx)
        };
        if let Some(stepper) = &stepper {
            if result.is_ready() {
                stepper.0.finished.set(true);
            }
            if stepper.0.paused.get() {
                stepper.0.advanced.notify_one();
            }
        }
        result
    }
}

async fn u6_step_to_http(
    stepper: &U6TurnStepper,
    expectation: &mut xai_grok_test_support::InferenceExpectation,
) {
    use futures_util::FutureExt;
    for _ in 0..256 {
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                biased;
                _ = expectation.wait_blocked() => true,
                advanced = stepper.step() => {
                    assert!(advanced, "H6_U63_REAL_CONTINUATION_NOT_STRANDED");
                    false
                }
            }
        })
        .await
        .expect("H6_U63_HTTP_PHASE_OUTCOME_RACE");
        if reached {
            stepper.0.permits.set(0);
            let _ = stepper.0.advanced.notified().now_or_never();
            return;
        }
    }
    panic!("H6_U63_HTTP_STEP_BOUND");
}

fn u6_resident_keys(actor: &SessionActor) -> Vec<Value> {
    let owner = xai_interjection_core::events::next_occurrence();
    let view = actor.pending_interjections.release_view(owner, |_| true);
    view.iter().map(|(key, entry)| json!({"key":key.as_u64(),"text_digest":native_content_digest(entry.text.as_bytes()),
        "images":entry.attachments.len(),"image_digest":native_content_digest(&serde_json::to_vec(&entry.attachments).unwrap())})).collect()
}

async fn u6_queue_resident(fx: &K2lgN, text: &str, images: Vec<acp::ImageContent>) {
    let mut blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
        text.to_owned(),
    ))];
    blocks.extend(images.into_iter().map(acp::ContentBlock::Image));
    let (respond_to, _response) = tokio::sync::oneshot::channel();
    let mut request = super::prompt_queue::QueueInputRequest::from_input_origin(
        blocks,
        format!("u6-queued-{}", uuid::Uuid::now_v7()),
        InputOrigin::new(PromptOrigin::User),
        PromptMode::Agent,
        respond_to,
    );
    request.client_identifier = fx
        .session
        .actor
        .state
        .lock()
        .await
        .pending_inputs
        .front()
        .and_then(|input| input.queue_meta.as_ref())
        .and_then(|meta| meta.owner.clone());
    assert!(
        !fx.session.actor.queue_input(request).await,
        "H6_U63_REAL_QUEUE_NO_CANCEL"
    );
    fx.session.actor.promote_queued_as_interjections().await;
    assert!(
        fx.session
            .actor
            .pending_interjections
            .snapshot()
            .iter()
            .any(|entry| entry.text == text),
        "H6_U63_REAL_PROMPT_QUEUE_TO_RESIDENT"
    );
}

#[derive(Clone)]
struct U6ParentInput {
    message_id: String,
    prompt_id: String,
    sender_session_id: String,
    text: Arc<str>,
}

struct U6ParentReceipt {
    input: U6ParentInput,
    result: tokio::sync::oneshot::Receiver<crate::session::commands::PromptTurnResult>,
}

async fn u6_admit_steer(actor: &Arc<SessionActor>, text: &str, sender: &str) -> U6ParentReceipt {
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
    let _coordinator = K0Abort(task.abort_handle());
    let (result_tx, _result_rx) = tokio::sync::oneshot::channel();
    commands
        .send(k0_task::SubagentEvent::Spawn(
            k0_task::SubagentSpawnRequest {
                request: Box::new(k0_task::SubagentRequest {
                    id: "k0-child".into(),
                    prompt: "in-process U6 Steer receiver".into(),
                    description: "U6 Steer fixture".into(),
                    subagent_type: "fixture".into(),
                    parent_session_id: sender.into(),
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
    let owner = sender.to_owned();
    let delivery_task = tokio::task::spawn_local(async move {
        let (delivery, respond_to) = delivery_rx.recv().await.unwrap();
        let message = delivery.message().clone();
        let (receipts, mut receipt_rx) = tokio::sync::mpsc::channel(1);
        let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        actor
            .admit_parent_agent_message(
                delivery,
                receipts,
                xai_grok_telemetry::TelemetryCtx::new(owner, Arc::new(tokio::sync::Mutex::new(0))),
                respond_to,
                completion,
            )
            .await;
        let receipt = receipt_rx.recv().await.expect("H6_U63_PARENT_REAL_RECEIPT");
        U6ParentReceipt {
            input: U6ParentInput {
                message_id: message.message_id,
                prompt_id: receipt.prompt_id,
                sender_session_id: message.sender_session_id,
                text: message.text,
            },
            result: receipt.result,
        }
    });
    let _delivery = K0Abort(delivery_task.abort_handle());
    let backend = ChannelBackend::for_coordinator_session(commands, sender);
    let outcome = tokio::time::timeout(
        K0_TIMEOUT,
        backend.send_active_message(
            k0_task::ActiveAgentMessageRequest::try_new_with_operation(
                "k0-child",
                text.to_owned(),
                k0_task::ActiveAgentMessageOperation::Steer,
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, k0_task::ActiveAgentMessageOutcome::Accepted { .. }),
        "H6_U63_PARENT_REAL_ADMISSION"
    );
    delivery_task.await.unwrap()
}

async fn u6_u63_prepare_fixture(
    custody: Option<&str>,
) -> (
    U6Fixture,
    K2lgNTurn,
    U6TurnStepper,
    xai_grok_test_support::InferenceExpectation,
) {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let stepper = U6TurnStepper::default();
    let mut turn =
        u6_handoff_with_salvage(&mut fixture.fx, None, None, None, Some(stepper.clone())).await;
    if let Some(text) = custody {
        u6_interject(&fixture.fx, text, Vec::new()).await;
        assert!(
            !k2lg_n_selected(
                &fixture.fx,
                fixture.fx.session.actor.drain_interjections_at_safe_point()
            )
            .await,
            "H6_U63_RAW_CUSTODY_ADMITTED"
        );
    }
    stepper.pause();
    let mut continuation = fixture.fx.server.expect_response_blocked(
        "U6 U63 real pending continuation",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fixture.fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    assert!(
        !turn.running.is_finished(),
        "H6_U63_REAL_CONTINUATION_IN_FLIGHT"
    );
    (fixture, turn, stepper, continuation)
}

async fn u6_u63_step_to_preparation(
    fixture: &U6Fixture,
    stepper: &U6TurnStepper,
    barrier_before: usize,
) {
    for _ in 0..256 {
        assert!(stepper.step().await, "H6_U63_PREPARATION_REACHABLE");
        if fixture.fx.session.actor.pending_interjections.is_empty() {
            assert_eq!(
                fixture
                    .persistence_control
                    .barriers
                    .load(std::sync::atomic::Ordering::SeqCst),
                barrier_before,
                "H6_U63_PREPARATION_NOT_PERSISTENCE_BARRIER"
            );
            return;
        }
    }
    panic!("H6_U63_PREPARATION_STEP_BOUND");
}

fn u6_image(side: u32) -> acp::ImageContent {
    let encoded = u6_png(side);
    acp::ImageContent::new(
        encoded
            .strip_prefix("data:image/png;base64,")
            .unwrap()
            .to_owned(),
        "image/png",
    )
}

async fn u6_l14() -> Value {
    const CUSTODY: &str = "U6 L14 raw custody";
    let (mut fixture, mut turn, stepper, continuation) =
        u6_u63_prepare_fixture(Some(CUSTODY)).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    u6_queue_resident(fx, "U6 L14 resident image", vec![u6_image(47)]).await;
    let original = u6_resident_keys(&fx.session.actor);
    let before = fx
        .session
        .actor
        .state
        .lock()
        .await
        .message_delivery
        .held_input
        .occupancy();
    let barriers = fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(
        &fixture.fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L14_CLOSED_CHECKPOINT",
    )
    .await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    let (during, expected_during, raw_shared) = {
        let state = fixture.fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        let mut expected = u6_expected_occupancy(&fixture.fx, &state, &[]);
        let entry = ledger
            .entries
            .iter()
            .find(|entry| {
                matches!(&entry.body,
            super::interjection::HeldBody::Human(raw) if raw.text == CUSTODY)
            })
            .unwrap();
        let super::interjection::HeldBody::Human(raw) = &entry.body else {
            unreachable!()
        };
        let typed = crate::session::placeholder_images::strip_paths_from_image_placeholders(
            raw.text.clone(),
        );
        let wrapped = xai_interjection_core::format_interjection(typed.clone());
        let item = ConversationItem::interjection(wrapped.clone());
        expected.entry_bytes += u6_accounted_preparation(
            entry.key.as_u64(),
            json!({"key":entry.key.as_u64(),"wrapped":wrapped,"typed":typed,"images":[],"item":item}),
        );
        (
            ledger.occupancy(),
            expected,
            ledger.entries.iter().any(|entry| {
                matches!(&entry.body, super::interjection::HeldBody::Human(raw)
                if raw.text == CUSTODY && Arc::strong_count(raw) > 1)
            }),
        )
    };
    u6_interject(&fixture.fx, "U6 L14 later resident", Vec::new()).await;
    let later = u6_resident_keys(&fixture.fx.session.actor);
    turn.running.abort();
    assert!(
        matches!(turn.running.await, Err(error) if error.is_cancelled()),
        "H6_L14_REAL_PREPARATION_ABORT"
    );
    let restored = u6_resident_keys(&fixture.fx.session.actor);
    let expected: Vec<_> = original.into_iter().chain(later).collect();
    assert_eq!(
        restored, expected,
        "H6_L14_ORIGINAL_KEYS_FRONT_ORDER_ATTACHMENTS"
    );
    let state = fixture.fx.session.actor.state.lock().await;
    let ledger = &state.message_delivery.held_input;
    assert!(ledger.entries.iter().any(|entry|
        matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == CUSTODY)),
        "H6_L14_RAW_CUSTODY_NEVER_LEFT");
    assert_eq!(
        ledger.occupancy(),
        before,
        "H6_L14_ABORT_CLEARS_PREPARED_CHARGE"
    );
    assert!(raw_shared, "H6_L14_ACTUAL_RAW_PREPARATION_REFERENCE");
    u6_observe(
        json!({"stage":"L14 real preparation abort", "prepared_raw_reference":raw_shared,
        "entry_bytes_before":before.entry_bytes,"entry_bytes_during":during.entry_bytes,
        "barriers_sent":fixture.persistence_control.barriers.load(std::sync::atomic::Ordering::SeqCst)-barriers}),
    );
    assert_eq!(
        during, expected_during,
        "H6_L10_INDEPENDENT_LIVE_COPY_OCCUPANCY_EQUALITY"
    );
    assert!(
        during.entry_bytes > before.entry_bytes,
        "H6_L14_LIVE_COPY_CHARGED"
    );
    assert!(
        !fixture
            .persistence_control
            .updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update.to_string().contains(CUSTODY)
                || update.to_string().contains("U6 L14 resident image")),
        "H6_L14_NO_HUMAN_PERSISTENCE"
    );
    json!({"await":"merged release prepare_interjection_images; fresh resident PNG, guard held and no FlushAndAck",
        "drop":"actual turn future abort","resident_keys_restored":true,"custody_retained":true})
}

async fn u6_l14c(straddling: bool) -> Value {
    let early = if straddling {
        "U6 L14c H0"
    } else {
        "U6 L14c H1"
    };
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, Some(stepper.clone())).await;
    let _stop = K0Abort(turn.running.abort_handle());
    if straddling {
        u6_interject(fx, early, Vec::new()).await;
    }
    let mut parents = Vec::new();
    if straddling {
        parents.push(u6_admit_steer(&fx.session.actor, "U6 L14c S_a", "u6-parent").await);
    }
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L14C_REAL_HELD_ADMISSION"
    );
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L14c real closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    u6_queue_resident(
        fx,
        if straddling { "U6 L14c I" } else { early },
        if straddling {
            vec![u6_image(49)]
        } else {
            Vec::new()
        },
    )
    .await;
    let image_key = straddling.then(|| u6_resident_keys(&fx.session.actor)[0].clone());
    let barriers = fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(
        &fixture.fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L14C_CLOSED_CHECKPOINT",
    )
    .await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    u6_interject(&fixture.fx, "U6 L14c H2", Vec::new()).await;
    u6_monitor(&fixture.fx, "u6-l14c", "U6 L14c M").await;
    parents.push(
        u6_admit_steer(
            &fixture.fx.session.actor,
            if straddling {
                "U6 L14c S_b"
            } else {
                "U6 L14c late Steer"
            },
            "u6-parent",
        )
        .await,
    );
    let mut first = Vec::new();
    for _ in 0..256 {
        assert!(stepper.step().await, "H6_L14C_FIRST_PREFIX_PROGRESS");
        first = fixture
            .fx
            .session
            .actor
            .chat_state_handle
            .get_conversation()
            .await;
        if first.iter().any(|item| item.text_content().contains(early)) {
            break;
        }
    }
    let labels = if straddling {
        vec![
            early,
            "U6 L14c S_a",
            "U6 L14c I",
            "U6 L14c H2",
            "U6 L14c M",
            "U6 L14c S_b",
        ]
    } else {
        vec![early, "U6 L14c H2", "U6 L14c M", "U6 L14c late Steer"]
    };
    let first_labels: Vec<_> = first
        .iter()
        .flat_map(|item| {
            labels
                .iter()
                .copied()
                .filter(move |label| item.text_content().contains(label))
        })
        .collect();
    u6_observe(
        json!({"stage":"L14c first logical prefix","straddling":straddling,"labels":first_labels}),
    );
    assert_eq!(first_labels, vec![early], "H6_L14C_FIRST_PREFIX_CUTOFF");
    if let Some(key) = &image_key {
        assert!(
            u6_resident_keys(&fixture.fx.session.actor).contains(key),
            "H6_L14C_IMAGE_SUFFIX_RESTORED_KEYED"
        );
    }
    fixture.fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l14c_after")),
    );
    stepper.resume();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L14C_SECOND_RELEASE_PROGRESS"
    );
    let after = fixture
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    let observed: Vec<_> = after
        .iter()
        .flat_map(|item| {
            labels
                .iter()
                .copied()
                .filter(move |label| item.text_content().contains(label))
        })
        .collect();
    assert_eq!(observed, labels, "H6_L14C_GLOBAL_ORDER_ACROSS_PREFIXES");
    if straddling {
        let bodies = k2lg_n_bodies(&fixture.fx.server);
        assert_eq!(
            bodies.last().unwrap()["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|part| part["type"] == "image")
                .count(),
            1,
            "H6_L14C_IMAGE_REACHES_SECOND_RELEASE"
        );
    }
    continuation.assert_satisfied();
    json!({"straddling":straddling,"cutoff":"earliest unprepared human, lowered to whole parent binding",
        "preparation_await":if straddling { "fresh image pipeline" } else { "current_model_id oneshot" },
        "first_prefix":first_labels,"final_order":observed,"new_prompt":false})
}

fn u6_source_name(source: super::interjection::HeldSource) -> &'static str {
    use super::interjection::HeldSource;
    match source {
        HeldSource::HumanInterjection => "human_interjection",
        HeldSource::ParentSteer => "parent_steer",
        HeldSource::MonitorEvent => "monitor_event",
        HeldSource::DeferredFollowUp => "deferred_followup",
        HeldSource::StationarityReminder => "stationarity_reminder",
        HeldSource::SkillReminder => "skill_reminder",
        HeldSource::PlanActivation => "plan_activation",
    }
}

fn u6_accounted_record(
    key: u64,
    source: &str,
    reference: Value,
    meta: &Value,
    payload: Value,
) -> u32 {
    let mut size = 0;
    for _ in 0..12 {
        let record = json!({"key":key,"source":source,"ref":reference,"meta":meta,"payload":payload,
            "preparation_control":if source == "human_interjection" { json!({"key":key,"maximum_charge":u32::MAX}) } else { Value::Null },"charged":size});
        let next = u32::try_from(serde_json::to_vec(&record).unwrap().len()).unwrap();
        if next == size {
            return size;
        }
        size = next;
    }
    panic!("H6_L10_INDEPENDENT_CANONICAL_RECORD_FIXPOINT");
}

fn u6_expected_occupancy(
    fx: &K2lgN,
    state: &State,
    parents: &[U6ParentReceipt],
) -> super::interjection::HeldOccupancy {
    use super::interjection::{HeldBody, HeldOccupancy};
    let ledger = &state.message_delivery.held_input;
    let binding = |meta: Option<&super::interjection::HeldBindingMeta>| {
        meta.map_or(
            Value::Null,
            |meta| json!({"prompt_id":meta.prompt_id,"epoch":meta.epoch}),
        )
    };
    let header = json!({"latch":binding(ledger.latch_meta.as_ref()),"latch_metadata":binding(ledger.latch_meta.as_ref()),
        "delivery_owner":binding(ledger.parent_owner_meta.as_ref()),"delivery_owner_metadata":binding(ledger.parent_owner_meta.as_ref()),
        "parent_persisted":ledger.parent_persisted,"pin_owner":ledger.pin_owner.map(|key| key.as_u64()),
        "pin_handle":if ledger.pinned_in.is_some() { json!("monitor_event_buffer") } else { Value::Null },
        "terminal":u8::from(ledger.terminal),"parent_stamps":[]});
    let stamp_slots: Vec<_> = (0..32)
        .map(|_| json!({"message_id":"x".repeat(126),"key":u64::MAX}))
        .collect();
    let header_bytes = u32::try_from(
        serde_json::to_vec(&header).unwrap().len()
            + serde_json::to_vec(&stamp_slots).unwrap().len()
            - 2,
    )
    .unwrap();
    let owner = ledger
        .pin_owner
        .unwrap_or_else(xai_interjection_core::events::next_occurrence);
    let view = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap()
        .release_view(owner, |_| true);
    let mut entry_bytes = 0;
    for entry in &ledger.entries {
        let reference = match &entry.body {
            HeldBody::Parent(id) => json!({"message_id":id}),
            HeldBody::Monitor => json!({"occurrence":entry.key.as_u64()}),
            HeldBody::Plan => json!({"activation_slot":1}),
            HeldBody::Human(_) | HeldBody::Owned(_) => Value::Null,
        };
        let payload = match &entry.body {
            HeldBody::Human(raw) => json!({"text":raw.text,"attachments":raw.attachments}),
            HeldBody::Owned(item) => serde_json::to_value(item).unwrap(),
            HeldBody::Monitor => {
                let event = view
                    .iter()
                    .find_map(|(key, event)| (key == entry.key).then_some(event))
                    .unwrap();
                json!({"owner_session_id":event.owner_session_id,"task_id":event.task_id,"event_text":event.event_text,"slot_pin":owner.as_u64()})
            }
            HeldBody::Parent(id) => {
                let input = &parents
                    .iter()
                    .find(|parent| &parent.input.message_id == id)
                    .expect("H6_L10_OBSERVED_PARENT_INPUT")
                    .input;
                let task = state.running_task.as_ref().unwrap();
                json!({"identity":id,"message_id":input.message_id,"prompt_id":input.prompt_id,
                    "binding_prompt_id":task.prompt_id,"epoch":format!("{:?}",task.epoch),
                    "sender_session_id":input.sender_session_id,"source":"agent","text":input.text,"telemetry_reference":1})
            }
            HeldBody::Plan => {
                json!({"text":fx.session.actor.plan_mode.lock().pending_activation_text().unwrap(),"prior_state_bits":1})
            }
        };
        entry_bytes += u6_accounted_record(
            entry.key.as_u64(),
            u6_source_name(entry.source),
            reference,
            &entry.meta,
            payload,
        );
    }
    let mut reserve_bytes = 0;
    for source in [
        "human_interjection",
        "parent_steer",
        "monitor_event",
        "deferred_followup",
        "stationarity_reminder",
        "skill_reminder",
        "plan_activation",
    ] {
        for reason in [
            "records",
            "entry_bytes",
            "total_bytes",
            "accounting",
            "terminal",
        ] {
            reserve_bytes = reserve_bytes.max(
                u32::try_from(
                    serde_json::to_vec(&json!({"source":source,"reason":reason,
                "key":u64::MAX,"observed_len":u64::MAX}))
                    .unwrap()
                    .len(),
                )
                .unwrap(),
            );
        }
    }
    HeldOccupancy {
        records: ledger.entries.len(),
        header_bytes,
        entry_bytes,
        reserve_bytes,
    }
}

async fn u6_check_occupancy(
    fx: &K2lgN,
    parents: &[U6ParentReceipt],
) -> super::interjection::HeldOccupancy {
    let state = fx.session.actor.state.lock().await;
    let expected = u6_expected_occupancy(fx, &state, parents);
    let actual = state.message_delivery.held_input.occupancy();
    assert_eq!(actual, expected, "H6_L10_INDEPENDENT_OCCUPANCY_EQUALITY");
    actual
}

async fn u6_l10(mode: &str) -> Value {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let mut parents = Vec::new();
    let mut refused = false;
    if mode == "parent-stamps" {
        for index in 0..32 {
            let before = u6_check_occupancy(fx, &parents).await;
            let mut parent = u6_admit_steer(
                &fx.session.actor,
                "U6 L10 parent payload fixed",
                &format!("u6-parent-{index:04}-{}", "p".repeat(index * 20)),
            )
            .await;
            assert!(
                parent.result.try_recv().is_err(),
                "H6_L10_NO_EARLY_PARENT_DELIVERY_ACK"
            );
            parents.push(parent);
            assert!(
                !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
                "H6_L10_PARENT_HOLD"
            );
            let after = u6_check_occupancy(fx, &parents).await;
            assert_eq!(
                after.records,
                before.records + 1,
                "H6_L10_REAL_PARENT_STAMP_ADMISSION"
            );
            u6_observe(
                json!({"stage":"L10 admission","mode":mode,"records":after.records,
                "entry_delta":after.entry_bytes-before.entry_bytes,"header_bytes":after.header_bytes,"reserve_bytes":after.reserve_bytes}),
            );
        }
        assert_eq!(
            fx.session
                .actor
                .state
                .lock()
                .await
                .message_delivery
                .held_input
                .parent_stamps
                .len(),
            32,
            "H6_L10_ALL_REAL_PARENT_STAMPS"
        );
    } else {
        let attempts = match mode {
            "records" => 129,
            "aggregate" => 12,
            "entry" => 2,
            "metadata" => 4,
            _ => unreachable!(),
        };
        for index in 0..attempts {
            let before = u6_check_occupancy(fx, &parents).await;
            if mode == "metadata" {
                u6_monitor_fixed_payload(
                    fx,
                    &format!(
                        "u6-{}-{index}",
                        "m".repeat([4000, 6000, 8000, 17000][index])
                    ),
                )
                .await;
                k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
            } else {
                let length = match mode {
                    "records" => 32,
                    "aggregate" => 30_900,
                    "entry" => [32_000, 32_768][index],
                    _ => unreachable!(),
                };
                u6_interject(
                    fx,
                    &format!("U6 L10 {mode} {}", "x".repeat(length)),
                    Vec::new(),
                )
                .await;
                assert!(
                    !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point())
                        .await,
                    "H6_L10_REAL_HELD_ADMISSION"
                );
            }
            let after = u6_check_occupancy(fx, &parents).await;
            let accepted = after.records == before.records + 1;
            if !accepted {
                assert_eq!(after, before, "H6_L10_REFUSAL_BEFORE_GROWTH");
                refused = true;
            }
            assert!(
                u64::from(after.header_bytes)
                    + u64::from(after.entry_bytes)
                    + u64::from(after.reserve_bytes)
                    <= 262_144,
                "H6_L10_AGGREGATE_BOUND"
            );
            u6_observe(
                json!({"stage":"L10 real admission event","mode":mode,"attempt":index,"accepted":accepted,
                "records":after.records,"entry_delta":after.entry_bytes-before.entry_bytes,"header_bytes":after.header_bytes,"reserve_bytes":after.reserve_bytes}),
            );
            if !accepted {
                break;
            }
        }
        assert!(refused, "H6_L10_EACH_REAL_CROSSING_REFUSES");
    }
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !history
            .iter()
            .any(|item| item.text_content().contains("U6 L10")),
        "H6_L10_NO_HISTORY_ADMISSION_OR_ACK"
    );
    assert!(
        !fixture
            .persistence_control
            .updates
            .lock()
            .unwrap()
            .iter()
            .any(|update| update.to_string().contains("U6 L10")),
        "H6_L10_NO_PERSISTED_HUMAN_CHUNK"
    );
    json!({"mode":mode,"real_producers":true,"oracle":"W11 independent canonical JSON; no charge functions or stored charge fields",
        "refused":refused,"shared_control_reporting":"deferred to apex-e4qs"})
}

async fn u6_monitor_fixed_payload(fx: &K2lgN, task: &str) {
    let buffer = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap();
    let before = buffer.len();
    fx.loop_tx
        .send(SessionCommand::InjectNotification {
            prompt_id: format!("u6-fixed-{task}"),
            prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                "U6 L10 fixed monitor payload",
            ))],
            priority: NotificationPriority::Next,
            source: NotificationSource::MonitorEvent {
                task_id: task.to_owned(),
            },
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, async {
        while buffer.len() <= before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_L10_METADATA_ONLY_REAL_MONITOR_INGRESS");
}

async fn u6_l10_reserve() -> Value {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    for index in 0..8 {
        u6_check_occupancy(fx, &[]).await;
        let n = if index < 7 { 30_900 } else { 16_000 };
        u6_interject(fx, &"r".repeat(n), Vec::new()).await;
        assert!(
            !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
            "H6_L10_RESERVE_REAL_FILL"
        );
        u6_check_occupancy(fx, &[]).await;
    }
    let before = u6_check_occupancy(fx, &[]).await;
    let free_without_reserve = 262_144 - before.header_bytes - before.entry_bytes;
    let key = xai_interjection_core::events::next_occurrence();
    let meta =
        json!({"owner_session_id":fx.session.actor.session_info.id.0.as_ref(),"raw_references":1});
    let overhead = u6_accounted_record(
        key.as_u64(),
        "human_interjection",
        Value::Null,
        &meta,
        json!({"text":"","attachments":[]}),
    );
    let length =
        usize::try_from(free_without_reserve - overhead - before.reserve_bytes / 2).unwrap();
    u6_interject(fx, &"v".repeat(length), Vec::new()).await;
    let owner = xai_interjection_core::events::next_occurrence();
    let view = fx
        .session
        .actor
        .pending_interjections
        .release_view(owner, |_| true);
    let (actual_key, input) = view.iter().next().unwrap();
    let candidate = u6_accounted_record(
        actual_key.as_u64(),
        "human_interjection",
        Value::Null,
        &meta,
        json!({"text":input.text,"attachments":input.attachments}),
    );
    drop(view);
    assert!(
        candidate <= 32_768
            && candidate <= free_without_reserve
            && candidate + before.reserve_bytes > free_without_reserve,
        "H6_L10_MEASURED_RESERVE_BOUNDARY"
    );
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L10_RESERVE_PENDING_HOLD"
    );
    let after = u6_check_occupancy(fx, &[]).await;
    assert_eq!(
        after, before,
        "H6_L10_RESERVE_CROSSING_REFUSES_BEFORE_GROWTH"
    );
    assert!(
        !fx.session.actor.pending_interjections.is_empty(),
        "H6_L10_RESERVE_RESIDENT_BACKPRESSURE"
    );
    u6_observe(
        json!({"stage":"L10 exact reserve crossing","candidate":candidate,"reserve":before.reserve_bytes,
        "free_without_reserve":free_without_reserve,"occupancy_unchanged":true}),
    );
    json!({"terminal_reserve_bytes":before.reserve_bytes,"candidate_fits_without_reserve":true,"refused_with_reserve":true})
}

async fn u6_l10_deferred_image() -> Value {
    let mut fixture =
        u6_fixture(Some(Arc::<str>::from(u6_png(160))), K2lgNOptions::default()).await;
    let fx = &mut fixture.fx;
    let mut turn = u6_handoff(fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    u6_interject(fx, "U6 L10 accepted prefix", Vec::new()).await;
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L10_IMAGE_PREFIX_ADMITTED"
    );
    let before = u6_check_occupancy(fx, &[]).await;
    fx.session.releases[1].add_permits(1);
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
        .await
        .expect("H6_L10_DEFERRED_IMAGE_TYPED_OUTCOME")
        .unwrap();
    let code = outcome
        .as_ref()
        .err()
        .and_then(|error| error.data.as_ref())
        .and_then(|data| data.get("code"))
        .and_then(Value::as_str);
    assert_eq!(
        code,
        Some("native_held_input_incomplete"),
        "H6_L10_OVERSIZED_DEFERRED_IMAGE_TYPED_INCOMPLETE"
    );
    let after = u6_check_occupancy(fx, &[]).await;
    assert_eq!(
        after, before,
        "H6_L10_DEFERRED_IMAGE_ACCEPTED_PREFIX_RETAINED"
    );
    assert_eq!(
        fx.server.messages_request_count(),
        2,
        "H6_L10_DEFERRED_IMAGE_NO_CONTINUATION_HTTP"
    );
    json!({"source":"real client MCP image follow-up","outcome":"native_held_input_incomplete", "accepted_prefix_retained":true})
}

async fn u6_l10b() -> Value {
    let (mut fixture, mut turn, stepper, continuation) = u6_u63_prepare_fixture(None).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let fx = &mut fixture.fx;
    let image = u6_image(220);
    assert!(
        serde_json::to_vec(&image).unwrap().len() > 32_768,
        "H6_L10B_REAL_OVERSIZED_RESIDENT_IMAGE"
    );
    u6_queue_resident(fx, "U6 L10b resident image", vec![image]).await;
    let original = u6_resident_keys(&fx.session.actor);
    u6_monitor(fx, "u6-l10b", "U6 L10b pinned monitor").await;
    k2lg_n_selected(fx, fx.session.actor.inject_pending_monitor_events()).await;
    assert_eq!(
        u6_resident_keys(&fx.session.actor),
        original,
        "H6_L10B_IMAGE_STAYS_UNADMITTED_RESIDENT"
    );
    let state = fx.session.actor.state.lock().await;
    assert!(
        state
            .message_delivery
            .held_input
            .entries
            .iter()
            .all(|entry| entry.source != super::interjection::HeldSource::HumanInterjection),
        "H6_L10B_NO_HELD_IMAGE_ADMISSION"
    );
    assert!(
        state
            .message_delivery
            .held_input
            .entries
            .iter()
            .any(|entry| entry.source == super::interjection::HeldSource::MonitorEvent),
        "H6_L10B_LATER_MONITOR_PINNED"
    );
    drop(state);
    fx.server.enqueue_response(
        "/v1/messages",
        k2_script(k2lg_n_after_frames("msg_u6_l10b_after")),
    );
    u6_release_after_checkpoint(
        fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L10B_CLOSED_CHECKPOINT",
    )
    .await;
    stepper.resume();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L10B_SAME_TURN_RELEASE_PROGRESS"
    );
    let bodies = k2lg_n_bodies(&fx.server);
    assert_eq!(
        bodies.len(),
        4,
        "H6_L10B_FIRST_RELEASE_NO_CANCEL_PROMPT_FALLBACK"
    );
    let blocks = bodies.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .as_array()
        .unwrap();
    let image_at = blocks
        .iter()
        .position(|part| part["type"] == "image")
        .expect("H6_L10B_IMAGE_DELIVERED");
    let monitor_at = blocks
        .iter()
        .position(|part| {
            part["text"]
                .as_str()
                .is_some_and(|text| text.contains("U6 L10b pinned monitor"))
        })
        .expect("H6_L10B_MONITOR_DELIVERED");
    assert!(
        image_at < monitor_at,
        "H6_L10B_IMAGE_BEFORE_LATER_PINNED_MONITOR"
    );
    continuation.assert_satisfied();
    json!({"producer":"QueueInputRequest->queue_input->promote_queued_as_interjections (prompt_queue 638-655)",
        "oversized_resident_preparation_not_held_admission":true,"order":"image,monitor", "new_prompt_or_cancel":false})
}

fn u6_accounted_preparation(key: u64, payload: Value) -> u32 {
    let mut size = 0;
    for _ in 0..12 {
        let value = json!({"key":key,"raw_reference":key,"charge_reference":key,"prepared":payload,"charged":size});
        let next = u32::try_from(serde_json::to_vec(&value).unwrap().len()).unwrap();
        if size == next {
            return size;
        }
        size = next;
    }
    panic!("H6_L10_INDEPENDENT_PREPARATION_RECORD_FIXPOINT");
}

async fn u6_l10_overlength_id() -> Value {
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let turn = u6_handoff(&mut fixture.fx).await;
    let _stop = K0Abort(turn.running.abort_handle());
    let id = "p".repeat(127);
    assert!(
        serde_json::to_vec(&id).unwrap().len() > 128,
        "H6_L10_OVERLENGTH_ID_ENCODED_BOUND"
    );
    let mut state = fixture.fx.session.actor.state.lock().await;
    let ledger = &mut state.message_delivery.held_input;
    let before = ledger.parent_stamps.len();
    ledger.stamp_parent(&id);
    assert_eq!(
        ledger.parent_stamps.len(),
        before,
        "H6_L10_OVERLENGTH_ID_NOT_EARLY_STAMPED"
    );
    let view = super::interjection::HeldParentView {
        key: ledger.parent_key(&id),
        message_id: id,
        text: Arc::<str>::from("constructed overlength parent"),
        payload: Value::Null,
    };
    assert!(
        view.key.is_none(),
        "H6_L10_OVERLENGTH_PARENT_HAS_NO_EARLY_KEY"
    );
    let lower = xai_interjection_core::events::next_occurrence();
    let release = view.release_key();
    let upper = xai_interjection_core::events::next_occurrence();
    assert!(
        lower < release && release < upper,
        "H6_L10_OVERLENGTH_PARENT_RECEIVES_RELEASE_KEY"
    );
    json!({"evidence":"constructed ordinary-ledger/view boundary, not a real overlength coordinator message",
        "real_coordinator_id_shape":"UUID; the 32 real-parent-stamp arm is separate",
        "no_early_stamp":true,"production_release_key_fallback":true})
}

async fn u6_step_to_persistence_barrier(stepper: &U6TurnStepper, control: &U6PersistenceControl) {
    use futures_util::FutureExt;
    for _ in 0..256 {
        if !control.held_barriers.lock().unwrap().is_empty() {
            return;
        }
        let reached = tokio::time::timeout(K0_TIMEOUT, async {
            tokio::select! {
                biased;
                _ = control.barrier_held.notified() => true,
                advanced = stepper.step() => {
                    assert!(advanced, "H6_L14B_REAL_BARRIER_TURN_ADVANCE");
                    false
                }
            }
        })
        .await
        .expect("H6_L14B_REAL_PERSISTENCE_BARRIER_RACE");
        if reached {
            stepper.0.permits.set(0);
            let _ = stepper.0.advanced.notified().now_or_never();
            assert!(
                !control.held_barriers.lock().unwrap().is_empty(),
                "H6_L14B_TEST_OWNS_REAL_FLUSH_REPLY"
            );
            return;
        }
    }
    panic!("H6_L14B_REAL_BARRIER_STEP_BOUND");
}

fn u6_forward_held_barrier(fixture: &U6Fixture) {
    let message = fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .remove(0);
    fixture
        .fx
        .session
        .persistence
        .tx
        .send(message)
        .expect("H6_L14B_REAL_PERSISTENCE_RESTORED");
}

fn u6_fail_held_barrier(fixture: &U6Fixture) {
    let message = fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .remove(0);
    let PersistenceMsg::FlushAndAck { respond_to } = message else {
        panic!("H6_L14B_EXPECTED_REAL_FLUSH");
    };
    let _ = respond_to.send(Err(std::io::Error::other(
        "U6 test-owned persistence unavailable",
    )));
}

fn u6_user_persists(control: &U6PersistenceControl, needle: &str) -> usize {
    control
        .user_chunks
        .lock()
        .unwrap()
        .iter()
        .filter(|update| update.to_string().contains(needle))
        .count()
}

async fn u6_mixed_keys(fx: &K2lgN) -> Vec<Value> {
    let state = fx.session.actor.state.lock().await;
    state
        .message_delivery
        .held_input
        .entries
        .iter()
        .map(|entry| json!({"key":entry.key.as_u64(),"source":u6_source_name(entry.source)}))
        .collect()
}

struct U6MixedFixture {
    fixture: U6Fixture,
    turn: K2lgNTurn,
    stepper: U6TurnStepper,
    continuation: xai_grok_test_support::InferenceExpectation,
    parent: U6ParentReceipt,
    keys: Vec<Value>,
    resident_keys: Vec<Value>,
    plan_text: String,
    announcements_before: usize,
    plan_writes_before: usize,
    monitor_keys: Vec<Value>,
}

async fn u6_l14b_fixture() -> U6MixedFixture {
    use crate::tools::todo::{TodoItem, TodoPriority, TodoState, TodoStatus};
    use xai_grok_tools::types::resources::State as ResourceState;
    let control = U6PersistenceControl {
        bookkeeping_feedback: true,
        plan_tools: true,
        ..Default::default()
    };
    let mut fixture = u6_fixture_controlled(
        Some(Arc::<str>::from(u6_png(32))),
        K2lgNOptions::default(),
        control,
    )
    .await;
    let fx = &mut fixture.fx;
    let bridge = fx.session.actor.agent.borrow().tool_bridge().clone();
    let mut todos = TodoState::default();
    todos.push(
        "u6-bookkeeping".into(),
        TodoItem {
            content: "U6 bookkeeping observer".into(),
            priority: TodoPriority::Medium,
            status: TodoStatus::InProgress,
            meta: None,
        },
    );
    bridge
        .shared_resources()
        .await
        .lock()
        .await
        .insert(ResourceState(todos));
    let stepper = U6TurnStepper::default();
    let mut turn = u6_handoff_with_salvage(fx, None, None, None, Some(stepper.clone())).await;
    u6_interject(fx, "U6 L14b custody human", Vec::new()).await;
    let parent = u6_admit_steer(&fx.session.actor, "U6 L14b parent zero", "u6-parent").await;
    let (responds_to, replied) = tokio::sync::oneshot::channel();
    fx.loop_tx
        .send(SessionCommand::SessionMode {
            session_mode: acp::SessionModeId::new("plan"),
            responds_to,
        })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, replied)
        .await
        .expect("H6_L14B_REAL_PLAN_COMMAND")
        .unwrap();
    fx.session
        .actor
        .apply_skill_update_effects(
            xai_grok_tools::types::skill_discovery_tracker::SkillUpdateEffects {
                system_reminder: Some("U6 L14b skill reminder".into()),
                send_available_commands: false,
                kind: xai_grok_tools::types::skill_discovery_tracker::SkillUpdateKind::Discovery,
            },
        )
        .await;
    u6_monitor(fx, "u6-l14b", "U6 L14b pinned monitor").await;
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_L14B_REAL_MIXED_HELD_ADMISSION"
    );
    k2lg_n_selected(fx, fx.session.actor.flush_skills_at_native_safe_point()).await;
    stepper.pause();
    let mut continuation = fx.server.expect_response_blocked(
        "U6 L14b results-only closure",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_closure_frames()),
    );
    fx.session.releases[1].add_permits(1);
    u6_step_to_http(&stepper, &mut continuation).await;
    let keys = u6_mixed_keys(fx).await;
    for source in [
        "human_interjection",
        "parent_steer",
        "deferred_followup",
        "monitor_event",
        "skill_reminder",
        "plan_activation",
    ] {
        assert!(
            keys.iter().any(|entry| entry["source"] == source),
            "H6_L14B_EACH_REAL_READY_SOURCE"
        );
    }
    let plan_text = fx
        .session
        .actor
        .plan_mode
        .lock()
        .pending_activation_text()
        .expect("H6_L14B_REAL_PENDING_ACTIVATION")
        .to_owned();
    let announcements_before = fixture
        .persistence_control
        .announcement_updates
        .load(std::sync::atomic::Ordering::SeqCst);
    let plan_writes_before = fixture
        .persistence_control
        .plan_updates
        .load(std::sync::atomic::Ordering::SeqCst);
    let monitor_keys = u6_pinned_monitor_keys(fx).await;
    assert_eq!(
        monitor_keys.len(),
        1,
        "H6_L14B_REAL_PINNED_MONITOR_AUTHORITY"
    );
    u6_queue_resident(fx, "U6 L14b guarded resident", Vec::new()).await;
    let resident_keys = u6_resident_keys(&fx.session.actor);
    let barriers = fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_release_after_checkpoint(
        &fixture.fx,
        &mut turn.running,
        &continuation,
        false,
        "H6_R15_L14B_CLOSED_CHECKPOINT",
    )
    .await;
    u6_u63_step_to_preparation(&fixture, &stepper, barriers).await;
    fixture
        .persistence_control
        .hold_barriers
        .store(true, std::sync::atomic::Ordering::SeqCst);
    u6_step_to_persistence_barrier(&stepper, &fixture.persistence_control).await;
    U6MixedFixture {
        fixture,
        turn,
        stepper,
        continuation,
        parent,
        keys,
        resident_keys,
        plan_text,
        announcements_before,
        plan_writes_before,
        monitor_keys,
    }
}

async fn u6_l14b_closed(latch_state: &str) -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    let resident_before = mixed.resident_keys.clone();
    let barrier_before = mixed
        .fixture
        .persistence_control
        .barriers
        .load(std::sync::atomic::Ordering::SeqCst);
    u6_readable_checkpoint(
        &mixed.fixture.fx,
        &mut mixed.turn.running,
        false,
        "H6_R15_L14B_PRE_CLOSE_CHECKPOINT",
    )
    .await;
    k2lg_n_close_mailbox(&mixed.fixture.fx).await;
    u6_forward_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    assert!(
        mixed
            .fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "H6_L14B_REAL_BOOKKEEPING_BETWEEN_ATTEMPTS"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .barriers
            .load(std::sync::atomic::Ordering::SeqCst),
        barrier_before + 1,
        "H6_L14B_REAL_PRECOMP_THEN_POST_BOOKKEEPING"
    );
    let progress_before_second = {
        let mut state = mixed.fixture.fx.session.actor.state.lock().await;
        let ledger = &mut state.message_delivery.held_input;
        let progress = ledger.parent_persisted;
        if latch_state == "absent" {
            ledger.latch = None;
            ledger.latch_meta = None;
        }
        if latch_state == "inert" {
            if let Some(latch) = &mut ledger.latch {
                latch.binding = xai_message_delivery_core::TurnBinding::new(
                    "U6 constructed inert latch".into(),
                    mixed.turn.epoch,
                );
            }
            if let Some(meta) = &mut ledger.latch_meta {
                meta.prompt_id = "U6 constructed inert latch".into();
            }
        }
        ledger.refresh_header();
        progress
    };
    let mut second = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b parent one",
        "u6-parent",
    )
    .await;
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    u6_forward_held_barrier(&mixed.fixture);
    mixed.stepper.resume();
    let outcome = tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
        .await
        .expect("H6_L14B_REAL_SECOND_ATTEMPT_FINISH")
        .unwrap();
    assert!(
        outcome.is_ok(),
        "H6_L14B_BOOKKEEPING_RETURNS_WITH_CLOSED_CHAT"
    );
    let (respond_to, observed) = tokio::sync::oneshot::channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .notifications
        .persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, observed)
        .await
        .expect("H6_L14B_FAILED_ATTEMPTS_RECEIVER_FENCE")
        .unwrap()
        .unwrap();
    assert_eq!(
        u6_pinned_monitor_keys(&mixed.fixture.fx).await,
        mixed.monitor_keys,
        "H6_L14B_PINNED_MONITOR_AUTHORITY_RETAINED"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .plan_updates
            .load(std::sync::atomic::Ordering::SeqCst),
        mixed.plan_writes_before,
        "H6_L14B_NO_PLAN_DELIVERY_EFFECTS_ON_FAILURE"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        0,
        "H6_L14B_NO_HUMAN_PERSIST_BEFORE_FAILED_SUBMIT"
    );
    assert_eq!(
        u6_user_persists(
            &mixed.fixture.persistence_control,
            "U6 L14b guarded resident"
        ),
        0,
        "H6_L14B_NO_RESIDENT_PERSIST_ON_FAILED_SUBMIT"
    );
    assert_eq!(
        u6_mixed_keys(&mixed.fixture.fx).await,
        mixed.keys,
        "H6_L14B_EACH_CUSTODY_KEY_RETAINED"
    );
    assert_eq!(
        u6_resident_keys(&mixed.fixture.fx.session.actor),
        resident_before,
        "H6_L14B_GUARDED_RESIDENT_RESTORED"
    );
    assert_eq!(
        mixed
            .fixture
            .fx
            .session
            .actor
            .plan_mode
            .lock()
            .pending_activation_text(),
        Some(mixed.plan_text.as_str()),
        "H6_L14B_REVOCABLE_PLAN_RETAINED_ON_FAILURE"
    );
    assert_eq!(
        mixed
            .fixture
            .persistence_control
            .announcement_updates
            .load(std::sync::atomic::Ordering::SeqCst),
        mixed.announcements_before,
        "H6_L14B_NO_SKILL_DELIVERY_EFFECTS_ON_FAILURE"
    );
    assert!(
        mixed.parent.result.try_recv().is_err() && second.result.try_recv().is_err(),
        "H6_L14B_NO_PARENT_DELIVERY_ACK_ON_FAILURE"
    );
    let first_count = u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b parent zero");
    let second_count = u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b parent one");
    u6_observe(
        json!({"stage":"L14b two real failed attempts","latch_state":latch_state,
        "first_parent_persists":first_count,"second_parent_persists":second_count,
        "progress_before_second":progress_before_second,"bookkeeping_observed":true,
        "callers":["turn.rs:3913","turn.rs:3924"]}),
    );
    assert_eq!(first_count, 1, "H6_L14B_PARENT_PREFIX_PERSISTED_ONCE");
    assert_eq!(second_count, 1, "H6_L14B_SECOND_STEER_UNSENT_SUFFIX_ONCE");
    assert_eq!(
        progress_before_second, 1,
        "H6_L14B_PROGRESS_OWNED_BY_ACTUAL_DELIVERY"
    );
    assert_eq!(
        mixed.fixture.fx.server.messages_request_count(),
        3,
        "H6_L14B_NO_FAILED_RELEASE_HTTP"
    );
    let parents = [mixed.parent, second];
    let _occupancy = u6_check_occupancy(&mixed.fixture.fx, &parents).await;
    mixed.continuation.assert_satisfied();
    json!({"latch_state":latch_state,"real_precomp_and_post_bookkeeping":true,"second_steer_between_attempts":true,
        "closed_mailbox_reopened":false,"human_persistence":0,"parent_persistence_per_identity":1})
}

async fn u6_l14b_persistence_recovery() -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    u6_fail_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    assert!(
        mixed
            .fixture
            .persistence_control
            .cleanup_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0,
        "H6_L14B_PERSISTENCE_FAILURE_REAL_BOOKKEEPING"
    );
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
    ] {
        assert_eq!(
            u6_user_persists(&mixed.fixture.persistence_control, text),
            0,
            "H6_L14B_UNAVAILABLE_PERSISTENCE_ATTEMPT_HAS_NO_CHUNK"
        );
    }
    assert_eq!(
        u6_mixed_keys(&mixed.fixture.fx).await,
        mixed.keys,
        "H6_L14B_PERSISTENCE_FAILURE_PREFIX_RETAINED"
    );
    let mut released_request = mixed.fixture.fx.server.expect_response_blocked(
        "U6 L14b first successful release request",
        xai_grok_test_support::InferenceRequestMatcher::foreground(
            xai_grok_test_support::InferenceEndpoint::Messages,
        ),
        k2_script(k2lg_n_after_frames("msg_u6_l14b_recovered")),
    );
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    u6_forward_held_barrier(&mixed.fixture);
    mixed.stepper.resume();
    tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            _ = released_request.wait_blocked() => (),
            _ = &mut mixed.turn.running => panic!("H6_L14B_RECOVERY_FIRST_RELEASE_HTTP"),
        }
    })
    .await
    .expect("H6_L14B_RECOVERY_FIRST_RELEASE_HTTP_BOUND");
    let bodies = k2lg_n_bodies(&mixed.fixture.fx.server);
    let released_text = u6_body_text(bodies.last().unwrap());
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
        "Image extracted from tool result",
        "U6 L14b pinned monitor",
        "U6 L14b skill reminder",
        mixed.plan_text.as_str(),
    ] {
        assert_eq!(
            released_text.matches(text).count(),
            1,
            "H6_L14B_FIRST_RELEASE_EACH_READY_SOURCE_ONCE"
        );
    }
    released_request.release();
    assert!(
        tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
            .await
            .unwrap()
            .unwrap()
            .is_ok(),
        "H6_L14B_RESTORED_TEST_PERSISTENCE_SUCCESS"
    );
    released_request.assert_satisfied();
    let history = mixed
        .fixture
        .fx
        .session
        .actor
        .chat_state_handle
        .get_conversation()
        .await;
    for text in [
        "U6 L14b parent zero",
        "U6 L14b custody human",
        "U6 L14b guarded resident",
        "Image extracted from tool result",
        "U6 L14b pinned monitor",
        "U6 L14b skill reminder",
        mixed.plan_text.as_str(),
    ] {
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L14B_RECOVERED_EACH_SOURCE_ONCE"
        );
    }
    assert!(
        mixed
            .fixture
            .fx
            .session
            .actor
            .plan_mode
            .lock()
            .pending_activation_text()
            .is_none(),
        "H6_L14B_PLAN_CONSUMED_ONLY_AFTER_SUCCESS"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        1,
        "H6_L14B_HUMAN_PERSIST_ONLY_AFTER_SUCCESS"
    );
    mixed.continuation.assert_satisfied();
    json!({"first_persistence_barrier":"real receiver error","second":"restored test-owned persistence",
        "real_callers":["turn.rs:3913","turn.rs:3924"],"each_ready_source_once":true})
}

async fn u6_l14b_command_cancel() -> Value {
    let mut mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    k2lg_n_close_mailbox(&mixed.fixture.fx).await;
    u6_forward_held_barrier(&mixed.fixture);
    u6_step_to_persistence_barrier(&mixed.stepper, &mixed.fixture.persistence_control).await;
    let old_progress = mixed
        .fixture
        .fx
        .session
        .actor
        .state
        .lock()
        .await
        .message_delivery
        .held_input
        .parent_persisted;
    let old_epoch = mixed.turn.epoch;
    mixed
        .fixture
        .fx
        .loop_tx
        .send(SessionCommand::Cancel(crate::session::CancelOptions {
            user_initiated: true,
            trigger: Some(crate::session::CancelTrigger::CtrlC),
            ..Default::default()
        }))
        .unwrap();
    let joined = tokio::time::timeout(K0_TIMEOUT, &mut mixed.turn.running)
        .await
        .expect("H6_L14B_REAL_COMMAND_CANCEL_FINISH");
    assert!(
        matches!(joined, Err(error) if error.is_cancelled()),
        "H6_L14B_COMMAND_CANCEL_ABORTS_ACTUAL_TURN"
    );
    tokio::time::timeout(K0_TIMEOUT, async {
        while mixed
            .fixture
            .fx
            .session
            .actor
            .state
            .lock()
            .await
            .running_task
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_L14B_COMMAND_CANCEL_SETTLES_OLD_OWNER");
    let old_parent_result = tokio::time::timeout(K0_TIMEOUT, &mut mixed.parent.result)
        .await
        .expect("H6_L14B_OLD_PARENT_TERMINAL_RECEIPT")
        .expect("H6_L14B_OLD_PARENT_RECEIPT_SENDER");
    assert!(
        matches!(old_parent_result, Ok(ref done) if done.stop_reason == acp::StopReason::Cancelled
        && matches!(&done.completion_kind, crate::session::commands::PromptCompletionKind::Cancelled { .. })),
        "H6_L14B_OLD_PARENT_OWNER_SETTLED"
    );
    let retained = u6_mixed_keys(&mixed.fixture.fx).await;
    for source in ["human_interjection", "deferred_followup", "skill_reminder"] {
        let before: Vec<_> = mixed
            .keys
            .iter()
            .filter(|entry| entry["source"] == source)
            .collect();
        let after: Vec<_> = retained
            .iter()
            .filter(|entry| entry["source"] == source)
            .collect();
        assert_eq!(
            after, before,
            "H6_L14B_COMMAND_CANCEL_RETAINS_NONPARENT_CUSTODY"
        );
    }
    assert_eq!(
        mixed.fixture.fx.server.messages_request_count(),
        3,
        "H6_L14B_CUSTODY_DOES_NOT_RESTART"
    );
    mixed
        .fixture
        .persistence_control
        .hold_barriers
        .store(false, std::sync::atomic::Ordering::SeqCst);
    while !mixed
        .fixture
        .persistence_control
        .held_barriers
        .lock()
        .unwrap()
        .is_empty()
    {
        u6_forward_held_barrier(&mixed.fixture);
    }
    let (respond_to, _response) = tokio::sync::oneshot::channel();
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let mut request = super::prompt_queue::QueueInputRequest::from_input_origin(
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            "U6 L14b new admitted turn",
        ))],
        "u6-l14b-new-owner".into(),
        InputOrigin::new(PromptOrigin::User),
        PromptMode::Agent,
        respond_to,
    );
    request.client_identifier = Some("U6".into());
    request.initial_child_prompt_ready = Some(ready);
    assert!(
        !mixed.fixture.fx.session.actor.queue_input(request).await,
        "H6_L14B_NEW_TURN_REAL_QUEUE_ADMISSION"
    );
    let (completion, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .clone()
        .maybe_start_running_task(completion)
        .await;
    let _publication_release = tokio::time::timeout(K0_TIMEOUT, ready_rx)
        .await
        .expect("H6_L14B_NEW_TURN_REAL_READINESS_BARRIER")
        .unwrap();
    let new_handle = {
        let state = mixed.fixture.fx.session.actor.state.lock().await;
        let task = state
            .running_task
            .as_ref()
            .expect("H6_L14B_NEW_ADMITTED_OWNER_RUNNING");
        assert!(
            task.epoch != old_epoch && task.prompt_id == "u6-l14b-new-owner",
            "H6_L14B_NEW_DELIVERY_BINDING"
        );
        task.handle.clone()
    };
    let _new_turn = K0Abort(new_handle);
    let _new_parent = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b new-owner parent",
        "u6-parent",
    )
    .await;
    let delivered = k2lg_n_selected(
        &mixed.fixture.fx,
        mixed
            .fixture
            .fx
            .session
            .actor
            .drain_admitted_messages_at_safe_point(),
    )
    .await;
    assert!(!delivered, "H6_L14B_CLOSED_MAILBOX_NEVER_REOPENS");
    let new_progress = mixed
        .fixture
        .fx
        .session
        .actor
        .state
        .lock()
        .await
        .message_delivery
        .held_input
        .parent_persisted;
    let (respond_to, observed) = tokio::sync::oneshot::channel();
    mixed
        .fixture
        .fx
        .session
        .actor
        .notifications
        .persistence_tx
        .send(PersistenceMsg::FlushAndAck { respond_to })
        .unwrap();
    tokio::time::timeout(K0_TIMEOUT, observed)
        .await
        .expect("H6_L14B_NEW_OWNER_RECEIVER_FENCE")
        .unwrap()
        .unwrap();
    let new_persists = u6_user_persists(
        &mixed.fixture.persistence_control,
        "U6 L14b new-owner parent",
    );
    u6_observe(
        json!({"stage":"L14b real command Cancel then new admitted owner", "old_progress":old_progress,
        "new_parent_persists":new_persists,"new_progress":new_progress,"new_queue_readiness":true,"closed_mailbox_reopened":false,
        "monitor_sweep":"U6.5 pre-state, NOT CREDITED","plan_teardown":"U6.7 pre-state, NOT CREDITED"}),
    );
    assert_eq!(
        old_progress, 1,
        "H6_L14B_OLD_OWNER_RETAINED_PREFIX_PROGRESS"
    );
    assert_eq!(
        new_persists, 1,
        "H6_L14B_NEW_OWNER_FIRST_PARENT_NOT_SKIPPED"
    );
    assert_eq!(
        u6_user_persists(&mixed.fixture.persistence_control, "U6 L14b custody human"),
        0,
        "H6_L14B_CANCEL_PATH_NO_FAILED_HUMAN_PERSISTENCE"
    );
    mixed.continuation.assert_satisfied();
    json!({"command":"SessionCommand::Cancel CtrlC", "new_turn":"real queue admission and publication readiness",
        "new_attempt":"new owner's first explicit real safe point, not old post-bookkeeping", "closed_mailbox_reopened":false,
        "full_cancel_rebuild_end_U6_6_credit":false,"pin_sweep_credit":false})
}

async fn u6_l14b_constructed_partial_loop() -> Value {
    use super::interjection::{
        HeldBindingMeta, HeldCommitError, HeldParentSelection, HeldParentView,
    };
    let mixed = u6_l14b_fixture().await;
    let _stop = K0Abort(mixed.turn.running.abort_handle());
    let _second = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b partial-loop parent one",
        "u6-parent",
    )
    .await;
    let _third = u6_admit_steer(
        &mixed.fixture.fx.session.actor,
        "U6 L14b partial-loop parent two",
        "u6-parent",
    )
    .await;
    let actor = &mixed.fixture.fx.session.actor;
    let send = |parent: &HeldParentView| {
        let update = acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
            acp::ContentBlock::Text(acp::TextContent::new(parent.text.to_string())),
        ));
        actor
            .notifications
            .persistence_tx
            .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                acp::SessionNotification::new(actor.session_info.id.clone(), update),
            ))))
            .map_err(|_| HeldCommitError::PersistenceUnavailable)
    };
    let mut state = actor.state.lock().await;
    let task = state.running_task.as_ref().unwrap();
    let binding = xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
    let meta = HeldBindingMeta {
        prompt_id: task.prompt_id.clone(),
        epoch: format!("{:?}", task.epoch),
    };
    let mut sends = 0;
    let first = actor.commit_held_parent_batch(
        &mut state,
        &binding,
        &meta,
        HeldParentSelection::All,
        |parents, ledger| {
            SessionActor::persist_held_parent_prefix(parents, ledger, |parent| {
                sends += 1;
                if sends == 2 {
                    return Err(HeldCommitError::PersistenceUnavailable);
                }
                send(parent)
            })
        },
    );
    assert!(
        matches!(first, Err(HeldCommitError::PersistenceUnavailable)),
        "H6_L14B_CONSTRUCTED_SECOND_SEND_FAILURE"
    );
    assert_eq!(
        state.message_delivery.held_input.parent_persisted, 1,
        "H6_L14B_CONSTRUCTED_PREFIX_PROGRESS_RETAINED"
    );
    let mut retried = Vec::new();
    let second = actor.commit_held_parent_batch(
        &mut state,
        &binding,
        &meta,
        HeldParentSelection::All,
        |parents, ledger| {
            SessionActor::persist_held_parent_prefix(parents, ledger, |parent| {
                send(parent)?;
                retried.push(parent.text.to_string());
                Ok(())
            })?;
            actor
                .chat_state_handle
                .try_push_user_messages_batch(
                    parents
                        .iter()
                        .map(|parent| ConversationItem::agent_message(parent.text.to_string()))
                        .collect(),
                )
                .map_err(|_| HeldCommitError::ChatStateUnavailable)
        },
    );
    assert!(second.is_ok(), "H6_L14B_CONSTRUCTED_SUFFIX_COMMIT_SUCCEEDS");
    assert_eq!(
        retried,
        vec![
            "U6 L14b partial-loop parent one",
            "U6 L14b partial-loop parent two"
        ],
        "H6_L14B_PARTIAL_LOOP_RETRIES_ONLY_UNSENT_SUFFIX"
    );
    assert_eq!(
        state.message_delivery.held_input.parent_persisted, 0,
        "H6_L14B_SETTLED_PARENT_PREFIX_CLEARED"
    );
    assert!(
        state.message_delivery.held_input.parent_owner.is_none(),
        "H6_L14B_SETTLED_PARENT_OWNER_CLEARED"
    );
    drop(state);
    tokio::time::timeout(K0_TIMEOUT, async {
        while [
            "U6 L14b parent zero",
            "U6 L14b partial-loop parent one",
            "U6 L14b partial-loop parent two",
        ]
        .into_iter()
        .map(|text| u6_user_persists(&mixed.fixture.persistence_control, text))
        .sum::<usize>()
            < 3
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("H6_L14B_PARTIAL_LOOP_REAL_PERSISTENCE_RECEIVER");
    let history = actor.chat_state_handle.get_conversation().await;
    for text in [
        "U6 L14b parent zero",
        "U6 L14b partial-loop parent one",
        "U6 L14b partial-loop parent two",
    ] {
        assert_eq!(
            u6_user_persists(&mixed.fixture.persistence_control, text),
            1,
            "H6_L14B_PARTIAL_LOOP_PARENT_PERSISTED_ONCE"
        );
        assert_eq!(
            history
                .iter()
                .filter(|item| item.text_content().contains(text))
                .count(),
            1,
            "H6_L14B_PARTIAL_LOOP_PARENT_SUBMITTED_ONCE"
        );
    }
    json!({"kind":"constructed partial-loop second-send failure", "helper":"actual persist_held_parent_prefix",
        "retry":"only unsent suffix", "production_closed_channel_reopened":false})
}

async fn u6_pinned_monitor_keys(fx: &K2lgN) -> Vec<Value> {
    let state = fx.session.actor.state.lock().await;
    let Some(owner) = state.message_delivery.held_input.pin_owner else {
        return Vec::new();
    };
    let buffer = fx
        .session
        .actor
        .tool_context
        .monitor_event_buffer
        .as_ref()
        .unwrap();
    let view = buffer.release_view(owner, |_| false);
    view.iter().map(|(key, event)| json!({"key":key.as_u64(),"task_id":event.task_id,
        "owner":event.owner_session_id,"text_digest":native_content_digest(event.event_text.as_bytes())})).collect()
}

tokio::task_local! {
    static U6_ARM_STEPPERS: std::rc::Rc<std::cell::RefCell<Vec<U6TurnStepper>>>;
}

pub(super) fn u6_record_native_safe_point(
    point: &str,
    observed: Option<(bool, bool, bool)>,
    holding: bool,
    retaining: bool,
) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        let mut records = observations.borrow_mut();
        assert!(records.len() < 4096, "H6_R15_OBSERVATION_BOUND");
        let (live, unavailable, chat_closed) = observed.unwrap_or_default();
        records.push(json!({"stage":"native safe point", "point":point,
            "selected":observed.is_some(), "live":live, "unavailable":unavailable,
            "chat_closed":chat_closed, "holding":holding,
            "release_path":if holding { "hold" } else if retaining { "merged_release" } else { "unheld" }}));
    });
}

pub(super) fn u6_record_release_outcome(committed: bool) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        observations
            .borrow_mut()
            .push(json!({"stage":"merged release outcome",
            "release_path":if committed { "submitted" } else { "submit_failed" }}));
    });
}

fn u6_current_stepper() -> U6TurnStepper {
    U6_ARM_STEPPERS.with(|steppers| steppers.borrow().last().unwrap().clone())
}

/// How the R15-4 precondition may step the turn (H-K3U6a-R19 R19-3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum U6PreconditionStepping {
    /// A response was just released: no step while an owned response is still being drained,
    /// because the drainer persists its native evidence before `Completed` drops the ownership.
    HoldUntilDrained,
    Free,
}

async fn u6_readable_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    expected_live: bool,
    tag: &str,
) -> K2Checkpoint {
    u6_persisted_readable_checkpoint(
        fx,
        running,
        expected_live,
        tag,
        U6PreconditionStepping::Free,
    )
    .await
}

/// Persisted native read served in the session persistence actor's own queue, so the
/// precondition never holds the native disk locks a concurrent persist takes under a deadline.
async fn u6_persisted_native_read(
    fx: &K2lgN,
) -> Result<xai_chat_state::persistence::native::NativeRead, k2_history::NativeUnavailable> {
    let (respond_to, read) = tokio::sync::oneshot::channel();
    if fx
        .session
        .persistence
        .tx
        .send(PersistenceMsg::ReadNativeHistory { respond_to })
        .is_err()
    {
        return Err(k2_history::NativeUnavailable::Persistence);
    }
    read.await
        .unwrap_or(Err(k2_history::NativeUnavailable::Persistence))
}

/// Holds the stepped turn until the checkpoint is readable in the actor and equal on disk:
/// the turn is stepped only while no owned response is draining and the view is unreadable.
async fn u6_persisted_readable_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    expected_live: bool,
    tag: &str,
    stepping: U6PreconditionStepping,
) -> K2Checkpoint {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    let steps = std::cell::Cell::new(0u32);
    let drain_holds = std::cell::Cell::new(0u32);
    let persisted_reads = std::cell::Cell::new(0u32);
    let wait = async {
        loop {
            if stepping == U6PreconditionStepping::HoldUntilDrained
                && !fx.session.actor.turn_stream_drained.lock().is_empty()
            {
                drain_holds.set(drain_holds.get() + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            let actor = fx
                .session
                .actor
                .chat_state_handle
                .read_native_history()
                .await;
            if let Some(checkpoint) = actor
                .as_ref()
                .ok()
                .and_then(|read| read.checkpoint.as_ref())
                && k2lg_n_live(checkpoint) == expected_live
            {
                persisted_reads.set(persisted_reads.get() + 1);
                let disk = u6_persisted_native_read(fx).await;
                if disk
                    .as_ref()
                    .ok()
                    .and_then(|read| read.checkpoint.as_ref())
                    .is_some_and(|disk| {
                        disk.source == checkpoint.source && disk.ledger == checkpoint.ledger
                    })
                {
                    return Some((**checkpoint).clone());
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            if stepper.0.finished.get() {
                return None;
            }
            if stepper
                .0
                .wake
                .ready
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                steps.set(steps.get() + 1);
                assert!(steps.get() <= 512, "H6_R15_NATIVE_PRECONDITION_STEP_BOUND");
                if !stepper.step().await {
                    return None;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    };
    let result = tokio::time::timeout(K0_TIMEOUT, async {
        tokio::select! {
            biased;
            checkpoint = wait => (checkpoint, false),
            _ = running => (None, true),
        }
    })
    .await;
    if !was_paused {
        stepper.resume();
    }
    let bounded_wait_completed = result.is_ok();
    let (checkpoint, turn_finished) = result.unwrap_or((None, false));
    u6_observe(json!({"stage":"native checkpoint precondition", "tag":tag,
        "expected_live":expected_live, "readable":checkpoint.is_some(),
        "turn_finished":turn_finished, "bounded_wait_completed":bounded_wait_completed,
        "hold_until_drained":stepping == U6PreconditionStepping::HoldUntilDrained,
        "drain_holds":drain_holds.get(), "steps":steps.get(),
        "persisted_reads":persisted_reads.get()}));
    assert!(checkpoint.is_some(), "{tag}");
    checkpoint.unwrap()
}

async fn u6_release_after_checkpoint(
    fx: &K2lgN,
    running: &mut tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>,
    response: &xai_grok_test_support::InferenceExpectation,
    expected_live: bool,
    tag: &str,
) {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    response.release();
    u6_persisted_readable_checkpoint(
        fx,
        running,
        expected_live,
        tag,
        U6PreconditionStepping::HoldUntilDrained,
    )
    .await;
    if !was_paused {
        stepper.resume();
    }
}

/// Runs the committed K2 `reload()` at a quiescent point (H-K3U6a-R20 R20-3). The stepped turn is
/// held and nothing is draining, and a native read served in the persistence actor's own queue on
/// the product route has completed every earlier persist, so no chat or native persist can run
/// between reload()'s flush and its disk reads.
async fn u6_quiescent_reload(fx: &K2lgN, tag: &str) -> (K2Checkpoint, Vec<ConversationItem>) {
    let stepper = u6_current_stepper();
    let was_paused = stepper.0.paused.get();
    stepper.pause();
    let drain_holds = std::cell::Cell::new(0u32);
    let fence_rounds = std::cell::Cell::new(0u32);
    let settled = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            if !fx.session.actor.turn_stream_drained.lock().is_empty() {
                drain_holds.set(drain_holds.get() + 1);
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            fence_rounds.set(fence_rounds.get() + 1);
            assert!(fence_rounds.get() <= 512, "H6_R20_QUIESCENCE_ROUND_BOUND");
            let persisted = fx.log.lock().unwrap().len();
            let _ = fx.session.actor.chat_state_handle.get_conversation().await;
            let (respond_to, read) = tokio::sync::oneshot::channel();
            let sent = fx
                .session
                .actor
                .notifications
                .persistence_tx
                .send(PersistenceMsg::ReadNativeHistory { respond_to })
                .is_ok();
            if !sent || read.await.is_err() {
                return false;
            }
            if fx.session.actor.turn_stream_drained.lock().is_empty()
                && fx.log.lock().unwrap().len() == persisted
            {
                return true;
            }
        }
    })
    .await;
    let quiet = matches!(settled, Ok(true));
    u6_observe(json!({"stage":"R20 quiescent reload", "tag":tag,
        "turn_finished":stepper.0.finished.get(), "settled":quiet,
        "drain_holds":drain_holds.get(), "fence_rounds":fence_rounds.get()}));
    assert!(quiet, "{tag}");
    let reloaded = fx.session.reload().await;
    if !was_paused {
        stepper.resume();
    }
    reloaded
}

async fn u6_unavailable_view_hold() -> Value {
    const HUMAN: &str = "U6 R15 unavailable native view human";
    let mut fixture = u6_fixture(None, K2lgNOptions::default()).await;
    let holder = tokio::task::spawn_local(std::future::pending::<()>());
    let _stop = K0Abort(holder.abort_handle());
    u6_install_running(&mut fixture.fx, holder.abort_handle()).await;
    let fx = &fixture.fx;
    let read = fx
        .session
        .actor
        .chat_state_handle
        .read_native_history()
        .await;
    assert!(
        read.as_ref().is_ok_and(|read| read.checkpoint.is_none()),
        "H6_R15_UNAVAILABLE_VIEW_PRECONDITION"
    );
    assert!(
        fx.session
            .actor
            .chat_state_handle
            .get_sampling_config()
            .await
            .is_some(),
        "H6_R15_OPEN_CHAT_PRECONDITION"
    );
    k2lg_n_selected(fx, fx.session.actor.arm_native_hold_latch()).await;
    u6_interject(fx, HUMAN, Vec::new()).await;
    let resident = u6_resident_keys(&fx.session.actor);
    let delivered = k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await;
    let history = fx.session.actor.chat_state_handle.get_conversation().await;
    assert!(
        !delivered
            && !history
                .iter()
                .any(|item| item.text_content().contains(HUMAN)),
        "H6_R15_UNAVAILABLE_STAYS_HELD"
    );
    let key = {
        let state = fx.session.actor.state.lock().await;
        let ledger = &state.message_delivery.held_input;
        assert!(ledger.latch.is_some(), "H6_R15_LATCH_RETAINED");
        let entry = ledger.entries.iter().find(|entry| {
            matches!(&entry.body, super::interjection::HeldBody::Human(raw) if raw.text == HUMAN)
        }).expect("H6_R15_RAW_CUSTODY_RETAINED");
        assert_eq!(
            entry.key.as_u64(),
            resident[0]["key"].as_u64().unwrap(),
            "H6_R15_ARRIVAL_KEY_PRESERVED"
        );
        entry.key
    };
    assert!(
        fx.session.actor.pending_interjections.is_empty(),
        "H6_R15_OPTION_K_CUSTODY"
    );
    assert!(
        !k2lg_n_selected(fx, fx.session.actor.drain_interjections_at_safe_point()).await,
        "H6_R15_REPEATED_SAFE_POINT_STILL_HELD"
    );
    let state = fx.session.actor.state.lock().await;
    assert!(
        state
            .message_delivery
            .held_input
            .entries
            .iter()
            .any(|entry| entry.key == key),
        "H6_R15_NO_UNAVAILABLE_VIEW_RELEASE"
    );
    assert_eq!(
        u6_user_persists(&fixture.persistence_control, HUMAN),
        0,
        "H6_R15_NO_UNAVAILABLE_VIEW_PERSISTENCE"
    );
    assert_eq!(
        fx.server.messages_request_count(),
        0,
        "H6_R15_NO_PROVIDER_REQUEST"
    );
    json!({"native_view":"unavailable", "chat_open":true,
        "safe_point_only":true, "latch_setup":"constructed running binding via W05 setter",
        "human_producer":"SessionCommand::Interject", "original_key_preserved":true,
        "terminal_settlement_credited":false, "terminal_settlement_owner":"koxd-U6.6",
        "authority":"H-K3U6a-R15 R15-2/R15-3"})
}

pub(super) fn u6_record_commit_observation(
    observed: Option<(bool, bool, bool)>,
    native_hold: bool,
    excluded: bool,
) {
    let _ = U6_ARM_OBSERVATIONS.try_with(|observations| {
        let (live, unavailable, chat_closed) = observed.unwrap_or_default();
        observations.borrow_mut().push(json!({"stage":"merged commit recheck",
            "live":live, "unavailable":unavailable, "chat_closed":chat_closed,
            "native_hold":native_hold, "excluded":excluded,
            "release_path":if excluded { "owner_excluded" } else if native_hold { "hold" } else { "commit" }}));
    });
}

/// Join handle of an arm's U6 stepped turn (H-K3U6a-R24 R24-2).
type U6Running = tokio::task::JoinHandle<Result<TurnOutcome, acp::Error>>;

/// Names-only outcome of a stepped turn that ended before awaited product progress
/// (H-K3U6a-R24 R24-2): the local error code, the typed kind fields and every
/// `NativeUnavailable` variant the refusal names. Message text and values are never kept.
fn u6_turn_end_names(
    outcome: &Result<Result<TurnOutcome, acp::Error>, tokio::task::JoinError>,
) -> Value {
    let name = |data: Option<&Value>, key: &str| {
        data.and_then(|data| data.get(key))
            .and_then(Value::as_str)
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .map(str::to_owned)
    };
    match outcome {
        Err(join) => json!({"turn_finished":true, "result":"join_error",
            "join":if join.is_cancelled() { "cancelled" } else { "panicked" }}),
        Ok(Ok(_)) => json!({"turn_finished":true, "result":"ok"}),
        Ok(Err(error)) => {
            let data = error.data.as_ref();
            let message = data
                .and_then(|data| data.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut unavailable: Vec<&str> = Vec::new();
            for word in message.split(|c: char| !c.is_ascii_alphanumeric()) {
                if !unavailable.contains(&word)
                    && serde_json::from_value::<k2_history::NativeUnavailable>(json!(word)).is_ok()
                {
                    unavailable.push(word);
                }
            }
            json!({"turn_finished":true, "result":"error",
                "error_code":name(data, "code"), "error_kind":name(data, "error_kind"),
                "error_type":name(data, "error_type"), "native_unavailable":unavailable})
        }
    }
}

/// Fails `tag` because the stepped turn ended before the awaited product progress
/// (H-K3U6a-R24 R24-2), recording the turn's outcome by name only.
fn u6_turn_ended(
    outcome: Result<Result<TurnOutcome, acp::Error>, tokio::task::JoinError>,
    stage: &str,
    tag: &str,
    progress: Value,
) -> ! {
    u6_observe(
        json!({"stage":stage, "tag":tag, "turn_ended_before_progress":true,
        "turn":u6_turn_end_names(&outcome), "progress":progress}),
    );
    panic!("{tag}");
}

/// Fails `tag` because the unchanged K0 bound expired first; records names and counts only.
fn u6_bound_expired(running: &U6Running, stage: &str, tag: &str, progress: Value) -> ! {
    u6_observe(
        json!({"stage":stage, "tag":tag, "turn_ended_before_progress":false,
        "bounded_wait_completed":false, "turn_finished":running.is_finished(),
        "progress":progress}),
    );
    panic!("{tag}");
}

/// Receives both handoff client-tool entries under the unchanged K0 bound, raced against the
/// stepped turn's outcome (H-K3U6a-R24 R24-2).
async fn u6_client_dispatch(
    entries: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    running: &mut U6Running,
    capture: Value,
    stage: &str,
    tag: &str,
    ended_tag: &str,
) -> Vec<String> {
    let mut entered = Vec::new();
    let raced = tokio::time::timeout(K0_TIMEOUT, async {
        while entered.len() < 2 {
            tokio::select! {
                biased;
                entry = entries.recv() => {
                    entered.push(entry.expect("H6_REAL_CLIENT_ENTRY_CHANNEL_OPEN"));
                }
                result = &mut *running => return Some(result),
            }
        }
        None
    })
    .await;
    let progress = json!({"capture":capture, "client_entries":entered.clone(),
        "client_entry_count":entered.len()});
    match raced {
        Ok(None) => entered,
        Ok(Some(result)) => u6_turn_ended(result, stage, ended_tag, progress),
        Err(_) => u6_bound_expired(running, stage, tag, progress),
    }
}

/// Polls `progressed` under the unchanged K0 bound, raced against the stepped turn's outcome
/// (H-K3U6a-R24 R24-2). A finished turn is consumed only after a final re-check fails, so a
/// turn that ends right after its progress never fails here.
async fn u6_await_turn_progress<F, P>(
    running: &mut U6Running,
    stage: &str,
    tag: &str,
    ended_tag: &str,
    mut progressed: F,
) where
    F: FnMut() -> P,
    P: std::future::Future<Output = bool>,
{
    let raced = tokio::time::timeout(K0_TIMEOUT, async {
        loop {
            if progressed().await {
                return false;
            }
            if running.is_finished() {
                return !progressed().await;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    match raced {
        Ok(false) => {}
        Ok(true) => u6_turn_ended(running.await, stage, ended_tag, json!({})),
        Err(_) => u6_bound_expired(running, stage, tag, json!({})),
    }
}
