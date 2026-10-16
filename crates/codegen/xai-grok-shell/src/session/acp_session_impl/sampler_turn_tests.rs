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
