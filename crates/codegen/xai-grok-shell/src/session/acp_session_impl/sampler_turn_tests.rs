use xai_grok_sampling_types::{
    SearchDateBound, ToolExposure, ToolOverrides, WebSearchOptions, XSearchOptions,
};
use xai_tool_types::definition::ToolDefinition;

use super::super::support;
use super::{
    CLASSIFIER_REQUEST_TOKEN_RESERVE, LengthSalvageAction, LengthSalvageStreak,
    MAX_OUTPUT_TOKEN_LIMIT_RETRIES, classifier_request_fits_context, resolve_configured_cutoff,
};

fn x_cut(to: &str) -> XSearchOptions {
    XSearchOptions {
        date_bound: Some(SearchDateBound::new(None, Some(to.into())).unwrap()),
    }
}

/// When every sample ends with finish reason Length while tools are active, the turn salvages up to the cap and then fails.
/// The reminder is injected only on the first salvage.
#[test]
fn length_salvage_streak_proceeds_to_the_cap_then_exhausts() {
    let mut streak = LengthSalvageStreak::default();
    for n in 1..=MAX_OUTPUT_TOKEN_LIMIT_RETRIES {
        match streak.on_sample(true) {
            LengthSalvageAction::Proceed { inject_reminder } => {
                assert_eq!(
                    inject_reminder,
                    n == 1,
                    "reminder only on the first salvage"
                );
            }
            other => panic!("salvage {n} within the cap must proceed, got {other:?}"),
        }
    }
    assert!(matches!(
        streak.on_sample(true),
        LengthSalvageAction::Exhausted
    ));
}

/// A non-Length (or tool-less) sample resets the streak: a later salvage starts a fresh streak and re-injects the reminder.
#[test]
fn length_salvage_streak_resets_on_non_length_sample() {
    let mut streak = LengthSalvageStreak::default();
    for _ in 0..MAX_OUTPUT_TOKEN_LIMIT_RETRIES {
        let _ = streak.on_sample(true);
    }
    assert!(matches!(
        streak.on_sample(false),
        LengthSalvageAction::NotSalvage
    ));
    assert!(matches!(
        streak.on_sample(true),
        LengthSalvageAction::Proceed {
            inject_reminder: true
        }
    ));
}

#[test]
fn classifier_request_bound_enforces_its_reserve_with_saturating_arithmetic() {
    let window = 12_000 + CLASSIFIER_REQUEST_TOKEN_RESERVE;
    for (input, context_window, expected) in [
        (12_000, window, true),
        (12_001, window, false),
        (u64::MAX, u64::MAX, false),
    ] {
        assert_eq!(
            classifier_request_fits_context(input, context_window),
            expected
        );
    }
}

#[test]
fn seed_cutoff_is_inherited_without_a_per_turn_update() {
    let seed = ToolOverrides {
        x_search: Some(x_cut("2020-01-01")),
        web_search: None,
    };
    assert_eq!(resolve_configured_cutoff(Some(seed.clone()), None), seed);
}

#[test]
fn non_empty_base_cutoff_wins_per_tool_and_an_empty_one_reverts_to_the_seed() {
    let seed = ToolOverrides {
        x_search: Some(x_cut("2020-01-01")),
        web_search: Some(WebSearchOptions {
            allowed_domains: Some(vec!["x.com".into()]),
            excluded_domains: None,
        }),
    };
    let base = ToolOverrides {
        x_search: Some(x_cut("2019-06-01")),
        web_search: Some(WebSearchOptions {
            allowed_domains: Some(vec![]),
            excluded_domains: None,
        }),
    };
    let got = resolve_configured_cutoff(Some(seed.clone()), Some(&base));
    assert_eq!(got.x_search, Some(x_cut("2019-06-01")));
    assert_eq!(got.web_search, seed.web_search);
}

#[test]
fn inherited_cutoff_agrees_with_the_wire_echo_so_the_two_implementations_cannot_drift() {
    use xai_grok_sampling_types::{HostedTool, apply_tool_overrides};
    let web = WebSearchOptions {
        allowed_domains: Some(vec!["x.com".into()]),
        excluded_domains: None,
    };
    let cases = [
        (
            Some(ToolOverrides {
                x_search: Some(x_cut("2020-01-01")),
                web_search: None,
            }),
            None,
        ),
        (
            Some(ToolOverrides {
                x_search: Some(x_cut("2020-01-01")),
                web_search: Some(web.clone()),
            }),
            Some(ToolOverrides {
                x_search: Some(x_cut("2019-06-01")),
                web_search: None,
            }),
        ),
        (
            None,
            Some(ToolOverrides {
                x_search: Some(x_cut("2018-01-01")),
                web_search: Some(web.clone()),
            }),
        ),
    ];
    for (seed, base) in cases {
        let mut tools = vec![
            HostedTool::WebSearch { options: None },
            HostedTool::XSearch { options: None },
        ];
        apply_tool_overrides(&mut tools, seed.as_ref());
        let wire_echo = apply_tool_overrides(&mut tools, base.as_ref());
        let inherited = resolve_configured_cutoff(seed.clone(), base.as_ref());
        assert_eq!(wire_echo, inherited, "seed={seed:?} base={base:?}");
    }
}

#[cfg(test)]
mod subagent_sampling_gate_tests {
    use super::super::super::support::create_test_actor;
    use super::super::acquire_subagent_sampling_permit;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    #[derive(Default)]
    struct ConcurrencyProbe {
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    impl ConcurrencyProbe {
        fn enter(&self) {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        }
        fn leave(&self) {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn subagent_submits_never_exceed_cap_and_excess_queues() {
        const CAP: usize = 3;
        const TURNS: usize = 12;
        let semaphore = Arc::new(Semaphore::new(CAP));
        let probe = Arc::new(ConcurrencyProbe::default());
        let ran = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..TURNS {
            let gate = Some(semaphore.clone());
            let probe = probe.clone();
            let ran = ran.clone();
            handles.push(tokio::spawn(async move {
                let permit = acquire_subagent_sampling_permit(&gate).await;
                assert!(permit.is_some(), "a subagent turn must receive a permit");
                probe.enter();
                tokio::time::sleep(Duration::from_millis(20)).await;
                probe.leave();
                ran.fetch_add(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            ran.load(Ordering::SeqCst),
            TURNS,
            "every queued turn ran (queued, not errored)"
        );
        assert!(
            probe.max_in_flight.load(Ordering::SeqCst) <= CAP,
            "in-flight subagent submits exceeded the cap: {} > {CAP}",
            probe.max_in_flight.load(Ordering::SeqCst),
        );
    }

    #[tokio::test]
    async fn cancelled_waiter_releases_without_deadlock() {
        let semaphore = Arc::new(Semaphore::new(1));
        let gate = Some(semaphore.clone());
        let held = acquire_subagent_sampling_permit(&gate).await;
        assert!(held.is_some());

        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move {
                let _permit = acquire_subagent_sampling_permit(&gate).await;
                std::future::pending::<()>().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            semaphore.available_permits(),
            0,
            "slot stays held while the second turn queues"
        );
        waiter.abort();
        let _ = waiter.await;

        drop(held);
        let next = tokio::time::timeout(
            Duration::from_millis(200),
            acquire_subagent_sampling_permit(&gate),
        )
        .await
        .expect("a permit must be free once the held one is released");
        assert!(next.is_some(), "the cancelled waiter did not leak the slot");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn submit_holds_permit_for_subagent_not_main() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let saturated = Arc::new(Semaphore::new(1));
                let _held = saturated.clone().acquire_owned().await.unwrap();

                let (gw_tx, _gw_rx) = tokio::sync::mpsc::unbounded_channel();
                let (p_tx, _p_rx) = tokio::sync::mpsc::unbounded_channel();
                let mut subagent = create_test_actor(0, 200_000, 80, gw_tx, p_tx).await;
                subagent.sampling_gate = Some(saturated.clone());
                let subagent = Arc::new(subagent);
                let (request, binding) = super::bound_test_request(&subagent, Vec::new()).await;

                let queued = tokio::time::timeout(
                    Duration::from_millis(150),
                    subagent.submit_turn_request(request, &binding, None),
                )
                .await;
                assert!(
                    queued.is_err(),
                    "a subagent submit must queue behind the drained gate, never reaching the sampler"
                );

                let (gw_tx, _gw_rx) = tokio::sync::mpsc::unbounded_channel();
                let (p_tx, _p_rx) = tokio::sync::mpsc::unbounded_channel();
                let main = create_test_actor(0, 200_000, 80, gw_tx, p_tx).await;
                assert!(main.sampling_gate.is_none());
                let main = Arc::new(main);
                let (request, binding) = super::bound_test_request(&main, Vec::new()).await;

                let ran = tokio::time::timeout(
                    Duration::from_millis(150),
                    main.submit_turn_request(request, &binding, None),
                )
                .await;
                assert!(
                    ran.is_ok(),
                    "the main session must reach the sampler even while the gate is drained"
                );
                assert!(
                    main.turn_stream_drained.lock().is_empty(),
                    "a result with no queued terminal event must release request ownership before returning"
                );
            })
            .await;
    }
}

/// apex-waj.86 acceptance 1 (firing cell): a row that marks one tool deferred
/// resolves it onto the surface, and the admitted route lowers it to
/// `defer_loading: true` on the wire — the end-to-end path the 9be727ac
/// lowering tests pin in isolation.
#[tokio::test(flavor = "current_thread")]
async fn a_row_marking_a_tool_deferred_withholds_it_on_the_admitted_route() {
    const ROW: &str = "deferred-row";
    const MARKED: &str = "lookup_weather";
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (manager, _tmp) = support::manager_with_entries();
            manager.insert_test_entry(
                ROW,
                support::entry_with_deferred_tools(
                    ROW,
                    /* supports_search_tool */ true,
                    &[MARKED],
                ),
            );
            let actor = support::actor_on_row(ROW, manager).await;
            // The mark resolves off the current row; point the manager at it — the
            // steady-state invariant the switch site maintains.
            actor
                .models_manager
                .set_current_model_id(agent_client_protocol::ModelId::new(ROW));
            let defs = vec![
                ToolDefinition::function(
                    MARKED,
                    None::<&str>,
                    serde_json::json!({ "type": "object" }),
                ),
                ToolDefinition::function(
                    "read_file",
                    None::<&str>,
                    serde_json::json!({ "type": "object" }),
                ),
            ];
            // The surface: the operator-marked tool rides withheld; the unmarked stays Immediate.
            let specs = actor.turn_base_tool_specs(&defs);
            let marked = specs
                .iter()
                .find(|s| s.name == MARKED)
                .expect("the marked tool is in the surface");
            assert_eq!(
                marked.exposure,
                ToolExposure::Deferred,
                "the operator-marked tool must ride withheld"
            );
            let unmarked = specs
                .iter()
                .find(|s| s.name == "read_file")
                .expect("the unmarked tool is in the surface");
            assert_eq!(
                unmarked.exposure,
                ToolExposure::Immediate,
                "an unmarked tool stays Immediate"
            );
            // The admitted route lowers the withheld tool to defer_loading: true.
            let req = support::turn_request_over(&actor, specs).await;
            assert!(
                req.search_admission.is_some_and(|a| a.admitted()),
                "anti-vacuity: the row admits: {:?}",
                req.search_admission
            );
            let body: xai_grok_sampling_types::rs::CreateResponse = (&req).into();
            let wire = serde_json::to_value(&body.tools).expect("tools serialize");
            let tools = wire.as_array().expect("tools lower to an array");
            let marked_wire = tools
                .iter()
                .find(|t| t["name"].as_str() == Some(MARKED))
                .expect("the marked tool is on the wire");
            assert_eq!(
                marked_wire["defer_loading"],
                serde_json::json!(true),
                "withheld on the admitted route: {wire:?}"
            );
        })
        .await;
}

/// apex-waj.86 acceptance 1 (no-marks cell, hard acceptance b): a row that
/// admits but withholds nothing rides no `defer_loading` byte — the admitted
/// array is byte-identical to the 9be727ac wire.
#[tokio::test(flavor = "current_thread")]
async fn an_admitted_row_that_withholds_nothing_moves_no_defer_loading_byte() {
    const ROW: &str = "unmarked-row";
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (manager, _tmp) = support::manager_with_entries();
            manager.insert_test_entry(ROW, support::entry_with_deferred_tools(ROW, true, &[]));
            let actor = support::actor_on_row(ROW, manager).await;
            actor
                .models_manager
                .set_current_model_id(agent_client_protocol::ModelId::new(ROW));
            let defs = vec![
                ToolDefinition::function(
                    "lookup_weather",
                    None::<&str>,
                    serde_json::json!({ "type": "object" }),
                ),
                ToolDefinition::function(
                    "read_file",
                    None::<&str>,
                    serde_json::json!({ "type": "object" }),
                ),
            ];
            // Nothing is marked: the surface withholds nothing.
            let specs = actor.turn_base_tool_specs(&defs);
            for spec in &specs {
                assert_eq!(
                    spec.exposure,
                    ToolExposure::Immediate,
                    "an unmarked row withholds nothing: {spec:?}"
                );
            }
            let req = support::turn_request_over(&actor, specs).await;
            assert!(
                req.search_admission.is_some_and(|a| a.admitted()),
                "anti-vacuity: the row admits: {:?}",
                req.search_admission
            );
            let body: xai_grok_sampling_types::rs::CreateResponse = (&req).into();
            let wire = serde_json::to_string(&body.tools).expect("tools serialize");
            assert!(
                !wire.contains("defer_loading"),
                "no marks, no bytes: the admitted array must not move a byte: {wire}"
            );
        })
        .await;
}

async fn writer_pair_actor() -> (
    std::sync::Arc<super::SessionActor>,
    xai_chat_state::RequestTransportBinding,
) {
    let (manager, _tmp) = support::manager_with_entries();
    let actor = support::actor_on_row("writer-a", manager).await;
    let mut a = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    a.config.base_url = "https://writer-a.example/v1".into();
    a.credentials.api_key = Some("writer-a-key".into());
    actor
        .chat_state_handle
        .install_transport_binding(a.clone())
        .await
        .unwrap();
    (actor, a)
}

#[tokio::test(flavor = "current_thread")]
async fn provider_401_write_uses_caller_capture() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let name = format!("u5-recovery-{}", uuid::Uuid::new_v4());
            let provider = xai_grok_login::test_counting_provider(&name, dir.path());
            let token = provider.ensure_fresh_token(None).await.rotated().unwrap();
            assert_eq!(token, "tok-1");
            xai_grok_login::test_backdate_provider_mint(&name, std::time::Duration::from_secs(60));
            let (actor, mut a) = writer_pair_actor().await;
            a.credentials.api_key = Some(token);
            actor
                .chat_state_handle
                .install_transport_binding(a.clone())
                .await
                .unwrap();
            let mut b = a.clone();
            b.config.model = "writer-b".into();
            b.config.base_url = "https://writer-b.example/v1".into();
            b.credentials.api_key = Some("b-not-provider-tokens".into());
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();
            let recovered = actor.try_provider_401_recovery(&provider, &mut a).await;
            assert!(
                recovered,
                "provider recovery outcome remains successful when actor write refuses"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("count"))
                    .unwrap()
                    .lines()
                    .count(),
                2,
                "captured rejected token must force the second provider run"
            );
            assert_eq!(
                actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap()
                    .credentials,
                b.credentials,
                "late recovery must not write A's recovered key into B"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_preflight_write_uses_caller_capture() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let name = format!("u5-preflight-{}", uuid::Uuid::new_v4());
            let provider = xai_grok_login::test_counting_provider(&name, dir.path());
            let (actor, a) = writer_pair_actor().await;
            let mut b = a.clone();
            b.config.model = "writer-b".into();
            b.config.base_url = "https://writer-b.example/v1".into();
            b.credentials.api_key = Some("b-not-provider-tokens".into());
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();
            let _ = actor
                .refresh_provider_token_pre_turn(
                    &provider,
                    a.credentials.api_key.as_deref(),
                    &a.config.model,
                    &a,
                )
                .await;
            assert_eq!(
                actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap()
                    .credentials,
                b.credentials,
                "late preflight rotation must use caller capture, not live B"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn byok_reload_refuses_stale_source_row() {
    if isolated_item1a_config_test(
        "session::acp_session::sampler_turn::tests::byok_reload_refuses_stale_source_row",
    ) {
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(async {
            use xai_grok_test_support::EnvGuard;
            let dir = tempfile::tempdir().unwrap();
            let _home = EnvGuard::set("GROK_HOME", dir.path().to_str().unwrap());
            std::fs::write(
                dir.path().join("config.toml"),
                "[model.writer-a]\napi_key = \"rotated-config-key\"\n",
            )
            .unwrap();
            let (actor, a) = writer_pair_actor().await;
            let mut b = a.clone();
            b.config.model = "writer-b".into();
            b.config.base_url = "https://writer-b.example/v1".into();
            b.credentials.api_key = Some("b-not-provider-tokens".into());
            let expected = a.clone();
            super::BYOK_WRITE_PROBE.with(|probe| *probe.borrow_mut() = Some(b.clone()));
            actor.reload_captured_api_key(&expected).await;
            assert_eq!(
                actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap()
                    .credentials,
                b.credentials,
                "config reload must refuse stale source row after late B install"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn failure_context_patch_refuses_stale_row() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, a) = writer_pair_actor().await;
            let mut b = a.clone();
            b.config.model = "failure-new-row".into();
            b.config.base_url = "https://failure-new.example/v1".into();
            b.credentials.api_key = Some("failure-new-key".into());
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();
            actor
                .apply_failure_context(&a, Some(std::num::NonZeroU64::new(400000).unwrap()))
                .await;
            let after = actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(after.config).unwrap(),
                serde_json::to_value(b.config).unwrap(),
                "failure-context patch must refuse stale source-row capture"
            );
            assert_eq!(after.credentials, b.credentials);
        })
        .await;
}

// Effective config reads a process-cached home; re-exec isolates it from sibling tests.
fn isolated_item1a_config_test(name: &str) -> bool {
    const CHILD: &str = "GROK_TEST_ITEM1A_CONFIG_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env(CHILD, name)
        .env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG")
        .env_remove("GROK_CONFIG_PATH")
        .env_remove("GROK_CAMPAIGNS_OVERRIDE")
        .env_remove("GROK_CAMPAIGNS")
        .env_remove("XAI_API_KEY")
        .env_remove("APEX_LLM_API_KEY")
        .args(["--exact", name, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated item-1a test failed: {stdout} {stderr}"
    );
    assert!(
        stdout.contains(&format!("test {name} ... ok")),
        "isolated item-1a exact test was not selected: {stdout} {stderr}"
    );
    true
}

#[derive(Default)]
struct BindingTrace {
    install_after_capture: Option<xai_chat_state::RequestTransportBinding>,
    refreshes: Vec<(&'static str, String, String)>,
    configs: Vec<(&'static str, xai_grok_sampler::SamplerConfig)>,
}

thread_local! {
    static BINDING_TRACE: std::cell::RefCell<Option<BindingTrace>> = const { std::cell::RefCell::new(None) };
}

struct BindingTraceGuard;

impl BindingTraceGuard {
    fn new() -> Self {
        BINDING_TRACE.with(|trace| {
            assert!(trace.borrow().is_none());
            *trace.borrow_mut() = Some(BindingTrace::default());
        });
        Self
    }
}

impl Drop for BindingTraceGuard {
    fn drop(&mut self) {
        BINDING_TRACE.with(|trace| *trace.borrow_mut() = None);
    }
}

fn take_binding_trace() -> BindingTrace {
    BINDING_TRACE.with(|trace| std::mem::take(trace.borrow_mut().as_mut().unwrap()))
}

pub(super) async fn install_after_legacy_capture(actor: &super::SessionActor) {
    let next = BINDING_TRACE.with(|trace| {
        trace
            .borrow_mut()
            .as_mut()
            .and_then(|trace| trace.install_after_capture.take())
    });
    if let Some(next) = next {
        actor
            .chat_state_handle
            .install_transport_binding(next)
            .await
            .unwrap();
    }
}

pub(super) fn record_binding_refresh(site: &'static str, model: &str, base_url: &str) {
    super::super::compaction::record_compaction_refresh(site, model, base_url);
    BINDING_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            assert!(trace.refreshes.len() < 32);
            trace
                .refreshes
                .push((site, model.to_owned(), base_url.to_owned()));
        }
    });
}

pub(super) fn record_binding_config(site: &'static str, config: &xai_grok_sampler::SamplerConfig) {
    BINDING_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            assert!(trace.configs.len() < 32);
            trace.configs.push((site, config.clone()));
        }
    });
}

#[derive(Debug)]
struct BindingAttribution;

impl xai_grok_sampler::Auth401AttributionCallback for BindingAttribution {
    fn record_401(&self, _: xai_grok_sampler::SamplingConsumer, _: Option<&str>) {}
}

#[derive(Default)]
struct BindingRefreshFailure(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl xai_grok_login::refresh::TokenRefresher for BindingRefreshFailure {
    async fn refresh(
        &self,
        reason: xai_grok_login::refresh::RefreshReason,
    ) -> xai_grok_login::refresh::RefreshOutcome {
        assert_eq!(reason, xai_grok_login::refresh::RefreshReason::PreRequest);
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        xai_grok_login::refresh::RefreshOutcome::TransientFailure {
            message: "binding-test refresh unavailable".into(),
        }
    }
}

fn binding_auth(key: &str, expired: bool) -> xai_grok_login::GrokAuth {
    xai_grok_login::GrokAuth {
        key: key.into(),
        auth_mode: xai_grok_login::AuthMode::Oidc,
        refresh_token: Some("binding-test-refresh".into()),
        expires_at: Some(
            chrono::Utc::now()
                + if expired {
                    chrono::Duration::hours(-1)
                } else {
                    chrono::Duration::hours(1)
                },
        ),
        ..xai_grok_login::GrokAuth::test_default()
    }
}

fn binding_auth_manager(
    expired: bool,
) -> (
    tempfile::TempDir,
    std::sync::Arc<xai_grok_login::AuthManager>,
    std::sync::Arc<BindingRefreshFailure>,
) {
    let dir = tempfile::tempdir().unwrap();
    let manager = std::sync::Arc::new(xai_grok_login::AuthManager::new(
        dir.path(),
        xai_grok_login::GrokComConfig::default(),
    ));
    let refresher = std::sync::Arc::new(BindingRefreshFailure::default());
    manager.hot_swap(binding_auth("binding-first-valid", expired));
    manager.set_refresher(refresher.clone());
    (dir, manager, refresher)
}

async fn binding_test_actor() -> (super::SessionActor, xai_chat_state::RequestTransportBinding) {
    use xai_grok_sampling_types::{CompactionAtTokens, CompactionsRemaining, ReasoningEffort};
    let (actor, mut binding) = writer_pair_actor().await;
    let mut actor = std::sync::Arc::try_unwrap(actor).ok().unwrap();
    actor.attribution_callback = Some(std::sync::Arc::new(BindingAttribution));
    actor.inference_idle_timeout = std::time::Duration::from_secs(187);
    actor.doom_loop_recovery = Some(xai_grok_sampling_types::DoomLoopRecoveryPolicy {
        max_threshold: 7,
        max_retries: 2,
        window_tokens: 2048,
    });
    actor.client_identifier = Some("binding-client".into());
    actor.origin_client = Some(xai_grok_sampler::OriginClientInfo {
        product: "binding-origin".into(),
        version: Some("binding-version".into()),
    });
    actor
        .compactions_remaining
        .set(Some(CompactionsRemaining::Fixed(3)));
    actor
        .compaction_at_tokens
        .set(Some(CompactionAtTokens::Enabled(true)));
    actor.supports_backend_search.set(true);
    let mut entry = support::entry_with_search_flag(&binding.config.model, true);
    entry.info.model_family = Some("binding-family".into());
    entry.info.strict_responses_input = true;
    entry.info.normalize_content_types = true;
    actor
        .models_manager
        .insert_test_entry(&binding.config.model, entry);
    binding.config.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
    binding.config.mtls_cert_dir = Some(std::path::PathBuf::from("binding-mtls"));
    binding.config.max_completion_tokens = Some(3210);
    binding.config.temperature = Some(0.375);
    binding.config.top_p = Some(0.625);
    binding.config.max_retries = Some(9);
    binding.config.rate_limit_retry_threshold = Some(6);
    binding.config.extra_headers = indexmap::indexmap! { "x-binding".into() => "row-a".into() };
    binding.config.query_params =
        indexmap::indexmap! { "binding-query".into() => "a query".into() };
    binding.config.env_http_headers =
        indexmap::indexmap! { "x-binding-env".into() => "BINDING_TEST_UNUSED_ENV".into() };
    binding.config.context_window = std::num::NonZeroU64::new(480_000).unwrap();
    binding.config.conversation_group_id = Some("binding-group".to_string().into());
    binding.config.reasoning_effort = Some(ReasoningEffort::High);
    binding.config.ultra_wire_effort = Some(ReasoningEffort::Medium);
    binding.config.stream_tool_calls = Some(true);
    binding.config.cache_ttl = Some("1h".into());
    binding.config.thinking_replay = Some("off".into());
    binding.config.top_k = Some(19);
    binding.config.stop_sequences = Some(vec!["binding-stop".into()]);
    binding.config.disable_parallel_tool_use = Some(true);
    binding.config.tool_cache_breakpoint =
        Some(xai_grok_sampling_types::conversation::ToolCacheBreakpoint::Last);
    binding.config.server_tools = Some(vec!["web_search_20250305".into()]);
    binding.credentials.alpha_test_key = Some("binding-alpha".into());
    binding.credentials.client_version = Some("binding-client-version".into());
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    actor
        .chat_state_handle
        .install_transport_binding(binding.clone())
        .await
        .unwrap();
    (actor, binding)
}

fn other_binding(
    a: &xai_chat_state::RequestTransportBinding,
) -> xai_chat_state::RequestTransportBinding {
    let mut b = a.clone();
    b.config.model = "binding-row-b".into();
    b.config.base_url = "https://binding-row-b.example/v1".into();
    b.config.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
    b.config.mtls_cert_dir = None;
    b.config.max_completion_tokens = Some(777);
    b.config.temperature = Some(0.875);
    b.config.top_p = Some(0.25);
    b.config.max_retries = Some(1);
    b.config.context_window = std::num::NonZeroU64::new(96_000).unwrap();
    b.config.reasoning_effort = Some(xai_grok_sampling_types::ReasoningEffort::Low);
    b.config.extra_headers = indexmap::indexmap! { "x-binding".into() => "row-b".into() };
    b.config.query_params.clear();
    b.config.env_http_headers.clear();
    b.credentials = xai_chat_state::Credentials {
        api_key: Some("binding-row-b-key".into()),
        auth_type: xai_chat_state::AuthType::ApiKey,
        alpha_test_key: Some("binding-row-b-alpha".into()),
        client_version: Some("binding-row-b-version".into()),
    };
    b
}

fn assert_binding_eq(
    actual: &xai_chat_state::RequestTransportBinding,
    expected: &xai_chat_state::RequestTransportBinding,
) {
    assert_eq!(
        serde_json::to_value(&actual.config).unwrap(),
        serde_json::to_value(&expected.config).unwrap()
    );
    assert_eq!(actual.credentials, expected.credentials);
}

fn assert_config_eq(
    actual: &xai_grok_sampler::SamplerConfig,
    expected: &xai_grok_sampler::SamplerConfig,
    message: &str,
) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap(),
        "{message}"
    );
}

fn assert_config_identity(
    actual: &xai_grok_sampler::SamplerConfig,
    expected: &xai_grok_sampler::SamplerConfig,
) {
    use std::sync::Arc;
    match (&actual.bearer_resolver, &expected.bearer_resolver) {
        (Some(actual), Some(expected)) => assert!(
            Arc::ptr_eq(actual, expected),
            "the assembled live resolver identity must survive"
        ),
        (None, None) => {}
        _ => panic!("the assembled resolver must not be replaced"),
    }
    assert!(Arc::ptr_eq(
        actual.attribution_callback.as_ref().unwrap(),
        expected.attribution_callback.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(
        actual.header_injector.as_ref().unwrap(),
        expected.header_injector.as_ref().unwrap()
    ));
}

fn traced_config(trace: &BindingTrace, site: &str) -> xai_grok_sampler::SamplerConfig {
    let values: Vec<_> = trace
        .configs
        .iter()
        .filter(|(name, _)| *name == site)
        .collect();
    assert_eq!(values.len(), 1, "one config at {site}");
    values[0].1.clone()
}

async fn install_session_binding(
    actor: &mut super::SessionActor,
    binding: &mut xai_chat_state::RequestTransportBinding,
    manager: std::sync::Arc<xai_grok_login::AuthManager>,
) {
    actor.auth_manager = Some(manager);
    actor.auth_method_id = support::test_auth_method_id("cached_token");
    binding.config.base_url = "https://api.x.ai/v1".into();
    actor
        .model_auth_memo
        .replace(Some(crate::session::acp_session::ModelAuthMemo {
            model_id: binding.config.model.clone(),
            facts: crate::agent::config::ModelAuthFacts {
                byok: crate::agent::auth_method::ModelByok::NotByok,
                auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
            },
            provider: None,
        }));
    actor
        .chat_state_handle
        .install_transport_binding(binding.clone())
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_reconstruction_uses_one_atomic_pair() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut actor, a) = binding_test_actor().await;
            let _guard = BindingTraceGuard::new();
            let expected_a = actor.reconstruct_full_config().await;
            let b = other_binding(&a);
            take_binding_trace();
            BINDING_TRACE.with(|trace| {
                trace.borrow_mut().as_mut().unwrap().install_after_capture = Some(b.clone())
            });
            let actual_a = actor.reconstruct_full_config().await;
            assert_config_eq(
                &actual_a,
                &expected_a,
                "legacy reconstruction must capture one complete A pair",
            );
            assert_binding_eq(
                &actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                &b,
            );
            let actual_b = actor.reconstruct_full_config().await;
            assert_eq!(actual_b.base_url, b.config.base_url);
            assert_eq!(actual_b.api_key, b.credentials.api_key);
            actor
                .chat_state_handle
                .install_transport_binding(a.clone())
                .await
                .unwrap();
            assert_config_eq(
                &actor.reconstruct_full_config().await,
                &expected_a,
                "later legacy reconstruction must follow the newly installed pair",
            );

            actor.chat_state_handle = xai_chat_state::ChatStateHandle::noop();
            actor.compactions_remaining.set(None);
            actor.compaction_at_tokens.set(None);
            let fallback = actor.reconstruct_full_config().await;
            let mut expected = xai_grok_sampler::SamplerConfig {
                context_window: 256_000,
                max_retries: Some(actor.max_retries),
                client_identifier: actor.client_identifier.clone(),
                origin_client: actor.origin_client.clone(),
                supports_backend_search: actor.supports_backend_search.get(),
                doom_loop_recovery: actor.doom_loop_recovery,
                deployment_id: crate::managed_config::resolve_deployment_id(
                    crate::managed_config::resolve_deployment_key().as_deref(),
                ),
                ..Default::default()
            };
            crate::agent::config::inject_url_derived_headers(&mut expected.extra_headers, None, "");
            assert_config_eq(
                &fallback,
                &expected,
                "closed actor must retain the complete legacy fallback",
            );
            assert!(fallback.header_injector.is_some());
            assert!(std::sync::Arc::ptr_eq(
                fallback.attribution_callback.as_ref().unwrap(),
                actor.attribution_callback.as_ref().unwrap()
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bound_assembly_preserves_live_resolver_and_transport_fields() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut actor, mut a) = binding_test_actor().await;
            let (_home, manager, refresher) = binding_auth_manager(false);
            install_session_binding(&mut actor, &mut a, manager.clone()).await;
            let _guard = BindingTraceGuard::new();
            let expected = actor.reconstruct_full_config().await;
            let b = other_binding(&a);
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();
            take_binding_trace();
            let config = actor.assemble_sampler_config_without_refresh(&a).await;
            assert_config_eq(
                &config,
                &expected,
                "bound assembly must use the captured transport fields",
            );
            let trace = take_binding_trace();
            assert!(
                trace.refreshes.is_empty(),
                "refresh-free assembly must not dispatch preflight or gated auth"
            );
            assert_eq!(refresher.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            manager.hot_swap(binding_auth("binding-second-valid", false));
            let resolved = config.bearer_resolver.as_ref().map_or_else(
                || config.api_key.clone(),
                |resolver| resolver.current_bearer(),
            );
            assert_eq!(
                resolved.as_deref(),
                Some("binding-second-valid"),
                "bound resolver must observe same-authority rotation"
            );
            assert_eq!(config.api_key.as_deref(), Some("binding-first-valid"));
            let resolver = config.bearer_resolver.as_ref().unwrap();
            resolver.prepare_for_send().await;
            assert_eq!(refresher.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_config_identity(&config, &traced_config(&trace, "assembled"));
            assert!(std::sync::Arc::ptr_eq(
                config.attribution_callback.as_ref().unwrap(),
                actor.attribution_callback.as_ref().unwrap()
            ));
            assert_binding_eq(
                &actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                &b,
            );
            assert_eq!(a.credentials.api_key.as_deref(), Some("writer-a-key"));

            let mut api_row = a.clone();
            api_row.config.base_url = "https://binding-api-key.example/v1".into();
            actor.auth_method_id = support::test_auth_method_id("xai.api_key");
            let api_config = actor
                .assemble_sampler_config_without_refresh(&api_row)
                .await;
            assert_eq!(api_config.api_key, api_row.credentials.api_key);
            assert!(
                api_config.bearer_resolver.is_none(),
                "API-key rows must not adopt the session bearer"
            );
            assert!(take_binding_trace().refreshes.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn turn_binding_builder_preserves_child_edits() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for child in ["ordinary", "output-budgeted", "retry-only"] {
                for parked in [false, true] {
                    let (mut actor, mut a) = binding_test_actor().await;
                    let (_home, manager, refresher) = binding_auth_manager(false);
                    install_session_binding(&mut actor, &mut a, manager).await;
                    if child == "output-budgeted" {
                        actor.tool_context.task_output_token_budget = Some(
                            crate::tools::tool_context::TaskOutputTokenBudget::limited(512),
                        );
                    }
                    actor.tool_context.sampler_retry_only_before_output = child == "retry-only";
                    let _guard = BindingTraceGuard::new();
                    let mut expected = actor.reconstruct_full_config().await;
                    expected.idle_timeout_secs = Some(187);
                    if child != "ordinary" {
                        expected.doom_loop_recovery = None;
                    }
                    let b = other_binding(&a);
                    actor
                        .chat_state_handle
                        .install_transport_binding(b.clone())
                        .await
                        .unwrap();
                    take_binding_trace();
                    let mut prepared = a.clone();
                    let actual = if parked {
                        actor.turn_sampler_config_from_binding(&prepared).await
                    } else {
                        actor.prepare_bound_sampler_for_turn(&mut prepared).await
                    };
                    assert_config_eq(
                        &actual,
                        &expected,
                        "turn override must retain captured fields and child/idle edits",
                    );
                    let trace = take_binding_trace();
                    assert_config_identity(&actual, &traced_config(&trace, "assembled"));
                    assert!(std::sync::Arc::ptr_eq(
                        actual.attribution_callback.as_ref().unwrap(),
                        actor.attribution_callback.as_ref().unwrap()
                    ));
                    let expected_refreshes = if parked {
                        vec![]
                    } else {
                        vec![
                            (
                                "preflight",
                                a.config.model.clone(),
                                a.config.base_url.clone(),
                            ),
                            (
                                "gated_auth",
                                a.config.model.clone(),
                                a.config.base_url.clone(),
                            ),
                        ]
                    };
                    assert_eq!(
                        trace.refreshes, expected_refreshes,
                        "fresh and parked refresh sites differ"
                    );
                    if parked {
                        assert!(!trace.configs.iter().any(|(site, _)| *site == "default"));
                        assert_binding_eq(&prepared, &a);
                    } else {
                        let default = traced_config(&trace, "default");
                        assert_config_eq(
                            &default,
                            &actual,
                            "default push must reuse the prepared turn config",
                        );
                        assert_config_identity(&default, &actual);
                        let mut refreshed_a = a.clone();
                        refreshed_a.credentials.api_key = Some("binding-first-valid".into());
                        assert_binding_eq(&prepared, &refreshed_a);
                    }
                    assert_eq!(refresher.0.load(std::sync::atomic::Ordering::SeqCst), 0);
                    assert_binding_eq(
                        &actor
                            .chat_state_handle
                            .capture_transport_binding()
                            .await
                            .unwrap(),
                        &b,
                    );
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bound_preparation_refreshes_captured_row_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use std::sync::atomic::Ordering;
            let (mut actor, mut a) = binding_test_actor().await;
            a.config.mtls_cert_dir = None;
            let (_home, manager, refresher) = binding_auth_manager(true);
            install_session_binding(&mut actor, &mut a, manager).await;
            let _guard = BindingTraceGuard::new();
            let expected_sites = vec![
                (
                    "preflight",
                    a.config.model.clone(),
                    a.config.base_url.clone(),
                ),
                (
                    "gated_auth",
                    a.config.model.clone(),
                    a.config.base_url.clone(),
                ),
            ];
            actor.prepare_sampler_for_turn().await;
            let legacy_turn = take_binding_trace();
            assert_eq!(
                legacy_turn.refreshes, expected_sites,
                "legacy turn preparation has one preflight and one gated auth"
            );
            assert_eq!(refresher.0.swap(0, Ordering::SeqCst), 2);
            let expected_turn = traced_config(&legacy_turn, "default");
            actor
                .chat_state_handle
                .install_transport_binding(a.clone())
                .await
                .unwrap();
            actor.prepare_chat_completion(true).await.unwrap();
            let legacy_chat = take_binding_trace();
            assert_eq!(
                legacy_chat.refreshes, expected_sites,
                "legacy chat preparation has one preflight and one gated auth"
            );
            assert_eq!(refresher.0.swap(0, Ordering::SeqCst), 2);
            let expected_chat = traced_config(&legacy_chat, "chat");
            let b = other_binding(&a);
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();

            let mut prepared = a.clone();
            let config = actor.prepare_bound_sampler_for_turn(&mut prepared).await;
            let trace = take_binding_trace();
            assert_eq!(
                trace.refreshes, expected_sites,
                "bound preparation must refresh captured A exactly once per site"
            );
            assert_eq!(
                refresher.0.swap(0, Ordering::SeqCst),
                2,
                "one preflight and one gated auth must reach the preserved refresher"
            );
            assert_config_eq(
                &config,
                &expected_turn,
                "bound turn keeps the legacy prepared fields",
            );
            assert_config_identity(&config, &traced_config(&trace, "assembled"));
            let default = traced_config(&trace, "default");
            assert_config_eq(
                &default,
                &config,
                "default update must use the returned prepared config",
            );
            assert_config_identity(&default, &config);
            let mut cleared = a.clone();
            cleared.credentials.api_key = None;
            assert_binding_eq(&prepared, &cleared);
            assert_binding_eq(
                &actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                &b,
            );

            for force_http1 in [true, false] {
                let mut prepared = a.clone();
                let client = actor
                    .prepare_chat_completion_from_binding(&mut prepared, force_http1)
                    .await
                    .unwrap();
                let trace = take_binding_trace();
                assert_eq!(
                    trace.refreshes, expected_sites,
                    "bound client preparation retains the two existing sites on A"
                );
                assert_eq!(refresher.0.swap(0, Ordering::SeqCst), 2);
                let actual = traced_config(&trace, "chat");
                let mut expected = expected_chat.clone();
                expected.force_http1 = force_http1;
                assert_config_eq(
                    &actual,
                    &expected,
                    "bound client keeps captured transport and HTTP/1 behavior",
                );
                assert_config_identity(&actual, &traced_config(&trace, "assembled"));
                assert_eq!(client.auth_info().auth_type, "none");
                assert_binding_eq(&prepared, &cleared);
                assert_binding_eq(
                    &actor
                        .chat_state_handle
                        .capture_transport_binding()
                        .await
                        .unwrap(),
                    &b,
                );
            }

            actor.reconstruct_full_config_from_binding(&a).await;
            assert_eq!(
                take_binding_trace().refreshes,
                vec![(
                    "gated_auth",
                    a.config.model.clone(),
                    a.config.base_url.clone()
                )]
            );
            assert_eq!(refresher.0.swap(0, Ordering::SeqCst), 1);
            actor.assemble_sampler_config_without_refresh(&a).await;
            actor.turn_sampler_config_from_binding(&a).await;
            assert!(
                take_binding_trace().refreshes.is_empty(),
                "refresh-free dispatches must remain zero"
            );
            assert_eq!(refresher.0.swap(0, Ordering::SeqCst), 0);

            let mut api_row = a.clone();
            api_row.config.base_url = "https://binding-api-key.example/v1".into();
            actor
                .model_auth_memo
                .replace(Some(crate::session::acp_session::ModelAuthMemo {
                    model_id: api_row.config.model.clone(),
                    facts: crate::agent::config::ModelAuthFacts {
                        byok: crate::agent::auth_method::ModelByok::Byok,
                        auth_scheme: xai_grok_sampler::AuthScheme::XApiKey,
                    },
                    provider: None,
                }));
            let mut session_b = b.clone();
            session_b.config.base_url = "https://api.x.ai/v1".into();
            actor
                .chat_state_handle
                .install_transport_binding(session_b.clone())
                .await
                .unwrap();
            let original_api_row = api_row.clone();
            let api_config = actor.prepare_bound_sampler_for_turn(&mut api_row).await;
            assert_eq!(
                take_binding_trace().refreshes,
                vec![(
                    "preflight",
                    api_row.config.model.clone(),
                    api_row.config.base_url.clone()
                )],
                "live B must not activate auth for a captured API-key row"
            );
            assert_eq!(refresher.0.load(Ordering::SeqCst), 0);
            assert_eq!(api_config.api_key, original_api_row.credentials.api_key);
            assert_eq!(
                api_config.auth_scheme,
                xai_grok_sampler::AuthScheme::XApiKey
            );
            assert!(api_config.bearer_resolver.is_none());
            assert_binding_eq(&api_row, &original_api_row);
            assert_binding_eq(
                &actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                &session_b,
            );
        })
        .await;
}

async fn bound_test_request(
    actor: &super::SessionActor,
    tools: Vec<xai_grok_sampling_types::ToolSpec>,
) -> (
    xai_grok_sampling_types::ConversationRequest,
    xai_chat_state::RequestTransportBinding,
) {
    actor
        .chat_state_handle
        .build_request_with_binding(
            xai_chat_state::NativeToolCandidates::builtins_only(tools),
            None,
            false,
            None,
            actor.session_id_string(),
            "u4-local-bound".into(),
        )
        .await
        .expect("chat state actor should be alive")
}

struct OuterBoundTrace {
    install_after_first: Option<xai_chat_state::RequestTransportBinding>,
    builds: Vec<(
        xai_grok_sampling_types::ConversationRequest,
        xai_chat_state::RequestTransportBinding,
    )>,
}

thread_local! {
    static OUTER_BOUND_TRACE: std::cell::RefCell<Option<OuterBoundTrace>> = const { std::cell::RefCell::new(None) };
}

struct OuterBoundGuard;

impl Drop for OuterBoundGuard {
    fn drop(&mut self) {
        OUTER_BOUND_TRACE.with(|trace| *trace.borrow_mut() = None);
    }
}

pub(super) async fn observe_outer_bound_build(
    actor: &super::SessionActor,
    request: &xai_grok_sampling_types::ConversationRequest,
    binding: &xai_chat_state::RequestTransportBinding,
) {
    let next = OUTER_BOUND_TRACE.with(|trace| {
        trace.borrow_mut().as_mut().and_then(|trace| {
            assert!(trace.builds.len() < 4);
            trace.builds.push((request.clone(), binding.clone()));
            trace.install_after_first.take()
        })
    });
    if let Some(next) = next {
        actor
            .chat_state_handle
            .install_transport_binding(next)
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn new_outer_request_uses_new_installed_row() {
    use super::super::rate_limit_backoff_tests::{drain_gateway, drain_persistence};
    use std::sync::Arc;
    use xai_grok_sampling_types::{ApiBackend, ToolSpec};
    use xai_grok_test_support::{MockInferenceServer, MockModelEntry, ScriptedResponse};

    tokio::task::LocalSet::new()
        .run_until(async {
            let server_a = MockInferenceServer::start_with_models(vec![MockModelEntry::new("outer-a")]).await.unwrap();
            let server_b = MockInferenceServer::start_with_models(vec![MockModelEntry::new("outer-b")]).await.unwrap();
            server_a.enqueue_response("/v1/responses", ScriptedResponse::json(503, serde_json::json!({"error": "outer resubmit"})));
            let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            drain_gateway(gateway_rx);
            drain_persistence(persistence_rx);
            let mut actor = support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let tools = vec![ToolSpec {
                name: "read_file".into(),
                description: Some("inherited prepared reader".into()),
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                exposure: ToolExposure::Deferred,
            }];
            actor.forked_tool_override = Some(tools.clone());
            actor.max_retries = 0;
            actor.transient_retry_enabled = true;
            let mut a = actor.chat_state_handle.capture_transport_binding().await.unwrap();
            a.config.model = "outer-a".into();
            a.config.base_url = server_a.url();
            a.config.api_backend = ApiBackend::Responses;
            a.config.max_retries = Some(0);
            a.config.supports_search_tool = true;
            a.credentials.api_key = Some("u4-outer-a-key".into());
            a.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
            actor.chat_state_handle.install_transport_binding(a.clone()).await.unwrap();
            let mut b = a.clone();
            b.config.model = "outer-b".into();
            b.config.base_url = server_b.url();
            b.config.api_backend = ApiBackend::Messages;
            b.config.supports_search_tool = false;
            b.credentials.api_key = Some("u4-outer-b-key".into());
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
                actor.turn_sampler_config_from_binding(&b).await,
                xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() },
                event_tx,
            );
            actor.workspace_ops.bind_local_session(
                &actor.session_id_string(), actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(),
                actor.agent.borrow().tool_bridge().toolset(), None,
            ).unwrap();
            let actor = Arc::new(actor);
            let drainer = actor.clone();
            let drain_task = tokio::task::spawn_local(async move {
                while let Some(event) = event_rx.recv().await {
                    drainer.handle_sampling_event(event).await;
                }
            });
            OUTER_BOUND_TRACE.with(|trace| {
                assert!(trace.borrow().is_none());
                *trace.borrow_mut() = Some(OuterBoundTrace {
                    install_after_first: Some(b.clone()), builds: Vec::new(),
                });
            });
            let _guard = OuterBoundGuard;
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(86_400),
                Box::pin(actor.handle_prompt(
                    "u4-outer", vec![agent_client_protocol::ContentBlock::Text(
                        agent_client_protocol::TextContent::new("answer once".to_string()),
                    )], super::super::PromptMode::Agent,
                    None, None, None, None, true, false, None, None, None,
                )),
            ).await.expect("outer loop completes");
            assert!(outcome.is_ok(), "outer resubmit must complete: {outcome:?}");
            assert_eq!(server_a.request_count_for("/v1/responses"), 1,
                "new outer build must send A then newly installed B");
            assert_eq!(server_b.request_count_for("/v1/messages"), 1,
                "new outer build must send A then newly installed B");
            let a_send = server_a.requests().into_iter().find(|r| r.path == "/v1/responses").unwrap();
            let b_send = server_b.requests().into_iter().find(|r| r.path == "/v1/messages").unwrap();
            assert_eq!(a_send.authorization.as_deref(), Some("Bearer u4-outer-a-key"));
            assert_eq!(b_send.authorization.as_deref(), Some("Bearer u4-outer-b-key"));
            assert_eq!(a_send.body.as_ref().unwrap()["model"], "outer-a");
            assert_eq!(b_send.body.as_ref().unwrap()["model"], "outer-b");
            let builds = OUTER_BOUND_TRACE.with(|trace| std::mem::take(&mut trace.borrow_mut().as_mut().unwrap().builds));
            assert_eq!(builds.len(), 2, "one atomic build per outer iteration");
            for ((request, binding), expected) in builds.iter().zip([&a, &b]) {
                assert_eq!(request.model.as_ref(), Some(&expected.config.model));
                assert_eq!(serde_json::to_value(&request.tools).unwrap(), serde_json::to_value(&tools).unwrap(), "prepared inherited specs must stay unchanged");
                assert_eq!(request.search_admission.unwrap().admitted(), expected.config.supports_search_tool);
                assert_binding_eq(binding, expected);
            }
            assert_binding_eq(&actor.chat_state_handle.capture_transport_binding().await.unwrap(), &b);
            drain_task.abort();
            let _ = drain_task.await;
        })
        .await;
}

fn originating_auth_error(
    credential: xai_grok_sampling_types::SentCredential,
) -> xai_grok_sampler::SamplingErrorInfo {
    xai_grok_sampler::SamplingErrorInfo {
        kind: xai_grok_sampler::SamplingErrorKind::Auth,
        status_code: Some(401),
        message: "originating request rejected".into(),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: None,
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential,
    }
}

fn originating_transient_state() -> super::TransientRetryState {
    super::TransientRetryState {
        step_attempts: 0,
        prompt_attempts: 0,
        episode_start: None,
        enabled: true,
    }
}

#[derive(Default)]
struct OriginatingRecoveryRefresher {
    calls: parking_lot::Mutex<Vec<xai_grok_login::refresh::RefreshReason>>,
}

#[async_trait::async_trait]
impl xai_grok_login::refresh::TokenRefresher for OriginatingRecoveryRefresher {
    async fn refresh(
        &self,
        reason: xai_grok_login::refresh::RefreshReason,
    ) -> xai_grok_login::refresh::RefreshOutcome {
        self.calls.lock().push(reason);
        xai_grok_login::refresh::RefreshOutcome::success(binding_auth(
            "u5-session-recovered",
            false,
        ))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failed_binding_selects_originating_recovery_arm() {
    use super::super::{RecoveredStore, SamplerFailureRecovery, TurnParkState};
    use xai_grok_sampling_types::SentCredential;

    tokio::task::LocalSet::new()
        .run_until(async {
            for provider_origin in [false, true] {
                for credential in [SentCredential::Sent, SentCredential::Missing, SentCredential::Unknown] {
                    let provider_dir = tempfile::tempdir().unwrap();
                    let provider_name = format!("u5-arm-{}", uuid::Uuid::new_v4());
                    let provider = xai_grok_login::test_counting_provider(&provider_name, provider_dir.path());
                    let first = provider.ensure_fresh_token(None).await.rotated().unwrap();
                    assert_eq!(first, "tok-1");
                    xai_grok_login::test_backdate_provider_mint(&provider_name, std::time::Duration::from_secs(60));
                    let (mut actor, mut a) = binding_test_actor().await;
                    let auth_dir = tempfile::tempdir().unwrap();
                    let manager = std::sync::Arc::new(xai_grok_login::AuthManager::new(auth_dir.path(), Default::default()));
                    let refresher = std::sync::Arc::new(OriginatingRecoveryRefresher::default());
                    manager.hot_swap(binding_auth("u5-expired-session", true));
                    manager.set_refresher(refresher.clone());
                    actor.auth_manager = Some(manager.clone());
                    actor.auth_method_id = support::test_auth_method_id("cached_token");
                    let mut b = other_binding(&a);
                    let provider_model = if provider_origin {
                        a.credentials.api_key = Some(first);
                        b.config.base_url = "https://api.x.ai/v1".into();
                        a.config.model.clone()
                    } else {
                        a.config.base_url = "https://api.x.ai/v1".into();
                        a.credentials.api_key = Some("u5-expired-session".into());
                        b.config.model.clone()
                    };
                    actor.model_auth_memo.replace(Some(crate::session::acp_session::ModelAuthMemo {
                        model_id: provider_model,
                        facts: crate::agent::config::ModelAuthFacts {
                            byok: crate::agent::auth_method::ModelByok::Byok,
                            auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
                        },
                        provider: Some(provider),
                    }));
                    actor.chat_state_handle.install_transport_binding(a.clone()).await.unwrap();
                    let mut failed = actor.chat_state_handle.capture_transport_binding().await.unwrap();
                    actor.chat_state_handle.install_transport_binding(b.clone()).await.unwrap();
                    let actor = std::sync::Arc::new(actor);
                    let _trace_guard = BindingTraceGuard::new();
                    let outcome = actor.handle_sampling_failure(
                        originating_auth_error(credential), &mut failed, 0,
                        originating_transient_state(), false, TurnParkState::Fresh,
                    ).await;
                    let expected_store = if provider_origin { RecoveredStore::AuthProvider } else { RecoveredStore::SessionToken };
                    assert!(matches!(outcome, Ok(SamplerFailureRecovery::RefreshAuthAndResubmit { credential: actual, store })
                        if actual == credential && store == expected_store),
                        "U5 recovery arm must follow failed A, not installed B");
                    assert_eq!(*refresher.calls.lock(), if provider_origin { vec![] } else {
                        vec![xai_grok_login::refresh::RefreshReason::ServerRejected]
                    }, "U5 only the originating recovery authority may run");
                    assert_eq!(std::fs::read_to_string(provider_dir.path().join("count")).unwrap().lines().count(),
                        if provider_origin { 2 } else { 1 }, "U5 provider runs must follow failed A");
                    let trace = take_binding_trace();
                    let mut expected_sites = vec![("preflight", a.config.model.clone(), a.config.base_url.clone())];
                    if !provider_origin {
                        expected_sites.push(("gated_auth", a.config.model.clone(), a.config.base_url.clone()));
                    }
                    assert_eq!(trace.refreshes, expected_sites, "U5 successful recovery prepares only failed A at existing sites");
                    let mut expected = a.clone();
                    expected.credentials.api_key = Some(if provider_origin { "tok-2" } else { "u5-session-recovered" }.into());
                    assert_binding_eq(&failed, &expected);
                    assert_binding_eq(&actor.chat_state_handle.capture_transport_binding().await.unwrap(), &b);
                    let default = traced_config(&trace, "default");
                    assert_eq!(default.model, a.config.model);
                    assert_eq!(default.base_url, a.config.base_url);
                    assert_eq!(default.api_key, expected.credentials.api_key);
                    if !provider_origin {
                        manager.hot_swap(binding_auth("u5-live-session", false));
                        assert_eq!(default.bearer_resolver.as_ref().unwrap().current_bearer().as_deref(), Some("u5-live-session"));
                        let calls = refresher.calls.lock().len();
                        let parked = actor.handle_sampling_failure(
                            originating_auth_error(SentCredential::Missing), &mut failed, 0,
                            originating_transient_state(), false, TurnParkState::Parked,
                        ).await;
                        assert!(matches!(parked, Ok(SamplerFailureRecovery::RefreshAuthAndResubmit {
                            credential: SentCredential::Missing, store: RecoveredStore::SessionToken,
                        })));
                        assert_eq!(refresher.calls.lock().len(), calls);
                        assert!(take_binding_trace().refreshes.is_empty(), "U5 parked recovery must not prepare");
                        assert_binding_eq(&actor.chat_state_handle.capture_transport_binding().await.unwrap(), &b);
                    }
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_recovery_updates_failed_binding_not_new_actor() {
    use xai_grok_test_support::{MockInferenceServer, MockModelEntry, ScriptedResponse};

    tokio::task::LocalSet::new()
        .run_until(async {
            let server =
                MockInferenceServer::start_with_models(vec![MockModelEntry::new("writer-a")])
                    .await
                    .unwrap();
            server.enqueue_response(
                "/v1/responses",
                ScriptedResponse::json(401, serde_json::json!({"error": "rejected A"})),
            );
            let dir = tempfile::tempdir().unwrap();
            let name = format!("u5-failed-binding-{}", uuid::Uuid::new_v4());
            let provider = xai_grok_login::test_counting_provider(&name, dir.path());
            let first = provider.ensure_fresh_token(None).await.rotated().unwrap();
            assert_eq!(first, "tok-1");
            xai_grok_login::test_backdate_provider_mint(&name, std::time::Duration::from_secs(60));
            let (actor, mut a) = writer_pair_actor().await;
            a.config.base_url = server.url();
            a.config.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            a.config.max_retries = Some(0);
            a.config
                .extra_headers
                .insert("x-u5-source".into(), "failed-a".into());
            a.credentials.api_key = Some(first);
            a.credentials.alpha_test_key = Some("u5-a-alpha".into());
            a.credentials.client_version = Some("u5-a-version".into());
            actor
                .chat_state_handle
                .install_transport_binding(a.clone())
                .await
                .unwrap();
            let (request, mut failed) = bound_test_request(&actor, Vec::new()).await;
            let config = actor.turn_sampler_config_from_binding(&failed).await;
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let sampler = xai_grok_sampler::SamplerActor::spawn(
                config.clone(),
                xai_grok_sampler::RetryPolicy {
                    max_retries: 0,
                    ..Default::default()
                },
                event_tx,
            );
            let rejected = sampler
                .submit_and_collect_with_config(
                    xai_grok_sampler::RequestId::random(),
                    request,
                    config,
                )
                .await
                .result
                .err()
                .expect("A must actually be rejected");
            assert_eq!(
                xai_grok_sampler::SamplingErrorInfo::from(&rejected).credential,
                xai_grok_sampling_types::SentCredential::Sent
            );
            let sends = server.requests();
            assert_eq!(sends.len(), 1);
            assert_eq!(sends[0].authorization.as_deref(), Some("Bearer tok-1"));
            assert_eq!(sends[0].body.as_ref().unwrap()["model"], a.config.model);
            let b = other_binding(&a);
            assert_ne!(b.credentials.api_key.as_deref(), Some("tok-1"));
            assert_ne!(b.credentials.api_key.as_deref(), Some("tok-2"));
            actor
                .chat_state_handle
                .install_transport_binding(b.clone())
                .await
                .unwrap();
            let recovered = actor
                .try_provider_401_recovery(&provider, &mut failed)
                .await;
            assert!(
                recovered,
                "U5 refused actor patch is still successful recovery"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("count"))
                    .unwrap()
                    .lines()
                    .count(),
                2,
                "U5 actually rejected A key must force a second provider mint"
            );
            assert_eq!(
                failed.credentials.api_key.as_deref(),
                Some("tok-2"),
                "U5 originating binding must carry the recovered key"
            );
            let mut expected = a;
            expected.credentials.api_key = Some("tok-2".into());
            assert_binding_eq(&failed, &expected);
            assert_binding_eq(
                &actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                &b,
            );
            assert_eq!(sampler.active_count().await, 0);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn failure_context_patch_uses_failed_binding() {
    use super::super::TurnParkState;
    use xai_grok_sampling_types::SentCredential;

    tokio::task::LocalSet::new()
        .run_until(async {
            for newer_row in [true, false] {
                let (actor, a) = writer_pair_actor().await;
                let mut failed = actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap();
                let b = if newer_row {
                    other_binding(&a)
                } else {
                    a.clone()
                };
                actor
                    .chat_state_handle
                    .install_transport_binding(b.clone())
                    .await
                    .unwrap();
                actor.chat_state_handle.record_token_usage(240_000);
                let error = xai_grok_sampler::SamplingErrorInfo {
                    kind: xai_grok_sampler::SamplingErrorKind::Api,
                    message: "prompt is too long".into(),
                    status_code: Some(400),
                    model_metadata: Some(xai_grok_sampling_types::ResponseModelMetadata {
                        context_window: Some(120_000),
                        max_completion_tokens: None,
                        models_etag: None,
                    }),
                    ..originating_auth_error(SentCredential::Sent)
                };
                assert!(actor.should_compact_on_error(&error).await);
                let result = actor
                    .handle_sampling_failure(
                        error,
                        &mut failed,
                        0,
                        originating_transient_state(),
                        false,
                        TurnParkState::Fresh,
                    )
                    .await;
                let error = result
                    .err()
                    .expect("empty fixture must stop before any compaction client");
                assert!(
                    error
                        .data
                        .as_ref()
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .contains("conversation is empty")
                );
                let mut expected = b;
                if !newer_row {
                    expected.config.context_window = std::num::NonZeroU64::new(120_000).unwrap();
                }
                let actual = actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&actual.config).unwrap(),
                    serde_json::to_value(&expected.config).unwrap(),
                    "U5 failure context must patch only the failed binding's source row"
                );
                assert_eq!(actual.credentials, expected.credentials);
                assert_binding_eq(&failed, &a);
            }
        })
        .await;
}

use xai_chat_state::{NativeToolCandidates, RequestTransportBinding};
use xai_grok_sampling_types::{ApiBackend, ConversationItem, ConversationRequest, ToolSpec};
use xai_grok_tools::types::tool::ToolNamespace;

type NativeCandidateSnapshot = Vec<(ToolDefinition, ToolNamespace)>;

struct NativeSnapshotTrace {
    snapshots: Vec<NativeCandidateSnapshot>,
    gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
}

thread_local! {
    static NATIVE_SNAPSHOT_TRACE: std::cell::RefCell<Option<NativeSnapshotTrace>> = const { std::cell::RefCell::new(None) };
}

struct NativeSnapshotGuard;

impl Drop for NativeSnapshotGuard {
    fn drop(&mut self) {
        NATIVE_SNAPSHOT_TRACE.with(|trace| *trace.borrow_mut() = None);
    }
}

fn trace_native_snapshots(
    gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> NativeSnapshotGuard {
    NATIVE_SNAPSHOT_TRACE.with(|trace| {
        assert!(trace.borrow().is_none());
        *trace.borrow_mut() = Some(NativeSnapshotTrace {
            snapshots: Vec::new(),
            gate,
        });
    });
    NativeSnapshotGuard
}

pub(super) async fn after_native_candidate_snapshot(snapshot: &[(ToolDefinition, ToolNamespace)]) {
    let gate = NATIVE_SNAPSHOT_TRACE.with(|trace| {
        trace.borrow_mut().as_mut().and_then(|trace| {
            trace.snapshots.push(snapshot.to_vec());
            trace.gate.take()
        })
    });
    if let Some((reached, resume)) = gate {
        reached.send(()).unwrap();
        resume.await.unwrap();
    }
}

fn observed_native_snapshots() -> Vec<NativeCandidateSnapshot> {
    NATIVE_SNAPSHOT_TRACE.with(|trace| trace.borrow().as_ref().unwrap().snapshots.clone())
}

#[derive(Debug, Clone)]
struct NativeRegisteredTool {
    name: &'static str,
    namespace: ToolNamespace,
    description: &'static str,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for NativeRegisteredTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> ToolNamespace {
        self.namespace
    }
    fn description_template(&self) -> &str {
        self.description
    }
}

impl xai_tool_runtime::Tool for NativeRegisteredTool {
    type Args = serde_json::Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, self.description)
    }
    async fn run(
        &self,
        _ctx: xai_tool_runtime::ToolCallContext,
        _args: serde_json::Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        panic!("native declaration fixtures never execute tools")
    }
}

fn register_native_fixture(
    actor: &super::SessionActor,
    name: &'static str,
    namespace: ToolNamespace,
    description: &'static str,
    schema: serde_json::Value,
) -> ToolSpec {
    actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(
            name.into(),
            NativeRegisteredTool {
                name,
                namespace,
                description,
            },
            Some(schema.clone()),
        )
        .unwrap();
    ToolSpec {
        name: name.into(),
        description: Some(description.into()),
        parameters: schema,
        exposure: ToolExposure::Immediate,
    }
}

fn native_alpha_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false})
}

fn native_zeta_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"url":{"type":"string"},"limit":{"type":"integer"}},"required":["url"],"additionalProperties":false})
}

fn register_native_pair(actor: &super::SessionActor) -> Vec<ToolSpec> {
    let zeta = register_native_fixture(
        actor,
        "zeta__fetch",
        ToolNamespace::MCP,
        "Fetch one URL with an optional limit",
        native_zeta_schema(),
    );
    let alpha = register_native_fixture(
        actor,
        "alpha__lookup",
        ToolNamespace::MCP,
        "Look up one exact query",
        native_alpha_schema(),
    );
    vec![alpha, zeta]
}

fn register_native_builtins(actor: &super::SessionActor) {
    register_native_fixture(
        actor,
        "z_builtin",
        ToolNamespace::GrokBuild,
        "Builtin path reader",
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
    );
    register_native_fixture(
        actor,
        "builtin__renamed",
        ToolNamespace::GrokBuild,
        "Delimiter-renamed builtin",
        serde_json::json!({"type":"object","properties":{"enabled":{"type":"boolean"}},"required":[],"additionalProperties":false}),
    );
}

struct NativeCandidateFixture {
    actor: super::SessionActor,
    events: tokio::sync::mpsc::UnboundedReceiver<xai_chat_state::ChatStateEvent>,
    cancel: tokio_util::sync::CancellationToken,
    _home: tempfile::TempDir,
}

impl Drop for NativeCandidateFixture {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn native_candidate_fixture(
    backend: &ApiBackend,
    flag: bool,
    deferred: &[&str],
) -> NativeCandidateFixture {
    let mut actor = support::plain_actor().await;
    actor.agent.replace(
        support::test_agent_with_tools(vec![
            xai_grok_tools::registry::types::ToolConfig::for_tool::<
                xai_grok_tools::implementations::search_tool::SearchTool,
            >(),
            xai_grok_tools::registry::types::ToolConfig::for_tool::<
                xai_grok_tools::implementations::use_tool::UseTool,
            >(),
        ])
        .await,
    );
    let (manager, home) = support::manager_with_entries();
    manager.insert_test_entry(
        "native-prepared",
        support::entry_with_deferred_tools("native-prepared", flag, deferred),
    );
    manager.set_current_model_id(agent_client_protocol::ModelId::new("native-prepared"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.model = "native-prepared".into();
    binding.config.base_url = "https://native-prepared.example/v1".into();
    binding.config.api_backend = backend.clone();
    binding.config.supports_search_tool = flag;
    binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
    binding.credentials.api_key = Some("native-prepared-fixture-key".into());
    let ConversationItem::User(mut user) = ConversationItem::user("inspect this image") else {
        unreachable!()
    };
    user.add_image(concat!("data:image/png;base64,",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="));
    let (event_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let cancel = tokio_util::sync::CancellationToken::new();
    actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![
            ConversationItem::system("native candidate system"),
            ConversationItem::User(user),
        ],
        binding.config.clone(),
        Box::new(xai_chat_state::NullChatPersistence),
        event_tx,
        cancel.clone(),
    );
    actor
        .chat_state_handle
        .install_transport_binding(binding)
        .await
        .unwrap();
    NativeCandidateFixture {
        actor,
        events,
        cancel,
        _home: home,
    }
}

async fn native_bound_request(
    actor: &super::SessionActor,
    candidates: NativeToolCandidates,
) -> (ConversationRequest, RequestTransportBinding) {
    actor
        .chat_state_handle
        .build_request_with_binding(
            actor.tool_candidates_for_turn(candidates, None),
            None,
            false,
            None,
            "native-conversation".into(),
            "native-request".into(),
        )
        .await
        .unwrap()
}

async fn native_legacy_request(
    actor: &super::SessionActor,
    specs: Vec<ToolSpec>,
) -> ConversationRequest {
    actor
        .chat_state_handle
        .build_request(
            specs,
            None,
            false,
            None,
            "native-conversation".into(),
            "native-request".into(),
        )
        .await
        .unwrap()
}

fn assert_native_specs(actual: &[ToolSpec], expected: &[ToolSpec], marker: &str) {
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap(),
        "{marker}"
    );
}

fn native_selected_mcp(specs: &[ToolSpec]) -> Vec<ToolSpec> {
    specs
        .iter()
        .cloned()
        .map(|mut spec| {
            spec.exposure = ToolExposure::Deferred;
            spec
        })
        .collect()
}

async fn assert_native_membership(
    actor: &super::SessionActor,
    candidates: NativeToolCandidates,
    snapshot: &[(ToolDefinition, ToolNamespace)],
) {
    use std::collections::BTreeSet;
    let plan_active = actor.plan_mode.lock().is_active();
    let builtin_defs = super::filter_cursor_tools_by_plan_mode(
        snapshot
            .iter()
            .filter(|(_, namespace)| *namespace != ToolNamespace::MCP)
            .map(|(def, _)| def.clone())
            .collect(),
        plan_active,
    );
    let builtins = actor.turn_base_tool_specs(&builtin_defs);
    let mut mcp: Vec<_> = super::filter_cursor_tools_by_plan_mode(
        snapshot
            .iter()
            .filter(|(def, namespace)| {
                *namespace == ToolNamespace::MCP
                    && xai_grok_mcp::servers::parse_mcp_qualified_name(&def.function.name).is_some()
            })
            .map(|(def, _)| def.clone())
            .collect(),
        plan_active,
    )
    .into_iter()
    .map(ToolSpec::from)
    .collect();
    mcp.sort_by(|a, b| a.name.cmp(&b.name));
    let expected_members: BTreeSet<_> = mcp.iter().map(|s| s.name.clone()).collect();
    let mut admitted_row = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap()
        .config;
    admitted_row.api_backend = ApiBackend::Messages;
    admitted_row.supports_search_tool = true;
    let mut other_row = admitted_row.clone();
    other_row.api_backend = ApiBackend::Responses;
    let admitted = candidates.clone().select_for_row(&admitted_row);
    let excluded = candidates.select_for_row(&other_row);
    let other_names: BTreeSet<_> = excluded.iter().map(|s| s.name.clone()).collect();
    let dropped: BTreeSet<_> = admitted
        .iter()
        .map(|s| s.name.clone())
        .filter(|name| !other_names.contains(name))
        .collect();
    let operator_deferred: BTreeSet<_> = builtins
        .iter()
        .filter(|s| s.exposure == ToolExposure::Deferred)
        .map(|s| s.name.clone())
        .collect();
    let deferred: BTreeSet<_> = admitted
        .iter()
        .filter(|s| s.exposure == ToolExposure::Deferred)
        .map(|s| s.name.clone())
        .filter(|name| !operator_deferred.contains(name))
        .collect();
    assert_eq!(
        dropped, expected_members,
        "U3 membership must equal the qualified typed subset of the captured snapshot"
    );
    assert_eq!(
        deferred, expected_members,
        "U3 Deferred proxy must equal the same qualified typed subset"
    );
    assert_native_specs(
        &excluded,
        &builtins,
        "U3 non-Messages builtin prefix must remain complete",
    );
    let expected: Vec<_> = builtins
        .into_iter()
        .chain(native_selected_mcp(&mcp))
        .collect();
    assert_native_specs(
        &admitted,
        &expected,
        "U3 qualified suffix must preserve lexical order and complete schemas",
    );
}

fn native_lowered_body(request: &ConversationRequest, backend: &ApiBackend) -> serde_json::Value {
    match backend {
        ApiBackend::Messages => serde_json::to_value(
            xai_grok_sampling_types::conversation::build_messages_request(request),
        )
        .unwrap(),
        ApiBackend::Responses => {
            serde_json::to_value(xai_grok_sampling_types::rs::CreateResponse::from(request))
                .unwrap()
        }
        ApiBackend::ChatCompletions => serde_json::to_value(
            xai_grok_sampling_types::ChatCompletionRequest::from(request.clone()),
        )
        .unwrap(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn typed_candidate_membership_equals_qualified_mcp_snapshot() {
    tokio::task::LocalSet::new().run_until(async {
        let fixture = native_candidate_fixture(&ApiBackend::Messages, true, &["builtin__renamed"]).await;
        let actor = &fixture.actor;
        register_native_builtins(actor);
        let pair = register_native_pair(actor);
        for name in ["plain_mcp", "bad___overlap", "bad__two__boundaries"] {
            assert!(xai_grok_mcp::servers::parse_mcp_qualified_name(name).is_none());
            register_native_fixture(actor, name, ToolNamespace::MCP, "Parser-invalid typed entry",
                serde_json::json!({"type":"object","properties":{"invalid":{"type":"boolean"}}}));
        }
        assert!(xai_grok_mcp::servers::parse_mcp_qualified_name("builtin__renamed").is_some());
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let _guard = trace_native_snapshots(Some((reached_tx, resume_rx)));
        let prepare = actor.prepare_native_tool_candidates_timed();
        tokio::pin!(prepare);
        tokio::select! {
            reached = reached_rx => reached.unwrap(),
            _ = &mut prepare => panic!("candidate preparation must reach the snapshot gate"),
        }
        let bridge = actor.agent.borrow().tool_bridge().clone();
        assert!(bridge.unregister_tool_by_name("alpha__lookup"));
        register_native_fixture(actor, "beta__new", ToolNamespace::MCP, "Published after captured snapshot",
            serde_json::json!({"type":"object","properties":{"later":{"type":"integer"}}}));
        resume_tx.send(()).unwrap();
        let (prepared, _) = prepare.await;
        let candidates = actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
        let snapshots = observed_native_snapshots();
        assert_eq!(snapshots.len(), 1, "one owned snapshot per candidate preparation");
        let captured = &snapshots[0];
        let typed_names: Vec<_> = captured.iter().filter(|(_, ns)| *ns == ToolNamespace::MCP)
            .map(|(def, _)| def.function.name.as_str()).collect();
        assert_eq!(typed_names, ["zeta__fetch", "alpha__lookup", "plain_mcp", "bad___overlap", "bad__two__boundaries"]);
        let builtins: Vec<_> = captured.iter().filter(|(_, ns)| *ns != ToolNamespace::MCP).map(|(def, _)| def.clone()).collect();
        assert_eq!(serde_json::to_value(&prepared.builtin_defs).unwrap(), serde_json::to_value(&builtins).unwrap());
        assert_eq!(pair.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["alpha__lookup", "zeta__fetch"]);
        assert_native_membership(actor, candidates, captured).await;
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn candidate_removal_and_reregistration_refresh_snapshot() {
    tokio::task::LocalSet::new().run_until(async {
        let fixture = native_candidate_fixture(&ApiBackend::Messages, true, &[]).await;
        let actor = &fixture.actor;
        register_native_builtins(actor);
        let pair = register_native_pair(actor);
        let _guard = trace_native_snapshots(None);
        let (original, _) = actor.prepare_native_tool_candidates_timed().await;
        let original_candidates = actor.turn_native_tool_candidates(&original.builtin_defs, &original.mcp_specs);
        let row = actor.chat_state_handle.capture_transport_binding().await.unwrap().config;
        let original_selected = original_candidates.clone().select_for_row(&row);
        let expected: Vec<_> = actor.turn_base_tool_specs(&original.builtin_defs).into_iter().chain(native_selected_mcp(&pair)).collect();
        assert_native_specs(&original_selected, &expected, "U3 lifecycle starts with the registered qualified schemas");
        let bridge = actor.agent.borrow().tool_bridge().clone();
        assert!(bridge.unregister_tool_by_name("alpha__lookup"));
        let (removed, _) = actor.prepare_native_tool_candidates_timed().await;
        let removed_candidates = actor.turn_native_tool_candidates(&removed.builtin_defs, &removed.mcp_specs);
        let remaining: Vec<_> = actor.turn_base_tool_specs(&original.builtin_defs).into_iter().chain(native_selected_mcp(&pair[1..])).collect();
        assert_native_specs(&removed_candidates.clone().select_for_row(&row), &remaining, "U3 removal must refresh the owned candidate snapshot");
        let replacement = register_native_fixture(actor, "alpha__lookup", ToolNamespace::MCP, "Changed lookup schema",
            serde_json::json!({"type":"object","properties":{"query":{"type":"integer"},"exact":{"type":"boolean"}},"required":["query","exact"],"additionalProperties":false}));
        let (updated, _) = actor.prepare_native_tool_candidates_timed().await;
        let updated_candidates = actor.turn_native_tool_candidates(&updated.builtin_defs, &updated.mcp_specs);
        let changed: Vec<_> = actor.turn_base_tool_specs(&original.builtin_defs).into_iter()
            .chain(native_selected_mcp(&[replacement, pair[1].clone()])).collect();
        assert_native_specs(&updated_candidates.clone().select_for_row(&row), &changed, "U3 reregistration must expose the changed schema in lexical order");
        assert_native_specs(&original_candidates.clone().select_for_row(&row), &original_selected, "U3 an old owned candidate snapshot must remain stable");
        let snapshots = observed_native_snapshots();
        assert_eq!(snapshots.len(), 3);
        for (candidate, snapshot) in [original_candidates, removed_candidates, updated_candidates].into_iter().zip(snapshots) {
            assert_native_membership(actor, candidate, &snapshot).await;
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn typed_mcp_registration_reaches_messages_deferred_declarations() {
    tokio::task::LocalSet::new().run_until(async {
        for (backend, flag) in [
            (ApiBackend::Messages, true), (ApiBackend::Messages, false),
            (ApiBackend::Responses, true), (ApiBackend::Responses, false),
            (ApiBackend::ChatCompletions, true), (ApiBackend::ChatCompletions, false),
        ] {
            let mut fixture = native_candidate_fixture(&backend, flag, &[]).await;
            let mut control = native_candidate_fixture(&backend, flag, &[]).await;
            register_native_builtins(&fixture.actor);
            register_native_builtins(&control.actor);
            let mcp = register_native_pair(&fixture.actor);
            let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
        let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
            let builtin_defs = control.actor.prepare_tool_definitions_inner().await;
            assert_eq!(serde_json::to_value(&prepared.builtin_defs).unwrap(), serde_json::to_value(&builtin_defs).unwrap());
            let builtins = control.actor.turn_base_tool_specs(&builtin_defs);
            assert!(builtins.iter().all(|s| s.exposure == ToolExposure::Immediate));
            assert!(builtins.iter().any(|s| s.name == "search_tool"));
            assert!(builtins.iter().any(|s| s.name == "use_tool"));
            let baseline = native_legacy_request(&control.actor, builtins.clone()).await;
            let (request, binding) = native_bound_request(&fixture.actor, candidates).await;
            assert_binding_eq(&binding, &fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap());
            for events in [&mut fixture.events, &mut control.events] {
                let mut budgets = Vec::new();
                while let Ok(event) = events.try_recv() {
                    if let xai_chat_state::ChatStateEvent::ImageBudget { inline_images, evicted, .. } = event {
                        budgets.push((inline_images, evicted));
                    }
                }
                assert_eq!(budgets, [(1, 0)], "U3 integrated candidate build must invoke the image-budget builder exactly once");
            }
            let eligible = backend == ApiBackend::Messages && flag;
            let body = native_lowered_body(&request, &backend);
            let control_body = native_lowered_body(&baseline, &backend);
            assert_eq!(request.search_admission, baseline.search_admission);
            if eligible {
                assert!(request.search_admission.unwrap().admitted());
                assert!(baseline.search_admission.unwrap().admitted());
                let bm25 = serde_json::json!({"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"});
                let mut placeholders = Vec::new();
                for (wire, client_count) in [(&body, request.tools.len()), (&control_body, baseline.tools.len())] {
                    let tools = wire["tools"].as_array().unwrap();
                    assert_eq!(tools.last(), Some(&bm25), "BM25 alone must not stand in for qualified MCP declarations");
                    let found: Vec<_> = tools.iter().enumerate().filter(|(_, tool)| tool["name"] == "DeferredToolPlaceholder").collect();
                    assert_eq!(found.len(), 1);
                    assert_eq!(found[0].0, client_count);
                    assert_eq!(found[0].1["defer_loading"], true);
                    assert_eq!(tools.len(), client_count + 2, "fresh D2 region is empty before checking MCP declarations");
                    placeholders.push(found[0].1);
                }
                assert_eq!(placeholders[0], placeholders[1], "unchanged placeholder is an anti-vacuity control, not an MCP member");
            }
            let expected: Vec<_> = builtins.iter().cloned().chain(if eligible { native_selected_mcp(&mcp) } else { Vec::new() }).collect();
            assert_native_specs(&request.tools, &expected, "U3 real registration must reach exact qualified deferred declarations");
            assert_eq!(request.search_admission, baseline.search_admission);
            let mut expected_request = baseline.clone();
            expected_request.tools = expected;
            assert_eq!(format!("{request:?}"), format!("{expected_request:?}"), "U3 additive build changes only selected tools from the isolated legacy baseline");
            if !eligible {
                assert_eq!(body, control_body, "U3 non-admitting MCP rows must equal the full isolated legacy body");
                continue;
            }
            assert!(baseline.search_admission.unwrap().admitted(), "builtin-only admission is an anti-vacuity control");
            assert!(request.items.iter().all(|item| matches!(item, ConversationItem::System(_) | ConversationItem::User(_))), "fresh conversation has no D2 materialisations");
            let tools = body["tools"].as_array().unwrap();
            let control_tools = control_body["tools"].as_array().unwrap();
            let placeholder = "DeferredToolPlaceholder";
            let clients: Vec<_> = tools.iter().filter(|tool| tool.get("input_schema").is_some() && tool["name"] != placeholder).cloned().collect();
            let control_clients: Vec<_> = control_tools.iter().filter(|tool| tool.get("input_schema").is_some() && tool["name"] != placeholder).cloned().collect();
            let mut expected_clients = control_clients.clone();
            expected_clients.extend(mcp.iter().map(|spec| serde_json::json!({
                "name": spec.name, "description": spec.description,
                "input_schema": spec.parameters, "defer_loading": true,
            })));
            assert_eq!(clients, expected_clients, "U3 parsed Messages client region must preserve exact builtin objects and qualified schemas/order");
            let deferred_names: Vec<_> = clients.iter().filter(|tool| tool.get("defer_loading").is_some()).map(|tool| {
                assert_eq!(tool["defer_loading"], true);
                tool["name"].as_str().unwrap()
            }).collect();
            assert_eq!(deferred_names, ["alpha__lookup", "zeta__fetch"], "only qualified MCP clients are deferred in the client region");
            assert!(control_clients.iter().all(|tool| tool.get("defer_loading").is_none()));
            let placeholders: Vec<_> = tools.iter().enumerate().filter(|(_, tool)| tool["name"] == placeholder).collect();
            let control_placeholders: Vec<_> = control_tools.iter().enumerate().filter(|(_, tool)| tool["name"] == placeholder).collect();
            assert_eq!(placeholders.len(), 1);
            assert_eq!(control_placeholders.len(), 1);
            assert_eq!(placeholders[0].0, clients.len());
            assert_eq!(control_placeholders[0].0, control_clients.len());
            assert_eq!(placeholders[0].1, control_placeholders[0].1, "inherited placeholder must remain byte-equivalent to the builtin control");
            assert_eq!(placeholders[0].1["defer_loading"], true);
            let bm25 = serde_json::json!({"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"});
            assert_eq!(tools.last(), Some(&bm25));
            assert_eq!(control_tools.last(), Some(&bm25));
            assert_eq!(tools.len(), clients.len() + 2, "D2 region is empty and BM25 follows the single placeholder");
            assert_eq!(control_tools.len(), control_clients.len() + 2);
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn actor_row_changes_both_directions_preserve_operator_snapshot() {
    tokio::task::LocalSet::new().run_until(async {
        let mut actual_cases = Vec::new();
        let mut expected_cases = Vec::new();
        for (prepared_backend, prepared_flag, built_backend, built_flag) in [
            (ApiBackend::Messages, true, ApiBackend::Responses, false),
            (ApiBackend::Responses, false, ApiBackend::Messages, true),
        ] {
            let fixture = native_candidate_fixture(&prepared_backend, prepared_flag, &[]).await;
            let control = native_candidate_fixture(&prepared_backend, prepared_flag, &[]).await;
            register_native_builtins(&fixture.actor);
            register_native_builtins(&control.actor);
            let mcp = register_native_pair(&fixture.actor);
            let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
        let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
            let builtins = fixture.actor.turn_base_tool_specs(&prepared.builtin_defs);
            let mut current = fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap();
            current.config.model = "native-build-row".into();
            current.config.base_url = "https://native-build-row.example/alternate".into();
            current.config.api_backend = built_backend.clone();
            current.config.supports_search_tool = built_flag;
            current.config.temperature = Some(0.375);
            current.config.max_completion_tokens = Some(1234);
            current.credentials.api_key = Some("native-build-row-fixture-key".into());
            fixture.actor.chat_state_handle.install_transport_binding(current.clone()).await.unwrap();
            control.actor.chat_state_handle.install_transport_binding(current.clone()).await.unwrap();
            let (request, binding) = native_bound_request(&fixture.actor, candidates).await;
            assert_binding_eq(&binding, &current);
            assert_eq!(request.model.as_deref(), Some(current.config.model.as_str()));
            let expected: Vec<_> = builtins.into_iter().chain(if built_backend == ApiBackend::Messages && built_flag {
                native_selected_mcp(&mcp)
            } else { Vec::new() }).collect();
            let legacy = native_legacy_request(&control.actor, expected.clone()).await;
            let context = format!("{prepared_backend:?}/{prepared_flag} -> {built_backend:?}/{built_flag}");
            actual_cases.push(serde_json::json!({"case":context,"tools":request.tools,"body":native_lowered_body(&request, &built_backend)}));
            expected_cases.push(serde_json::json!({"case":context,"tools":expected,"body":native_lowered_body(&legacy, &built_backend)}));
            assert_eq!(request.search_admission, legacy.search_admission);
        }
        assert_eq!(actual_cases, expected_cases, "U3 captured-row cases must select real prepared candidates in both switch directions");

        let fixture = native_candidate_fixture(&ApiBackend::Messages, true, &["z_builtin"]).await;
        fixture.actor.agent.replace(support::test_agent_backend_search(vec![xai_grok_sampling_types::HostedTool::WebSearch { options: None }]).await);
        fixture.actor.supports_backend_search.set(true);
        register_native_builtins(&fixture.actor);
        register_native_fixture(&fixture.actor, "web_search", ToolNamespace::GrokBuild, "Prepared web builtin",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}));
        register_native_pair(&fixture.actor);
        let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
        let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
        let mut non_messages = fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap().config;
        non_messages.api_backend = ApiBackend::Responses;
        let builtin_snapshot = candidates.clone().select_for_row(&non_messages);
        assert!(builtin_snapshot.iter().any(|s| s.name == "z_builtin" && s.exposure == ToolExposure::Deferred));
        assert!(builtin_snapshot.iter().all(|s| s.name != "web_search"));
        let mut current = fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap();
        current.config.model = "native-build-row".into();
        current.config.api_backend = ApiBackend::Responses;
        current.config.supports_search_tool = false;
        current.config.base_url = "https://operator-target.example/v1".into();
        current.credentials.api_key = Some("operator-target-fixture-key".into());
        fixture.actor.chat_state_handle.install_transport_binding(current.clone()).await.unwrap();
        fixture.actor.models_manager.insert_test_entry("native-prepared", support::entry_with_deferred_tools("native-prepared", false, &["builtin__renamed"]));
        fixture.actor.models_manager.insert_test_entry("native-build-row", support::entry_with_deferred_tools("native-build-row", false, &["builtin__renamed"]));
        fixture.actor.models_manager.set_current_model_id(agent_client_protocol::ModelId::new("native-build-row"));
        fixture.actor.supports_backend_search.set(false);
        let (request, binding) = native_bound_request(&fixture.actor, candidates).await;
        assert_binding_eq(&binding, &current);
        assert_native_specs(&request.tools, &builtin_snapshot, "U3 operator exposure and backend-search filtering remain the preparation snapshot after catalog and actor changes");
        assert!(!request.search_admission.unwrap().admitted());

        let fixture = native_candidate_fixture(&ApiBackend::Messages, true, &[]).await;
        let bridge = fixture.actor.agent.borrow().tool_bridge().clone();
        for (definition, namespace) in bridge.toolset().tools_namespaced() {
            if namespace != ToolNamespace::MCP {
                assert!(bridge.unregister_tool_by_name(&definition.function.name));
            }
        }
        register_native_pair(&fixture.actor);
        let (prepared, _) = fixture.actor.prepare_native_tool_candidates_timed().await;
        let candidates = fixture.actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs);
        assert!(prepared.builtin_defs.is_empty());
        let mut current = fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap();
        current.config.api_backend = ApiBackend::Responses;
        current.config.supports_search_tool = true;
        fixture.actor.chat_state_handle.install_transport_binding(current.clone()).await.unwrap();
        let (request, binding) = native_bound_request(&fixture.actor, candidates).await;
        assert_binding_eq(&binding, &current);
        assert!(request.tools.is_empty());
        assert!(!request.search_admission.unwrap().admitted(), "U3 admission must use the selected empty surface, not the candidate inventory");
    }).await;
}

async fn install_native_loop_row(
    actor: &std::sync::Arc<super::SessionActor>,
    model: &str,
    base_url: String,
    backend_search: bool,
) -> RequestTransportBinding {
    actor
        .models_manager
        .set_current_model_id(agent_client_protocol::ModelId::new(model));
    actor
        .handle_set_session_model(
            xai_grok_sampler::SamplerConfig {
                model: model.into(),
                base_url,
                api_key: Some(format!("{model}-fixture-key")),
                api_backend: ApiBackend::Messages,
                supports_backend_search: backend_search,
                context_window: 256_000,
                max_retries: Some(0),
                ..Default::default()
            },
            false,
            false,
            false,
            false,
            85,
        )
        .await
        .unwrap();
    actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn normal_turn_reprepares_non_mcp_tools_after_model_switch() {
    use super::super::rate_limit_backoff_tests::{drain_gateway, drain_persistence};
    use std::sync::Arc;
    use xai_grok_test_support::{
        InferenceEndpoint, InferenceRequestMatcher, MockInferenceServer, MockModelEntry,
        ScriptedResponse,
    };

    tokio::task::LocalSet::new().run_until(async {
        for first_backend_search in [true, false] {
            let server_a = MockInferenceServer::start_with_models(vec![MockModelEntry::new("native-loop-a")]).await.unwrap();
            let server_b = MockInferenceServer::start_with_models(vec![MockModelEntry::new("native-loop-b")]).await.unwrap();
            let mut first_response = server_a.expect_response_blocked(
                "first-normal-iteration",
                InferenceRequestMatcher::foreground(InferenceEndpoint::Messages),
                ScriptedResponse::json(503, serde_json::json!({"error":"retry on the next iteration"})),
            );
            let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            drain_gateway(gateway_rx);
            drain_persistence(persistence_rx);
            let mut actor = support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let hosted = vec![xai_grok_sampling_types::HostedTool::WebSearch { options: Some(WebSearchOptions {
                allowed_domains: Some(vec!["example.com".into()]), excluded_domains: None,
            }) }];
            actor.agent.replace(support::test_agent_backend_search(hosted.clone()).await);
            register_native_builtins(&actor);
            register_native_fixture(&actor, "web_search", ToolNamespace::GrokBuild, "Prepared web builtin",
                serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}));
            let mcp = register_native_pair(&actor);
            let builtins: Vec<_> = actor.prepare_tool_definitions_inner().await.into_iter().map(ToolSpec::from).collect();
            assert_eq!(builtins.iter().map(|spec| spec.name.as_str()).collect::<Vec<_>>(), ["z_builtin", "builtin__renamed", "web_search"]);
            let (manager, _home) = support::manager_with_entries();
            manager.insert_test_entry("native-loop-a", support::entry_with_deferred_tools("native-loop-a", true, &["z_builtin"]));
            manager.insert_test_entry("native-loop-b", support::entry_with_deferred_tools("native-loop-b", true, &["builtin__renamed"]));
            actor.models_manager = manager;
            actor.max_retries = 0;
            actor.transient_retry_enabled = true;
            assert!(actor.forked_tool_override.is_none());
            assert!(!actor.startup_hints.is_subagent);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
                xai_grok_sampler::SamplerConfig { base_url: server_a.url(), model: "native-loop-a".into(), api_backend: ApiBackend::Messages, max_retries: Some(0), ..Default::default() },
                xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, event_tx,
            );
            actor.workspace_ops.bind_local_session(
                &actor.session_id_string(), actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(), actor.agent.borrow().tool_bridge().toolset(), None,
            ).unwrap();
            let actor = Arc::new(actor);
            actor.state.lock().await.running_task = Some(support::running_task_stub("native-loop"));
            let a = install_native_loop_row(&actor, "native-loop-a", server_a.url(), first_backend_search).await;
            actor.chat_state_handle.replace_conversation(vec![ConversationItem::user("answer after the row switch")]);
            server_b.set_response("normal row-switch completed");
            let drainer = actor.clone();
            let drain_task = tokio::task::spawn_local(async move {
                while let Some(event) = event_rx.recv().await { drainer.handle_sampling_event(event).await; }
            });
            OUTER_BOUND_TRACE.with(|trace| {
                assert!(trace.borrow().is_none());
                *trace.borrow_mut() = Some(OuterBoundTrace { install_after_first: None, builds: Vec::new() });
            });
            let _outer_guard = OuterBoundGuard;
            let _snapshot_guard = trace_native_snapshots(None);
            let mut salvage = super::super::length_salvage::LengthSalvage::new(None);
            let mut sampling = Default::default();
            let mut turn = Box::pin(actor.process_conversation_turn_with_recovery(
                "native-loop", None, None, None, &mut salvage, &mut sampling,
            ));
            tokio::select! {
                _ = first_response.wait_blocked() => {},
                result = &mut turn => panic!("first request must reach its response gate: {:?}", result.err()),
            }
            assert_eq!(server_a.messages_request_count(), 1);
            assert_eq!(server_b.messages_request_count(), 0);
            let b = install_native_loop_row(&actor, "native-loop-b", server_b.url(), !first_backend_search).await;
            first_response.release();
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), turn).await.expect("normal turn finishes");
            drain_task.abort();
            let _ = drain_task.await;
            assert!(outcome.is_ok(), "normal turn must complete after one outer retry: {:?}", outcome.err());
            first_response.assert_satisfied();
            assert_eq!(server_a.messages_request_count(), 1);
            assert_eq!(server_b.messages_request_count(), 1);
            let builds = OUTER_BOUND_TRACE.with(|trace| std::mem::take(&mut trace.borrow_mut().as_mut().unwrap().builds));
            assert_eq!(builds.len(), 2, "exactly one bound build for each normal iteration");
            for (index, ((request, binding), (server, expected_binding, backend_search, deferred))) in builds.iter().zip([
                (&server_a, &a, first_backend_search, "z_builtin"),
                (&server_b, &b, !first_backend_search, "builtin__renamed"),
            ]).enumerate() {
                assert_binding_eq(binding, expected_binding);
                assert!(request.search_admission.unwrap().admitted());
                let expected_tools: Vec<_> = builtins.iter()
                    .filter(|spec| !backend_search || spec.name != "web_search")
                    .cloned().map(|mut spec| {
                        spec.exposure = if spec.name == deferred { ToolExposure::Deferred } else { ToolExposure::Immediate };
                        spec
                    }).chain(native_selected_mcp(&mcp)).collect();
                assert_native_specs(&request.tools, &expected_tools, if index == 0 {
                    "U3-R1 first normal iteration must send the registered qualified MCP declarations"
                } else {
                    "U3-R1 next normal iteration must reprepare builtin filtering and deferral from its new row"
                });
                let expected_hosted = if backend_search { hosted.clone() } else { Vec::new() };
                let mut expected_request = request.clone();
                expected_request.tools = expected_tools;
                expected_request.hosted_tools = expected_hosted;
                let expected_body = native_lowered_body(&expected_request, &ApiBackend::Messages);
                let sent = server.requests().into_iter().find(|sent| sent.path == "/v1/messages").unwrap();
                assert_eq!(sent.authorization, Some(format!("Bearer {}-fixture-key", expected_binding.config.model)));
                let body = sent.body.unwrap();
                assert_eq!(body["model"], expected_binding.config.model);
                assert_eq!(body["tools"], expected_body["tools"], "U3-R1 complete sent client and hosted tools must agree with each iteration's row");
                let clients = body["tools"].as_array().unwrap();
                for spec in &mcp {
                    let declared: Vec<_> = clients.iter().filter(|tool| tool["name"] == spec.name).collect();
                    assert_eq!(declared, vec![&serde_json::json!({"name":spec.name,"description":spec.description,"input_schema":spec.parameters,"defer_loading":true})]);
                }
            }
            assert_eq!(observed_native_snapshots().len(), 1, "the registry snapshot stays once per turn across a row switch");
        }
    }).await;
}

#[derive(Debug, Clone)]
struct ChildRootMessageTool;

impl xai_grok_tools::types::tool_metadata::ToolMetadata for ChildRootMessageTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::ActiveAgentMessage
    }
    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Root-only child-filter discriminator"
    }
}

impl xai_tool_runtime::Tool for ChildRootMessageTool {
    type Args = serde_json::Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new("policy__root_message").unwrap()
    }
    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            "policy__root_message",
            "Root-only child-filter discriminator",
        )
    }
    async fn run(
        &self,
        _ctx: xai_tool_runtime::ToolCallContext,
        _args: serde_json::Value,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        panic!("child declaration fixtures never execute tools")
    }
}

fn child_structured_output_spec() -> ToolSpec {
    ToolSpec {
        name: "StructuredOutput".into(),
        description: Some(
            "Return your final answer as JSON matching the required schema. \
             Call this exactly once, at the end."
                .into(),
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false,
        }),
        exposure: ToolExposure::Immediate,
    }
}

fn assert_child_candidate_membership(
    candidates: NativeToolCandidates,
    config: &xai_grok_sampling_types::SamplingConfig,
    admitted: &[ToolSpec],
    excluded: &[ToolSpec],
    expected_members: &[&str],
    marker: &str,
) {
    use std::collections::BTreeSet;

    let mut row = config.clone();
    row.api_backend = ApiBackend::Messages;
    row.supports_search_tool = true;
    let actual_admitted = candidates.clone().select_for_row(&row);
    row.api_backend = ApiBackend::Responses;
    let actual_excluded = candidates.clone().select_for_row(&row);
    let other_names: BTreeSet<_> = actual_excluded
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    let dropped: BTreeSet<_> = actual_admitted
        .iter()
        .map(|spec| spec.name.as_str())
        .filter(|name| !other_names.contains(name))
        .collect();
    let operator_deferred: BTreeSet<_> = excluded
        .iter()
        .filter(|spec| spec.exposure == ToolExposure::Deferred)
        .map(|spec| spec.name.as_str())
        .collect();
    let deferred: BTreeSet<_> = actual_admitted
        .iter()
        .filter(|spec| spec.exposure == ToolExposure::Deferred)
        .map(|spec| spec.name.as_str())
        .filter(|name| !operator_deferred.contains(name))
        .collect();
    let expected: BTreeSet<_> = expected_members.iter().copied().collect();
    assert_eq!(dropped, expected, "{marker}: exclusion proxy");
    assert_eq!(deferred, expected, "{marker}: Deferred proxy");
    assert_native_specs(&actual_admitted, admitted, marker);
    assert_native_specs(&actual_excluded, excluded, marker);
    row.api_backend = ApiBackend::Messages;
    row.supports_search_tool = false;
    assert_native_specs(&candidates.select_for_row(&row), excluded, marker);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rebuilt_children_retain_only_surviving_mcp_candidates() {
    use super::super::rate_limit_backoff_tests::{drain_gateway, drain_persistence};
    use std::sync::Arc;
    use xai_grok_test_support::{MockInferenceServer, MockModelEntry, ScriptedResponse};

    tokio::task::LocalSet::new()
        .run_until(async {
            for flag in [true, false] {
                let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("rebuilt-child")])
                    .await
                    .unwrap();
                let structured = child_structured_output_spec();
                server.enqueue_response(
                    "/v1/messages",
                    ScriptedResponse::sse(xai_grok_test_support::sse::messages_api_tool_use_events(
                        "child-output", "StructuredOutput", r#"{"answer":"done"}"#, "rebuilt-child",
                    )),
                );
                let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
                let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
                drain_gateway(gateway_rx);
                drain_persistence(persistence_rx);
                let mut actor = support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
                actor.agent.replace(support::test_agent_with_tools(Vec::new()).await);
                actor.startup_hints.is_subagent = true;
                actor.startup_hints.subagent_type = Some("explore".into());
                assert!(actor.forked_tool_override.is_none());
                let (manager, _home) = support::manager_with_entries();
                manager.insert_test_entry("rebuilt-child", support::entry_with_deferred_tools(
                    "rebuilt-child", flag, &["child_operator"],
                ));
                manager.set_current_model_id(agent_client_protocol::ModelId::new("rebuilt-child"));
                actor.models_manager = manager;
                let mut operator = register_native_fixture(
                    &actor, "child_operator", ToolNamespace::GrokBuild,
                    "Operator-prepared child builtin", native_zeta_schema(),
                );
                operator.exposure = ToolExposure::Deferred;
                let delimiter = register_native_fixture(
                    &actor, "child__builtin", ToolNamespace::GrokBuild,
                    "Child delimiter builtin", serde_json::json!({"type":"object","properties":{"confirm":{"type":"boolean"}},"required":["confirm"],"additionalProperties":false}),
                );
                let filtered_schema = serde_json::json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false});
                let bridge = actor.agent.borrow().tool_bridge().clone();
                bridge.register_mcp_tools("policy__root_message".into(), ChildRootMessageTool, Some(filtered_schema.clone())).unwrap();
                assert_eq!(bridge.tool_kind("policy__root_message"), Some(xai_grok_tools::types::tool::ToolKind::ActiveAgentMessage));
                let survivor = register_native_fixture(
                    &actor, "child__lookup", ToolNamespace::MCP,
                    "Eligible rebuilt child lookup", native_alpha_schema(),
                );
                let mut binding = actor.chat_state_handle.capture_transport_binding().await.unwrap();
                binding.config.model = "rebuilt-child".into();
                binding.config.base_url = server.url();
                binding.config.api_backend = ApiBackend::Messages;
                binding.config.supports_search_tool = flag;
                binding.config.max_retries = Some(0);
                binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
                binding.credentials.api_key = Some("rebuilt-child-fixture-key".into());
                actor.chat_state_handle.install_transport_binding(binding.clone()).await.unwrap();
                actor.chat_state_handle.replace_conversation(vec![ConversationItem::user("return the child answer")]);
                let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
                actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
                    actor.turn_sampler_config_from_binding(&binding).await,
                    xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, event_tx,
                );
                actor.workspace_ops.bind_local_session(
                    &actor.session_id_string(), actor.tool_context.cwd.as_path().to_path_buf(),
                    actor.tool_context.hunk_tracker_handle.clone(), bridge.toolset(), None,
                ).unwrap();
                let actor = Arc::new(actor);
                let drainer = actor.clone();
                let drain_task = tokio::task::spawn_local(async move {
                    while let Some(event) = event_rx.recv().await { drainer.handle_sampling_event(event).await; }
                });
                OUTER_BOUND_TRACE.with(|trace| {
                    assert!(trace.borrow().is_none());
                    *trace.borrow_mut() = Some(OuterBoundTrace { install_after_first: None, builds: Vec::new() });
                });
                let _outer_guard = OuterBoundGuard;
                let _snapshot_guard = trace_native_snapshots(None);
                let mut salvage = super::super::length_salvage::LengthSalvage::new(None);
                let mut sampling = Default::default();
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    actor.process_conversation_turn_with_recovery(
                        "rebuilt-child", None, None, Some(structured.parameters.clone()), &mut salvage, &mut sampling,
                    ),
                ).await.expect("rebuilt child turn completes");
                drain_task.abort();
                let _ = drain_task.await;
                assert!(outcome.is_ok(), "rebuilt child turn must complete: {:?}", outcome.err());
                let builds = OUTER_BOUND_TRACE.with(|trace| std::mem::take(&mut trace.borrow_mut().as_mut().unwrap().builds));
                assert_eq!(builds.len(), 1, "one actual bound build per child turn");
                let (request, actual_binding) = &builds[0];
                assert_binding_eq(actual_binding, &binding);
                let admitted: Vec<_> = [operator.clone(), delimiter.clone()]
                    .into_iter().chain(native_selected_mcp(std::slice::from_ref(&survivor)))
                    .chain([structured.clone()]).collect();
                let excluded = vec![operator, delimiter, structured.clone()];
                let expected = if flag { &admitted } else { &excluded };
                assert_native_specs(&request.tools, expected,
                    "U4 rebuilt bound build keeps the eligible MCP survivor and exact structured output");
                assert_eq!(request.search_admission.unwrap().admitted(), flag);
                assert_eq!(server.messages_request_count(), 1);
                let sent = server.requests().into_iter().find(|sent| sent.path == "/v1/messages").unwrap();
                assert_eq!(sent.authorization.as_deref(), Some("Bearer rebuilt-child-fixture-key"));
                let body = sent.body.unwrap();
                assert_eq!(body["tools"], native_lowered_body(request, &ApiBackend::Messages)["tools"],
                    "U4 rebuilt child send uses the selected bound tools");
                let structured_wire: Vec<_> = body["tools"].as_array().unwrap().iter()
                    .filter(|tool| tool["name"] == "StructuredOutput").collect();
                assert_eq!(structured_wire, vec![&serde_json::json!({
                    "name": structured.name, "description": structured.description, "input_schema": structured.parameters,
                })], "U4 structured output remains Immediate and byte-exact on the wire");
                assert_eq!(observed_native_snapshots().len(), 1,
                    "rebuilt children use the single post-wait typed snapshot");

                let (prepared, _) = actor.prepare_native_tool_candidates_timed().await;
                assert_eq!(prepared.mcp_specs.iter().map(|spec| spec.name.as_str()).collect::<Vec<_>>(),
                    ["child__lookup", "policy__root_message"]);
                let projected = actor.tool_candidates_for_turn(
                    actor.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs),
                    Some(structured.parameters.clone()),
                );
                assert_child_candidate_membership(
                    projected.clone(), &binding.config, &admitted, &excluded, &["child__lookup"],
                    "U4 rebuilt membership contains only the surviving typed candidate",
                );
                let replacement = ToolSpec {
                    name: "policy__root_message".into(),
                    description: Some("Non-MCP append after child filtering".into()),
                    parameters: serde_json::json!({"type":"object","properties":{"accepted":{"type":"boolean"}},"required":["accepted"],"additionalProperties":false}),
                    exposure: ToolExposure::Immediate,
                };
                let replaced = projected.map_specs(|mut specs| {
                    specs.push(replacement.clone());
                    specs
                });
                let mut replaced_admitted = admitted;
                replaced_admitted.push(replacement.clone());
                let mut replaced_excluded = excluded;
                replaced_excluded.push(replacement);
                assert_child_candidate_membership(
                    replaced, &binding.config, &replaced_admitted, &replaced_excluded, &["child__lookup"],
                    "U4 filtered membership is pruned before same-name non-MCP append",
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn definition_display_consumers_remain_builtins_only() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for (backend, flag) in [
                (ApiBackend::Messages, true),
                (ApiBackend::Messages, false),
                (ApiBackend::Responses, true),
                (ApiBackend::Responses, false),
            ] {
                let baseline =
                    native_candidate_fixture(&backend, flag, &["builtin__renamed"]).await;
                let candidate =
                    native_candidate_fixture(&backend, flag, &["builtin__renamed"]).await;
                for fixture in [&baseline, &candidate] {
                    fixture.actor.agent.replace(
                        support::test_agent_backend_search(vec![
                            xai_grok_sampling_types::HostedTool::WebSearch { options: None },
                        ])
                        .await,
                    );
                    register_native_builtins(&fixture.actor);
                    register_native_fixture(
                        &fixture.actor,
                        "web_search",
                        ToolNamespace::GrokBuild,
                        "Display web builtin",
                        native_alpha_schema(),
                    );
                }
                let mcp = register_native_pair(&candidate.actor);
                let (prepared, _) = candidate.actor.prepare_native_tool_candidates_timed().await;
                assert_native_specs(
                    &prepared.mcp_specs,
                    &mcp,
                    "U5 display fixture has real qualified typed candidates",
                );
                for backend_search in [false, true] {
                    for fixture in [&baseline, &candidate] {
                        fixture.actor.supports_backend_search.set(backend_search);
                    }
                    let expected = baseline.actor.prepare_tool_definitions_inner().await;
                    assert_eq!(
                        expected
                            .iter()
                            .map(|definition| definition.function.name.as_str())
                            .collect::<Vec<_>>(),
                        ["z_builtin", "builtin__renamed", "web_search"]
                    );
                    let expected_json = serde_json::to_value(&expected).unwrap();
                    assert_eq!(
                        serde_json::to_value(
                            candidate.actor.prepare_tool_definitions_inner().await
                        )
                        .unwrap(),
                        expected_json,
                        "U5 inner definition preparation remains exact and builtin-only"
                    );
                    assert_eq!(
                        serde_json::to_value(candidate.actor.prepare_tool_definitions().await)
                            .unwrap(),
                        expected_json,
                        "U5 waited definition preparation remains exact and builtin-only"
                    );
                    let (timed, _) = candidate.actor.prepare_tool_definitions_timed().await;
                    assert_eq!(
                        serde_json::to_value(timed).unwrap(),
                        expected_json,
                        "U5 timed definition preparation remains exact and builtin-only"
                    );
                    let specs = baseline.actor.turn_base_tool_specs(&expected);
                    assert!(specs.iter().any(|spec| spec.name == "builtin__renamed"
                        && spec.exposure == ToolExposure::Deferred));
                    assert_eq!(
                        specs.iter().any(|spec| spec.name == "web_search"),
                        !backend_search
                    );
                    let displayed: Vec<_> = expected
                        .into_iter()
                        .filter(|definition| {
                            !backend_search || definition.function.name != "web_search"
                        })
                        .collect();
                    let expected_counts = (
                        displayed.len() as u64,
                        xai_chat_state::estimate_tool_definitions_tokens(&displayed),
                    );
                    let baseline_info = baseline.actor.build_session_info().await;
                    let candidate_info = candidate.actor.build_session_info().await;
                    assert_eq!(
                        (
                            baseline_info.context.tool_definitions_count,
                            baseline_info.context.tool_definitions_tokens
                        ),
                        expected_counts
                    );
                    assert_eq!(
                        (
                            candidate_info.context.tool_definitions_count,
                            candidate_info.context.tool_definitions_tokens
                        ),
                        expected_counts,
                        "U5 session info counts and tokens contain only exact builtin definitions"
                    );
                    assert_eq!(candidate_info.model, baseline_info.model);
                    assert_eq!(candidate_info.api_backend, baseline_info.api_backend);
                }
            }
        })
        .await;
}
