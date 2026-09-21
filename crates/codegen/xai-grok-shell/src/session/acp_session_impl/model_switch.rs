use super::*;
use crate::remote::DEFAULT_CONTEXT_WINDOW;
use xai_chat_state::conversation_util::replace_or_insert_system_head;
impl SessionActor {
    pub(super) async fn handle_set_session_model(
        self: &std::sync::Arc<Self>,
        sampling_config: xai_grok_sampler::SamplerConfig,
        use_concise: bool,
        is_family_switch: bool,
        apply_prompt_override: bool,
        skip_prompt_rewrite: bool,
        auto_compact_threshold_percent: u8,
    ) -> Result<acp::ModelId, acp::Error> {
        let mut sampling_config = sampling_config;
        let prev_config = self.chat_state_handle.get_sampling_config().await;
        let prev_model = prev_config.as_ref().map(|c| c.model.clone());
        if let Some(id) = prev_config.and_then(|c| c.conversation_group_id) {
            sampling_config.conversation_group_id = Some(id);
        }
        let model_id = acp::ModelId::new(sampling_config.model.clone());
        // MA-3.1 (M-1): keep the spec's session-model slot in sync so a
        // later agent rebuild re-evaluates the v2 gate against THIS
        // session's row (D-4) — the process-shared cursor is not the gate
        // input (the TUI/Leader switch path never moves it).
        *self
            .rebuild_spec
            .session_model_id
            .write()
            .expect("session model lock poisoned") = model_id.clone();
        let new_context_window = self.compaction.context_window_override.unwrap_or_else(|| {
            std::num::NonZeroU64::new(sampling_config.context_window).unwrap_or_else(|| {
                std::num::NonZeroU64::new(DEFAULT_CONTEXT_WINDOW)
                    .expect("DEFAULT_CONTEXT_WINDOW is non-zero")
            })
        });
        let prev_threshold = self.compaction.threshold_percent.get();
        if prev_threshold != auto_compact_threshold_percent {
            tracing::info!(
                session_id = %self.session_info.id.0,
                new_model = %sampling_config.model,
                old_threshold = prev_threshold,
                new_threshold = auto_compact_threshold_percent,
                "auto_compact_threshold_percent updated for model switch"
            );
        }
        self.compaction
            .threshold_percent
            .set(auto_compact_threshold_percent);
        self.supports_backend_search
            .set(sampling_config.supports_backend_search);
        self.compactions_remaining
            .set(sampling_config.compactions_remaining);
        self.compaction_at_tokens
            .set(sampling_config.compaction_at_tokens);
        xai_grok_telemetry::unified_log::info(
            "backend_search: model switch",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "new_model": &sampling_config.model,
                "api_backend": format!("{:?}", sampling_config.api_backend),
                "supports_backend_search": sampling_config.supports_backend_search,
            })),
        );
        self.chat_state_handle
            .update_sampling_config(xai_grok_sampling_types::SamplingConfig {
                base_url: sampling_config.base_url.clone(),
                mtls_cert_dir: sampling_config.mtls_cert_dir.clone(),
                model: sampling_config.model.clone(),
                max_completion_tokens: sampling_config.max_completion_tokens,
                temperature: sampling_config.temperature,
                top_p: sampling_config.top_p,
                max_retries: Some(xai_grok_sampler::resolve_max_retries(
                    sampling_config.max_retries,
                )),
                rate_limit_retry_threshold: sampling_config.rate_limit_retry_threshold,
                api_backend: sampling_config.api_backend.clone(),
                extra_headers: sampling_config.extra_headers.clone(),
                conversation_group_id: sampling_config.conversation_group_id.clone(),
                query_params: sampling_config.query_params.clone(),
                env_http_headers: sampling_config.env_http_headers.clone(),
                context_window: new_context_window,
                reasoning_effort: sampling_config.reasoning_effort,
                ultra_wire_effort: sampling_config.ultra_wire_effort,
                stream_tool_calls: Some(sampling_config.stream_tool_calls),
                cache_ttl: sampling_config.cache_ttl.clone(),
                top_k: sampling_config.top_k,
                stop_sequences: sampling_config.stop_sequences.clone(),
                disable_parallel_tool_use: sampling_config.disable_parallel_tool_use,
                tool_cache_breakpoint: sampling_config.tool_cache_breakpoint,
            });
        let existing = self.chat_state_handle.get_credentials().await;
        let session_key = self
            .auth_manager
            .as_ref()
            .and_then(|am| am.current_or_expired().map(|a| a.key));
        self.chat_state_handle
            .update_credentials(xai_chat_state::Credentials {
                api_key: sampling_config.api_key.clone(),
                auth_type: crate::agent::config::resolve_chat_state_auth_type(
                    sampling_config.model.as_str(),
                    session_key.as_deref(),
                    existing.auth_type,
                ),
                alpha_test_key: existing.alpha_test_key,
                client_version: sampling_config.client_version.clone(),
            });
        self.invalidate_model_auth_memo();
        self.signals_handle()
            .record_model_usage(&sampling_config.model);
        if apply_prompt_override && !skip_prompt_rewrite {
            let mut conversation = self.chat_state_handle.get_conversation().await;
            for item in conversation.iter_mut() {
                if let ConversationItem::System(sys) = item {
                    if use_concise {
                        sys.content = std::sync::Arc::<str>::from(
                            xai_grok_agent::prompt::template::COMPACT_SYSTEM_PROMPT,
                        );
                    } else {
                        sys.content =
                            std::sync::Arc::<str>::from(self.agent.borrow().system_prompt());
                    }
                    break;
                }
            }
            self.chat_state_handle.replace_conversation(conversation);
        } else if !apply_prompt_override {
            tracing::info!(
                session_id = %self.session_info.id.0,
                model_id = %model_id.0,
                "handle_set_session_model: skipping prompt override (apply_prompt_override=false)"
            );
        } else {
            tracing::info!(
                session_id = %self.session_info.id.0,
                model_id = %model_id.0,
                "handle_set_session_model: skipping prompt rewrite (just rebuilt harness)"
            );
        }
        let agent_name = self.agent.borrow().definition().name.clone();
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CurrentModel {
                model_id: model_id.clone(),
                agent_name: Some(agent_name),
                reasoning_effort: Some(sampling_config.reasoning_effort),
            });
        self.emit_status_snapshot_detached();
        let turn_in_flight = self.state.lock().await.running_task.is_some();
        if turn_in_flight && is_family_switch {
            tracing::warn!("Family-switch compact skipped: turn in flight");
        }
        if is_family_switch
            && !turn_in_flight
            && family_switch_compact_required(sampling_config.api_backend)
            && self.history_has_model_minted_items().await
        {
            self.abort_and_clear_prefire().await;
            let estimated_total_tokens = self.chat_state_handle.get_estimated_total_tokens().await;
            let context_window = new_context_window.get();
            let trigger_info = compaction::AutoCompactTriggerInfo {
                tokens_used: estimated_total_tokens,
                context_window,
                percentage: xai_token_estimation::usage_percentage_u8(
                    estimated_total_tokens,
                    context_window,
                ),
            };
            tracing::info!("Family-switch compact: -> {}", sampling_config.model);
            if let Err(e) = self.run_compact_only(trigger_info, true).await {
                tracing::error!(error = %e, "Family-switch compaction failed; switching anyway");
            }
        }
        // XW-PROJECT-1 (apex-ayl.71): proactive switch-time projection for the
        // cross-wire switch; the actor no-ops (NoMatch) when the history is
        // already in the target form. Gated: real model-id change, no turn in flight.
        if !turn_in_flight
            && prev_model.as_deref() != Some(sampling_config.model.as_str())
        {
            // XW-ENC-AFFINITY-1 (apex-mf6): the target row's affinity pin
            // (empty header = unpin → None) feeds the switch-time gate.
            let target_pin = sampling_config
                .extra_headers
                .get(xai_grok_sampling_types::ENC_AFFINITY_PIN_HEADER)
                .map(|value| value.as_str())
                .filter(|value| !value.is_empty());
            self.apply_switch_projection(&sampling_config.model, target_pin).await;
        }
        Ok(model_id)
    }
    /// Set the reasoning effort on the live sampling config, applying the same
    /// support check and per-effort model routing as `apply_supported_effort`.
    pub(super) async fn handle_set_reasoning_effort(
        self: &std::sync::Arc<Self>,
        effort: xai_grok_sampling_types::ReasoningEffort,
    ) -> Result<acp::ModelId, acp::Error> {
        let Some(mut cfg) = self.chat_state_handle.get_sampling_config().await else {
            return Err(acp::Error::internal_error().data("session has no sampling config"));
        };
        if !self
            .models_manager
            .model_supports_reasoning_effort(&cfg.model)
        {
            return Err(acp::Error::invalid_params()
                .data("the session's current model does not support reasoning effort"));
        }
        if let Some(routed) = self.models_manager.model_for_effort(&cfg.model, effort) {
            cfg.model = routed;
        }
        cfg.reasoning_effort = Some(effort);
        // PROACTIVE-ULTRA-1 (apex-ayl.86, M1): the /effort writer is the
        // second session-effort writer — it maintains the sibling
        // menu-derived field against the POST-routing model (the same
        // post-routing anchor as the seed writer's second assignment), so
        // both writers produce identical egress for the same (effort, menu).
        cfg.ultra_wire_effort = self.models_manager.ultra_wire_effort_for(&cfg.model);
        let model_id = acp::ModelId::new(cfg.model.clone());
        self.chat_state_handle.update_sampling_config(cfg);
        let agent_name = self.agent.borrow().definition().name.clone();
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CurrentModel {
                model_id: model_id.clone(),
                agent_name: Some(agent_name),
                reasoning_effort: Some(Some(effort)),
            });
        self.emit_status_snapshot_detached();
        Ok(model_id)
    }
    /// Handle [`SessionCommand::RebuildAgentForDefinition`].
    /// Builds a fresh [`xai_grok_agent::Agent`] from the cached [`crate::session::agent_rebuild::AgentRebuildSpec`] and the supplied definition.
    /// Triggered from `MvpAgent::set_session_model` only when the new model's `agent_type` differs from the session's `active_agent_type`.
    pub(super) async fn handle_rebuild_agent_for_definition(
        &self,
        definition: xai_grok_agent::AgentDefinition,
    ) -> Result<(), acp::Error> {
        {
            let state = self.state.lock().await;
            if state.running_task.is_some() {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    new_agent_type = %definition.name,
                    "handle_rebuild_agent_for_definition: turn in flight, rejecting rebuild"
                );
                return Err(acp::Error::internal_error()
                    .data("rebuild_agent: turn in flight, refusing to rebuild harness"));
            }
        }
        let new_agent_name = definition.name.clone();
        tracing::info!(
            session_id = %self.session_info.id.0,
            new_agent_type = %new_agent_name,
            "handle_rebuild_agent_for_definition: rebuilding harness"
        );
        let new_agent = self
            .rebuild_spec
            .build_agent(definition)
            .await
            .map_err(|e| {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    new_agent_type = %new_agent_name,
                    error = %e,
                    "handle_rebuild_agent_for_definition: AgentBuilder::build failed"
                );
                acp::Error::internal_error().data(format!(
                    "rebuild_agent: build failed for agent_type={new_agent_name}: {e}"
                ))
            })?;
        let new_system_prompt = new_agent.system_prompt().to_string();
        let mut new_prompt_context = new_agent.prompt_context().clone();
        new_prompt_context.normalize_for_persistence();
        self.abort_and_clear_prefire().await;
        *self.agent.borrow_mut() = new_agent;
        *self.active_agent_type.lock() = Some(new_agent_name.clone());
        self.emit_resolved_tool_overrides();
        self.queue_exit_reminder_on_approved_exit.store(
            self.is_cursor_harness(),
            std::sync::atomic::Ordering::Relaxed,
        );
        if let Err(e) = self.workspace_ops.bind_local_session(
            &self.session_id_string(),
            self.tool_context.cwd.as_path().to_path_buf(),
            self.tool_context.hunk_tracker_handle.clone(),
            self.agent.borrow().tool_bridge().toolset(),
            None,
        ) {
            tracing::warn!(error = %e, "failed to rebind local session toolset after agent rebuild");
        }
        {
            let bridge = self.agent.borrow().tool_bridge().clone();
            let snapshot = self.tool_metadata_snapshot.clone();
            let tool_index = crate::session::tool_index::Bm25ToolSearchIndex::new(snapshot);
            bridge
                .update_resource(xai_grok_tools::types::tool_index::ToolIndex(
                    std::sync::Arc::new(tool_index),
                ))
                .await;
            if let Some(client) = self.rebuild_spec.managed_gateway_tool_client.clone() {
                bridge.update_resource(client).await;
            }
            let plan_path = self.plan_mode.lock().plan_file_path().to_path_buf();
            bridge
                .update_resource(xai_grok_tools::types::resources::PlanFilePath(plan_path))
                .await;
            if let Some(display_cwd) = self.display_cwd.get() {
                bridge
                    .set_display_cwd(std::path::PathBuf::from(display_cwd))
                    .await;
            }
            bridge
                .update_resource(
                    xai_grok_tools::implementations::grok_build::workflow::WorkflowLaunchHandle(
                        self.workflow_launch_tx.clone(),
                    ),
                )
                .await;
            if !self.goal_runs_on_workflow_engine() {
                bridge
                    .update_resource(
                        xai_grok_tools::implementations::grok_build::update_goal::GoalUpdateHandle(
                            self.goal_update_tx.clone(),
                        ),
                    )
                    .await;
            }
            if let Some(reservations) = self.tool_context.task_completion_reservations.clone() {
                bridge.update_resource(reservations).await;
            }
            if let Some(gate) = self.tool_context.task_wake_suppressed.clone() {
                bridge.update_resource(gate).await;
            }
            self.inject_deny_read_globs().await;
        }
        let claim = self.restart_mcp_init(&mut *self.mcp_state.lock().await);
        self.re_register_mcp_tools_on_rebuilt_bridge().await;
        self.run_mcp_init_with_claim(claim).await;
        self.deferred_prefix.cancel();
        let new_user_prefix = self
            .build_prefix_after_mcp_wait(self.requires_full_mcp_wait())
            .await;
        {
            let mut conversation = self.chat_state_handle.get_conversation().await;
            let _ = replace_or_insert_system_head(&mut conversation, &new_system_prompt);
            let drop_startup_skill_reminder = false;
            Self::rewrite_zero_turn_prefix(
                &mut conversation,
                new_user_prefix,
                drop_startup_skill_reminder,
            );
            if !conversation_has_project_instructions(&conversation)
                && let Some(agents_md_reminder) = self.agent.borrow().agents_md_user_reminder()
            {
                let agents_md_at = conversation.len().min(2);
                conversation.insert(
                    agents_md_at,
                    ConversationItem::project_instructions(agents_md_reminder),
                );
            }
            self.inject_baseline_skill_reminder(&mut conversation).await;
            self.chat_state_handle.replace_conversation(conversation);
        }
        save_prompt_context(&self.session_info, &new_prompt_context);
        save_system_prompt(&self.session_info, &new_system_prompt);
        let snapshot = self.chat_state_handle.get_conversation().await;
        persist_chat_history_jsonl_sync(&self.session_info, &snapshot);
        self.mcp_reminder_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.send_available_commands_update(AdvertiseTrigger::HarnessRebuild)
            .await;
        tracing::info!(
            session_id = %self.session_info.id.0,
            new_agent_type = %new_agent_name,
            "handle_rebuild_agent_for_definition: harness rebuild complete"
        );
        Ok(())
    }
    /// Apply a client-supplied `systemPromptOverride` on attach without wiping user/assistant history: swap only the leading `System` message.
    /// The swap happens atomically inside the `ChatStateActor`.
    /// `system_prompt.txt` (not owned by the persistence actor) is saved directly, even on a head no-op, so a diverged secondary artifact self-heals.
    pub(super) async fn handle_replace_system_prompt(&self, system_prompt: String) {
        if self.startup_hints.preserve_inherited_system {
            tracing::debug!(
                session_id = %self.session_info.id.0,
                "handle_replace_system_prompt: skipped (preserve_inherited_system)"
            );
            return;
        }
        let Some(changed) = self
            .chat_state_handle
            .replace_system_head(&system_prompt)
            .await
        else {
            tracing::error!(
                session_id = %self.session_info.id.0,
                "handle_replace_system_prompt: chat-state actor unavailable; override not applied"
            );
            return;
        };
        save_system_prompt(&self.session_info, &system_prompt);
        if changed {
            tracing::info!(
                session_id = %self.session_info.id.0,
                prompt_len = system_prompt.len(),
                "handle_replace_system_prompt: client override applied"
            );
        } else {
            tracing::debug!(
                session_id = %self.session_info.id.0,
                "handle_replace_system_prompt: head already matches, no-op"
            );
        }
    }
    /// Whether the conversation has anything a family switch must compact away.
    async fn history_has_model_minted_items(&self) -> bool {
        self.chat_state_handle
            .get_conversation()
            .await
            .iter()
            .any(|item| {
                matches!(
                    item,
                    xai_grok_sampling_types::ConversationItem::Assistant(_)
                        | xai_grok_sampling_types::ConversationItem::Reasoning(_)
                        | xai_grok_sampling_types::ConversationItem::BackendToolCall(_)
                )
            })
    }
    /// Abort and join an in-flight prefire pass-1 and drop its NOTE1 cache.
    pub(super) async fn abort_and_clear_prefire(&self) {
        if let Some(handle) = self.compaction.prefire.take_handle() {
            handle.abort();
            let _ = handle.await;
            self.compaction.prefire.finish();
        }
        self.compaction.prefire.clear();
    }
}

/// XW-XREPLAY-1 (apex-ayl.123): the family-switch compact must clear away
/// foreign model-minted state ONLY when the TARGET wire cannot portably
/// carry foreign reasoning items.
/// - `Messages` (/v1/messages, Vertex rows): signed thinking blocks — reasoning
///   text must be stripped for the summarizer (compaction.rs:1412-1415) and the
///   /messages build has no portable foreign-reasoning site; the monorepo
///   compact stands (pre-cut behavior preserved — GUARD-1 pin).
/// - `ChatCompletions`: fail-closed — monorepo behavior stands (no portability
///   evidence on that wire for foreign reasoning).
/// - `Responses` (/v1/responses): foreign reasoning IS portable — the .71
///   switch-time projection (T1 xw_ re-key + encrypted_content strip +
///   summary/content kept, projection.rs:165-215) + the send-time strict/lenient
///   projectors (strict: content+id stripped, summary rides — REPLAY-1
///   wire-proven, provider.rs:419-434; lenient: verbatim — M1 wire-proven) +
///   the mf6 affinity gate (ciphertext) + the 78a236f reactive net (400
///   fallback) own the whole seam. The preemptive compact would replace the
///   history BEFORE the projection runs and destroy the .62 R-1 replay — skip.
fn family_switch_compact_required(
    api_backend: xai_grok_sampling_types::ApiBackend,
) -> bool {
    !matches!(api_backend, xai_grok_sampling_types::ApiBackend::Responses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{Config, ModelEntry, ModelInfo};
    use indexmap::IndexMap;
    use xai_grok_sampling_types::{ReasoningEffort, ReasoningEffortOption};

    fn entry_with_menu(id: &str, supports: bool, menu: &[(ReasoningEffort, bool)]) -> ModelEntry {
        let mut info = ModelInfo::fallback(id);
        info.supports_reasoning_effort = supports;
        info.reasoning_efforts = menu
            .iter()
            .map(|(effort, default)| ReasoningEffortOption {
                id: effort.as_str().to_owned(),
                value: *effort,
                label: effort.as_str().to_owned(),
                description: None,
                default: *default,
            })
            .collect();
        ModelEntry {
            info,
            mtls_cert_dir: None,
            api_key: None,
            env_key: None,
            auth_provider: None,
            api_base_url: None,
        }
    }

    fn manager_with_entries() -> (
        crate::agent::remote_config::ModelsManager,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = std::sync::Arc::new(
            xai_grok_login::AuthManager::new(tmp.path(), xai_grok_login::GrokComConfig::default()),
        );
        let manager = crate::agent::remote_config::ModelsManager::new(
            None,
            IndexMap::new(),
            acp::ModelId::new("default"),
            auth,
            Config::default(),
        );
        (manager, tmp)
    }

    /// S7 (SDD §4, M1 fix — the /effort seed): the /effort write path
    /// (`handle_set_reasoning_effort`) seeds `ultra_wire_effort` against the
    /// POST-routing model alongside the raw effort write; the
    /// unsupported-model Err early return leaves both fields untouched.
    /// Harness: the session-actor test support (`create_test_actor_ex`,
    /// `#[path] mod support` in acp_session.rs); the actor's
    /// `models_manager` is swapped for a manager carrying the menu under
    /// test.
    #[tokio::test(flavor = "current_thread")]
    async fn effort_path_seeds_the_field() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // (i) supported model advertising ultra: /effort ultra writes
                // reasoning_effort = Some(Ultra) AND
                // ultra_wire_effort = Some(Xhigh)
                {
                    let (gateway_tx, _gateway_rx) =
                        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
                    let (persistence_tx, _persistence_rx) =
                        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
                    let (mut actor, _event_rx) =
                        super::super::support::create_test_actor_ex(
                            0,
                            256_000,
                            85,
                            gateway_tx,
                            persistence_tx,
                        )
                        .await;
                    let (manager, _tmp) = manager_with_entries();
                    manager.insert_test_entry(
                        "qwen-ultra",
                        entry_with_menu(
                            "qwen-ultra",
                            true,
                            &[
                                (ReasoningEffort::Ultra, false),
                                (ReasoningEffort::Xhigh, true),
                                (ReasoningEffort::Medium, false),
                                (ReasoningEffort::Low, false),
                            ],
                        ),
                    );
                    actor.models_manager = manager;
                    let mut cfg = actor
                        .chat_state_handle
                        .get_sampling_config()
                        .await
                        .expect("the test actor carries a sampling config");
                    cfg.model = "qwen-ultra".to_string();
                    actor.chat_state_handle.update_sampling_config(cfg);
                    let actor = std::sync::Arc::new(actor);
                    let model_id = actor
                        .handle_set_reasoning_effort(ReasoningEffort::Ultra)
                        .await
                        .expect("an ultra-advertising menu accepts /effort ultra");
                    assert_eq!(model_id.0.as_ref(), "qwen-ultra");
                    let cfg = actor
                        .chat_state_handle
                        .get_sampling_config()
                        .await
                        .expect("the config persists after the /effort write");
                    assert_eq!(
                        cfg.reasoning_effort,
                        Some(ReasoningEffort::Ultra),
                        "the raw /effort write lands"
                    );
                    assert_eq!(
                        cfg.ultra_wire_effort,
                        Some(ReasoningEffort::Xhigh),
                        "the /effort writer must seed ultra_wire_effort against the POST-routing model"
                    );
                }
                // (ii) unsupported model: the Err early return leaves BOTH
                // fields untouched
                {
                    let (gateway_tx, _gateway_rx) =
                        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
                    let (persistence_tx, _persistence_rx) =
                        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
                    let (mut actor, _event_rx) =
                        super::super::support::create_test_actor_ex(
                            0,
                            256_000,
                            85,
                            gateway_tx,
                            persistence_tx,
                        )
                        .await;
                    let (manager, _tmp) = manager_with_entries();
                    manager.insert_test_entry(
                        "no-effort-model",
                        entry_with_menu(
                            "no-effort-model",
                            false,
                            &[(ReasoningEffort::Low, true)],
                        ),
                    );
                    actor.models_manager = manager;
                    let mut cfg = actor
                        .chat_state_handle
                        .get_sampling_config()
                        .await
                        .expect("the test actor carries a sampling config");
                    cfg.model = "no-effort-model".to_string();
                    cfg.reasoning_effort = Some(ReasoningEffort::Medium);
                    cfg.ultra_wire_effort = Some(ReasoningEffort::High);
                    actor.chat_state_handle.update_sampling_config(cfg);
                    let actor = std::sync::Arc::new(actor);
                    let result = actor.handle_set_reasoning_effort(ReasoningEffort::Ultra).await;
                    assert!(
                        result.is_err(),
                        "the unsupported-model support check must reject the /effort write"
                    );
                    let cfg = actor
                        .chat_state_handle
                        .get_sampling_config()
                        .await
                        .expect("the config persists after the rejected /effort write");
                    assert_eq!(
                        cfg.reasoning_effort,
                        Some(ReasoningEffort::Medium),
                        "the rejected /effort write leaves reasoning_effort untouched"
                    );
                    assert_eq!(
                        cfg.ultra_wire_effort,
                        Some(ReasoningEffort::High),
                        "the rejected /effort write leaves ultra_wire_effort untouched"
                    );
                }
            })
            .await;
    }

    /// XW-XREPLAY-1 (apex-ayl.123): the family-switch compact gate requires
    /// the compact only when the TARGET wire cannot portably carry foreign
    /// reasoning — skipped on /v1/responses (the .71 switch-time projection
    /// plus the send-time strict/lenient projectors own that seam), kept on
    /// /v1/messages (signed-thinking invariant, compaction.rs:1412-1415) and
    /// /v1/chat/completions (fail-closed: no portability evidence on that
    /// wire for foreign reasoning).
    #[test]
    fn family_switch_compact_required_by_backend() {
        use xai_grok_sampling_types::ApiBackend;
        assert!(
            !family_switch_compact_required(ApiBackend::Responses),
            "Responses targets are skipped — the .71 projection owns the seam"
        );
        assert!(
            family_switch_compact_required(ApiBackend::Messages),
            "Messages targets keep the compact (signed-thinking invariant)"
        );
        assert!(
            family_switch_compact_required(ApiBackend::ChatCompletions),
            "ChatCompletions targets keep the compact (fail-closed)"
        );
    }
}
