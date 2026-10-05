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

                let queued = tokio::time::timeout(
                    Duration::from_millis(150),
                    subagent.submit_turn_request(Default::default()),
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

                let ran = tokio::time::timeout(
                    Duration::from_millis(150),
                    main.submit_turn_request(Default::default()),
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
