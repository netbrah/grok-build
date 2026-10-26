//! Drives the real `cursor-subagents` sidecar (`grok-runner.mjs`) through
//! `SubagentCoordinator` + `ModChildControl`. The SDK is the repo stub, so
//! this never contacts Cursor. PluginRegistry dispatch itself lives in the
//! shell `mod_adapter_tests` e2e; this crate is the path that links here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use xai_grok_mods::{
    BorrowedSendFuture, ModChildControl, ModConfig, ModHost, ModRunInput, ModSessionStore,
    ModStarted, VendorToolActivity, run_mod_child_with_store, select_resume_key,
};
use xai_grok_tools::implementations::grok_build::task::backend::{
    ChannelBackend, SubagentBackend,
};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ChildCompletion, ChildRunOutput, ChildRunRequest, ChildRunner, CoordinatorConfig,
    LocalBoxFuture, StartedChild, SubagentCoordinator, SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    ActiveAgentMessageOperation, ActiveAgentMessageOutcome, ActiveAgentMessageRequest,
    SubagentDescribeOutcome, SubagentOwner, SubagentRequest, SubagentResult,
    SubagentSnapshotStatus, SubagentValidateTypeOutcome,
};

fn stage_cursor_subagents(dest: &Path) -> PathBuf {
    let src = dunce::canonicalize(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../cursor-subagents"),
    )
    .expect("cursor-subagents checkout");
    for rel in [".grok-plugin", "grok", "hooks", "types", "sidecar"] {
        let status = std::process::Command::new("cp")
            .args(["-a", src.join(rel).to_str().unwrap(), dest.to_str().unwrap()])
            .status()
            .expect("cp");
        assert!(status.success(), "failed to copy {rel}");
    }
    std::fs::copy(src.join("mod.json"), dest.join("mod.json")).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dest.join("mod.json")).unwrap()).unwrap();
    manifest["runtime"]["timeoutSecs"] = serde_json::json!(20);
    std::fs::write(
        dest.join("mod.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let sdk_dir = dest.join("sidecar/node_modules/@cursor/sdk");
    std::fs::create_dir_all(&sdk_dir).unwrap();
    std::fs::write(
        sdk_dir.join("package.json"),
        r#"{"name":"@cursor/sdk","version":"1.0.38","type":"module","main":"./index.mjs"}"#,
    )
    .unwrap();
    let stub = std::fs::read_to_string(src.join("tests/grok/fixtures/stub-sdk.mjs")).unwrap();
    let stub = stub.replacen(
        "let out = `answer:${text.slice(-40)}`",
        "let out = text.includes('SCHEMA_RETRY')\n          ? (agent.resumed ? JSON.stringify({ ok: true, agentId: agent.agentId }) : 'not-json')\n          : `answer:${text.slice(-40)}`",
        1,
    );
    assert!(stub.contains("SCHEMA_RETRY"), "stub answer line drifted");
    std::fs::write(sdk_dir.join("index.mjs"), stub).unwrap();
    sdk_dir
}

fn calls_log(sdk_dir: &Path) -> Vec<serde_json::Value> {
    let Ok(raw) = std::fs::read_to_string(sdk_dir.join("calls.jsonl")) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn model_id(call: &serde_json::Value) -> Option<&str> {
    call["model"]
        .as_str()
        .or_else(|| call["model"]["id"].as_str())
}

struct Seen {
    models: Mutex<HashMap<String, String>>,
    vendors: Mutex<HashMap<String, String>>,
}

struct Host {
    reporter: xai_grok_tools::implementations::grok_build::task::coordinator::ChildReporter<
        ModChildControl,
    >,
    id: String,
    cwd: String,
    resumed_from: Option<String>,
    seen: Arc<Seen>,
}

impl ModHost for Host {
    fn on_started<'a>(
        &'a self,
        started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool> {
        self.seen
            .models
            .lock()
            .unwrap()
            .insert(self.id.clone(), started.effective_model.clone());
        self.seen
            .vendors
            .lock()
            .unwrap()
            .insert(self.id.clone(), started.vendor_session_id.clone());
        let child = StartedChild {
            child_session_id: self.id.clone(),
            persona: None,
            resumed_from: self.resumed_from.clone(),
            child_cwd: self.cwd.clone(),
            worktree_path: None,
            effective_model_id: format!("cursor-subagents/{}", started.effective_model),
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

#[derive(Clone)]
struct Runner {
    validated: xai_grok_mods::ValidatedMod,
    store: ModSessionStore,
    cwd: PathBuf,
    seen: Arc<Seen>,
}

impl ChildRunner for Runner {
    type Control = ModChildControl;
    type CompletionData = ();
    type RunFuture = LocalBoxFuture<ChildRunOutput<()>>;
    type ValidateFuture = LocalBoxFuture<SubagentValidateTypeOutcome>;
    type DescribeFuture = LocalBoxFuture<SubagentDescribeOutcome>;

    fn run(&self, run: ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let validated = self.validated.clone();
        let store = self.store.clone();
        let cwd = self.cwd.clone();
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            let is_wake = run.wake_origin.is_some();
            let resume_key = select_resume_key(
                &run.request.id,
                run.request.resume_from.as_deref(),
                is_wake,
            );
            let vendor_model = run
                .request
                .subagent_type
                .split_once(':')
                .map(|(_, model)| model)
                .unwrap_or("composer-2")
                .to_owned();
            let host = Arc::new(Host {
                reporter: run.reporter,
                id: run.request.id.clone(),
                cwd: cwd.to_string_lossy().into_owned(),
                resumed_from: run.request.resume_from.clone(),
                seen,
            });
            let out = run_mod_child_with_store(
                ModRunInput {
                    validated,
                    subagent_id: run.request.id.clone(),
                    parent_session_id: run.request.parent_session_id.clone(),
                    subagent_type: run.request.subagent_type.clone(),
                    agent_name: vendor_model.clone(),
                    vendor_model,
                    cwd,
                    prompt: run.request.prompt.clone(),
                    system_prompt: None,
                    resume_key,
                    read_only: false,
                    reasoning_effort: Some("max".into()),
                    bridged_tools: Vec::new(),
                    cancellation: run.cancellation,
                },
                host,
                &store,
            )
            .await;
            ChildRunOutput {
                result: SubagentResult {
                    success: out.error.is_none() && !out.cancelled,
                    output: Arc::from(out.final_text),
                    error: out.error,
                    subagent_id: run.request.id.clone(),
                    child_session_id: run.request.id,
                    turns: out.turns,
                    tool_calls: out.tool_calls,
                    warnings: out.warnings,
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
        true
    }
}

fn request(id: &str, prompt: &str, subagent_type: &str) -> SubagentRequest {
    SubagentRequest {
        id: id.into(),
        prompt: prompt.into(),
        description: "e2e".into(),
        subagent_type: subagent_type.into(),
        parent_session_id: "setup-parent".into(),
        parent_prompt_id: None,
        resume_from: None,
        cwd: None,
        runtime_overrides: Default::default(),
        run_in_background: false,
        surface_completion: true,
        await_to_completion: false,
        fork_context: false,
        context: Default::default(),
        owner: SubagentOwner::Task,
        cancel_token: CancellationToken::new(),
        spawn_root: xai_grok_tools::implementations::grok_build::task::types::SpawnRootSpan::new(
            tracing::Span::none(),
        ),
    }
}

async fn admit(
    backend: &ChannelBackend,
    id: &str,
    text: &str,
    operation: ActiveAgentMessageOperation,
) -> ActiveAgentMessageOutcome {
    let mut last = ActiveAgentMessageOutcome::NotActiveOrFinalizing;
    for _ in 0..100 {
        let req = ActiveAgentMessageRequest::try_new_with_operation(id, text, operation)
            .expect("message");
        last = backend.send_active_message(req).await;
        if matches!(last, ActiveAgentMessageOutcome::Accepted { .. }) {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    last
}

#[tokio::test(flavor = "current_thread")]
async fn cursor_subagents_steer_queue_wake_and_schema_retry() {
    let node = std::process::Command::new("node").arg("--version").output();
    if node.is_err() {
        eprintln!("skipping: node not on PATH");
        return;
    }

    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let plugin_root = temp.path().join("cursor-subagents");
            std::fs::create_dir_all(&plugin_root).unwrap();
            let sdk_dir = stage_cursor_subagents(&plugin_root);
            let grok_home = temp.path().join("grok-home");
            std::fs::create_dir_all(&grok_home).unwrap();
            unsafe {
                std::env::set_var("GROK_HOME", &grok_home);
                std::env::set_var("CURSOR_API_KEY", "test-cursor-key");
            }
            let validated = ModConfig::load("cursor-subagents", &plugin_root).expect("mod.json");
            let store = ModSessionStore::new(temp.path().join("sessions"));
            let seen = Arc::new(Seen {
                models: Mutex::new(HashMap::new()),
                vendors: Mutex::new(HashMap::new()),
            });
            let (cmd_tx, cmd_rx) = SubagentCoordinator::<Runner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    cmd_rx,
                    Runner {
                        validated,
                        store,
                        cwd: plugin_root.clone(),
                        seen: Arc::clone(&seen),
                    },
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(cmd_tx, "setup-parent");

            let gpt = backend
                .spawn(
                    request(
                        "sub-gpt",
                        "dispatch GPT56_END_TOKEN_1234567890",
                        "cursor-subagents:gpt-5.6-sol",
                    ),
                    None,
                )
                .await
                .expect("gpt spawn");
            assert!(gpt.success, "gpt failed: {:?} {:?}", gpt.error, gpt.warnings);
            assert!(
                gpt.output.contains("GPT56_END_TOKEN_1234567890"),
                "gpt output: {}",
                gpt.output
            );
            assert!(
                calls_log(&sdk_dir).iter().any(|c| {
                    c["op"] == "create" && model_id(c) == Some("gpt-5.6-sol") && c["hasKey"] == true
                }),
                "gpt create: {:?}",
                calls_log(&sdk_dir)
            );
            assert!(
                seen.models
                    .lock()
                    .unwrap()
                    .get("sub-gpt")
                    .is_some_and(|m| m.contains("gpt-5.6-sol")),
                "effective model: {:?}",
                seen.models.lock().unwrap()
            );

            let grok = backend
                .spawn(
                    request(
                        "sub-grok",
                        "dispatch GROK47_END_TOKEN_0987654321",
                        "cursor-subagents:grok-4.7",
                    ),
                    None,
                )
                .await
                .expect("grok spawn");
            assert!(
                grok.success,
                "grok failed: {:?} {:?}",
                grok.error,
                grok.warnings
            );
            assert!(
                grok.output.contains("GROK47_END_TOKEN_0987654321"),
                "grok output: {}",
                grok.output
            );
            assert!(
                calls_log(&sdk_dir)
                    .iter()
                    .any(|c| c["op"] == "create" && model_id(c) == Some("grok-4.7")),
                "grok create: {:?}",
                calls_log(&sdk_dir)
            );

            let slow_spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move {
                    backend
                        .spawn(
                            request(
                                "sub-slow",
                                "SLOW hold this turn open",
                                "cursor-subagents:gpt-5.6-sol",
                            ),
                            None,
                        )
                        .await
                }
            });
            let mut saw_send = false;
            for _ in 0..100 {
                if calls_log(&sdk_dir).iter().any(|c| c["op"] == "send") {
                    saw_send = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(saw_send, "no Agent.send: {:?}", calls_log(&sdk_dir));
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;

            let queued = admit(
                &backend,
                "sub-slow",
                "QUEUE_TOKEN_ABCDEFGHIJ",
                ActiveAgentMessageOperation::Queue,
            )
            .await;
            assert!(
                matches!(queued, ActiveAgentMessageOutcome::Accepted { .. }),
                "queue not accepted: {queued:?}"
            );
            let steered = admit(
                &backend,
                "sub-slow",
                "STEER_TOKEN_ABCDEFGHIJ",
                ActiveAgentMessageOperation::Steer,
            )
            .await;
            assert!(
                matches!(steered, ActiveAgentMessageOutcome::Accepted { .. }),
                "steer not accepted: {steered:?}"
            );
            let slow = slow_spawn.await.unwrap().expect("slow spawn");
            assert!(slow.success, "slow failed: {:?} {:?}", slow.error, slow.warnings);
            assert!(
                slow.output.contains("QUEUE_TOKEN_ABCDEFGHIJ"),
                "queue drain missing: {}",
                slow.output
            );
            assert!(
                calls_log(&sdk_dir).iter().any(|c| c["op"] == "steer"),
                "no live steer: {:?}",
                calls_log(&sdk_dir)
            );

            let slow_vendor = seen
                .vendors
                .lock()
                .unwrap()
                .get("sub-slow")
                .cloned()
                .expect("slow vendor id");
            let wake = admit(
                &backend,
                "sub-slow",
                "WAKE_TOKEN_ABCDEFGHIJKL",
                ActiveAgentMessageOperation::Queue,
            )
            .await;
            assert!(
                matches!(wake, ActiveAgentMessageOutcome::Accepted { .. }),
                "wake not accepted: {wake:?}"
            );
            let woke = backend
                .query("sub-slow", true, Some(20_000))
                .await
                .expect("wake snapshot");
            match woke.status {
                SubagentSnapshotStatus::Completed { ref output, .. } => {
                    assert!(
                        output.contains("WAKE_TOKEN_ABCDEFGHIJKL"),
                        "wake output: {output}"
                    );
                }
                other => panic!("expected completed wake, got {other:?}"),
            }
            assert!(
                calls_log(&sdk_dir).iter().any(|c| {
                    c["op"] == "resume" && c["agentId"].as_str() == Some(slow_vendor.as_str())
                }),
                "wake did not resume {slow_vendor}: {:?}",
                calls_log(&sdk_dir)
            );

            let first = backend
                .spawn(
                    request(
                        "sub-schema",
                        "SCHEMA_RETRY first attempt",
                        "cursor-subagents:grok-4.7",
                    ),
                    None,
                )
                .await
                .expect("schema first");
            assert!(first.success, "{:?} {:?}", first.error, first.warnings);
            assert!(
                serde_json::from_str::<serde_json::Value>(first.output.as_ref()).is_err(),
                "first schema output should not be JSON: {}",
                first.output
            );
            let created = seen
                .vendors
                .lock()
                .unwrap()
                .get("sub-schema")
                .cloned()
                .expect("schema vendor");
            let mut retry = request(
                "sub-schema-retry",
                "SCHEMA_RETRY second attempt",
                "cursor-subagents:grok-4.7",
            );
            retry.resume_from = Some("sub-schema".into());
            let second = backend.spawn(retry, None).await.expect("schema retry");
            assert!(second.success, "{:?} {:?}", second.error, second.warnings);
            let parsed: serde_json::Value =
                serde_json::from_str(&second.output).expect("retry output is JSON");
            assert_eq!(parsed["ok"].as_bool(), Some(true));
            assert_eq!(parsed["agentId"].as_str(), Some(created.as_str()));
            assert!(
                calls_log(&sdk_dir).iter().any(|c| {
                    c["op"] == "resume" && c["agentId"].as_str() == Some(created.as_str())
                }),
                "schema retry did not resume {created}: {:?}",
                calls_log(&sdk_dir)
            );

            drop(backend);
            coordinator.await.unwrap();
        })
        .await;
}
