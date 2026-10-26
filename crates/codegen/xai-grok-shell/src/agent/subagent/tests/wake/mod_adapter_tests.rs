//! T8–T11: mod subagents through the real coordinator + `run_shell_child` dispatch.
use super::*;
use xai_grok_tools::implementations::grok_build::task::backend::{ChannelBackend, SubagentBackend};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    CoordinatorConfig, SubagentCoordinator,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    ActiveAgentMessageOperation, AgentMailboxMessage, AgentMailboxMessageKind, NativeAgentSpawn,
};

const MOCK_SIDECAR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../xai-grok-mods/tests/fixtures/mock-sidecar.mjs"
);

/// Writes a minimal `cursor` mod plugin and returns a registry with it enabled.
fn mod_plugin_registry(
    dir: &std::path::Path,
    trusted: bool,
) -> std::sync::Arc<xai_grok_agent::plugins::PluginRegistry> {
    use xai_grok_agent::plugins::TrustStore;
    use xai_grok_agent::plugins::discovery::{DiscoveryConfig, discover_plugins};
    let root = dir.join("cursor");
    std::fs::create_dir_all(root.join(".grok-plugin")).unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::create_dir_all(root.join("sidecar")).unwrap();
    std::fs::write(
        root.join(".grok-plugin/plugin.json"),
        r#"{"name":"cursor","version":"1.0.0","agents":["./agents/"]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("agents/composer-2.md"),
        "---\nname: composer-2\ndescription: mock cursor agent\nmodel: composer-2\n---\n",
    )
    .unwrap();
    std::fs::copy(MOCK_SIDECAR, root.join("sidecar/runner.mjs")).unwrap();
    std::fs::write(
        root.join("mod.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "namespace": "cursor",
            "runtime": {
                "command": "node",
                "args": ["sidecar/runner.mjs"],
                "timeoutSecs": 20,
                "gracefulKillMs": 100,
                "envAllowlist": ["PATH", "HOME"]
            },
            "capabilities": {
                "liveSteer": true, "sessionResume": true,
                "mcpBridge": false, "mcpInReadOnly": false
            }
        })
        .to_string(),
    )
    .unwrap();
    let config = DiscoveryConfig {
        cli_plugin_dirs: vec![root],
        ..Default::default()
    };
    let trust = TrustStore::load_from(dir.join("trust.json"));
    let mut discovered = discover_plugins(Some(dir), &config, &trust, true);
    discovered.retain(|p| p.manifest.name == "cursor");
    assert_eq!(discovered.len(), 1, "mock cursor plugin must be discovered");
    discovered[0].trusted = trusted;
    std::sync::Arc::new(xai_grok_agent::plugins::PluginRegistry::from_discovered(
        discovered,
        &[],
        &["cursor".to_owned()],
    ))
}

fn mod_ctx(
    server: &xai_grok_test_support::MockInferenceServer,
    meta_dir: &std::path::Path,
    registry: &std::sync::Arc<xai_grok_agent::plugins::PluginRegistry>,
    parent_cmd_tx: &mpsc::UnboundedSender<SessionCommand>,
) -> SubagentSpawnContext {
    let mut ctx = ctx_with_toggle(HashMap::new());
    configure_completion_harness(
        &mut ctx,
        server,
        RunShellChildHarnessConfig::new(meta_dir.to_path_buf(), InitialAttemptBehavior::Normal),
    );
    ctx.plugin_registry = Some(std::sync::Arc::clone(registry));
    ctx.parent_cmd_tx = Some(parent_cmd_tx.clone());
    ctx
}

fn mod_request(id: &str, prompt: &str) -> SubagentRequest {
    let mut request = auto_wake_test_request(id);
    request.subagent_type = "cursor:composer-2".into();
    request.prompt = prompt.into();
    request.parent_session_id = "setup-parent".into();
    request
}

fn count_updates(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>,
    needle: &str,
) -> usize {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|message| {
            matches!(
                message,
                xai_acp_lib::AcpClientMessage::ExtNotification(args)
                    if args.request.params.get().contains(needle)
            )
        })
        .count()
}

macro_rules! require_node {
    () => {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipping: node not on PATH");
            return;
        }
    };
}

#[tokio::test(flavor = "current_thread")]
async fn t8_task_mod_spawn_writes_meta_and_output_and_emits_notifications() {
    require_node!();
    xai_test_utils::require_git!();
    use xai_test_utils::git::seed_repo_with_remote;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let (repo, _remote) = seed_repo_with_remote(temp.path());
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let mut ctx = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            ctx.parent_cwd = repo;
            ctx.remote_settings = Some(crate::util::config::RemoteSettings {
                subagent_worktree_snapshot_enabled: Some(true),
                ..Default::default()
            });
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([ctx], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            let mut req = mod_request(&id, "hello");
            req.runtime_overrides.isolation =
                Some(xai_tool_types::SubagentIsolationMode::Worktree);
            let result = backend.spawn(req, None).await.expect("spawn");
            assert!(result.success, "mod spawn failed: {:?}", result.error);
            assert_eq!(result.output.as_ref(), format!("reply:hello:vsid=v-{id}"));

            let persisted: SubagentMeta = serde_json::from_str(
                &std::fs::read_to_string(meta_dir.join("meta.json")).expect("meta"),
            )
            .unwrap();
            assert_eq!(persisted.status, "completed");
            assert_eq!(persisted.effective_model_id.as_deref(), Some("cursor/composer-2"));
            assert!(persisted.snapshot_ref.is_some());
            assert_eq!(
                read_subagent_output(&meta_dir).as_deref(),
                Some(result.output.as_ref())
            );
            let updates: Vec<_> = std::iter::from_fn(|| gateway_rx.try_recv().ok()).collect();
            let count = |needle: &str| {
                updates
                    .iter()
                    .filter(|m| matches!(m, xai_acp_lib::AcpClientMessage::ExtNotification(a)
                        if a.request.params.get().contains(needle)))
                    .count()
            };
            assert_eq!(count("\"subagent_spawned\""), 1);
            assert_eq!(count("\"subagent_finished\""), 1);
            assert!(count("\"subagent_progress\"") >= 1);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t9_v2_spawn_folds_message_steers_and_wakes_with_deferred_start() {
    require_node!();
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let first = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let wake = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([first, wake], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            // (a) v2 spawn: native_agent.message body is folded into run.start.prompt.
            let mut request = mod_request(&id, "WAIT_FOR_ACTIVE_MSG");
            request.runtime_overrides.native_agent = Some(NativeAgentSpawn {
                task_name: "worker".into(),
                message: Some(AgentMailboxMessage {
                    message_id: "m-0".into(),
                    team_scope_id: "setup-parent".into(),
                    from_agent_id: "setup-parent".into(),
                    to_agent_id: id.clone(),
                    kind: AgentMailboxMessageKind::NativeMessage,
                    body: "inbox body".into(),
                    created_at_ms: 1,
                    native: None,
                }),
                ..Default::default()
            });
            let spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move { backend.spawn(request, None).await }
            });

            // (b) live steer while running ⇒ Accepted (ChildControl returned Admitted).
            let mut steered = None;
            for _ in 0..200 {
                let outcome = backend
                    .send_active_message(
                        ActiveAgentMessageRequest::try_new(&id, "steer-1").expect("request"),
                    )
                    .await;
                if matches!(outcome, ActiveAgentMessageOutcome::Accepted { .. }) {
                    steered = Some(outcome);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert!(steered.is_some(), "steer was never admitted");
            let first_result = spawn.await.unwrap().expect("spawn");
            assert!(first_result.success, "{:?}", first_result.error);
            assert!(
                first_result
                    .output
                    .starts_with("reply:WAIT_FOR_ACTIVE_MSG\n\ninbox body:vsid="),
                "prompt fold missing: {}",
                first_result.output
            );
            assert!(first_result.output.contains("|steered:"));
            while gateway_rx.try_recv().is_ok() {}

            // (c) idle wake ⇒ deferred start settles, resume key = request.id ⇒ same vendor session.
            let outcome = backend
                .send_active_message(
                    ActiveAgentMessageRequest::try_new(&id, "continue").expect("wake request"),
                )
                .await;
            assert!(
                matches!(outcome, ActiveAgentMessageOutcome::Accepted { .. }),
                "wake not accepted: {outcome:?}"
            );
            let woke = backend.query(&id, true, Some(20_000)).await.expect("woken snapshot");
            match woke.status {
                SubagentSnapshotStatus::Completed { ref output, .. } => {
                    assert_eq!(output.to_string(), format!("reply:continue:vsid=v-{id}"));
                }
                other => panic!("expected completed wake, got {other:?}"),
            }
            assert_eq!(count_updates(&mut gateway_rx, "\"subagent_spawned\""), 1);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t10_rhai_retry_resumes_vendor_session_id() {
    require_node!();
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), true);
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let a = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let b = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, _gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([a, b], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");

            // Attempt 1: sidecar returns invalid JSON (what Rhai's agent_json would reject).
            let first_id = uuid::Uuid::now_v7().to_string();
            let first = backend
                .spawn(mod_request(&first_id, "JSON_RETRY"), None)
                .await
                .expect("first");
            assert_eq!(first.output.as_ref(), "not-json");
            assert!(serde_json::from_str::<serde_json::Value>(&first.output).is_err());

            // Attempt 2: the Rhai retry path re-spawns with resume_from = first id.
            let retry_id = uuid::Uuid::now_v7().to_string();
            let mut retry = mod_request(&retry_id, "JSON_RETRY");
            retry.resume_from = Some(first_id.clone());
            let second = backend.spawn(retry, None).await.expect("retry");
            let parsed: serde_json::Value =
                serde_json::from_str(&second.output).expect("retry output is valid JSON");
            assert_eq!(parsed["vsid"], format!("v-{first_id}"));

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn t11_untrusted_mod_plugin_fails_closed_without_spawning() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let meta_dir = temp.path().join("meta");
            let registry = mod_plugin_registry(temp.path(), false);
            // Make any accidental spawn observable: replace the sidecar with a marker writer.
            let marker = temp.path().join("spawned.marker");
            std::fs::write(
                temp.path().join("cursor/sidecar/runner.mjs"),
                format!(
                    "require('node:fs').writeFileSync({:?}, 'x');",
                    marker.to_string_lossy()
                ),
            )
            .unwrap();
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));
            let ctx = mod_ctx(&server, &meta_dir, &registry, &parent_cmd_tx);
            let (gateway, mut gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new([ctx], false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");
            let id = uuid::Uuid::now_v7().to_string();

            let result = backend.spawn(mod_request(&id, "hello"), None).await.expect("spawn");
            assert!(!result.success);
            assert!(
                result.error.as_deref().unwrap_or_default().contains("not trusted"),
                "unexpected error: {:?}",
                result.error
            );
            assert!(!marker.exists(), "untrusted mod must not start a process");
            assert!(!meta_dir.join("meta.json").exists());
            assert_eq!(count_updates(&mut gateway_rx, "\"subagent_spawned\""), 0);

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}

/// Stage `cursor-subagents` with the stub SDK so the real `grok-runner.mjs`
/// speaks protocol v1 without contacting Cursor.
fn stage_cursor_subagents(dest: &std::path::Path) -> std::path::PathBuf {
    let src = dunce::canonicalize(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../cursor-subagents"),
    )
    .expect("cursor-subagents checkout");
    for rel in [".grok-plugin", "grok", "hooks", "types", "sidecar"] {
        let from = src.join(rel);
        let status = std::process::Command::new("cp")
            .args([
                "-a",
                from.to_str().expect("utf8 path"),
                dest.to_str().expect("utf8 path"),
            ])
            .status()
            .expect("cp");
        assert!(status.success(), "failed to copy {rel}");
    }
    std::fs::copy(src.join("mod.json"), dest.join("mod.json")).unwrap();
    // Keep a hung sidecar from blocking the suite for the production 1800s.
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
    assert!(
        stub.contains("SCHEMA_RETRY"),
        "stub fixture no longer has the answer line the test patches"
    );
    std::fs::write(sdk_dir.join("index.mjs"), stub).unwrap();
    sdk_dir
}

fn cursor_subagents_registry(
    plugin_root: &std::path::Path,
    trust_dir: &std::path::Path,
) -> std::sync::Arc<xai_grok_agent::plugins::PluginRegistry> {
    use xai_grok_agent::plugins::TrustStore;
    use xai_grok_agent::plugins::discovery::{DiscoveryConfig, discover_plugins};
    let config = DiscoveryConfig {
        cli_plugin_dirs: vec![plugin_root.to_path_buf()],
        ..Default::default()
    };
    let trust = TrustStore::load_from(trust_dir.join("trust.json"));
    let mut discovered = discover_plugins(Some(trust_dir), &config, &trust, true);
    discovered.retain(|p| p.manifest.name == "cursor-subagents");
    assert_eq!(
        discovered.len(),
        1,
        "cursor-subagents plugin must be discovered from --plugin-dir"
    );
    discovered[0].trusted = true;
    std::sync::Arc::new(xai_grok_agent::plugins::PluginRegistry::from_discovered(
        discovered,
        &[],
        &["cursor-subagents".to_owned()],
    ))
}

fn calls_log(sdk_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let path = sdk_dir.join("calls.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Qualified dispatch, live steer, queue drain, idle wake, and schema-retry
/// resume against the real `cursor-subagents` sidecar (stub SDK, no network).
#[tokio::test(flavor = "current_thread")]
async fn e2e_cursor_subagents_registry_steer_queue_wake_and_schema_retry() {
    require_node!();
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let plugin_root = temp.path().join("cursor-subagents");
            std::fs::create_dir_all(&plugin_root).unwrap();
            let sdk_dir = stage_cursor_subagents(&plugin_root);
            let grok_home = temp.path().join("grok-home");
            std::fs::create_dir_all(&grok_home).unwrap();
            // This test is the only reader of these vars; mod-adapter tests run single-threaded.
            unsafe {
                std::env::set_var("GROK_HOME", &grok_home);
                std::env::set_var("CURSOR_API_KEY", "test-cursor-key");
            }
            let registry = cursor_subagents_registry(&plugin_root, temp.path());
            let server = xai_grok_test_support::MockInferenceServer::start().await.unwrap();
            let (parent_cmd_tx, parent_cmd_rx) = mpsc::unbounded_channel();
            let usage_ack = tokio::task::spawn_local(acknowledge_parent_usage(parent_cmd_rx));

            let gpt_meta = temp.path().join("meta-gpt");
            let grok_meta = temp.path().join("meta-grok");
            let slow_meta = temp.path().join("meta-slow");
            let schema_meta = temp.path().join("meta-schema");
            let schema_retry_meta = temp.path().join("meta-schema-retry");
            let contexts = [
                mod_ctx(&server, &gpt_meta, &registry, &parent_cmd_tx),
                mod_ctx(&server, &grok_meta, &registry, &parent_cmd_tx),
                mod_ctx(&server, &slow_meta, &registry, &parent_cmd_tx),
                mod_ctx(&server, &slow_meta, &registry, &parent_cmd_tx),
                mod_ctx(&server, &schema_meta, &registry, &parent_cmd_tx),
                mod_ctx(&server, &schema_retry_meta, &registry, &parent_cmd_tx),
            ];
            let (gateway, _gateway_rx) = test_gateway_with_receiver();
            let (command_tx, command_rx) = SubagentCoordinator::<RunShellChildTestRunner>::channel();
            let coordinator = tokio::task::spawn_local(
                SubagentCoordinator::from_channel(
                    command_rx,
                    RunShellChildTestRunner::new(contexts, false, gateway),
                    CoordinatorConfig::default(),
                )
                .run(),
            );
            let backend = ChannelBackend::for_coordinator_session(command_tx, "setup-parent");

            let gpt_id = uuid::Uuid::now_v7().to_string();
            let mut gpt = mod_request(&gpt_id, "dispatch GPT56_END_TOKEN_1234567890");
            gpt.subagent_type = "cursor-subagents:gpt-5-6-sol".into();
            let gpt_result = backend.spawn(gpt, None).await.expect("gpt spawn");
            assert!(gpt_result.success, "gpt failed: {:?}", gpt_result.error);
            assert!(
                gpt_result.output.contains("GPT56_END_TOKEN_1234567890"),
                "gpt output: {}",
                gpt_result.output
            );
            let gpt_meta_json: SubagentMeta = serde_json::from_str(
                &std::fs::read_to_string(gpt_meta.join("meta.json")).expect("gpt meta"),
            )
            .unwrap();
            assert_eq!(gpt_meta_json.status, "completed");
            assert_eq!(
                gpt_meta_json.effective_model_id.as_deref(),
                Some("cursor-subagents/gpt-5.6-sol")
            );

            let grok_id = uuid::Uuid::now_v7().to_string();
            let mut grok = mod_request(&grok_id, "dispatch GROK47_END_TOKEN_0987654321");
            grok.subagent_type = "cursor-subagents:grok-4-7".into();
            let grok_result = backend.spawn(grok, None).await.expect("grok spawn");
            assert!(grok_result.success, "grok failed: {:?}", grok_result.error);
            assert!(
                grok_result.output.contains("GROK47_END_TOKEN_0987654321"),
                "grok output: {}",
                grok_result.output
            );

            let slow_id = uuid::Uuid::now_v7().to_string();
            let mut slow = mod_request(&slow_id, "SLOW hold this turn open");
            slow.subagent_type = "cursor-subagents:gpt-5-6-sol".into();
            let slow_spawn = tokio::task::spawn_local({
                let backend = backend.clone();
                async move { backend.spawn(slow, None).await }
            });
            let mut saw_send = false;
            for _ in 0..100 {
                if calls_log(&sdk_dir).iter().any(|c| c["op"] == "send") {
                    saw_send = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(
                saw_send,
                "sidecar never called Agent.send: {:?}",
                calls_log(&sdk_dir)
            );
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let queued = backend
                .send_active_message(
                    ActiveAgentMessageRequest::try_new(&slow_id, "QUEUE_TOKEN_ABCDEFGHIJ")
                        .expect("queue request"),
                )
                .await;
            assert!(
                matches!(queued, ActiveAgentMessageOutcome::Accepted { .. }),
                "queue not accepted: {queued:?}"
            );
            let steered = backend
                .send_active_message(
                    ActiveAgentMessageRequest::try_new_with_operation(
                        &slow_id,
                        "STEER_TOKEN_ABCDEFGHIJ",
                        ActiveAgentMessageOperation::Steer,
                    )
                    .expect("steer request"),
                )
                .await;
            assert!(
                matches!(steered, ActiveAgentMessageOutcome::Accepted { .. }),
                "steer not accepted: {steered:?}"
            );
            let slow_result = slow_spawn.await.unwrap().expect("slow spawn");
            assert!(slow_result.success, "slow failed: {:?}", slow_result.error);
            assert!(
                slow_result.output.contains("QUEUE_TOKEN_ABCDEFGHIJ"),
                "queue drain missing from output: {}",
                slow_result.output
            );
            assert!(
                calls_log(&sdk_dir).iter().any(|c| c["op"] == "steer"),
                "expected a live steer call, got {:?}",
                calls_log(&sdk_dir)
            );

            let wake = backend
                .send_active_message(
                    ActiveAgentMessageRequest::try_new(&slow_id, "WAKE_TOKEN_ABCDEFGHIJKL")
                        .expect("wake"),
                )
                .await;
            assert!(
                matches!(wake, ActiveAgentMessageOutcome::Accepted { .. }),
                "wake not accepted: {wake:?}"
            );
            let woke = backend
                .query(&slow_id, true, Some(20_000))
                .await
                .expect("woken snapshot");
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
                calls_log(&sdk_dir).iter().any(|c| c["op"] == "resume"),
                "wake did not resume the vendor session: {:?}",
                calls_log(&sdk_dir)
            );

            let first_id = uuid::Uuid::now_v7().to_string();
            let mut first = mod_request(&first_id, "SCHEMA_RETRY first attempt");
            first.subagent_type = "cursor-subagents:grok-4-7".into();
            let first_result = backend.spawn(first, None).await.expect("schema first");
            assert!(first_result.success, "{:?}", first_result.error);
            assert!(
                serde_json::from_str::<serde_json::Value>(first_result.output.as_ref()).is_err(),
                "first schema output should be invalid JSON: {}",
                first_result.output
            );
            let created = calls_log(&sdk_dir)
                .iter()
                .rev()
                .find(|c| c["op"] == "create")
                .and_then(|c| c["agentId"].as_str().map(str::to_owned))
                .expect("create call");
            let retry_id = uuid::Uuid::now_v7().to_string();
            let mut retry = mod_request(&retry_id, "SCHEMA_RETRY second attempt");
            retry.subagent_type = "cursor-subagents:grok-4-7".into();
            retry.resume_from = Some(first_id);
            let second = backend.spawn(retry, None).await.expect("schema retry");
            assert!(second.success, "{:?}", second.error);
            let parsed: serde_json::Value =
                serde_json::from_str(&second.output).expect("retry output is JSON");
            assert_eq!(parsed["ok"].as_bool(), Some(true));
            assert_eq!(parsed["agentId"].as_str(), Some(created.as_str()));
            assert!(
                calls_log(&sdk_dir).iter().any(|c| {
                    c["op"] == "resume" && c["agentId"].as_str() == Some(created.as_str())
                }),
                "retry did not resume {created}: {:?}",
                calls_log(&sdk_dir)
            );

            drop(backend);
            coordinator.await.unwrap();
            usage_ack.abort();
        })
        .await;
}
