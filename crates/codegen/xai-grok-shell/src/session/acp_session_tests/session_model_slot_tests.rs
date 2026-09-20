//! MA-4 pre-roll (micro-fix B): write-side pin for the spec's
//! `session_model_id` slot (see the test docs below).

use super::support::*;
use super::*;
use crate::terminal::AsyncTerminalRunner;
use crate::terminal::runner::{TerminalError, TerminalRunRequest, TerminalRunResult};
use tokio::sync::mpsc;
use xai_grok_paths::AbsPathBuf;
use xai_grok_workspace::file_system::MockFs;
use xai_grok_workspace::permission::PermissionHandle;
#[derive(Debug)]
struct DummyTerminal;
#[async_trait::async_trait]
impl AsyncTerminalRunner for DummyTerminal {
    async fn run(&self, _request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        Err(TerminalError::Other("dummy terminal".into()))
    }
}
/// Create a minimal SessionActor for testing auto-compact logic.
async fn create_test_actor(
    total_tokens: u64,
    context_window: u64,
    threshold_percent: u8,
    gateway_tx: mpsc::UnboundedSender<xai_acp_lib::AcpClientMessage>,
    persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
) -> SessionActor {
    let cwd = AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap();
    let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
    let terminal = Arc::new(DummyTerminal {});
    let (hunk_tx, _hunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let hunk_tracker_handle = xai_hunk_tracker::HunkTrackerActor::spawn(
        "test-auto-compact".to_string(),
        cwd.to_path_buf(),
        hunk_tx,
        xai_hunk_tracker::TrackingMode::AgentOnly,
        tokio_util::sync::CancellationToken::new(),
    );
    let tool_context = ToolContext::new(cwd.clone(), None, None, fs, terminal, hunk_tracker_handle);
    let state = TokioMutex::new(State {
        running_task: None,
        finalization_gate: Default::default(),
        message_delivery: Default::default(),
        pending_inputs: VecDeque::new(),
        edit_holds: HashMap::new(),
        pending_notifications: Vec::new(),
        notifications_suppressed: false,
        rewindable: false,
        front_message_committed: false,
        hook_block_hold: Default::default(),
        nudges_used_this_session: 0,
    });
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let chat_state_handle = xai_chat_state::ChatStateActor::spawn(
        vec![],
        xai_grok_sampling_types::SamplingConfig {
            base_url: "http://localhost".to_string(),
            mtls_cert_dir: None,
            model: "test".to_string(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            max_retries: None,
            rate_limit_retry_threshold: None,
            api_backend: Default::default(),
            extra_headers: Default::default(),
            conversation_group_id: None,
            query_params: Default::default(),
            env_http_headers: Default::default(),
            context_window: std::num::NonZeroU64::new(context_window)
                .expect("test context_window must be non-zero"),
            reasoning_effort: None,
            ultra_wire_effort: None,
            stream_tool_calls: None,
            cache_ttl: None,
        },
        Box::new(xai_chat_state::NullChatPersistence),
        event_tx,
        tokio_util::sync::CancellationToken::new(),
    );
    chat_state_handle.record_token_usage(total_tokens);
    SessionActor {
        pending_native_agent_messages: Default::default(),
        repo_status_prefetch: crate::session::repo_status_prefix::RepoStatusPrefetchState::default(
        ),
        transient_retry_enabled: true,
        transient_retries_prompt_total: std::cell::Cell::new(0),
        transient_episode_start: std::cell::Cell::new(None),
        status_wake: Default::default(),
        active_work: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        session_info: SessionInfo {
            id: acp::SessionId::new("test-auto-compact"),
            cwd: cwd.as_str().to_string(),
        },
        rebuild_spec: crate::session::agent_rebuild::test_rebuild_spec_default(),
        auth_method_id: test_auth_method_id("test-auth"),
        model_auth_memo: std::cell::RefCell::new(None),
        attribution_callback: None,
        auth_manager: None,
        is_chat_kind: false,
        state,
        notifications: NotificationSender::for_tests(
            GatewaySender::new(gateway_tx),
            persistence_tx,
        ),
        permissions: PermissionHandle::allow_all(),
        tool_context,
        deny_read_globs: Vec::new(),
        mcp_state: Arc::new(TokioMutex::new(McpState::new(vec![]))),
        mcp_strategy: std::cell::Cell::new(McpInitStrategy::Blocking),
        delivery_tools: std::cell::RefCell::new(Vec::new()),
        attach_non_interactive: std::rc::Rc::new(std::cell::Cell::new(false)),
        chat_state_handle,
        unattributed_background_usage: std::sync::atomic::AtomicBool::new(false),
        current_prompt_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
        pending_interactions: std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        current_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
        turn_start_prompt_mode: parking_lot::Mutex::new(PromptMode::Agent),
        turn_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
        telemetry_enabled: false,
        supports_backend_search: std::cell::Cell::new(false),
        tool_overrides: std::cell::RefCell::new(None),
        resolved_tool_overrides: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
        compactions_remaining: std::cell::Cell::new(None),
        compaction_at_tokens: std::cell::Cell::new(None),
        doom_loop_recovery: None,
        doom_loop_turn_tally: Default::default(),
        file_state_tracker: Arc::new(FileStateTracker::new()),
        rewind_pending_prompt: std::sync::Mutex::new(None),
        startup_hints: StartupHints::default(),
        forked_tool_override: None,
        compaction: crate::session::compaction_config::CompactionConfig {
            threshold_percent: std::cell::Cell::new(threshold_percent),
            force_compact: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            context_window_override: None,
            count: std::sync::atomic::AtomicU64::new(0),
            auto_compact_suppressed: std::sync::atomic::AtomicU8::new(0),
            previous_model: std::cell::Cell::new(None),
            compaction_mode: xai_chat_state::CompactionMode::Transcript,
            verbatim_input: true,
            tool_choice: crate::util::config::CompactionToolChoice::Auto,
            prefire: crate::session::compaction_config::PrefireState::default(),
            prefix_released: std::sync::atomic::AtomicBool::new(false),
            cancel: Default::default(),
        },
        memory: crate::session::memory_state::SessionMemory {
            configured_mode: None,
            configured_storage: None,
            flush_config: crate::config::MemoryFlushConfig::default(),
            is_flushing: std::sync::atomic::AtomicBool::new(false),
            last_flush_compaction: std::sync::atomic::AtomicU64::new(0),
            storage: std::cell::RefCell::new(None),
            save_on_end: true,
            backend_params: None,
            initial_injection_config: Default::default(),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            flush_count: std::sync::atomic::AtomicU64::new(0),
            last_flush_content: std::cell::RefCell::new(None),
            flush_success_count: std::sync::atomic::AtomicU64::new(0),
            flush_error_count: std::sync::atomic::AtomicU64::new(0),
            search_counter: std::cell::RefCell::new(None),
            injection_count: std::sync::atomic::AtomicU64::new(0),
            compaction_recovery_count: std::sync::atomic::AtomicU64::new(0),
            chunks_added: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            init_reindex_handle: std::cell::RefCell::new(None),
            dream_config: Default::default(),
            dream_count: std::sync::atomic::AtomicU64::new(0),
            dream_success_count: std::sync::atomic::AtomicU64::new(0),
            dream_error_count: std::sync::atomic::AtomicU64::new(0),
        },
        session_start: std::time::Instant::now(),
        inference_idle_timeout: Duration::from_secs(300),
        uncharged_401_park_enabled: true,
        max_retries: 3,
        rate_limit_waits: crate::session::acp_session::RateLimitWaitConfig::default(),
        max_turns: None,
        pending_interjections: InterjectionBuffer::new(),
        pending_skill_reminders: Mutex::new(Vec::new()),
        idle_flush_timeout: None,
        dream_check_timeout: None,
        last_idle_flush_conversation_len: std::sync::atomic::AtomicUsize::new(0),
        event_tx: {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            tx
        },
        buffering_settings: None,
        client_identifier: None,
        origin_client: None,
        feedback_manager: Arc::new(FeedbackManager::local_only("test-session")),
        upload_queue: Arc::new(OnceLock::new()),
        sync_loop_cancel: None,
        agent: std::cell::RefCell::new(test_agent_default().await),
        last_reported_branch: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        git_head_enabled: false,
        status_line_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        models_manager: Default::default(),
        display_cwd: std::sync::OnceLock::new(),
        active_agent_type: parking_lot::Mutex::new(None),
        queue_exit_reminder_on_approved_exit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        emit_local_background_tasks: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        active_skill: parking_lot::Mutex::new(None),
        plan_mode: Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PlanModeTracker::new(std::path::PathBuf::from(
                "/tmp/test-session",
            )),
        )),
        goal_enabled: false,
        background_workflows_enabled: false,
        goal_harness_enabled: std::sync::atomic::AtomicBool::new(false),
        goal_harness_availability_reconciled: std::sync::atomic::AtomicBool::new(false),
        goal_tracker: Arc::new(parking_lot::Mutex::new(
            crate::session::goal_tracker::GoalTracker::new(std::path::PathBuf::from(
                "/tmp/test-session",
            )),
        )),
        goal_turn_task_ids: parking_lot::Mutex::new(std::collections::HashSet::new()),
        goal_continuation_streak: std::sync::atomic::AtomicU32::new(0),
        goal_blocked_streak: std::sync::atomic::AtomicU32::new(0),
        goal_update_rx: std::cell::RefCell::new(None),
        goal_update_tx: tokio::sync::mpsc::unbounded_channel().0,
        workflow_manager: crate::session::workflow::manager::WorkflowManager::test_bundle().0,
        workflow_launch_tx: tokio::sync::mpsc::unbounded_channel().0,
        goal_classifier_enabled: false,
        goal_planner_enabled: false,
        goal_summary_enabled: false,
        length_salvage_remote_budget: None,
        goal_verifier_skeptic_count: 1,
        goal_role_models: Default::default(),
        goal_use_current_model_only: false,
        goal_classifier_max_runs: crate::session::goal_classifier::GOAL_CLASSIFIER_MAX_RUNS_DEFAULT,
        goal_strategist_every: 5,
        goal_reverify_after: crate::session::acp_session::GOAL_REVERIFY_AFTER_DEFAULT,
        goal_plan_reconciled: std::sync::atomic::AtomicBool::new(false),
        pending_classifier_completions: parking_lot::Mutex::new(std::collections::VecDeque::new()),
        goal_classifier_in_flight: std::sync::atomic::AtomicBool::new(false),
        managed_mcp_handle: Default::default(),
        initial_client_mcp_servers: vec![],
        tool_metadata_snapshot: Arc::new(std::sync::Mutex::new(Default::default())),
        mcp_announcements: Default::default(),
        mcp_reminder_mode: McpReminderMode::Delta,
        mcp_reminder_dirty: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        mcp_connecting_reminder_injected: std::cell::Cell::new(false),
        mcp_refresh_gate: Arc::new(tokio::sync::Mutex::new(())),
        user_input_generation: std::sync::atomic::AtomicU64::new(0),
        laziness_debug_log: None,
        last_live_orphan_reconcile: std::cell::Cell::new(None),
        deferred_prefix: DeferredPrefix::new(),
        mcp_startup_waits: Default::default(),
        mcp_init_tasks: Default::default(),
        weak_self: std::sync::Weak::new(),
        startup_tasks: Default::default(),
        extension_registry: xai_agent_lifecycle::LocalExtensionRegistry::default(),
        last_announced_local_date: std::cell::Cell::new(chrono::Local::now().date_naive()),
        prefix_carries_fallback_date: std::cell::Cell::new(false),
        last_search_prompt_index: std::sync::atomic::AtomicI64::new(-1),
        last_api_request_at: std::sync::atomic::AtomicI64::new(0),
        hook_registry: std::cell::RefCell::new(None),
        hook_disabled: Default::default(),
        turn_report: Default::default(),
        turn_abort: Default::default(),
        turn_end_tx: Default::default(),
        client_hooks: Default::default(),
        hook_resolved_workspace_root: String::new(),
        vcs_kind: xai_grok_workspace::session::git::VcsKind::Git,
        hook_load_errors: std::cell::RefCell::new(Vec::new()),
        plugin_registry: std::cell::RefCell::new(None),
        plugin_registry_handle: None,
        events: crate::session::events::EventTracker::new(std::path::Path::new("/tmp")),
        observability_bridge: noop_observability_bridge(),
        current_turn_number: std::cell::Cell::new(0),
        turn_phases: std::sync::Arc::default(),
        last_recap_main_turn: std::cell::Cell::new(0),
        recap_in_flight: std::cell::Cell::new(false),
        recap_epoch: std::cell::Cell::new(0),
        turn_summary_task: std::cell::RefCell::new(None),
        turn_summary_generation: std::cell::Cell::new(0),
        title_refresh_task: std::cell::RefCell::new(None),
        title_refresh_generation: std::cell::Cell::new(0),
        next_title_refresh_idx: std::cell::Cell::new(0),
        turn_summary_enabled: false,
        title_refresh_enabled: false,
        session_turn_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        streaming_turn_capture: parking_lot::Mutex::new(StreamingTurnCapture::default()),
        stream_apply_span: parking_lot::Mutex::new(None),
        current_turn_span_id: parking_lot::Mutex::new(None),
        turn_stream_drained: parking_lot::Mutex::new(std::collections::HashMap::new()),
        pending_image_strip: parking_lot::Mutex::new(std::collections::HashMap::new()),
        pending_model_bound_strip: parking_lot::Mutex::new(std::collections::HashMap::new()),
        image_strip_rewrite_barrier: ImageStripRewriteBarrier::new(),
        sampler_handle: xai_grok_sampler::SamplerHandle::noop(),
        sampling_gate: None,
        image_description_model: crate::test_support::TEST_MODEL.to_owned(),
        image_describe_cache: Arc::new(crate::session::image_describe::ImageDescribeCache::new()),
        subagent_token_records: parking_lot::Mutex::new(HashMap::new()),
        workspace_ops: xai_grok_workspace::WorkspaceOps::for_test(),
        trace_config_template: std::cell::RefCell::new(None),
    }
}

/// MA-4 pre-roll (micro-fix B): write-side pin for the spec's
/// `session_model_id` slot. The M-1 units (agent_rebuild.rs) pin the READ
/// side of the v2 gate — they write the slot directly and assert the gate
/// resolves `model_row` against it. The WRITE side was unpinned:
///
/// (1) `handle_set_session_model` must move the slot on SetSessionModel —
///     if that write is lost, a mid-session switch leaves the gate
///     evaluating the stale row (Q1.3/D-4 breakage);
/// (2) the slot is spawn-seeded with the session model —
///     `spawn_child_session` takes `session_model_id` as a parameter and
///     stores it verbatim in the spec (acp_session_impl/spawn.rs:1121);
///     the production constructor has no unit-testable seam, so the test
///     models the seed by writing the session model into the slot exactly
///     as the constructor does and pins that the seeded value is what the
///     read side (the M-1 units, the gate) sees.
#[tokio::test(flavor = "current_thread")]
async fn session_model_slot_tracks_set_session_model() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor = Arc::new(
                create_test_actor(50_000, 100_000, 85, gateway_tx, persistence_tx).await,
            );

            // (2) Spawn-model seed: the slot holds the session model at
            // construction time (the spawn.rs:1121 pass-through).
            let session_model = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("test actor has sampling config")
                .model;
            *actor
                .rebuild_spec
                .session_model_id
                .write()
                .expect("session model lock") = acp::ModelId::new(session_model.clone());
            assert_eq!(
                *actor
                    .rebuild_spec
                    .session_model_id
                    .read()
                    .expect("session model lock"),
                acp::ModelId::new(session_model.clone()),
                "spawn-seeded slot must hold the session model"
            );

            // (1) SetSessionModel: the handler moves the slot to the new
            // model — the v2 gate (agent_rebuild.rs) reads exactly this
            // slot on the next rebuild.
            let mut incoming = actor.reconstruct_full_config().await;
            incoming.model = "switched-model".to_string();
            actor
                .handle_set_session_model(incoming, false, false, false, true, 85)
                .await
                .expect("model switch succeeds");
            assert_eq!(
                *actor
                    .rebuild_spec
                    .session_model_id
                    .read()
                    .expect("session model lock"),
                acp::ModelId::new("switched-model".to_string()),
                "handle_set_session_model must move the spec slot (the v2 gate reads it)"
            );

            // Reverse direction: the slot must track the switch back (Q1.3
            // re-evaluates the gate in either direction).
            let mut back = actor.reconstruct_full_config().await;
            back.model = session_model.clone();
            actor
                .handle_set_session_model(back, false, false, false, true, 85)
                .await
                .expect("reverse model switch succeeds");
            assert_eq!(
                *actor
                    .rebuild_spec
                    .session_model_id
                    .read()
                    .expect("session model lock"),
                acp::ModelId::new(session_model.clone()),
                "the slot must track the reverse switch too"
            );
        })
        .await;
}
