use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use xai_grok_mods::{
    BorrowedSendFuture, ModChildControl, ModConfig, ModHost, ModRunInput, ModSessionStore,
    ModStarted, VendorToolActivity, run_mod_child_with_store,
};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ChildControl, SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    AgentMailboxMessage, AgentMailboxMessageKind,
};

struct RecordingHost {
    admit_start: bool,
    inject_on_finalizing: Option<String>,
    control: Mutex<Option<ModChildControl>>,
}

impl ModHost for RecordingHost {
    fn on_started<'a>(
        &'a self,
        _started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool> {
        *self.control.lock() = Some(control);
        let ok = self.admit_start;
        Box::pin(async move { ok })
    }

    fn on_progress(&self, _progress: &SubagentProgress, _tool: Option<&VendorToolActivity>) {}

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
        if let Some(ref body) = self.inject_on_finalizing {
            if let Some(ref ctrl) = *self.control.lock() {
                ctrl.deliver_followup(&AgentMailboxMessage {
                    message_id: "late-msg-1".into(),
                    team_scope_id: "scope".into(),
                    from_agent_id: "/root".into(),
                    to_agent_id: "/root/worker".into(),
                    kind: AgentMailboxMessageKind::Message,
                    body: body.clone(),
                    created_at_ms: 1,
                    native: None,
                });
            }
        }
        Box::pin(async { true })
    }

    fn call_mcp_tool<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _args: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
        Box::pin(async { Err("unused".into()) })
    }
}

fn setup_plugin(dir: &std::path::Path, extra_env: &[&str]) -> xai_grok_mods::ValidatedMod {
    let root = dunce::canonicalize(dir).unwrap();
    std::fs::create_dir_all(root.join("sidecar")).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-sidecar.mjs");
    std::fs::copy(&fixture, root.join("sidecar/runner.mjs")).unwrap();
    let mut allow = vec!["PATH", "HOME"];
    allow.extend_from_slice(extra_env);
    let manifest = serde_json::json!({
        "schemaVersion": 1,
        "namespace": "cursor",
        "runtime": {
            "command": "node",
            "args": ["sidecar/runner.mjs"],
            "timeoutSecs": 10,
            "gracefulKillMs": 100,
            "envAllowlist": allow,
        },
        "capabilities": {
            "liveSteer": true,
            "sessionResume": true,
            "mcpBridge": true,
            "mcpInReadOnly": false
        }
    });
    std::fs::write(root.join("mod.json"), manifest.to_string()).unwrap();
    ModConfig::load("cursor", &root).unwrap()
}

#[tokio::test]
async fn t6_normal_lifecycle_crash_and_cancel_reaps_grandchild() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = ModSessionStore::new(store_dir.path());
    let validated = setup_plugin(dir.path(), &[]);

    // 1. Normal completion + session persistence
    let host = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-1".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "hello".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host,
        &store,
    )
    .await;
    assert!(out.promoted);
    assert!(out.error.is_none());
    assert_eq!(out.final_text, "reply:hello:vsid=v-sub-1");
    assert_eq!(
        store.lookup("cursor", "sub-1", "parent-1").as_deref(),
        Some("v-sub-1")
    );

    // 2. Resume miss records warning and starts fresh
    let host_miss = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_miss = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-2".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "fresh".into(),
            system_prompt: None,
            resume_key: Some("missing-key".into()),
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_miss,
        &store,
    )
    .await;
    assert_eq!(
        out_miss.warnings,
        vec!["vendor session for missing-key not found; started fresh".to_string()]
    );

    // 3. on_started = false refuses promotion, sends cancel, terminates sidecar
    let host_refuse = Arc::new(RecordingHost {
        admit_start: false,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_refuse = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-refuse".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "refuse".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_refuse,
        &store,
    )
    .await;
    assert!(!out_refuse.promoted);
    assert!(out_refuse.cancelled);

    // 4. Unexpected EOF / crash synthesizes failure with stderr tail
    let host_crash = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: None,
        control: Mutex::new(None),
    });
    let out_crash = run_mod_child_with_store(
        ModRunInput {
            validated: validated.clone(),
            subagent_id: "sub-crash".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "CRASH".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host_crash,
        &store,
    )
    .await;
    assert!(out_crash.error.as_deref().unwrap_or_default().contains("fatal mock crash"));

    // 5. Hang + cancellation token escalates to SIGKILL of process group and reaps grandchild
    #[cfg(unix)]
    {
        let pid_file = dir.path().join("grandchild.pid");
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            cancel_clone.cancel();
        });
        let host_hang = Arc::new(RecordingHost {
            admit_start: true,
            inject_on_finalizing: None,
            control: Mutex::new(None),
        });
        let out_hang = run_mod_child_with_store(
            ModRunInput {
                validated: validated.clone(),
                subagent_id: "sub-hang".into(),
                parent_session_id: "parent-1".into(),
                subagent_type: "cursor:composer-2".into(),
                agent_name: "composer-2".into(),
                vendor_model: "composer-2".into(),
                cwd: dir.path().to_path_buf(),
                prompt: format!("HANG:{}", pid_file.display()),
                system_prompt: None,
                resume_key: None,
                read_only: false,
                reasoning_effort: None,
                bridged_tools: Vec::new(),
                cancellation: cancel,
            },
            host_hang,
            &store,
        )
        .await;
        assert!(out_hang.cancelled);
        if let Ok(raw_pid) = std::fs::read_to_string(&pid_file) {
            if let Ok(gpid) = raw_pid.trim().parse::<i32>() {
                let alive = unsafe { libc::kill(gpid, 0) == 0 };
                assert!(!alive, "grandchild pid {gpid} should be killed with process group");
            }
        }
    }
}

#[tokio::test]
async fn t7_idle_finalizing_drain_consumes_pending_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = ModSessionStore::new(store_dir.path());
    let validated = setup_plugin(dir.path(), &[]);

    let host = Arc::new(RecordingHost {
        admit_start: true,
        inject_on_finalizing: Some("extra-work".into()),
        control: Mutex::new(None),
    });
    let out = run_mod_child_with_store(
        ModRunInput {
            validated,
            subagent_id: "sub-drain".into(),
            parent_session_id: "parent-1".into(),
            subagent_type: "cursor:composer-2".into(),
            agent_name: "composer-2".into(),
            vendor_model: "composer-2".into(),
            cwd: dir.path().to_path_buf(),
            prompt: "initial".into(),
            system_prompt: None,
            resume_key: None,
            read_only: false,
            reasoning_effort: None,
            bridged_tools: Vec::new(),
            cancellation: CancellationToken::new(),
        },
        host,
        &store,
    )
    .await;
    assert!(out.error.is_none());
    assert_eq!(out.turns, 2);
    assert!(out.final_text.contains("|steered:followup:extra-work"));
}

#[tokio::test(flavor = "current_thread")]
async fn t2_coordinator_send_active_message_admits_and_drains() {
    use xai_grok_tools::implementations::grok_build::task::backend::{
        ChannelBackend, SubagentBackend,
    };
    use xai_grok_tools::implementations::grok_build::task::coordinator::{
        ChildCompletion, ChildRunOutput, ChildRunRequest, ChildRunner, CoordinatorConfig,
        LocalBoxFuture, StartedChild, SubagentCoordinator,
    };
    use xai_grok_tools::implementations::grok_build::task::types::{
        ActiveAgentMessageOutcome, ActiveAgentMessageRequest, SubagentDescribeOutcome,
        SubagentOwner, SubagentRequest, SubagentResult, SubagentValidateTypeOutcome,
    };

    #[derive(Clone)]
    struct ModTestRunner {
        validated: xai_grok_mods::ValidatedMod,
        store: ModSessionStore,
        cwd: PathBuf,
    }

    struct CoordinatorModHost {
        reporter: xai_grok_tools::implementations::grok_build::task::coordinator::ChildReporter<
            ModChildControl,
        >,
        id: String,
        cwd: String,
    }

    impl ModHost for CoordinatorModHost {
        fn on_started<'a>(
            &'a self,
            started: ModStarted,
            control: ModChildControl,
        ) -> BorrowedSendFuture<'a, bool> {
            let child = StartedChild {
                child_session_id: self.id.clone(),
                persona: None,
                resumed_from: None,
                child_cwd: self.cwd.clone(),
                worktree_path: None,
                effective_model_id: format!("cursor/{}", started.effective_model),
                definition_background: false,
                control,
            };
            Box::pin(async move { self.reporter.started(child).await })
        }
        fn on_progress(&self, _: &SubagentProgress, _: Option<&VendorToolActivity>) {}
        fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
            Box::pin(async move { self.reporter.finalizing().await })
        }
        fn call_mcp_tool<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: serde_json::Value,
        ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async { Err("unused".into()) })
        }
    }

    impl ChildRunner for ModTestRunner {
        type Control = ModChildControl;
        type CompletionData = ();
        type RunFuture = LocalBoxFuture<ChildRunOutput<()>>;
        type ValidateFuture = LocalBoxFuture<SubagentValidateTypeOutcome>;
        type DescribeFuture = LocalBoxFuture<SubagentDescribeOutcome>;

        fn run(&self, run: ChildRunRequest<Self::Control>) -> Self::RunFuture {
            let validated = self.validated.clone();
            let store = self.store.clone();
            let cwd = self.cwd.clone();
            Box::pin(async move {
                let host = Arc::new(CoordinatorModHost {
                    reporter: run.reporter,
                    id: run.request.id.clone(),
                    cwd: cwd.to_string_lossy().into_owned(),
                });
                let out = run_mod_child_with_store(
                    ModRunInput {
                        validated,
                        subagent_id: run.request.id.clone(),
                        parent_session_id: run.request.parent_session_id.clone(),
                        subagent_type: run.request.subagent_type.clone(),
                        agent_name: "composer-2".into(),
                        vendor_model: "composer-2".into(),
                        cwd,
                        prompt: run.request.prompt.clone(),
                        system_prompt: None,
                        resume_key: None,
                        read_only: false,
                        reasoning_effort: None,
                        bridged_tools: Vec::new(),
                        cancellation: run.cancellation,
                    },
                    host,
                    &store,
                )
                .await;
                ChildRunOutput {
                    result: SubagentResult {
                        success: out.error.is_none(),
                        output: Arc::from(out.final_text),
                        subagent_id: run.request.id.clone(),
                        child_session_id: run.request.id,
                        turns: out.turns,
                        tool_calls: out.tool_calls,
                        ..Default::default()
                    },
                    completion_data: (),
                    snapshot_ref: None,
                }
            })
        }
        fn validate_type(&self, _: String, _: String) -> Self::ValidateFuture {
            Box::pin(std::future::ready(SubagentValidateTypeOutcome::Ok))
        }
        fn describe_type(&self, _: String, _: Option<String>, _: String) -> Self::DescribeFuture {
            Box::pin(std::future::ready(SubagentDescribeOutcome::Unavailable))
        }
        fn on_completed(&self, _: ChildCompletion<()>, done: Box<dyn FnOnce() + Send>) {
            done();
        }
        fn supports_wake(&self) -> bool {
            false
        }
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let store_dir = tempfile::tempdir().unwrap();
            let store = ModSessionStore::new(store_dir.path());
            let validated = setup_plugin(dir.path(), &[]);
            let (cmd_tx, cmd_rx) = SubagentCoordinator::<ModTestRunner>::channel();
            let _coord = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    cmd_rx,
                    ModTestRunner {
                        validated,
                        store,
                        cwd: dir.path().to_path_buf(),
                    },
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(cmd_tx, "parent-1");
            let req = SubagentRequest {
                id: "sub-live-1".into(),
                prompt: "WAIT_FOR_ACTIVE_MSG".into(),
                description: "test".into(),
                subagent_type: "cursor:composer-2".into(),
                parent_session_id: "parent-1".into(),
                parent_prompt_id: None,
                resume_from: None,
                cwd: None,
                runtime_overrides: Default::default(),
                run_in_background: true,
                surface_completion: true,
                await_to_completion: false,
                fork_context: false,
                context: Default::default(),
                owner: SubagentOwner::Task,
                cancel_token: CancellationToken::new(),
                spawn_root: xai_grok_tools::implementations::grok_build::task::types::SpawnRootSpan::new(
                    tracing::Span::none(),
                ),
            };
            let spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move { backend.spawn(req, None).await }
            });
            // Poll until the child is promoted and the live control admits the message.
            let mut admitted = false;
            for _ in 0..200 {
                let msg_req =
                    ActiveAgentMessageRequest::try_new("sub-live-1", "live-steer-msg").unwrap();
                if matches!(
                    backend.send_active_message(msg_req).await,
                    ActiveAgentMessageOutcome::Accepted { .. }
                ) {
                    admitted = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(admitted, "live steer was never admitted through ModChildControl");
            let _ = spawn.await;
        })
        .await;
}
