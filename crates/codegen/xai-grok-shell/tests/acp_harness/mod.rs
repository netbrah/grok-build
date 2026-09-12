//! Drives a real in-process `MvpAgent` over ACP on duplex pipes.
//! This lives outside `tests/common/` because that compiles into every integration binary and would pull the transport stack into all of them.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use agent_client_protocol::{self as acp, Agent as _};
use serde_json::json;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing_subscriber::prelude::*;
use xai_acp_lib::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};
use xai_grok_shell::agent::config::Config as AgentConfig;
use xai_grok_shell::agent::mvp_agent::MvpAgent;

/// Matches production's `MAX_BUFFER_SIZE` in `agent::app`.
pub const DUPLEX_BUFFER_BYTES: usize = 8 * 1024 * 1024;

pub const RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// This is compiled into each including binary, so a client only one uses is dead code in the others.
#[allow(dead_code)]
pub struct AutoApproveClient;

#[async_trait::async_trait(?Send)]
impl acp::Client for AutoApproveClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(allow_once(&args)))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// Auto-approving client that records `subagent_finished` ids so a test can wait for children to finish.
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct SubagentFinishedRecorder {
    finished: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    changed: std::rc::Rc<tokio::sync::Notify>,
}

#[allow(dead_code)]
impl SubagentFinishedRecorder {
    pub async fn wait_for_subagent_finished(&self, ids: &[&str], timeout: Duration) {
        tokio::time::timeout(timeout, async {
            loop {
                if ids
                    .iter()
                    .all(|id| self.finished.borrow().iter().any(|f| f == id))
                {
                    return;
                }
                self.changed.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "subagent_finished never arrived for {ids:?}; saw {:?}",
                self.finished.borrow()
            )
        });
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for SubagentFinishedRecorder {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(allow_once(&args)))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        if args.method.as_ref() != "x.ai/session_notification" {
            return Ok(());
        }
        let Ok(params) = serde_json::from_str::<serde_json::Value>(args.params.get()) else {
            return Ok(());
        };
        let update = &params["update"];
        if update["sessionUpdate"] == "subagent_finished"
            && let Some(subagent_id) = update["subagent_id"].as_str()
        {
            self.finished.borrow_mut().push(subagent_id.to_owned());
            self.changed.notify_one();
        }
        Ok(())
    }
}

/// Auto-approving client that records every `session_notification` update (the streamed
/// agent text plus the raw update JSON) and forwards `x.ai/session_notification` ext
/// updates to a built-in [`SubagentFinishedRecorder`] so tests can wait on
/// `subagent_finished`. Used by the live L2 acceptance tests (`responses_acceptance.rs`).
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct RecordingClient {
    records: std::rc::Rc<std::cell::RefCell<RecordedStream>>,
    subagents: SubagentFinishedRecorder,
}

#[derive(Default)]
struct RecordedStream {
    agent_text: String,
    updates: Vec<serde_json::Value>,
}

#[allow(dead_code)]
impl RecordingClient {
    /// Agent message text streamed so far (the `AgentMessageChunk` blocks concatenated).
    pub fn recorded_text(&self) -> String {
        self.records.borrow().agent_text.clone()
    }

    /// Raw JSON of every recorded `session_notification` update, in arrival order.
    pub fn recorded_updates(&self) -> Vec<serde_json::Value> {
        self.records.borrow().updates.clone()
    }

    /// Waits until at least one `subagent_finished` ext notification arrives and returns
    /// the recorded subagent ids. Child ids are server-generated (UUIDv7), so a
    /// pre-known-id wait is not possible.
    pub async fn wait_for_subagent_finished_any(&self, timeout: Duration) -> Vec<String> {
        tokio::time::timeout(timeout, async {
            loop {
                if !self.subagents.finished.borrow().is_empty() {
                    return self.subagents.finished.borrow().clone();
                }
                self.subagents.changed.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("subagent_finished never arrived within {timeout:?} (live subagent scenario)")
        })
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for RecordingClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(allow_once(&args)))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        if let acp::SessionUpdate::AgentMessageChunk(chunk) = &args.update
            && let acp::ContentBlock::Text(text) = &chunk.content
        {
            self.records.borrow_mut().agent_text.push_str(&text.text);
        }
        if let Ok(raw) = serde_json::to_value(&args.update) {
            self.records.borrow_mut().updates.push(raw);
        }
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        acp::Client::ext_notification(&self.subagents, args).await
    }
}

pub fn allow_once(args: &acp::RequestPermissionRequest) -> acp::RequestPermissionOutcome {
    args.options
        .iter()
        .find(|o| o.kind == acp::PermissionOptionKind::AllowOnce)
        .or(args.options.first())
        .map(|o| {
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                o.option_id.clone(),
            ))
        })
        .unwrap_or(acp::RequestPermissionOutcome::Cancelled)
}

/// Client-half ends of the duplex pair linking a client to a stood-up agent.
pub struct AgentPipes {
    pub to_agent: tokio::io::DuplexStream,
    pub from_agent: tokio::io::DuplexStream,
}

/// Stand up `MvpAgent` plus its ACP connection and IO tasks on the current `LocalSet`.
/// `remote` is installed before `MvpAgent::new` so the grove gate does not fail
/// closed as `remote_unavailable`.
fn spawn_agent_local(remote: Option<xai_grok_shell::util::config::RemoteSettings>) -> AgentPipes {
    let mut agent_config = AgentConfig::default();
    agent_config.remote_settings = remote;
    spawn_agent_local_with_config(agent_config)
}

/// [`spawn_agent_local`] with a caller-built agent config (live L2 customization).
fn spawn_agent_local_with_config(agent_config: AgentConfig) -> AgentPipes {
    let (c2a_a, c2a_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (a2c_a, a2c_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);

    let auth_manager = Arc::new(agent_config.create_auth_manager());
    let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
    let agent = MvpAgent::new(
        GatewaySender::new(gw_tx),
        &agent_config,
        auth_manager,
        None,
        None,
    )
    .expect("valid config");

    let agent_incoming = LineBufferedRead::spawn_local(c2a_b.compat());
    let (agent_conn, agent_io) =
        acp::AgentSideConnection::new(agent, a2c_a.compat_write(), agent_incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    tokio::task::spawn_local(
        GatewayReceiver::new(gw_rx, agent_conn)
            .with_on_meta(xai_grok_otel::span_from_meta_traceparent)
            .run(),
    );
    tokio::task::spawn_local(agent_io);

    AgentPipes {
        to_agent: c2a_a,
        from_agent: a2c_b,
    }
}

/// IO tasks spawn on the current `LocalSet`.
#[allow(dead_code)]
pub async fn connect_and_auth<C>(
    client: C,
    client_type: &str,
) -> (acp::ClientSideConnection, acp::InitializeResponse)
where
    C: acp::Client + 'static,
{
    connect_and_auth_with_remote(client, client_type, None).await
}

/// [`connect_and_auth`] with a seeded remote-settings object (grove gate).
#[allow(dead_code)]
pub async fn connect_and_auth_with_remote<C>(
    client: C,
    client_type: &str,
    remote: Option<xai_grok_shell::util::config::RemoteSettings>,
) -> (acp::ClientSideConnection, acp::InitializeResponse)
where
    C: acp::Client + 'static,
{
    let pipes = spawn_agent_local(remote);
    connect_client(client, client_type, pipes).await
}

/// Live sibling of [`connect_and_auth_with_remote`]: the same handshake, but the agent is
/// stood up on the caller-built config (the `GROK_HOME` + `[endpoints]` wiring lives in
/// [`run_agent_test_live_proxy`]).
#[allow(dead_code)]
pub async fn connect_and_auth_live<C>(
    client: C,
    client_type: &str,
    agent_config: AgentConfig,
) -> (acp::ClientSideConnection, acp::InitializeResponse)
where
    C: acp::Client + 'static,
{
    let pipes = spawn_agent_local_with_config(agent_config);
    connect_client(client, client_type, pipes).await
}

/// Initialize plus API-key auth over `pipes`; the one handshake every harness topology shares.
pub async fn connect_client<C>(
    client: C,
    client_type: &str,
    pipes: AgentPipes,
) -> (acp::ClientSideConnection, acp::InitializeResponse)
where
    C: acp::Client + 'static,
{
    let AgentPipes {
        to_agent,
        from_agent,
    } = pipes;
    let client_incoming = LineBufferedRead::spawn_local(from_agent.compat());
    let (client_conn, client_io) =
        acp::ClientSideConnection::new(client, to_agent.compat_write(), client_incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    tokio::task::spawn_local(client_io);

    let init = tokio::time::timeout(
        RPC_TIMEOUT,
        client_conn.initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(
                    acp::ClientCapabilities::new()
                        .fs(acp::FileSystemCapabilities::new())
                        .terminal(false),
                )
                .meta(
                    json!({
                        "startupHints": {
                            "nonInteractive": true,
                            "skipGitStatus": true,
                            "skipProjectLayout": true,
                        },
                        "clientType": client_type,
                        "clientVersion": "0.0-test",
                    })
                    .as_object()
                    .cloned(),
                ),
        ),
    )
    .await
    .expect("initialize timed out")
    .expect("initialize failed");

    // API-key auth so sessions resolve the mock's `test-model`.
    let method = init
        .auth_methods
        .iter()
        .find(|m| &*m.id().0 == "xai.api_key")
        .expect("xai.api_key auth method not advertised");
    tokio::time::timeout(
        RPC_TIMEOUT,
        client_conn.authenticate(
            acp::AuthenticateRequest::new(method.id().clone())
                .meta(json!({ "headless": true }).as_object().cloned()),
        ),
    )
    .await
    .expect("authenticate timed out")
    .expect("authenticate failed");

    (client_conn, init)
}

// Dead-code allows below: same per-binary compilation as `AutoApproveClient` above; each helper is used by some including test binaries, not all
#[allow(dead_code)]
pub async fn ext_method(
    conn: &acp::ClientSideConnection,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let params_json =
        serde_json::value::RawValue::from_string(params.to_string()).expect("serialize ext params");
    let resp = tokio::time::timeout(
        RPC_TIMEOUT,
        conn.ext_method(acp::ExtRequest::new(method, Arc::from(params_json))),
    )
    .await
    .unwrap_or_else(|_| panic!("{method} timed out"))
    .unwrap_or_else(|e| panic!("{method} failed: {e}"));
    serde_json::from_str(resp.0.get()).unwrap_or_else(|e| panic!("{method}: bad response: {e}"))
}

#[allow(dead_code)]
pub async fn new_session(
    conn: &acp::ClientSideConnection,
    cwd: &std::path::Path,
) -> acp::SessionId {
    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.new_session(
            acp::NewSessionRequest::new(cwd.to_path_buf())
                .meta(json!({ "modelId": "test-model" }).as_object().cloned()),
        ),
    )
    .await
    .expect("session/new timed out")
    .expect("session/new failed")
    .session_id
}

#[allow(dead_code)]
pub async fn prompt_turn(
    conn: &acp::ClientSideConnection,
    session_id: &acp::SessionId,
    text: &str,
) {
    let resp = tokio::time::timeout(
        RPC_TIMEOUT,
        conn.prompt(acp::PromptRequest::new(
            session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                text.to_owned(),
            ))],
        )),
    )
    .await
    .unwrap_or_else(|_| panic!("prompt on {} timed out", session_id.0))
    .unwrap_or_else(|e| panic!("prompt on {} failed: {e}", session_id.0));
    assert!(
        matches!(resp.stop_reason, acp::StopReason::EndTurn),
        "expected EndTurn on {}, got {:?}",
        session_id.0,
        resp.stop_reason
    );
}

/// Clears process-global prefetch / profile / OTEL state on enter and drop.
struct RestoreProcessGlobals;

impl RestoreProcessGlobals {
    fn enter() -> Self {
        Self::reset();
        Self
    }

    fn reset() {
        // These seams exist only when the library is built with test-support
        // (integration tests) or as a unit-test crate.
        #[cfg(feature = "test-support")]
        {
            xai_grok_shell::managed_config::clear_startup_profile_for_tests();
        }
        xai_grok_telemetry::external::mark_external_otel_settings_resolved();
    }
}

impl Drop for RestoreProcessGlobals {
    fn drop(&mut self) {
        Self::reset();
    }
}

fn set_test_env(grok_home: &std::path::Path, server_url: &str) {
    // SAFETY: the only live threads are the mock's HTTP workers, which never read env.
    unsafe {
        std::env::set_var("GROK_HOME", grok_home);
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", server_url);
        std::env::set_var("GROK_XAI_API_BASE_URL", server_url);
        std::env::set_var("XAI_API_KEY", "test-key-for-ci");
        std::env::set_var("GROK_TELEMETRY_ENABLED", "false");
        std::env::set_var("GROK_FEEDBACK_ENABLED", "false");
        std::env::set_var("GROK_TRACE_UPLOAD", "false");
        // Turn summaries fire one more request to the same mock endpoint after the turn, on a spawned task
        // The race makes request-count assertions flaky
        std::env::set_var("GROK_TURN_SUMMARY", "false");
    }
}

/// Runs `body` against a mock inference server with `GROK_HOME` isolated to a
/// temp dir. `body` gets the cwd and the mock, and opens its own connection,
/// since each test wants a different `acp::Client`.
#[allow(dead_code)]
pub fn run_agent_test<F, Fut>(body: F)
where
    F: FnOnce(std::path::PathBuf, std::rc::Rc<xai_grok_test_support::MockInferenceServer>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    run_agent_test_with_models(
        vec![xai_grok_test_support::MockModelEntry::new("test-model")],
        body,
    )
}

/// [`run_agent_test`] with a custom `/v1/models` catalog.
#[allow(dead_code)]
pub fn run_agent_test_with_models<F, Fut>(
    models: Vec<xai_grok_test_support::MockModelEntry>,
    body: F,
) where
    F: FnOnce(std::path::PathBuf, std::rc::Rc<xai_grok_test_support::MockInferenceServer>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _env_guard = hold_global_env();
    xai_grok_shell::agent::remote_config::settings_get::reset_startup_settings_for_tests();
    xai_grok_extra_ca::ensure_default_crypto_provider();

    // Own thread: agent startup blocks on a models prefetch and would starve the mock.
    let mock_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("mock runtime");
    let server = std::rc::Rc::new(
        mock_rt
            .block_on(xai_grok_test_support::MockInferenceServer::start_with_models(models))
            .expect("mock server"),
    );
    let grok_home = tempfile::TempDir::new().expect("grok home");
    let workdir = tempfile::TempDir::new().expect("workdir");
    set_test_env(grok_home.path(), &server.url());
    // After GROK_HOME is the temp dir, so teardown cannot OnceLock ~/.grok.
    let _globals = RestoreProcessGlobals::enter();

    let agent_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("agent runtime");
    let local = tokio::task::LocalSet::new();
    agent_rt.block_on(local.run_until(body(
        workdir.path().to_path_buf(),
        std::rc::Rc::clone(&server),
    )));
}

/// Live-proxy env contract under the harness ENV_LOCK (smoke-harness-spec.md, Layer 2):
/// the `set_test_env` hygiene minus the mock base-URL/`XAI_API_KEY` pair, plus the ambient
/// provider-key removals and the login-refresh decline. The credential itself is read from
/// process env (`CODEX_LLM_PROXY_KEY`) and never written to disk.
/// Minimal local HTTP responder that answers every request with `404`; it
/// contains the first-party env-key probe (`GET {GROK_XAI_API_BASE_URL}/api-key`)
/// the agent runs at `initialize`.
///
/// The live runner points `GROK_XAI_API_BASE_URL` at it — the live analog of
/// `set_test_env`, which points the probe at the mock server. `404` is the
/// `Unknown` probe verdict, which fails OPEN, so the `xai.api_key` advertisement
/// becomes deterministic instead of depending on the ambient first-party
/// endpoint (api.x.ai answers `403` for the proxy credential, a fail-CLOSED
/// `Unusable`). The credential's real validity stays fully guarded: every turn
/// carries it, and any auth failure surfaces as an `auth_error`-class ACP
/// update, which the scenarios assert on. `xai_api_base_url` is probe-only in
/// the custom-endpoint topology — `ModelFetchAuth::resolve` picks
/// `CustomEndpoint` (models_base_url set) for the catalog fetch and the turns.
pub struct ProbeSink {
    url: String,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ProbeSink {
    fn spawn() -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe sink");
        listener
            .set_nonblocking(true)
            .expect("nonblocking probe sink");
        let url = format!(
            "http://{}/v1",
            listener.local_addr().expect("probe sink addr")
        );
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
                        let _ = sock.read(&mut buf);
                        let _ = sock
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        Self {
            url,
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for ProbeSink {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn set_live_proxy_env(grok_home: &std::path::Path, probe_sink_url: &str) {
    // SAFETY: the harness ENV_LOCK serializes the whole live body (agent included), so
    // no other agent in this process reads env concurrently; same single-writer
    // contract as `set_test_env`.
    let proxy_key = std::env::var("CODEX_LLM_PROXY_KEY").expect("checked by the caller");
    unsafe {
        std::env::set_var("GROK_HOME", grok_home);
        // Probe containment (see `ProbeSink`): deterministic advertise, hermetic
        // initialize — the live path's answer to the mock path's
        // `GROK_XAI_API_BASE_URL = <mock server>`.
        std::env::set_var("GROK_XAI_API_BASE_URL", probe_sink_url);
        for var in [
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
        ] {
            std::env::remove_var(var);
        }
        // First-party env-key parity with `set_test_env` (which sets `XAI_API_KEY`
        // itself): the advertise/BYOK seam reads `XAI_API_KEY`, and the operator
        // ambient env sets it to the same credential as `CODEX_LLM_PROXY_KEY`.
        // Pinning it from the env-only key here keeps the auth-method advertisement
        // deterministic instead of ambient-dependent.
        std::env::set_var("XAI_API_KEY", proxy_key);
        std::env::set_var("GROK_AUTH_EXPIRED", "1");
        std::env::set_var("GROK_TELEMETRY_ENABLED", "false");
        std::env::set_var("GROK_FEEDBACK_ENABLED", "false");
        std::env::set_var("GROK_TRACE_UPLOAD", "false");
        std::env::set_var("GROK_TURN_SUMMARY", "false");
    }
}

/// Live-proxy sibling of [`run_agent_test_with_models`]: spawns NO mock server.
/// `GROK_HOME` is a temp dir carrying the P1 `[endpoints]` block (mirrored from the live
/// `~/.grok`; the key is referenced by name only — the credential is env-only) plus the
/// documented `[model.claude-sonnet-5]` messages-wire row (P1 spec: the live config pins
/// claude to `api_backend = "messages"`; without it the L2 messages-wire scenario would
/// hydrate claude onto the responses wire and stop guarding that transport). Env hygiene
/// is [`set_live_proxy_env`]; `body` gets the workdir and a fresh `AgentConfig` to
/// customize (e.g. `cli_agents`) before standing up the agent via
/// [`connect_and_auth_live`].
#[allow(dead_code)]
pub fn run_agent_test_live_proxy<F, Fut>(body: F)
where
    F: FnOnce(std::path::PathBuf, AgentConfig) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _env_guard = hold_global_env();
    // Env-only credential: refuse to run without it rather than read it from disk.
    match std::env::var_os("CODEX_LLM_PROXY_KEY") {
        Some(key) if !key.to_string_lossy().trim().is_empty() => {}
        _ => panic!("CODEX_LLM_PROXY_KEY not set (live L2 requires it)"),
    }
    xai_grok_shell::agent::remote_config::settings_get::reset_startup_settings_for_tests();
    xai_grok_extra_ca::ensure_default_crypto_provider();
    // Live-run observability: GROK_L2_LOG sets the tracing filter (e.g. "debug" or
    // "xai_grok_shell::agent=debug"); unset = no subscriber (hermetic default).
    if let Ok(filter) = std::env::var("GROK_L2_LOG") {
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(filter))
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .try_init();
    }

    let proxy_base = std::env::var("GROK_L2_PROXY_BASE_URL")
        .unwrap_or_else(|_| "https://llm-proxy-api.ai.eng.netapp.com/v1".to_owned());
    let grok_home = tempfile::TempDir::new().expect("grok home");
    let workdir = tempfile::TempDir::new().expect("workdir");
    std::fs::write(
        grok_home.path().join("config.toml"),
        format!(
            "[endpoints]\n\
             models_base_url = \"{proxy_base}\"\n\
             default_api_backend = \"responses\"\n\
             default_env_key = \"CODEX_LLM_PROXY_KEY\"\n\
             default_context_window = 256000\n\
             default_model_family = \"codex\"\n\
             default_agent_type = \"grok-build-plan\"\n\
             \n\
             [model.claude-sonnet-5]\n\
             api_backend = \"messages\"\n"
        ),
    )
    .expect("write live-proxy [endpoints] config.toml");
    let probe_sink = ProbeSink::spawn();
    set_live_proxy_env(grok_home.path(), &probe_sink.url);
    // After GROK_HOME is the temp dir, so teardown cannot OnceLock ~/.grok.
    let _globals = RestoreProcessGlobals::enter();

    let agent_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("agent runtime");
    let local = tokio::task::LocalSet::new();
    agent_rt.block_on(local.run_until(async {
        // The production headless startup path (pager headless.rs): effective
        // config layers from $GROK_HOME (the temp dir written above), parsed
        // into an AgentConfig with runtime fields resolved.
        let raw_config = xai_grok_shell::config::load_effective_config()
            .expect("load effective config from tempdir GROK_HOME");
        let mut agent_config =
            AgentConfig::new_from_toml_cfg(&raw_config).expect("parse effective config");
        agent_config.resolve_runtime_fields(
            &xai_grok_shell::agent::config::RuntimeResolutionContext {
                raw_config: &raw_config,
                remote_settings: None,
                is_headless: true,
                cli_subagents: None,
                cli_web_search_model: None,
                cli_session_summary_model: None,
                memory_enabled_override: None,
                disable_web_search: false,
                todo_gate: false,
                laziness_debug_log: None,
                storage_mode: None,
            },
        );
        body(workdir.path().to_path_buf(), agent_config).await
    }));
    // Live-debug knob: retain the temp GROK_HOME (the agent's `unified_log` file lives
    // under it) so a failing run's auth/probe evidence can be inspected.
    if std::env::var_os("GROK_L2_KEEP_HOME").is_some() {
        eprintln!(
            "GROK_L2_KEEP_HOME: grok home retained at {}",
            grok_home.path().display()
        );
        std::mem::forget(grok_home);
    }
}

fn hold_global_env() -> MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
