//! The turn-end hooks, driven through the real cancel and dispatch paths.

use super::support::*;
use super::turn_end_hooks::ReportOutcome;
use super::turn_end_hooks::TurnEnd;
use super::turn_report_slot::{CommitOutcome, TurnReportState};
use super::*;
use crate::session::CancelTrigger;
use xai_grok_hooks::event::{HookEventName, StopCancelledReason, StopFailureKind};

#[derive(Default)]
pub(super) struct RecordingLifecycle {
    pub(super) aborts: std::cell::Cell<usize>,
    idles: std::cell::Cell<usize>,
}

#[async_trait::async_trait(?Send)]
impl xai_agent_lifecycle::LocalTurnLifecycleContributor for RecordingLifecycle {
    async fn on_turn_abort(&self, _input: &xai_agent_lifecycle::TurnAbortInput) {
        self.aborts.set(self.aborts.get() + 1);
    }
}

#[async_trait::async_trait(?Send)]
impl xai_agent_lifecycle::LocalSessionLifecycleContributor for RecordingLifecycle {
    async fn on_session_idle(&self, _input: &xai_agent_lifecycle::SessionIdleInput) {
        self.idles.set(self.idles.get() + 1);
    }
}

struct Harness {
    actor: Arc<SessionActor>,
    lifecycle: std::rc::Rc<RecordingLifecycle>,
    gateway: Option<tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>>,
    events: Option<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>,
    /// Stands in for the one `run_session` owns; [`Self::spawn_loop`] retires it.
    queue: Option<super::turn_end_hooks::TurnEndQueue>,
    /// Held only so the loop does not see its chat channel close.
    chat: Option<tokio::sync::mpsc::UnboundedSender<xai_chat_state::ChatStateEvent>>,
    hook_workspace: Option<tempfile::TempDir>,
}

impl Harness {
    async fn new() -> Self {
        Self::build(false, None).await
    }

    async fn subagent() -> Self {
        Self::build(true, None).await
    }

    async fn with_hook_workspace() -> Self {
        Self::build(false, Some(tempfile::TempDir::new().expect("hook cwd"))).await
    }

    async fn build(is_subagent: bool, hook_workspace: Option<tempfile::TempDir>) -> Self {
        let (gateway_tx, gateway) = tokio::sync::mpsc::unbounded_channel();
        let (persistence_tx, mut persistence) =
            tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
        // Drop each message rather than hold the queue: that drops any ack channel inside it
        // A writer waiting on one then fails fast instead of waiting forever
        tokio::task::spawn_local(async move { while persistence.recv().await.is_some() {} });
        let (mut actor, events) =
            create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
        if let Some(dir) = hook_workspace.as_ref() {
            actor.hook_resolved_workspace_root = dir.path().to_string_lossy().into_owned();
        }
        actor.startup_hints.is_subagent = is_subagent;
        if is_subagent {
            actor.startup_hints.subagent_type = Some("explore".into());
        }
        let lifecycle = std::rc::Rc::new(RecordingLifecycle::default());
        let mut builder = xai_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        builder.turn_lifecycle_contributor(lifecycle.clone());
        builder.session_lifecycle_contributor(lifecycle.clone());
        actor.extension_registry = builder.build();
        let actor = Arc::new(actor);
        Self {
            queue: Some(super::turn_end_hooks::TurnEndQueue::spawn(actor.clone())),
            actor,
            lifecycle,
            gateway: Some(gateway),
            events: Some(events),
            chat: None,
            hook_workspace,
        }
    }

    /// Drains without respawning the queue, leaving the next report nowhere to go.
    async fn close_turn_end_queue(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
    }

    /// Runs every queued report, then respawns the queue, so a test never races the worker.
    async fn drain_turn_ends(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
        self.queue = Some(super::turn_end_hooks::TurnEndQueue::spawn(
            self.actor.clone(),
        ));
    }

    /// Drives the real `run_session`.
    /// Takes the gateway, whose notifications must be acknowledged or the actor blocks; hook events land in the returned buffer.
    async fn spawn_loop(
        &mut self,
    ) -> (
        tokio::sync::mpsc::UnboundedSender<SessionCommand>,
        std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    ) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        self.chat = Some(chat_tx);
        let events = self.events.take().expect("one loop per harness");
        let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = fired.clone();
        let mut gateway = self.gateway.take().expect("the loop owns the gateway");
        tokio::task::spawn_local(async move {
            while let Some(msg) = gateway.recv().await {
                match msg {
                    xai_acp_lib::AcpClientMessage::ExtNotification(args) => {
                        if args.request.method.as_ref() == "x.ai/hooks/event" {
                            sink.borrow_mut()
                                .push(serde_json::from_str(args.request.params.get()).unwrap());
                        }
                    }
                    xai_acp_lib::AcpClientMessage::SessionNotification(args) => {
                        let _ = args.response_tx.send(Ok(()));
                    }
                    _ => {}
                }
            }
        });
        tokio::task::spawn_local(super::run_session(
            self.actor.clone(),
            cmd_rx,
            chat_rx,
            events,
            None,
            Arc::new(parking_lot::Mutex::new(
                xai_grok_workspace::file_system::CodebaseIndexManager::new(),
            )),
            std::path::PathBuf::from("/tmp"),
            crate::session::fs_watch::FsWatchCapabilities::none(),
        ));
        (cmd_tx, fired)
    }

    fn listen(&self, events: &[HookEventName]) {
        let mut hooks = crate::extensions::hooks::ClientHooks::new();
        for event in events {
            hooks.insert(
                *event,
                vec![crate::extensions::hooks::ClientHookGroup {
                    matcher: None,
                    callback_ids: vec!["cb".to_string()],
                    timeout: None,
                }],
            );
        }
        *self.actor.client_hooks.borrow_mut() = hooks;
    }

    /// A running turn whose prompt is queued at the front, which is what makes a completion this actor's own.
    async fn queue_turn(
        &self,
        prompt_id: &str,
    ) -> tokio::sync::oneshot::Receiver<PromptTurnResult> {
        let (item, rx) = super::turn_completion_emit_tests::pending_input(prompt_id);
        let handle = tokio::task::spawn_local(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        })
        .abort_handle();
        self.run_turn(prompt_id, item, handle).await;
        rx
    }

    async fn start_turn(&self, prompt_id: &str) {
        let handle = tokio::task::spawn_local(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        })
        .abort_handle();
        self.run_turn(prompt_id, user_item(prompt_id, "test"), handle)
            .await;
    }

    async fn run_turn(&self, prompt_id: &str, item: InputItem, handle: tokio::task::AbortHandle) {
        *self
            .actor
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned") = Some(prompt_id.to_string());
        let mut state = self.actor.state.lock().await;
        state.running_task = Some(AgentTask::new_at_epoch(
            prompt_id,
            self.actor.turn_report.epoch(),
            handle,
        ));
        state.pending_inputs.push_back(item);
    }

    async fn cancel(&self, trigger: CancelTrigger) -> super::cancel::CancelOutcome {
        self.cancel_with(trigger, true).await
    }

    async fn cancel_with(
        &self,
        trigger: CancelTrigger,
        cancel_subagents: bool,
    ) -> super::cancel::CancelOutcome {
        self.actor
            .cancel_running_task(crate::session::CancelOptions {
                cancel_subagents,
                trigger: Some(trigger),
                user_initiated: true,
                ..Default::default()
            })
            .await
    }

    fn fired(&mut self) -> Vec<String> {
        self.fired_payloads()
            .iter()
            .filter_map(|p| p["hookEventName"].as_str().map(str::to_string))
            .collect()
    }

    fn fired_payloads(&mut self) -> Vec<serde_json::Value> {
        let gateway = self
            .gateway
            .as_mut()
            .expect("the loop owns the gateway; assert on the sink it returns");
        let mut events = Vec::new();
        while let Ok(msg) = gateway.try_recv() {
            if let xai_acp_lib::AcpClientMessage::ExtNotification(args) = msg
                && args.request.method.as_ref() == "x.ai/hooks/event"
            {
                events.push(serde_json::from_str(args.request.params.get()).unwrap());
            }
        }
        events
    }
}

async fn run(test: impl std::future::Future<Output = ()>) {
    tokio::task::LocalSet::new().run_until(test).await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_a_turn_reports_stop_cancelled_once() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled, HookEventName::StopFailure]);
        h.actor
            .chat_state_handle
            .push_assistant_response(ConversationItem::assistant("partway through"));
        h.start_turn("p1").await;

        let outcome = h.cancel(CancelTrigger::CtrlC).await;
        assert!(outcome.turn_stopped);
        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["hookEventName"], "stop_cancelled");
        assert_eq!(fired[0]["reason"], "user_interrupt");
        assert_eq!(fired[0]["cancelTrigger"], "ctrl_c");
        assert_eq!(fired[0]["lastAssistantMessage"], "partway through");

        let second = h.actor.claim_and_queue(
            "p1",
            h.actor.turn_report.epoch(),
            TurnEnd::Failed {
                error: StopFailureKind::Unknown,
                error_details: None,
                last_assistant_message: None,
            },
        );
        assert_eq!(
            second,
            ReportOutcome::AlreadyReported,
            "the turn's one report is used up"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_a_parked_stop_gate_still_reports() {
    for cancel_subagents in [true, false] {
        run(async move {
            let mut h = Harness::new().await;
            h.listen(&[HookEventName::Stop, HookEventName::StopCancelled]);

            let actor = h.actor.clone();
            let gate = tokio::task::spawn_local(async move { actor.run_stop_gate("p1", 0).await });
            h.run_turn("p1", user_item("p1", "test"), gate.abort_handle())
                .await;
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !matches!(h.actor.turn_report.state(), TurnReportState::Held { .. }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the gate must reach its claim");
            assert!(!gate.is_finished());

            let _ = h.cancel_with(CancelTrigger::CtrlC, cancel_subagents).await;
            h.drain_turn_ends().await;
            assert_eq!(h.fired(), ["stop_cancelled"], "{cancel_subagents}");
        })
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancel_after_a_committed_gate_reports_nothing() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let claim = h
            .actor
            .turn_report
            .claim_for_gate()
            .expect("the gate claims");
        assert_eq!(claim.commit(), CommitOutcome::Reported, "the gate commits");

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert!(h.fired_payloads().is_empty(), "the turn already reported");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_the_turn_after_a_reported_one_still_reports() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert_eq!(h.fired(), ["stop_cancelled"]);

        {
            let mut state = h.actor.state.lock().await;
            state.pending_inputs.push_back(user_item("p2", "alice"));
        }
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        h.actor
            .clone()
            .maybe_start_running_task(completion_tx)
            .await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert_eq!(
            h.fired(),
            ["stop_cancelled"],
            "a turn interrupted before its first poll still reports"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_late_report_cannot_spend_the_next_turns_slot() {
    run(async {
        let h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);

        let epoch = h.actor.turn_report.epoch();
        h.actor.turn_report.start_next_turn();

        let late = h.actor.claim_and_queue(
            "p1",
            epoch,
            TurnEnd::Cancelled {
                reason: StopCancelledReason::UserInterrupt,
                trigger: None,
                reason_details: None,
                last_assistant_message: None,
            },
        );
        assert_eq!(late, ReportOutcome::AlreadyReported);
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_rewind_reports_nothing_but_still_settles_the_session() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        {
            let mut state = h.actor.state.lock().await;
            state.rewindable = true;
        }
        h.start_turn("p1").await;

        let outcome = h
            .actor
            .cancel_running_task(crate::session::CancelOptions {
                history: crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                    prompt_id: None,
                },
                trigger: Some(CancelTrigger::Esc),
                user_initiated: true,
                ..Default::default()
            })
            .await;
        assert!(outcome.turn_stopped);
        assert_eq!(h.lifecycle.aborts.get(), 1);
        h.drain_turn_ends().await;
        assert!(h.fired_payloads().is_empty(), "a rewind ends no turn");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_turn_announces_its_abort_once() {
    run(async {
        let h = Harness::new().await;
        let interrupted = xai_agent_lifecycle::TurnAbortReason::Interrupted;
        let first = h.actor.turn_report.epoch();
        h.actor.notify_turn_abort(first, interrupted).await;
        h.actor.notify_turn_abort(first, interrupted).await;
        assert_eq!(h.lifecycle.aborts.get(), 1);

        h.actor.turn_report.start_next_turn();
        let second = h.actor.turn_report.epoch();
        h.actor.notify_turn_abort(first, interrupted).await;
        assert_eq!(h.lifecycle.aborts.get(), 1, "the old turn does not repeat");
        h.actor.notify_turn_abort(second, interrupted).await;
        assert_eq!(
            h.lifecycle.aborts.get(),
            2,
            "the new turn announces its own"
        );
    })
    .await;
}

#[cfg(target_os = "linux")]
struct SubreaperGuard;

#[cfg(target_os = "linux")]
impl SubreaperGuard {
    fn arm() -> Self {
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1);
        }
        Self
    }
}

#[cfg(target_os = "linux")]
impl Drop for SubreaperGuard {
    fn drop(&mut self) {
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn session_end_cancels_in_flight_start_hook() {
    run(async {
        #[cfg(target_os = "linux")]
        let _subreaper = SubreaperGuard::arm();
        let mut h = Harness::with_hook_workspace().await;
        let _cwd = h.hook_workspace.as_ref().expect("owned hook cwd");
        h.listen(&[HookEventName::SessionStart, HookEventName::SessionEnd]);
        let gate = tempfile::TempDir::new().unwrap();
        let release = gate.path().join("release");
        let marker = gate.path().join("grandchild_alive");
        let leader_pid = gate.path().join("leader.pid");
        let child_pid = gate.path().join("child.pid");
        *h.actor.hook_registry.borrow_mut() = Some(Arc::new(
            super::client_hooks_tests::file_registry_with_spec(
                HookEventName::SessionStart,
                &format!(
                    "echo $$ > '{}'; sh -c 'echo $$ > \"{}\"; sleep 30; echo alive > \"{}\"' & while [ ! -f '{}' ]; do sleep 0.05; done",
                    leader_pid.display(),
                    child_pid.display(),
                    marker.display(),
                    release.display()
                ),
            ),
        ));
        let (cmd_tx, fired) = h.spawn_loop().await;
        cmd_tx
            .send(SessionCommand::DispatchSessionStartHook {
                source: "new".into(),
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if child_pid.exists() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("deferred start must spawn a grandchild before cancel");
        drop(cmd_tx);

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let saw_end = fired
                    .borrow()
                    .iter()
                    .any(|e| e["hookEventName"] == "session_end");
                if saw_end {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("session-end must not wait out a gated start hook");

        std::fs::write(&release, b"go").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let names: Vec<String> = fired
            .borrow()
            .iter()
            .filter_map(|e| e["hookEventName"].as_str().map(String::from))
            .collect();
        let end = names.iter().position(|n| n == "session_end");
        assert!(end.is_some(), "session-end missing: {names:?}");
        assert!(
            !names
                .iter()
                .skip(end.unwrap())
                .any(|n| n == "session_start"),
            "cancelled start must not fire after session-end, got {names:?}"
        );
        assert!(
            !marker.exists(),
            "cancelled start grandchild wrote after session-end"
        );
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            for path in [&leader_pid, &child_pid] {
                let pid: u32 = std::fs::read_to_string(path)
                    .unwrap_or_default()
                    .trim()
                    .parse()
                    .expect("hook pid");
                let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));
                while proc_path.exists() && std::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                assert!(
                    !proc_path.exists(),
                    "hook grandchild/leader pid {pid} from {} still present after session-end \
                     (zombie or live; no external init reaped it)",
                    path.display()
                );
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_session_end_names_the_child() {
    run(async {
        let events = [HookEventName::SessionEnd, HookEventName::Stop];

        let mut parent = Harness::new().await;
        parent.listen(&events);
        let timer = xai_grok_telemetry::session_end::SessionEndTimer::new_shared();
        super::run_loop::fire_session_end_hooks(
            &parent.actor,
            "shutdown",
            &timer,
            &mut super::run_loop::DeferredStart::new(),
        )
        .await;
        assert_eq!(parent.fired(), vec!["session_end", "stop"]);

        let mut child = Harness::subagent().await;
        child.listen(&events);
        let child_timer = xai_grok_telemetry::session_end::SessionEndTimer::new_shared();
        super::run_loop::fire_session_end_hooks(
            &child.actor,
            "shutdown",
            &child_timer,
            &mut super::run_loop::DeferredStart::new(),
        )
        .await;
        let fired = child.fired_payloads();
        assert_eq!(
            fired.len(),
            1,
            "the session-end `Stop` stays subagent-guarded"
        );
        assert_eq!(fired[0]["hookEventName"], "session_end");
        assert_eq!(fired[0]["subagentType"], "explore");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_hook_on_another_event_leaves_the_slot_free() {
    run(async {
        let h = Harness::new().await;
        h.listen(&[HookEventName::PreToolUse]);
        // A file registry too, so only the per-event check holds the slot back.
        *h.actor.hook_registry.borrow_mut() = Some(Arc::new(
            super::client_hooks_tests::file_registry_with_spec(HookEventName::PreToolUse, "true"),
        ));
        h.start_turn("p1").await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
        assert_eq!(
            h.actor.report_turn_end(
                "p1",
                TurnEnd::Failed {
                    error: StopFailureKind::Unknown,
                    error_details: None,
                    last_assistant_message: None,
                },
            ),
            ReportOutcome::NoListener
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn only_a_settled_main_session_offers_an_idle_ping() {
    run(async {
        let parent = Harness::new().await;
        parent.start_turn("p1").await;
        parent.actor.emit_session_idle_if_idle().await;
        assert_eq!(
            parent.lifecycle.idles.get(),
            0,
            "a running turn is not idle"
        );

        {
            let mut state = parent.actor.state.lock().await;
            state.pending_inputs.clear();
            state.running_task = None;
            // Suppressed by the interrupt that ended the turn, which must not swallow the ping.
            state.notifications_suppressed = true;
        }
        parent.actor.emit_session_idle_if_idle().await;
        assert_eq!(parent.lifecycle.idles.get(), 1);

        let child = Harness::subagent().await;
        child.actor.emit_session_idle_if_idle().await;
        assert_eq!(child.lifecycle.idles.get(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_reports_only_its_own_turn_ends() {
    run(async {
        let mut h = Harness::subagent().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let epoch = h.actor.turn_report.epoch();
        let end = |reason| TurnEnd::Cancelled {
            reason,
            trigger: None,
            reason_details: None,
            last_assistant_message: None,
        };

        let inherited =
            h.actor
                .claim_and_queue("p1", epoch, end(StopCancelledReason::UserInterrupt));
        assert_eq!(
            inherited,
            ReportOutcome::InheritedInterrupt,
            "a child inherits the parent's interrupt"
        );

        let own = h
            .actor
            .claim_and_queue("p1", epoch, end(StopCancelledReason::MaxTurns));
        assert_eq!(
            own,
            ReportOutcome::Queued,
            "a subagent's own max-turns is its own report"
        );

        h.listen(&[HookEventName::StopCancelled, HookEventName::StopFailure]);
        h.actor.turn_report.start_next_turn();
        let failed = h.actor.claim_and_queue(
            "p2",
            h.actor.turn_report.epoch(),
            TurnEnd::Failed {
                error: StopFailureKind::ServerError,
                error_details: Some("y".repeat(2000)),
                last_assistant_message: None,
            },
        );
        assert_eq!(
            failed,
            ReportOutcome::Queued,
            "a subagent's own failure is its own report"
        );
        h.drain_turn_ends().await;

        let fired = h.fired_payloads();
        assert_eq!(fired[0]["reason"], "max_turns");
        assert_eq!(fired[0]["subagentType"], "explore");
        assert_eq!(fired[1]["hookEventName"], "stop_failure");
        assert_eq!(fired[1]["subagentType"], "explore");
        let details = fired[1]["errorDetails"].as_str().expect("a detail");
        assert!(details.ends_with("… [+1000 chars]"), "{details}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_loop_reports_and_settles_a_cancelled_turn() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let (cmd, fired) = h.spawn_loop().await;

        cmd.send(SessionCommand::Cancel(crate::session::CancelOptions {
            cancel_subagents: true,
            trigger: Some(CancelTrigger::CtrlC),
            user_initiated: true,
            ..Default::default()
        }))
        .expect("the loop is running");

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while fired.borrow().is_empty() || h.lifecycle.idles.get() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the loop must report and settle");

        let fired = fired.borrow();
        assert_eq!(fired.len(), 1, "the loop must dispatch the queued report");
        assert_eq!(fired[0]["hookEventName"], "stop_cancelled");
        assert_eq!(h.lifecycle.idles.get(), 1, "the loop must settle the host");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_queue_leaves_the_turn_reportable() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let end = || TurnEnd::Cancelled {
            reason: StopCancelledReason::UserInterrupt,
            trigger: None,
            reason_details: None,
            last_assistant_message: None,
        };

        h.close_turn_end_queue().await;
        assert_eq!(
            h.actor.report_turn_end("p1", end()),
            ReportOutcome::QueueClosed,
            "a report nobody can dispatch is not this turn's report"
        );
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);

        h.drain_turn_ends().await;
        assert_eq!(h.actor.report_turn_end("p1", end()), ReportOutcome::Queued);
        h.drain_turn_ends().await;
        assert_eq!(h.fired(), ["stop_cancelled"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn draining_runs_the_worker_down_instead_of_timing_out() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let _ = h.cancel(CancelTrigger::CtrlC).await;

        let queue = h.queue.take().expect("a live queue");
        let started = std::time::Instant::now();
        queue.drain().await;

        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain waited {:?}, so it never saw the worker exit",
            started.elapsed()
        );
        assert_eq!(h.fired(), ["stop_cancelled"], "and it still dispatched");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_queue_leaves_nothing_reportable() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;

        drop(h.queue.take().expect("a live queue"));

        assert_eq!(
            h.actor.report_turn_end(
                "p1",
                TurnEnd::Cancelled {
                    reason: StopCancelledReason::UserInterrupt,
                    trigger: None,
                    reason_details: None,
                    last_assistant_message: None,
                },
            ),
            ReportOutcome::QueueClosed
        );
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_long_assistant_message_is_clipped() {
    run(async {
        let over = xai_grok_hooks::event::MAX_ASSISTANT_MESSAGE_CHARS + 500;
        // Four bytes a char, the UTF-8 worst case the char budget is derived from.
        let long = || ConversationItem::assistant("\u{1f642}".repeat(over));
        let assert_clipped = |text: &str| {
            assert!(
                text.ends_with(" chars]"),
                "unclipped at {} chars",
                text.chars().count()
            );
            assert!(
                text.chars().count() <= xai_grok_hooks::event::MAX_ASSISTANT_MESSAGE_CHARS + 32,
                "{} chars",
                text.chars().count()
            );
        };

        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.actor.chat_state_handle.push_assistant_response(long());
        h.start_turn("p1").await;
        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_clipped(
            fired[0]["lastAssistantMessage"]
                .as_str()
                .expect("a last message"),
        );

        // This runs as a subagent because that branch of the gate's builder skips the work snapshot, which would need a live tool bridge to answer
        let sub = Harness::subagent().await;
        sub.actor.chat_state_handle.push_assistant_response(long());
        sub.start_turn("p1").await;
        let xai_grok_hooks::event::HookPayload::SubagentStop {
            last_assistant_message,
            ..
        } = sub.actor.stop_payload_for_test().await
        else {
            panic!("a subagent gate builds SubagentStop")
        };
        assert_clipped(&last_assistant_message.expect("a last message"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn teardown_runs_queued_reports_before_the_session_end_hooks() {
    for shutdown in [true, false] {
        run(async move {
            let mut h = Harness::new().await;
            h.listen(&[
                HookEventName::StopCancelled,
                HookEventName::SessionEnd,
                HookEventName::Stop,
            ]);
            h.start_turn("p1").await;
            let (cmd, fired) = h.spawn_loop().await;

            cmd.send(SessionCommand::Cancel(crate::session::CancelOptions {
                cancel_subagents: true,
                trigger: Some(CancelTrigger::CtrlC),
                user_initiated: true,
                ..Default::default()
            }))
            .expect("the loop is running");
            if shutdown {
                cmd.send(SessionCommand::Shutdown(
                    crate::session::ShutdownKind::CancelRunningTurn,
                ))
                .expect("the loop is running");
            }
            // The cancel is buffered, so it is handled before `recv` sees the close.
            drop(cmd);

            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while fired.borrow().len() < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("teardown must run the queued report and the session-end hooks");

            let names: Vec<String> = fired
                .borrow()
                .iter()
                .filter_map(|p| p["hookEventName"].as_str().map(str::to_string))
                .collect();
            assert_eq!(
                names,
                ["stop_cancelled", "session_end", "stop"],
                "{shutdown}"
            );
        })
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stale_completion_does_not_post_process_the_successor_turn() {
    run(async {
        let mut h = Harness::new().await;
        let _old_response = h.queue_turn("same").await;
        let stale_epoch = h.actor.turn_report.epoch();
        assert!(h.cancel(CancelTrigger::CtrlC).await.turn_stopped);

        let live_epoch = h.actor.turn_report.start_next_turn();
        let (item, mut response_rx) = super::turn_completion_emit_tests::pending_input("same");
        let handle = tokio::task::spawn_local(std::future::pending::<()>()).abort_handle();
        {
            let mut state = h.actor.state.lock().await;
            state.pending_inputs.push_back(item);
            state.running_task = Some(AgentTask::new_at_epoch("same", live_epoch, handle));
        }
        *h.actor
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned") = Some("same".into());
        h.actor.pending_interjections.push(PendingInterjection {
            text: "keep this with the successor".into(),
            attachments: vec![],
        });
        let idles_before = h.lifecycle.idles.get();
        let (command, _fired) = h.spawn_loop().await;
        let (processed_tx, processed_rx) = tokio::sync::oneshot::channel();

        command
            .send(SessionCommand::InjectTurnCompletion {
                prompt_id: "same".into(),
                epoch: stale_epoch,
                result: Box::new(crate::session::commands::ok_end_turn(0, None)),
                elapsed_ms: Some(0),
                processed: processed_tx,
            })
            .expect("the actor loop accepts the stale completion");
        tokio::time::timeout(std::time::Duration::from_secs(5), processed_rx)
            .await
            .expect("the stale completion is processed")
            .expect("the actor loop acknowledges the completion");
        drop(command);

        assert!(matches!(
            response_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(!h.actor.pending_interjections.is_empty());
        let state = h.actor.state.lock().await;
        assert_eq!(state.pending_inputs.len(), 1);
        let task = state
            .running_task
            .as_ref()
            .expect("successor task remains installed");
        assert_eq!((task.prompt_id.as_str(), task.epoch), ("same", live_epoch));
        assert_eq!(h.lifecycle.idles.get(), idles_before);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_completion_arriving_after_its_cancel_reports_nothing() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        let _p1 = h.queue_turn("p1").await;
        let identity = completion_identity(&h.actor);

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.actor
            .handle_completion(
                "p1".into(),
                TurnEpoch::default(),
                &identity,
                Ok(PromptTurnOk {
                    stop_reason: acp::StopReason::Cancelled,
                    total_tokens: 0,
                    turn_snapshot: None,
                    completion_kind: PromptCompletionKind::Cancelled {
                        category: Some(crate::session::events::CancellationCategory::MidTurnAbort),
                        context: None,
                    },
                    structured_output: None,
                    usage: None,
                    tool_overrides: None,
                }),
                Some(0),
            )
            .await;
        h.drain_turn_ends().await;

        assert_eq!(h.fired(), ["stop_cancelled"], "one turn, one report");
        assert!(!h.actor.state.lock().await.finalization_gate.is_active());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_completion_reports_its_own_cancel_reason() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        let ok = |kind| {
            Ok(PromptTurnOk {
                stop_reason: acp::StopReason::EndTurn,
                total_tokens: 0,
                turn_snapshot: None,
                completion_kind: kind,
                structured_output: None,
                usage: None,
                tool_overrides: None,
            })
        };

        let _p1 = h.queue_turn("p1").await;
        h.actor
            .handle_completion(
                "p1".into(),
                h.actor.turn_report.epoch(),
                &completion_identity(&h.actor),
                ok(PromptCompletionKind::MaxTurnsReached { limit: 1 }),
                Some(0),
            )
            .await;

        h.actor.turn_report.start_next_turn();
        let _p2 = h.queue_turn("p2").await;
        h.actor
            .handle_completion(
                "p2".into(),
                h.actor.turn_report.epoch(),
                &completion_identity(&h.actor),
                ok(PromptCompletionKind::Cancelled {
                    category: Some(
                        crate::session::events::CancellationCategory::PermissionRejected,
                    ),
                    context: Some(crate::session::CancellationContext {
                        tool_name: Some("read_file".into()),
                        reason: Some("x".repeat(2000)),
                        hook_name: None,
                        trigger: Some(format!("ctrl_c{}", "y".repeat(2000))),
                    }),
                }),
                Some(0),
            )
            .await;
        h.drain_turn_ends().await;

        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0]["reason"], "max_turns");
        assert_eq!(fired[0]["promptId"], "p1");
        assert_eq!(fired[1]["reason"], "permission_rejected");
        for (field, prefix, max) in [
            (
                "cancelTrigger",
                "ctrl_c",
                xai_grok_hooks::event::MAX_CANCEL_TRIGGER_CHARS,
            ),
            (
                "reasonDetails",
                "read_file: ",
                xai_grok_hooks::event::MAX_STOP_ENTRY_TEXT_CHARS,
            ),
        ] {
            let text = fired[1][field]
                .as_str()
                .unwrap_or_else(|| panic!("{field}"));
            assert!(text.starts_with(prefix), "{field}: {text}");
            assert!(text.ends_with(" chars]"), "{field}: {text}");
            assert!(text.chars().count() < max + 32, "{field}: {}", text.len());
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_flush_leaves_the_queue_open() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        for prompt_id in ["p1", "p2"] {
            h.actor.turn_report.start_next_turn();
            h.start_turn(prompt_id).await;
            let _ = h.cancel(CancelTrigger::CtrlC).await;
            h.queue.as_mut().expect("a live queue").flush().await;
        }

        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0]["promptId"], "p1");
        assert_eq!(fired[1]["promptId"], "p2");
    })
    .await;
}

#[derive(Debug, Clone)]
struct SnapshotRegisteredTool {
    name: &'static str,
    namespace: xai_grok_tools::types::tool::ToolNamespace,
    kind: xai_grok_tools::types::tool::ToolKind,
    description: &'static str,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for SnapshotRegisteredTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        self.kind
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        self.namespace
    }
    fn description_template(&self) -> &str {
        self.description
    }
}

impl xai_tool_runtime::Tool for SnapshotRegisteredTool {
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
        panic!("snapshot declaration fixtures never execute tools")
    }
}

fn register_snapshot_fixture(
    actor: &SessionActor,
    tool: SnapshotRegisteredTool,
    schema: serde_json::Value,
) -> xai_grok_sampling_types::ToolSpec {
    let spec = xai_grok_sampling_types::ToolSpec {
        name: tool.name.into(),
        description: Some(tool.description.into()),
        parameters: schema.clone(),
        exposure: xai_grok_sampling_types::ToolExposure::Immediate,
    };
    actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(tool.name.into(), tool, Some(schema))
        .unwrap();
    spec
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn fork_snapshot_and_children_exclude_registered_mcp() {
    use std::collections::BTreeSet;
    use xai_chat_state::NativeToolCandidates;
    use xai_grok_sampling_types::{ApiBackend, ToolExposure, ToolSpec};
    use xai_grok_test_support::{MockInferenceServer, MockModelEntry, ScriptedResponse};
    use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};

    run(async {
        let mut parent = Harness::new().await;
        parent.actor.agent.replace(test_agent_backend_search(Vec::new()).await);
        parent.actor.supports_backend_search.set(true);
        parent.actor.models_manager.insert_test_entry("snapshot-parent",
            entry_with_deferred_tools("snapshot-parent", true, &["z_parent"]));
        parent.actor.models_manager.set_current_model_id(acp::ModelId::new("snapshot-parent"));
        let mut expected_snapshot = Vec::new();
        for (name, kind) in [
            ("z_parent", ToolKind::Other),
            ("builtin__renamed", ToolKind::Other),
            ("root_message", ToolKind::ActiveAgentMessage),
            ("web_search", ToolKind::Other),
            ("ask_user_question", ToolKind::Other),
            ("workflow", ToolKind::Workflow),
        ] {
            let mut spec = register_snapshot_fixture(&parent.actor, SnapshotRegisteredTool {
                name, namespace: ToolNamespace::GrokBuild, kind, description: name,
            }, serde_json::json!({"type":"object","properties":{"value":{"type":"string","description":name}},"required":["value"],"additionalProperties":false}));
            if name == "z_parent" { spec.exposure = ToolExposure::Deferred; }
            if !["root_message", "web_search"].contains(&name) { expected_snapshot.push(spec); }
        }
        let parent_mcp = register_snapshot_fixture(&parent.actor, SnapshotRegisteredTool {
            name: "parent__fetch", namespace: ToolNamespace::MCP, kind: ToolKind::Other,
            description: "Parent MCP with a parent-only schema",
        }, serde_json::json!({"type":"object","properties":{"parent":{"type":"boolean"}},"required":["parent"],"additionalProperties":false}));
        let (commands, _) = parent.spawn_loop().await;
        let mut snapshot = Vec::new();
        for plan_active in [false, true] {
            if plan_active { assert!(parent.actor.plan_mode.lock().activate_from_tool()); }
            let (respond_to, reply) = tokio::sync::oneshot::channel();
            commands.send(SessionCommand::SnapshotToolDefinitions { respond_to }).unwrap();
            snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), reply).await.unwrap().unwrap();
            assert_eq!(serde_json::to_value(&snapshot).unwrap(), serde_json::to_value(&expected_snapshot).unwrap(),
                "U4 actual parent snapshot keeps exact projected builtins and excludes registered MCP");
        }
        let structured = ToolSpec {
            name: "StructuredOutput".into(),
            description: Some("Return your final answer as JSON matching the required schema. Call this exactly once, at the end.".into()),
            parameters: serde_json::json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}),
            exposure: ToolExposure::Immediate,
        };
        for (flag, is_subagent) in [(true, true), (false, true), (true, false), (false, false)] {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("snapshot-child")]).await.unwrap();
            server.enqueue_response("/v1/messages", ScriptedResponse::sse(
                xai_grok_test_support::sse::messages_api_tool_use_events(
                    "fork-output", "StructuredOutput", r#"{"answer":"done"}"#, "snapshot-child",
                ),
            ));
            let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            super::rate_limit_backoff_tests::drain_gateway(gateway_rx);
            super::rate_limit_backoff_tests::drain_persistence(persistence_rx);
            let mut child = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            child.agent.replace(test_agent_with_tools(Vec::new()).await);
            child.startup_hints.is_subagent = is_subagent;
            child.startup_hints.subagent_type = is_subagent.then(|| "explore".into());
            child.forked_tool_override = Some(super::ToolSpecBundle::without_identity(snapshot.clone()));
            let (manager, _home) = manager_with_entries();
            manager.insert_test_entry("snapshot-child", entry_with_deferred_tools(
                "snapshot-child", flag, &["builtin__renamed"],
            ));
            manager.set_current_model_id(acp::ModelId::new("snapshot-child"));
            child.models_manager = manager;
            let local_mcp = register_snapshot_fixture(&child, SnapshotRegisteredTool {
                name: "local__lookup", namespace: ToolNamespace::MCP, kind: ToolKind::Other,
                description: "Child MCP with a local-only schema",
            }, serde_json::json!({"type":"object","properties":{"local":{"type":"integer"}},"required":["local"],"additionalProperties":false}));
            assert_ne!(local_mcp.parameters, parent_mcp.parameters);
            let mut binding = child.chat_state_handle.capture_transport_binding().await.unwrap();
            binding.config.model = "snapshot-child".into();
            binding.config.base_url = server.url();
            binding.config.api_backend = ApiBackend::Messages;
            binding.config.supports_search_tool = flag;
            binding.config.max_retries = Some(0);
            binding.credentials.auth_type = xai_chat_state::AuthType::ApiKey;
            binding.credentials.api_key = Some("snapshot-child-fixture-key".into());
            child.chat_state_handle.install_transport_binding(binding.clone()).await.unwrap();
            child.chat_state_handle.replace_conversation(vec![ConversationItem::user("return the fork answer")]);
            let expected: Vec<_> = snapshot.iter().filter(|spec| {
                !is_subagent || !["ask_user_question", "workflow"].contains(&spec.name.as_str())
            }).cloned().chain([structured.clone()]).collect();
            let (prepared, _) = child.prepare_native_tool_candidates_timed().await;
            assert_eq!(serde_json::to_value(&prepared.mcp_specs).unwrap(), serde_json::to_value([&local_mcp]).unwrap());
            let candidates: NativeToolCandidates = child.tool_candidates_for_turn(
                child.turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs),
                Some(structured.parameters.clone()),
            );
            let mut row = binding.config.clone();
            row.api_backend = ApiBackend::Messages;
            row.supports_search_tool = true;
            let admitted = candidates.clone().select_for_row(&row);
            row.api_backend = ApiBackend::Responses;
            let excluded = candidates.clone().select_for_row(&row);
            let excluded_names: BTreeSet<_> = excluded.iter().map(|spec| spec.name.as_str()).collect();
            let dropped: BTreeSet<_> = admitted.iter().map(|spec| spec.name.as_str())
                .filter(|name| !excluded_names.contains(name)).collect();
            let operator_deferred: BTreeSet<_> = expected.iter()
                .filter(|spec| spec.exposure == ToolExposure::Deferred).map(|spec| spec.name.as_str()).collect();
            let deferred: BTreeSet<_> = admitted.iter().filter(|spec| spec.exposure == ToolExposure::Deferred)
                .map(|spec| spec.name.as_str()).filter(|name| !operator_deferred.contains(name)).collect();
            assert!(dropped.is_empty() && deferred.is_empty(),
                "U4 bare overrides have empty membership even for delimiter-renamed builtins");
            for actual in [&admitted, &excluded] {
                assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(&expected).unwrap(),
                    "U4 bare override surface excludes parent and child registry unions");
            }
            let (request, actual_binding) = child.chat_state_handle.build_request_with_binding(
                u8_with_entry(candidates), None, false, None, child.session_id_string(), "fork-bound".into(),
            ).await.unwrap();
            assert_eq!(serde_json::to_value(&request.tools).unwrap(), serde_json::to_value(&expected).unwrap(),
                "U4 forked bound build preserves the authoritative bare override");
            assert_eq!(serde_json::to_value(&actual_binding.config).unwrap(), serde_json::to_value(&binding.config).unwrap());
            assert_eq!(actual_binding.credentials, binding.credentials);
            assert_eq!(request.search_admission.unwrap().admitted(), flag);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            child.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
                child.turn_sampler_config_from_binding(&binding).await,
                xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, event_tx,
            );
            child.workspace_ops.bind_local_session(
                &child.session_id_string(), child.tool_context.cwd.as_path().to_path_buf(),
                child.tool_context.hunk_tracker_handle.clone(), child.agent.borrow().tool_bridge().toolset(), None,
            ).unwrap();
            let child = Arc::new(child);
            let drainer = child.clone();
            let drain_task = tokio::task::spawn_local(async move {
                while let Some(event) = event_rx.recv().await { drainer.handle_sampling_event(event).await; }
            });
            let mut salvage = super::length_salvage::LengthSalvage::new(None);
            let mut sampling = Default::default();
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(300),
                child.process_conversation_turn_with_recovery(
                    "snapshot-child", None, None, Some(structured.parameters.clone()), &mut salvage, &mut sampling,
                ),
            ).await.expect("forked child turn completes");
            drain_task.abort();
            let _ = drain_task.await;
            assert!(outcome.is_ok(), "forked turn must complete: {:?}", outcome.err());
            assert_eq!(server.messages_request_count(), 1);
            let sent = server.requests().into_iter().find(|sent| sent.path == "/v1/messages").unwrap();
            assert_eq!(sent.authorization.as_deref(), Some("Bearer snapshot-child-fixture-key"));
            let body = sent.body.unwrap();
            let expected_body = serde_json::to_value(
                xai_grok_sampling_types::conversation::build_messages_request(&request),
            ).unwrap();
            assert_eq!(body["tools"], expected_body["tools"],
                "U4 actual forked send retains the bare builtin surface and structured output");
            let tools = body["tools"].as_array().unwrap();
            assert!(tools.iter().all(|tool| tool["name"] != parent_mcp.name && tool["name"] != local_mcp.name));
            let structured_wire: Vec<_> = tools.iter().filter(|tool| tool["name"] == "StructuredOutput").collect();
            assert_eq!(structured_wire, vec![&serde_json::json!({
                "name": structured.name, "description": structured.description, "input_schema": structured.parameters,
            })], "U4 fork structured output never acquires MCP membership");
            assert_eq!(serde_json::to_value(&child.forked_tool_override.as_ref().unwrap().specs).unwrap(),
                serde_json::to_value(&snapshot).unwrap(), "bare fork carrier is unchanged");
        }
        commands.send(SessionCommand::Shutdown(crate::session::ShutdownKind::Graceful)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), commands.closed()).await.unwrap();
        parent.chat.take();
    }).await;
}

// The config home is process-cached; isolate before any actor reads it.
fn parent_golden_isolated(name: &str) -> bool {
    const CHILD: &str = "GROK_TEST_3V1S_PARENT_ISOLATED";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("GROK_HOME", home.path().join("grok"))
        .env("TMPDIR", home.path())
        .env("RUST_MIN_STACK", "67108864")
        .env(CHILD, name)
        .args(["--exact", name, "--nocapture"])
        .stdin(std::process::Stdio::null());
    for variable in ["GROK_TOOLS_BUNDLE_RG_PATH", "GROK_SHELL_BUNDLE_RG_PATH"] {
        if let Some(value) = std::env::var_os(variable) {
            command.env(variable, value);
        }
    }
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stdout.matches("running 1 test").count(), 1, "{stdout}");
    assert!(
        output.status.success(),
        "isolated parent witness failed: {stdout}\n{stderr}"
    );
    assert!(stdout.contains(&format!("test {name} ... ok")), "{stdout}");
    true
}

#[derive(Clone)]
struct ParentGoldenExchange {
    path: String,
    request_id: Option<String>,
    headers: Vec<(String, bool)>,
    raw_body: Vec<u8>,
    parsed_body: serde_json::Value,
    response_body: String,
}

struct ParentGoldenWire {
    url: String,
    exchanges: std::rc::Rc<std::cell::RefCell<Vec<ParentGoldenExchange>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ParentGoldenWire {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ParentGoldenWire {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let exchanges = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = exchanges.clone();
        let task = tokio::task::spawn_local(async move {
            for _ in 0..8 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut received = Vec::new();
                let mut buffer = [0; 8192];
                let header_end = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(n, 0, "complete HTTP headers required");
                    received.extend_from_slice(&buffer[..n]);
                    assert!(received.len() <= 2 * 1024 * 1024);
                    if let Some(index) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    {
                        break index + 4;
                    }
                };
                let headers = std::str::from_utf8(&received[..header_end]).unwrap();
                let mut lines = headers.lines();
                let first = lines.next().unwrap();
                let mut request_line = first.split_whitespace();
                assert_eq!(request_line.next(), Some("POST"));
                let path = request_line.next().unwrap().to_owned();
                let mut length = None;
                let mut request_id = None;
                let mut header_flags = Vec::new();
                for line in lines.filter(|line| !line.is_empty()) {
                    let (name, value) = line.split_once(':').unwrap();
                    header_flags.push((name.to_ascii_lowercase(), value.trim().is_empty()));
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                    if name.eq_ignore_ascii_case("x-grok-req-id") {
                        request_id = Some(value.trim().to_owned());
                    }
                    assert!(!name.eq_ignore_ascii_case("transfer-encoding"));
                }
                let length = length.expect("encoded request has a Content-Length");
                assert!(length <= 2 * 1024 * 1024);
                while received.len() < header_end + length {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(n, 0, "complete HTTP body required");
                    received.extend_from_slice(&buffer[..n]);
                }
                // Keep exactly the received entity bytes before any JSON parsing.
                let raw_body = received[header_end..header_end + length].to_vec();
                let parsed_body: serde_json::Value = serde_json::from_slice(&raw_body).unwrap();
                let response_body = parent_golden_response(&path, &parsed_body);
                captured.borrow_mut().push(ParentGoldenExchange {
                    path,
                    request_id,
                    headers: header_flags,
                    raw_body,
                    parsed_body,
                    response_body: response_body.clone(),
                });
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\nx-request-id: parent-capture-response\r\n\r\n{response_body}",
                    response_body.len(),
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            url,
            exchanges,
            task,
        }
    }
}

fn parent_golden_response(path: &str, body: &serde_json::Value) -> String {
    let model = body["model"].as_str().unwrap();
    if path == "/v1/responses" {
        return xai_grok_test_support::sse::responses_api_script_exact(
            "Parent capture complete",
            model,
        )
        .into_iter()
        .map(|event| format!("data: {}\n\n", event.data))
        .collect();
    }
    assert_eq!(path, "/v1/messages");
    let events = vec![
        serde_json::json!({"type":"message_start","message":{"id":"msg_parent_capture","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}),
        serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Parent capture complete"}}),
        serde_json::json!({"type":"content_block_stop","index":0}),
        serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5,"input_tokens":10}}),
        serde_json::json!({"type":"message_stop"}),
    ];
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

fn parent_golden_current_user_id() -> String {
    #[cfg(unix)]
    let identity = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .ok()
        .flatten()
        .map(|user| user.name)
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("LOGNAME").ok());
    #[cfg(not(unix))]
    let identity = std::env::var("USERNAME").ok().map(|name| {
        std::env::var("USERDOMAIN")
            .map_or_else(|_| name.clone(), |domain| format!("{domain}\\{name}"))
    });
    use sha2::Digest;
    let identity = identity.expect("the capture host has an OS account identity");
    let hash = sha2::Sha256::digest(format!("grok-responses:metadata-user-id:v1:{identity}"));
    format!("{hash:x}")
}

fn parent_golden_validate_body(body: &serde_json::Value) {
    let parsed: serde_json::Value =
        serde_json::from_str(body["raw_body"].as_str().unwrap()).unwrap();
    assert_eq!(
        parsed, body["parsed_body"],
        "parsed body must come from received bytes"
    );
    if let Some(user_id) = parsed.pointer("/metadata/user_id") {
        let current = parent_golden_current_user_id();
        assert_eq!(
            user_id.as_str(),
            Some(current.as_str()),
            "metadata identity must match the current OS account"
        );
        static FIRST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        assert_eq!(
            FIRST.get_or_init(|| current.clone()),
            &current,
            "all cases use the same OS account"
        );
    }
}

fn parent_golden_map_body_identity(body: &mut serde_json::Value) {
    let Some(user_id) = body["parsed_body"]
        .pointer("/metadata/user_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    assert_eq!(user_id.len(), 64);
    assert!(
        user_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    // Map only this wire field; schemas, messages and every other byte stay literal.
    let needle = format!(r#""metadata":{{"user_id":"{user_id}"}}"#);
    let raw = body["raw_body"].as_str().unwrap();
    assert_eq!(raw.matches(&needle).count(), 1);
    body["raw_body"] = serde_json::Value::String(raw.replacen(
        &needle,
        r#""metadata":{"user_id":"<os-user-sha256>"}"#,
        1,
    ));
    body["parsed_body"]["metadata"]["user_id"] = serde_json::json!("<os-user-sha256>");
}

fn parent_golden_assert_body(actual: &serde_json::Value, expected_raw: &str, case: &str) {
    parent_golden_validate_body(actual);
    let expected_parsed: serde_json::Value = serde_json::from_str(expected_raw).unwrap();
    let mut expected =
        serde_json::json!({"raw_body": expected_raw, "parsed_body": expected_parsed});
    let mut actual = actual.clone();
    parent_golden_map_body_identity(&mut actual);
    parent_golden_map_body_identity(&mut expected);
    assert_eq!(
        actual, expected,
        "frozen parent raw and parsed body: {case}"
    );
}

fn parent_golden_record(case: &str, value: serde_json::Value) {
    let frozen = &value["frozen"];
    let expected_raw = match case {
        "fork/chat-admission-off"
        | "fork/chat-admission-on"
        | "fork/messages-admission-off"
        | "fork/missing-child-bridge-registration"
        | "fork/responses-admission-off"
        | "fork/responses-admission-on"
        | "fork/same-bound-row"
        | "fork/same-named-unrelated-child-registration" => Some(
            r####"{"model":"r12-parent","messages":[{"role":"user","content":[{"type":"text","text":"Find the parent schema"}]},{"role":"assistant","content":[{"type":"tool_use","id":"call_parent_capture","name":"search_tool","input":{"query":"parent schema"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_parent_capture","content":[{"type":"tool_reference","tool_name":"historic__lookup"}]}]},{"role":"assistant","content":[{"type":"text","text":"[tool_search results] 1 tool"},{"type":"text","text":"Found both schema families"}]},{"role":"user","content":[{"type":"text","text":"Check the second step"}]},{"role":"assistant","content":[{"type":"text","text":"Second step complete"}]},{"role":"user","content":[{"type":"text","text":"Check the third step","cache_control":{"type":"ephemeral"}}]},{"role":"assistant","content":[{"type":"text","text":"Third step complete"}]},{"role":"user","content":[{"type":"text","text":"Continue the fork after discovery"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="},"cache_control":{"type":"ephemeral"}}]}],"max_tokens":512,"system":[{"type":"text","text":"Parent capture system"},{"type":"text","text":"The following deferred tools are now available via search_tool. Their schemas are NOT loaded — calling them directly will fail with InputValidationError. Use search_tool with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\nhistoric__lookup","cache_control":{"type":"ephemeral"}}],"tools":[{"name":"search_tool","description":"Fetches full schema definitions for deferred tools so they can be called.\n\nDeferred tools appear by name in <system-reminder> messages. Until fetched, only the name is known — there is no parameter schema, so the tool cannot be invoked. This tool takes a query, matches it against the deferred tool list, and returns the matched tools' complete JSONSchema definitions inside a <functions> block. Once a tool's schema appears in that result, it is callable exactly like any tool defined at the top of the prompt.\n\nResult format: each matched tool appears as one <function>{\"description\": \"...\", \"name\": \"...\", \"parameters\": {...}}</function> line inside the <functions> block — the same encoding as the tool list at the top of this prompt.\n\nQuery forms:\n- \"select:Read,Edit,Grep\" — fetch these exact tools by name\n- \"notebook jupyter\" — keyword search, up to max_results best matches\n- \"+slack send\" — require \"slack\" in the name, rank by remaining terms","input_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"query":{"description":"Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.","type":"string"},"max_results":{"description":"Maximum number of results to return (default: 5)","default":5,"type":"number"}},"required":["query","max_results"],"additionalProperties":false}},{"name":"use_tool","description":"Call an MCP integration tool.\n\nThe `tool_name` must be the qualified `server__tool` name (e.g., `linear__save_issue`). The `tool_input` must conform exactly to the tool's input schema as returned by `search_tool`.","input_schema":{"$schema":"http://json-schema.org/draft-07/schema#","required":["tool_name","tool_input"],"properties":{"tool_name":{"description":"The qualified name of the integration tool to call (e.g., \"linear__save_issue\").\nMust be a tool previously discovered via `search_tool`.","type":"string"},"tool_input":{"description":"The arguments to pass to the tool, as a JSON object.\nUse the parameter schema returned by `search_tool` to construct this.","type":"object","additionalProperties":true}},"type":"object"}},{"name":"z_builtin","description":"z_builtin","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"z_builtin"}},"required":["value"],"additionalProperties":false},"defer_loading":true},{"name":"builtin__renamed","description":"builtin__renamed","input_schema":{"type":"object","properties":{"value":{"type":"string","description":"builtin__renamed"}},"required":["value"],"additionalProperties":false}},{"name":"DeferredToolPlaceholder","description":"Reserved placeholder that keeps deferred tool loading active; never call this tool.","input_schema":{"type":"object","properties":{}},"defer_loading":true},{"name":"historic__lookup","description":"Frozen historical lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},"defer_loading":true},{"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"}],"stream":true,"metadata":{"user_id":"3bb0043ea6227e7cb2e918da9aaa6dd78d5a9847c9aef422a9b814ad4776b61c"}}"####,
        ),
        _ => None,
    };
    if let Some(expected_raw) = expected_raw {
        parent_golden_assert_body(frozen, expected_raw, case);
        return;
    }
    let expected = match case {
        _ => panic!("no valid frozen parent outcome for {case}"),
    };
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    assert_eq!(*frozen, expected, "frozen parent lifecycle outcome: {case}");
}

#[derive(Clone, Debug)]
struct ParentGoldenTool {
    name: &'static str,
    namespace: xai_grok_tools::types::tool::ToolNamespace,
    kind: xai_grok_tools::types::tool::ToolKind,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for ParentGoldenTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        self.kind
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        self.namespace
    }
    fn description_template(&self) -> &str {
        self.name
    }
}

impl xai_tool_runtime::Tool for ParentGoldenTool {
    type Args = serde_json::Value;
    type Output = xai_grok_tools::types::output::ToolOutput;
    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, self.name)
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        _: Self::Args,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        panic!("parent capture never executes a declared fixture tool")
    }
}

fn parent_golden_register(
    actor: &crate::session::acp_session::SessionActor,
    name: &'static str,
    namespace: xai_grok_tools::types::tool::ToolNamespace,
    kind: xai_grok_tools::types::tool::ToolKind,
) {
    let schema = serde_json::json!({"type":"object","properties":{"value":{"type":"string","description":name}},"required":["value"],"additionalProperties":false});
    actor
        .agent
        .borrow()
        .tool_bridge()
        .register_mcp_tools(
            name.into(),
            ParentGoldenTool {
                name,
                namespace,
                kind,
            },
            Some(schema),
        )
        .unwrap();
}

fn parent_golden_history() -> Vec<xai_grok_sampling_types::ConversationItem> {
    use xai_grok_sampling_types::ConversationItem;
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    let raw = vec![
        serde_json::json!({"type":"tool_search_call","id":"tsc_parent_capture","call_id":"call_parent_capture","execution":"client","status":"completed","arguments":{"query":"parent schema"}}),
        serde_json::json!({"type":"tool_search_output","id":"tso_parent_capture","call_id":"call_parent_capture","execution":"client","status":"completed","tools":[{"type":"function","name":"historic__lookup","description":"Frozen historical lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}}]}),
        serde_json::json!({"type":"server_tool_use","id":"srvtoolu_parent_capture","name":"tool_search_tool_bm25","input":{"query":"server schema"},"status":"completed"}),
        serde_json::json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_parent_capture","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"alpha__lookup"}]},"status":"completed"}),
    ];
    let discovery: Vec<_> = raw
        .into_iter()
        .map(|raw| ToolSearchItem::from_wire(raw).unwrap())
        .collect();
    assert_eq!(
        xai_grok_sampling_types::conversation::tool_search::partner_indices(&discovery),
        vec![Some(1), Some(0), Some(3), Some(2)]
    );
    let mut history = vec![
        ConversationItem::system("Parent capture system"),
        ConversationItem::user("Find the parent schema"),
    ];
    history.extend(
        discovery
            .into_iter()
            .map(|item| ConversationItem::Discovery { item }),
    );
    history.extend([
        ConversationItem::assistant("Found both schema families"),
        ConversationItem::user("Check the second step"),
        ConversationItem::assistant("Second step complete"),
        ConversationItem::user("Check the third step"),
        ConversationItem::assistant("Third step complete"),
    ]);
    history
}

struct ParentGoldenFixture {
    actor: std::sync::Arc<crate::session::acp_session::SessionActor>,
    binding: xai_chat_state::RequestTransportBinding,
    wire: ParentGoldenWire,
    live_wire: ParentGoldenWire,
    history: Vec<xai_grok_sampling_types::ConversationItem>,
    _home: tempfile::TempDir,
}

async fn parent_golden_fixture(
    backend: xai_grok_sampling_types::ApiBackend,
) -> ParentGoldenFixture {
    use crate::session::acp_session::support;
    use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
    let wire = ParentGoldenWire::start().await;
    let live_wire = ParentGoldenWire::start().await;
    let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    support::drain_gateway(gateway_rx);
    support::drain_persistence(persistence_rx);
    let mut actor = support::create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.auth_method_id = support::test_auth_method_id("xai.api_key");
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
    for (name, namespace, kind) in [
        ("z_builtin", ToolNamespace::GrokBuild, ToolKind::Other),
        (
            "builtin__renamed",
            ToolNamespace::GrokBuild,
            ToolKind::Other,
        ),
        (
            "policy__root_message",
            ToolNamespace::GrokBuild,
            ToolKind::ActiveAgentMessage,
        ),
        ("zeta__fetch", ToolNamespace::MCP, ToolKind::Other),
        ("alpha__lookup", ToolNamespace::MCP, ToolKind::Other),
    ] {
        parent_golden_register(&actor, name, namespace, kind);
    }
    let (manager, home) = support::manager_with_entries();
    for model in ["r12-parent", "r12-live"] {
        manager.insert_test_entry(
            model,
            support::entry_with_deferred_tools(model, true, &["z_builtin"]),
        );
    }
    manager.set_current_model_id(agent_client_protocol::ModelId::new("r12-parent"));
    actor.models_manager = manager;
    let mut binding = actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    binding.config.model = "r12-parent".into();
    binding.config.base_url = wire.url.clone();
    binding.config.api_backend = backend;
    binding.config.supports_search_tool = true;
    binding.config.max_retries = Some(0);
    binding.config.max_completion_tokens = Some(512);
    binding.config.extra_headers.clear();
    binding.credentials = xai_chat_state::Credentials {
        api_key: Some("parent-capture-fixture".into()),
        auth_type: xai_chat_state::AuthType::ApiKey,
        alpha_test_key: None,
        client_version: None,
    };
    actor
        .chat_state_handle
        .install_transport_binding(binding.clone())
        .await
        .unwrap();
    let history = parent_golden_history();
    actor
        .chat_state_handle
        .replace_conversation(history.clone());
    assert_eq!(
        serde_json::to_value(actor.chat_state_handle.get_conversation().await).unwrap(),
        serde_json::to_value(&history).unwrap()
    );
    ParentGoldenFixture {
        actor: std::sync::Arc::new(actor),
        binding,
        wire,
        live_wire,
        history,
        _home: home,
    }
}

fn parent_golden_variants() -> [(&'static str, xai_grok_sampling_types::ApiBackend, bool); 6] {
    use xai_grok_sampling_types::ApiBackend;
    [
        ("same-bound-row", ApiBackend::Messages, true),
        ("messages-admission-off", ApiBackend::Messages, false),
        ("responses-admission-on", ApiBackend::Responses, true),
        ("responses-admission-off", ApiBackend::Responses, false),
        ("chat-admission-on", ApiBackend::ChatCompletions, true),
        ("chat-admission-off", ApiBackend::ChatCompletions, false),
    ]
}

fn parent_golden_live_binding(
    fixture: &ParentGoldenFixture,
    scenario: &str,
    backend: xai_grok_sampling_types::ApiBackend,
    flag: bool,
) -> xai_chat_state::RequestTransportBinding {
    if scenario == "same-bound-row" {
        return fixture.binding.clone();
    }
    let mut live = fixture.binding.clone();
    live.config.model = "r12-live".into();
    live.config.base_url = fixture.live_wire.url.clone();
    live.config.api_backend = backend;
    live.config.supports_search_tool = flag;
    live.credentials.api_key = Some("live-capture-fixture".into());
    live
}

fn parent_golden_registry(actor: &crate::session::acp_session::SessionActor) -> serde_json::Value {
    let bridge = actor.agent.borrow().tool_bridge().clone();
    let inventory: Vec<_> = bridge.toolset().tools_namespaced().into_iter().enumerate()
        .map(|(coordinate, (definition, namespace))| serde_json::json!({
            "coordinate":coordinate, "kind":format!("{:?}", bridge.tool_kind(&definition.function.name)),
            "namespace":format!("{namespace:?}"), "definition":definition,
        })).collect();
    serde_json::to_value(inventory).unwrap()
}

fn parent_golden_capture_body(
    case: &str,
    fixture: &ParentGoldenFixture,
    request: &xai_grok_sampling_types::ConversationRequest,
    selected_from: &str,
    parent_registry: serde_json::Value,
) {
    assert!(
        fixture.live_wire.exchanges.borrow().is_empty(),
        "installed B must receive no submission"
    );
    let exchanges = fixture.wire.exchanges.borrow();
    assert_eq!(exchanges.len(), 1, "exactly one actual surface send");
    let exchange = &exchanges[0];
    let expected_path = match fixture.binding.config.api_backend {
        xai_grok_sampling_types::ApiBackend::Messages => "/v1/messages",
        xai_grok_sampling_types::ApiBackend::Responses => "/v1/responses",
        xai_grok_sampling_types::ApiBackend::ChatCompletions => "/v1/chat/completions",
    };
    assert_eq!(exchange.path, expected_path);
    assert_eq!(exchange.parsed_body["model"], fixture.binding.config.model);
    assert_eq!(
        request.model.as_deref(),
        Some(fixture.binding.config.model.as_str())
    );
    if case.starts_with("title-") {
        assert!(request.tools.is_empty());
        assert!(request.hosted_tools.is_empty());
        assert!(!request.search_admission.unwrap().admitted());
    } else {
        assert!(request.search_admission.unwrap().admitted());
        let tools = exchange.parsed_body["tools"].as_array().unwrap();
        for name in [
            "search_tool",
            "use_tool",
            "historic__lookup",
            "tool_search_tool_bm25",
        ] {
            assert!(
                tools.iter().any(|tool| tool["name"] == name),
                "parent declaration missing: {name}"
            );
        }
        let contents: Vec<_> = exchange.parsed_body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap().iter())
            .collect();
        assert!(
            contents
                .iter()
                .any(|block| block["type"] == "tool_use" && block["name"] == "search_tool"),
            "full canonical D2 bridge call must be present"
        );
        assert!(
            contents.iter().any(|block| block["type"] == "tool_result"
                && block["tool_use_id"] == "call_parent_capture"),
            "full canonical D2 result must be present"
        );
        if !case.starts_with("fork/") {
            let mcp: Vec<_> = request
                .tools
                .iter()
                .filter(|tool| ["alpha__lookup", "zeta__fetch"].contains(&tool.name.as_str()))
                .collect();
            assert_eq!(mcp.len(), 2);
            for spec in mcp {
                assert_eq!(
                    spec.exposure,
                    xai_grok_sampling_types::ToolExposure::Deferred
                );
                let client = tools.iter().find(|tool| tool["name"] == spec.name).unwrap();
                assert_eq!(client["input_schema"], spec.parameters);
                assert_eq!(client["defer_loading"], true);
            }
        } else {
            assert!(!request.tools.iter().any(|tool| {
                ["alpha__lookup", "zeta__fetch", "child__lookup"].contains(&tool.name.as_str())
            }));
        }
    }
    if fixture.binding.config.api_backend == xai_grok_sampling_types::ApiBackend::Responses {
        assert_eq!(exchange.parsed_body["store"], false);
    }
    parent_golden_record(
        case,
        serde_json::json!({
            "frozen": {"raw_body":String::from_utf8(exchange.raw_body.clone()).unwrap(), "parsed_body":exchange.parsed_body},
            "provenance": {
                "binding_config":fixture.binding.config,
                "backend":fixture.binding.config.api_backend,
                "dialect":format!("{:?}", fixture.binding.config.api_backend),
                "admission":request.search_admission.map(|a| serde_json::json!({"supports_search_tool":a.supports_search_tool,"has_searchable_tools":a.has_searchable_tools})),
                "input_history":fixture.history,
                "request_items":request.items,
                "selected_specs":request.tools,
                "selected_hosted_tools":format!("{:?}", request.hosted_tools),
                "selected_specs_observation":selected_from,
                "parent_registry":parent_registry,
                "request_path":exchange.path,
                "request_id":exchange.request_id,
                "header_flags":exchange.headers,
                "response_id":"parent-capture-response",
                "response_body":exchange.response_body,
                "submission_count":exchanges.len(),
                "normalization_mapping":[],
                "normalization_note":"No body normalization. Request correlation IDs exist only in headers and are retained separately.",
            }
        }),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn interim_messages_fork_override_matches_committed_parent() {
    const NAME: &str = "session::acp_session::turn_end_reporting_tests::interim_messages_fork_override_matches_committed_parent";
    if parent_golden_isolated(NAME) {
        return;
    }
    tokio::task::LocalSet::new().run_until(async {
        use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
        let mut parent = Harness::new().await;
        parent.actor.agent.replace(test_agent_with_tools(vec![
            xai_grok_tools::registry::types::ToolConfig::for_tool::<xai_grok_tools::implementations::search_tool::SearchTool>(),
            xai_grok_tools::registry::types::ToolConfig::for_tool::<xai_grok_tools::implementations::use_tool::UseTool>(),
        ]).await);
        parent.actor.models_manager.insert_test_entry("r12-parent", entry_with_deferred_tools("r12-parent", true, &["z_builtin"]));
        parent.actor.models_manager.set_current_model_id(acp::ModelId::new("r12-parent"));
        for (name, namespace, kind) in [
            ("z_builtin", ToolNamespace::GrokBuild, ToolKind::Other),
            ("builtin__renamed", ToolNamespace::GrokBuild, ToolKind::Other),
            ("policy__root_message", ToolNamespace::GrokBuild, ToolKind::ActiveAgentMessage),
            ("zeta__fetch", ToolNamespace::MCP, ToolKind::Other),
            ("alpha__lookup", ToolNamespace::MCP, ToolKind::Other),
        ] { parent_golden_register(&parent.actor, name, namespace, kind); }
        let parent_registry = parent_golden_registry(&parent.actor);
        let (commands, _) = parent.spawn_loop().await;
        let (respond_to, reply) = tokio::sync::oneshot::channel();
        commands.send(SessionCommand::SnapshotToolDefinitions { respond_to }).unwrap();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(30), reply).await.unwrap().unwrap();
        assert!(snapshot.iter().any(|tool| tool.name == "search_tool"));
        assert!(snapshot.iter().any(|tool| tool.name == "use_tool"));
        assert!(!snapshot.iter().any(|tool| ["alpha__lookup", "zeta__fetch", "policy__root_message"].contains(&tool.name.as_str())));
        let mut scenarios: Vec<_> = parent_golden_variants().into_iter().map(|(id, backend, flag)| (id, backend, flag, "matching")).collect();
        scenarios.extend([
            ("missing-child-bridge-registration", xai_grok_sampling_types::ApiBackend::Messages, true, "missing"),
            ("same-named-unrelated-child-registration", xai_grok_sampling_types::ApiBackend::Messages, true, "unrelated"),
        ]);
        for (scenario, backend, flag, registration) in scenarios {
            let mut fixture = parent_golden_fixture(xai_grok_sampling_types::ApiBackend::Messages).await;
            let actor = std::sync::Arc::get_mut(&mut fixture.actor).unwrap();
            actor.startup_hints.is_subagent = true;
            actor.startup_hints.subagent_type = Some("explore".into());
            actor.forked_tool_override = Some(super::ToolSpecBundle::without_identity(snapshot.clone()));
            if registration != "matching" {
                actor.agent.replace(test_agent_with_tools(Vec::new()).await);
                if registration == "unrelated" {
                    for name in ["search_tool", "use_tool"] {
                        parent_golden_register(actor, name, ToolNamespace::GrokBuild, ToolKind::Other);
                    }
                }
            }
            parent_golden_register(actor, "child__lookup", ToolNamespace::MCP, ToolKind::Other);
            let xai_grok_sampling_types::ConversationItem::User(mut user) = xai_grok_sampling_types::ConversationItem::user("Continue the fork after discovery") else { unreachable!() };
            user.add_image(concat!("data:image/png;base64,", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="));
            fixture.history.push(xai_grok_sampling_types::ConversationItem::User(user));
            let (chat_events, mut chat_rx) = tokio::sync::mpsc::unbounded_channel();
            let cancel = tokio_util::sync::CancellationToken::new();
            actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
                fixture.history.clone(), fixture.binding.config.clone(), Box::new(xai_chat_state::NullChatPersistence), chat_events, cancel.clone(),
            );
            actor.chat_state_handle.install_transport_binding(fixture.binding.clone()).await.unwrap();
            let selected = actor.tool_candidates_for_turn(xai_chat_state::NativeToolCandidates::builtins_only(Vec::new()), None)
                .select_for_row(&fixture.binding.config);
            assert_eq!(serde_json::to_value(&selected).unwrap(), serde_json::to_value(&snapshot).unwrap());
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
                actor.turn_sampler_config_from_binding(&fixture.binding).await,
                xai_grok_sampler::RetryPolicy { max_retries: 0, ..Default::default() }, event_tx,
            );
            actor.workspace_ops.bind_local_session(
                &actor.session_id_string(), actor.tool_context.cwd.as_path().to_path_buf(),
                actor.tool_context.hunk_tracker_handle.clone(), actor.agent.borrow().tool_bridge().toolset(), None,
            ).unwrap();
            let live = parent_golden_live_binding(&fixture, scenario, backend, flag);
            let drainer = fixture.actor.clone();
            let drain = tokio::task::spawn_local(async move {
                while let Some(event) = event_rx.recv().await { drainer.handle_sampling_event(event).await; }
            });
            let mut salvage = super::length_salvage::LengthSalvage::new(None);
            let mut sampling = Default::default();
            let mut turn = Box::pin(fixture.actor.process_conversation_turn_with_recovery(
                "parent-fork-capture", None, None, None, &mut salvage, &mut sampling,
            ));
            tokio::time::timeout(std::time::Duration::from_secs(60), std::future::poll_fn(|cx| {
                while let Ok(event) = chat_rx.try_recv() {
                    if let xai_chat_state::ChatStateEvent::ImageBudget { inline_images, .. } = event {
                        assert_eq!(inline_images, 1);
                        return std::task::Poll::Ready(());
                    }
                }
                if let std::task::Poll::Ready(result) = std::future::Future::poll(turn.as_mut(), cx) {
                    panic!("fork completed before the bound-build gate: {:?}", result.err());
                }
                std::task::Poll::Pending
            })).await.expect("fork bound-build event is reachable");
            assert!(fixture.wire.exchanges.borrow().is_empty(), "binding gate precedes the first send");
            fixture.actor.chat_state_handle.install_transport_binding(live.clone()).await.unwrap();
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), turn).await.expect("actual fork turn completes");
            drain.abort();
            let _ = drain.await;
            assert!(outcome.is_ok(), "parent fork refused: {:?}", outcome.err());
            let request = xai_grok_sampling_types::ConversationRequest {
                tools: selected,
                search_admission: Some(xai_grok_sampling_types::SearchAdmission::for_row(true, &snapshot)),
                model: Some(fixture.binding.config.model.clone()),
                ..Default::default()
            };
            assert_eq!(serde_json::to_value(&fixture.actor.forked_tool_override.as_ref().unwrap().specs).unwrap(), serde_json::to_value(&snapshot).unwrap());
            parent_golden_capture_body(&format!("fork/{scenario}"), &fixture, &request,
                "actual parent SnapshotToolDefinitions reply through the unchanged fork selector, validated against HTTP; ImageBudget gate holds the turn before the bound-build reply is consumed", parent_registry.clone());
            cancel.cancel();
        }
        for (state, bundle) in parent.actor.a4_identity_snapshots(commands.clone(), &snapshot).await {
            assert_eq!(bundle.has_valid_identity(), state == "valid");
            a4_fork_companion_scenarios(&bundle, parent_registry.clone()).await;
        }
    }).await;
}

async fn a3_fork_surface(snapshot: &super::ToolSpecBundle, registration: &str, admitted: bool) {
    use xai_grok_sampling_types::ApiBackend;
    use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
    let mut fixture = parent_golden_fixture(ApiBackend::Responses).await;
    fixture.binding.config.supports_search_tool = admitted;
    fixture
        .history
        .push(ConversationItem::user("Complete the actual fork request."));
    let actor = Arc::get_mut(&mut fixture.actor).unwrap();
    actor.agent.replace(test_agent_with_tools(Vec::new()).await);
    if registration == "unrelated" {
        for name in ["bridge__discover", "bridge__invoke"] {
            parent_golden_register(actor, name, ToolNamespace::GrokBuild, ToolKind::Other);
        }
    }
    parent_golden_register(actor, "child__lookup", ToolNamespace::MCP, ToolKind::Other);
    actor.startup_hints.is_subagent = true;
    actor.startup_hints.subagent_type = Some("explore".into());
    actor.forked_tool_override = Some(snapshot.clone());
    let mut entry = entry_with_deferred_tools("r12-parent", admitted, &["child__lookup"]);
    entry.info.model_family = Some("xai".into());
    actor.models_manager.insert_test_entry("r12-parent", entry);
    actor.max_retries = 0;
    actor.transient_retry_enabled = false;
    actor
        .chat_state_handle
        .install_transport_binding(fixture.binding.clone())
        .await
        .unwrap();
    actor
        .chat_state_handle
        .replace_conversation(fixture.history.clone());
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        actor
            .turn_sampler_config_from_binding(&fixture.binding)
            .await,
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
    let live = parent_golden_live_binding(
        &fixture,
        "messages-admission-off",
        ApiBackend::Messages,
        false,
    );
    let _trace = SessionActor::a3_trace_outer(Some(live.clone()));
    let drainer = fixture.actor.clone();
    let drain = tokio_util::task::AbortOnDropHandle::new(tokio::task::spawn_local(async move {
        while let Some(event) = event_rx.recv().await {
            drainer.handle_sampling_event(event).await;
        }
    }));
    let mut salvage = super::length_salvage::LengthSalvage::new(None);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        fixture.actor.process_conversation_turn_with_recovery(
            "a3-fork",
            None,
            None,
            None,
            &mut salvage,
            &mut Default::default(),
        ),
    )
    .await
    .expect("A3 actual fork completes");
    drain.abort();
    let _ = drain.await;
    let builds = SessionActor::a3_take_outer_builds();
    assert_eq!(
        builds.len(),
        1,
        "A3 fork uses exactly one bound actor build"
    );
    let (original, binding) = &builds[0];
    assert_eq!(
        serde_json::to_value(&binding.config).unwrap(),
        serde_json::to_value(&fixture.binding.config).unwrap()
    );
    assert_eq!(binding.credentials, fixture.binding.credentials);
    assert_eq!(original.search_admission.unwrap().admitted(), admitted);
    assert_eq!(
        serde_json::to_value(&original.tools).unwrap(),
        serde_json::to_value(&snapshot.specs).unwrap(),
        "A3 fork keeps exact parent coordinates, schemas, exposure and empty MCP membership"
    );
    let installed = fixture
        .actor
        .chat_state_handle
        .capture_transport_binding()
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&installed.config).unwrap(),
        serde_json::to_value(&live.config).unwrap()
    );
    assert!(
        fixture.live_wire.exchanges.borrow().is_empty(),
        "A3 fork keeps captured transport"
    );
    assert_eq!(
        serde_json::to_value(&fixture.actor.forked_tool_override.as_ref().unwrap().specs).unwrap(),
        serde_json::to_value(&snapshot.specs).unwrap(),
        "A3 fork input remains unchanged"
    );
    if admitted && !snapshot.has_valid_identity() {
        let Err(error) = outcome else {
            panic!("A3 admitted fork refuses missing or invalid identity");
        };
        assert!(format!("{error:?}").contains("fork bridge identity is unavailable or mismatched"));
        assert!(
            fixture.wire.exchanges.borrow().is_empty(),
            "A3 refusal precedes submission"
        );
        return;
    }
    assert!(
        outcome.is_ok(),
        "A3 actual fork failed: {:?}",
        outcome.err()
    );
    assert_eq!(fixture.wire.exchanges.borrow().len(), 1);
    let sent = fixture.wire.exchanges.borrow()[0].clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&sent.raw_body).unwrap(),
        sent.parsed_body
    );
    let mut baseline_request = original.clone();
    baseline_request.hosted_tools = fixture.actor.hosted_tools_for_turn();
    let client = xai_grok_sampler::SamplingClient::new(
        fixture
            .actor
            .turn_sampler_config_from_binding(binding)
            .await,
    )
    .unwrap();
    let (stream, _, _) = client
        .conversation_stream_responses(baseline_request, &mut None)
        .await
        .unwrap();
    let events: Vec<_> = futures::TryStreamExt::try_collect(stream).await.unwrap();
    assert!(!events.is_empty());
    assert_eq!(fixture.wire.exchanges.borrow().len(), 2);
    let mut expected = fixture.wire.exchanges.borrow()[1].parsed_body.clone();
    if admitted {
        expected["tools"].as_array_mut().unwrap().retain(|tool| {
            !(tool["type"] == "function"
                && ["bridge__discover", "bridge__invoke"]
                    .contains(&tool["name"].as_str().unwrap_or_default()))
        });
    }
    assert_eq!(
        sent.parsed_body["tools"], expected["tools"],
        "A3 actual fork removes only parent bridge identities: {registration}, admitted={admitted}"
    );
    assert_eq!(
        sent.parsed_body, expected,
        "A3 fork full remaining body is unchanged"
    );
    assert_eq!(sent.parsed_body["store"], false);
    assert_eq!(
        sent.parsed_body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tool| tool["type"] == "tool_search")
            .count(),
        usize::from(admitted)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn responses_fork_override_bridge_cutover() {
    const NAME: &str =
        "session::acp_session::turn_end_reporting_tests::responses_fork_override_bridge_cutover";
    if parent_golden_isolated(NAME) {
        return;
    }
    run(async {
        use xai_grok_tools::registry::types::ToolConfig;
        use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
        let mut parent = Harness::new().await;
        parent.actor.agent.replace(test_agent_with_tools(vec![
            ToolConfig::for_tool::<xai_grok_tools::implementations::search_tool::SearchTool>().with_name("bridge__discover"),
            ToolConfig::for_tool::<xai_grok_tools::implementations::use_tool::UseTool>().with_name("bridge__invoke"),
        ]).await);
        parent.actor.models_manager.insert_test_entry("r12-parent",
            entry_with_deferred_tools("r12-parent", true, &["bridge__invoke", "z_builtin"]));
        parent.actor.models_manager.set_current_model_id(acp::ModelId::new("r12-parent"));
        for (name, namespace, kind) in [
            ("z_builtin", ToolNamespace::GrokBuild, ToolKind::Other),
            ("builtin__renamed", ToolNamespace::GrokBuild, ToolKind::Other),
            ("search_tool", ToolNamespace::GrokBuild, ToolKind::Other),
            ("use_tool", ToolNamespace::GrokBuild, ToolKind::Other),
            ("policy__root_message", ToolNamespace::GrokBuild, ToolKind::ActiveAgentMessage),
            ("ask_user_question", ToolNamespace::GrokBuild, ToolKind::Other),
            ("workflow", ToolNamespace::GrokBuild, ToolKind::Workflow),
            ("alpha__lookup", ToolNamespace::MCP, ToolKind::Other),
        ] { parent_golden_register(&parent.actor, name, namespace, kind); }
        let (commands, _) = parent.spawn_loop().await;
        let mut snapshots = Vec::new();
        for _ in 0..2 {
            let (respond_to, reply) = tokio::sync::oneshot::channel();
            commands.send(SessionCommand::SnapshotToolDefinitionsWithIdentity { respond_to }).unwrap();
            let snapshot = tokio::time::timeout(std::time::Duration::from_secs(30), reply).await.unwrap().unwrap()
                .filter_for_subagent();
            assert!(snapshot.has_valid_identity());
            assert!(!snapshot.specs.iter().any(|spec| ["alpha__lookup", "policy__root_message", "ask_user_question", "workflow"].contains(&spec.name.as_str())));
            for name in ["bridge__discover", "bridge__invoke", "search_tool", "use_tool", "builtin__renamed"] {
                assert!(snapshot.specs.iter().any(|spec| spec.name == name), "A3 parent snapshot anti-vacuity: {name}");
            }
            snapshots.push(snapshot);
        }
        for registration in ["missing", "unrelated"] {
            a3_fork_surface(&snapshots[0], registration, true).await;
        }
        for state in ["missing", "stale", "mismatched"] {
            let mut snapshot = snapshots[0].clone();
            match state {
                "missing" => snapshot.companion = None,
                "stale" => snapshot.companion = snapshots[1].companion.clone(),
                "mismatched" => snapshot.specs[0].parameters = serde_json::json!({"type":"object","properties":{"replacement":{"type":"integer"}}}),
                _ => unreachable!(),
            }
            assert!(!snapshot.has_valid_identity());
            a3_fork_surface(&snapshot, "missing", true).await;
            a3_fork_surface(&snapshot, "missing", false).await;
        }
        commands.send(SessionCommand::Shutdown(crate::session::ShutdownKind::Graceful)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), commands.closed()).await.unwrap();
        parent.chat.take();
    }).await;
}

async fn a4_fork_companion_scenarios(
    bundle: &super::ToolSpecBundle,
    parent_registry: serde_json::Value,
) {
    use xai_grok_tools::types::tool::{ToolKind, ToolNamespace};
    let snapshot = &bundle.specs;
    let mut scenarios: Vec<_> = parent_golden_variants()
        .into_iter()
        .map(|(id, backend, flag)| (id, backend, flag, "matching"))
        .collect();
    scenarios.extend([
        (
            "missing-child-bridge-registration",
            xai_grok_sampling_types::ApiBackend::Messages,
            true,
            "missing",
        ),
        (
            "same-named-unrelated-child-registration",
            xai_grok_sampling_types::ApiBackend::Messages,
            true,
            "unrelated",
        ),
    ]);
    for (scenario, backend, flag, registration) in scenarios {
        let mut fixture =
            parent_golden_fixture(xai_grok_sampling_types::ApiBackend::Messages).await;
        let actor = std::sync::Arc::get_mut(&mut fixture.actor).unwrap();
        actor.startup_hints.is_subagent = true;
        actor.startup_hints.subagent_type = Some("explore".into());
        actor.forked_tool_override = Some(bundle.clone());
        if registration != "matching" {
            actor.agent.replace(test_agent_with_tools(Vec::new()).await);
            if registration == "unrelated" {
                for name in ["search_tool", "use_tool"] {
                    parent_golden_register(actor, name, ToolNamespace::GrokBuild, ToolKind::Other);
                }
            }
        }
        parent_golden_register(actor, "child__lookup", ToolNamespace::MCP, ToolKind::Other);
        let xai_grok_sampling_types::ConversationItem::User(mut user) =
            xai_grok_sampling_types::ConversationItem::user("Continue the fork after discovery")
        else {
            unreachable!()
        };
        user.add_image(concat!("data:image/png;base64,", "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="));
        fixture
            .history
            .push(xai_grok_sampling_types::ConversationItem::User(user));
        let (chat_events, mut chat_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        actor.chat_state_handle = xai_chat_state::ChatStateActor::spawn(
            fixture.history.clone(),
            fixture.binding.config.clone(),
            Box::new(xai_chat_state::NullChatPersistence),
            chat_events,
            cancel.clone(),
        );
        actor
            .chat_state_handle
            .install_transport_binding(fixture.binding.clone())
            .await
            .unwrap();
        let selected = actor
            .tool_candidates_for_turn(
                xai_chat_state::NativeToolCandidates::builtins_only(Vec::new()),
                None,
            )
            .select_for_row(&fixture.binding.config);
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            serde_json::to_value(&snapshot).unwrap()
        );
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        actor.sampler_handle = xai_grok_sampler::SamplerActor::spawn(
            actor
                .turn_sampler_config_from_binding(&fixture.binding)
                .await,
            xai_grok_sampler::RetryPolicy {
                max_retries: 0,
                ..Default::default()
            },
            event_tx,
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
        let live = parent_golden_live_binding(&fixture, scenario, backend, flag);
        let drainer = fixture.actor.clone();
        let drain = tokio::task::spawn_local(async move {
            while let Some(event) = event_rx.recv().await {
                drainer.handle_sampling_event(event).await;
            }
        });
        let mut salvage = super::length_salvage::LengthSalvage::new(None);
        let mut sampling = Default::default();
        let mut turn = Box::pin(fixture.actor.process_conversation_turn_with_recovery(
            "parent-fork-capture",
            None,
            None,
            None,
            &mut salvage,
            &mut sampling,
        ));
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            std::future::poll_fn(|cx| {
                while let Ok(event) = chat_rx.try_recv() {
                    if let xai_chat_state::ChatStateEvent::ImageBudget { inline_images, .. } = event
                    {
                        assert_eq!(inline_images, 1);
                        return std::task::Poll::Ready(());
                    }
                }
                if let std::task::Poll::Ready(result) = std::future::Future::poll(turn.as_mut(), cx)
                {
                    panic!(
                        "fork completed before the bound-build gate: {:?}",
                        result.err()
                    );
                }
                std::task::Poll::Pending
            }),
        )
        .await
        .expect("fork bound-build event is reachable");
        assert!(
            fixture.wire.exchanges.borrow().is_empty(),
            "binding gate precedes the first send"
        );
        fixture
            .actor
            .chat_state_handle
            .install_transport_binding(live.clone())
            .await
            .unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), turn)
            .await
            .expect("actual fork turn completes");
        drain.abort();
        let _ = drain.await;
        assert!(outcome.is_ok(), "parent fork refused: {:?}", outcome.err());
        let request = xai_grok_sampling_types::ConversationRequest {
            tools: selected,
            search_admission: Some(xai_grok_sampling_types::SearchAdmission::for_row(
                true, &snapshot,
            )),
            model: Some(fixture.binding.config.model.clone()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&fixture.actor.forked_tool_override.as_ref().unwrap().specs)
                .unwrap(),
            serde_json::to_value(&snapshot).unwrap()
        );
        parent_golden_capture_body(
            &format!("fork/{scenario}"),
            &fixture,
            &request,
            "actual parent SnapshotToolDefinitions reply through the unchanged fork selector, validated against HTTP; ImageBudget gate holds the turn before the bound-build reply is consumed",
            parent_registry.clone(),
        );
        cancel.cancel();
    }
}

/// waj2-U8 transport precondition: a bound build now needs an explicitly
/// captured entry. These fixtures drive the legacy-equivalent route, so they
/// attach Inactive/SendOnly; every assertion stays as written.
fn u8_inactive_entry() -> xai_chat_state::NativeHistoryEntry {
    xai_chat_state::NativeHistoryEntry::send_only(
        xai_grok_sampling_types::conversation::native_search_history::NativeHistoryRequirement::Inactive,
    )
}

fn u8_with_entry(
    candidates: xai_chat_state::NativeToolCandidates,
) -> xai_chat_state::NativeToolCandidates {
    candidates
        .with_history_entry(u8_inactive_entry())
        .expect("freshly built candidates carry no entry")
}
