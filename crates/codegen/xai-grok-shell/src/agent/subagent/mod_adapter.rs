// crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use xai_grok_agent::config::{AgentDefinition, ModelOverride};
use xai_grok_mods::{
    BorrowedSendFuture, BridgedTool, ModChildControl, ModConfig, ModHost, ModRunInput,
    ModStarted, ValidatedMod, VendorToolActivity, select_resume_key,
};
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ChildReporter, StartedChild, SubagentProgress,
};
use xai_tool_types::SubagentCapabilityMode;

use super::start_artifact_publication::{
    PreparedStartArtifacts, PublicationBoundary, StartArtifactPublication,
};
use super::*;
use agent_client_protocol as acp;

/// Every binding in scope at `handle_request.rs:919` that the adapter needs.
pub(super) struct Dispatch {
    pub validated: Result<ValidatedMod, String>,
    pub request: SubagentRequest,
    pub ctx: SubagentSpawnContext,
    pub completion_data: ShellCompletionData,
    pub gateway: GatewaySender,
    pub reporter: ChildReporter<ShellChildRuntime>,
    pub cancel_token: tokio_util::sync::CancellationToken,
    pub attempt_id: Option<String>,
    pub agent_address: Option<xai_message_delivery_core::AgentAddress>,
    pub is_wake: bool,
    pub queued_for: Option<std::time::Duration>,
    pub session_running: usize,
    pub definition: AgentDefinition,
    pub effective_runtime: xai_grok_subagent_resolution::EffectiveRuntimeConfig,
    pub resume_source: Option<ResumeSourceData>,
    pub worktree_path: Option<PathBuf>,
    pub worktree_freshly_created: bool,
    pub child_depth: u32,
    pub spawn_prepare_span: xai_grok_telemetry::region::Region,
    pub spawn_timer: xai_grok_telemetry::subagent_spawn::SharedSubagentSpawnTimer,
    pub start: std::time::Instant,
}

/// Spec §4.6.
pub(super) fn detect(
    def: &AgentDefinition,
    ctx: &SubagentSpawnContext,
) -> Option<Result<ValidatedMod, String>> {
    let name = def.plugin_name.as_deref()?;
    let registry = ctx.plugin_registry.as_ref()?;
    let plugin = registry.get(name)?;
    if !plugin.canonical_root.join("mod.json").is_file() {
        return None;
    }
    if !plugin.trusted {
        return Some(Err(format!("mod plugin '{name}' is not trusted")));
    }
    Some(ModConfig::load(name, &plugin.canonical_root).map_err(|e| e.to_string()))
}

struct ShellModHost {
    reporter: ChildReporter<ShellChildRuntime>,
    gateway: GatewaySender,
    parent_cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<SessionCommand>>,
    parent_session_id: String,
    subagent_id: String,
    attempt_id: Option<String>,
    started_at: std::time::Instant,
    is_wake: bool,
    child_template: Mutex<Option<StartedChildTemplate>>,
    start_artifacts: Arc<Mutex<StartArtifactPublication>>,
    completion_data: ShellCompletionData,
    mcp_pool: Option<crate::session::mcp_servers::SharedMcpPool>,
}

struct StartedChildTemplate {
    persona: Option<String>,
    resumed_from: Option<String>,
    child_cwd: String,
    worktree_path: Option<String>,
    effective_model_id: String,
    definition_background: bool,
}

impl ModHost for ShellModHost {
    fn on_started<'a>(
        &'a self,
        _started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool> {
        Box::pin(async move {
            let Some(t) = self.child_template.lock().take() else {
                return false;
            };
            let child = StartedChild {
                child_session_id: self.subagent_id.clone(),
                persona: t.persona,
                resumed_from: t.resumed_from,
                child_cwd: t.child_cwd,
                worktree_path: t.worktree_path,
                effective_model_id: t.effective_model_id,
                definition_background: t.definition_background,
                control: ShellChildRuntime::Mod(control),
            };
            if !self.is_wake {
                return self.reporter.started(child).await;
            }
            if !self.reporter.started_deferred(child).await {
                return false;
            }
            if !self.reporter.settle_deferred_start(true).await {
                let _ = self.reporter.settle_deferred_start(false).await;
                self.start_artifacts.lock().forbid_terminal_persistence();
                return false;
            }
            if self.start_artifacts.lock().publish_at(PublicationBoundary::Started) {
                self.completion_data.mark_spawned_notification_emitted();
            }
            true
        })
    }

    fn on_progress(&self, p: &SubagentProgress, _activity: Option<&VendorToolActivity>) {
        let update = SessionUpdate::SubagentProgress {
            subagent_id: self.subagent_id.clone(),
            attempt_id: self.attempt_id.clone(),
            parent_session_id: self.parent_session_id.clone(),
            child_session_id: self.subagent_id.clone(),
            duration_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            turn_count: p.turn_count,
            tool_call_count: p.tool_call_count,
            tokens_used: p.tokens_used,
            context_window_tokens: p.context_window_tokens,
            context_usage_pct: p.context_usage_pct,
            tools_used: p.tools_used.clone(),
            error_count: p.error_count,
        };
        super::spawn::emit_subagent_notification(
            &self.gateway,
            &self.parent_session_id,
            update,
            self.parent_cmd_tx.as_ref(),
        );
    }

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool> {
        Box::pin(async move { self.reporter.finalizing().await })
    }

    fn call_mcp_tool<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        args: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>> {
        Box::pin(async move {
            let client = self
                .mcp_pool
                .as_ref()
                .and_then(|p| p.get_client(server))
                .cloned()
                .ok_or_else(|| format!("MCP server '{server}' not available"))?;
            let out = client.call_tool(tool, args).await.map_err(|e| e.to_string())?;
            serde_json::to_value(out).map_err(|e| e.to_string())
        })
    }
}

/// Spec §6.7: `<server>__<tool>` from the parent snapshot, restricted to inherited, non-denied servers.
fn bridged_tools(
    pool: Option<&crate::session::mcp_servers::SharedMcpPool>,
    defs: Option<&[xai_grok_sampling_types::ToolSpec]>,
    deny: &[String],
) -> Vec<BridgedTool> {
    let (Some(pool), Some(defs)) = (pool, defs) else {
        return Vec::new();
    };
    let servers: std::collections::HashSet<&str> = pool
        .server_names()
        .filter(|name| !deny.iter().any(|d| d == name))
        .collect();
    defs.iter()
        .filter_map(|spec| {
            let (server, tool) = spec.name.split_once("__")?;
            servers.contains(server).then(|| BridgedTool {
                server: server.to_owned(),
                tool: tool.to_owned(),
                name: spec.name.clone(),
                description: spec.description.clone(),
                input_schema: spec.parameters.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
fn meta_dir_for(ctx: &SubagentSpawnContext, parent_session_dir: &std::path::Path, id: &str) -> PathBuf {
    ctx.run_shell_child_harness
        .as_ref()
        .map(|h| h.meta_dir.clone())
        .unwrap_or_else(|| parent_session_dir.join("subagents").join(id))
}
#[cfg(not(test))]
fn meta_dir_for(_ctx: &SubagentSpawnContext, parent_session_dir: &std::path::Path, id: &str) -> PathBuf {
    parent_session_dir.join("subagents").join(id)
}

async fn remove_unpromoted_worktree(d: &Dispatch) {
    if d.worktree_freshly_created
        && let Some(wt) = d.worktree_path.as_deref()
        && let Err(e) = crate::session::worktree::remove_subagent_worktree(wt).await
    {
        tracing::warn!(subagent_id = %d.request.id, error = %e, "failed to remove unpromoted mod worktree");
    }
}

pub(super) async fn run(mut d: Dispatch) -> ChildRunOutput<ShellCompletionData> {
    // Same gauge the native path holds for the child's lifetime (handle_request.rs:1657).
    let _active = super::handle_request::SUBAGENTS_ACTIVE.enter();
    let validated = match std::mem::replace(&mut d.validated, Err(String::new())) {
        Ok(v) => v,
        Err(msg) => {
            remove_unpromoted_worktree(&d).await;
            return child_run_output(failure_result(&d.request, &msg), d.completion_data, None);
        }
    };
    let ns = validated.config.namespace.clone();
    let mut warnings = Vec::new();

    // §5.4 step 3
    let vendor_model = match &d.definition.model {
        ModelOverride::Override(m) => m.clone(),
        ModelOverride::Inherit => {
            remove_unpromoted_worktree(&d).await;
            let msg = format!("mod agent '{}' must declare `model`", d.request.subagent_type);
            return child_run_output(failure_result(&d.request, &msg), d.completion_data, None);
        }
    };
    let effective_model_id = format!("{ns}/{vendor_model}");

    // §5.5
    if d.request.fork_context {
        warnings.push("fork_context is not supported by mod subagents; started fresh".to_owned());
    }
    if !d.definition.mcp_servers.is_empty() {
        warnings.push("agent-owned mcpServers are ignored by mod subagents; only inherited servers are bridged".to_owned());
    }

    // §5.4 step 2
    let mut prompt = d.request.prompt.clone();
    if !d.is_wake
        && let Some(msg) = d.request.runtime_overrides.native_agent.as_ref().and_then(|n| n.message.as_ref())
    {
        if msg.native.as_ref().is_some_and(|n| n.encrypted) {
            remove_unpromoted_worktree(&d).await;
            return child_run_output(
                failure_result(&d.request, "encrypted native messages are not supported by mod subagents"),
                d.completion_data,
                None,
            );
        }
        prompt = format!("{prompt}\n\n{}", msg.body);
    }

    // §5.4 step 1
    let child_cwd = resolve_child_cwd(
        d.worktree_path.as_deref(),
        select_override_cwd(d.resume_source.as_ref(), d.request.cwd.as_deref()),
        &d.ctx.parent_cwd,
    );
    let child_cwd_string = child_cwd.to_string_lossy().into_owned();
    let parent_session_dir = session::persistence::session_dir(&SessionInfo {
        id: acp::SessionId::new(d.ctx.parent_session_id.clone()),
        cwd: d.ctx.parent_cwd.to_string_lossy().to_string(),
    });
    let meta_dir = meta_dir_for(&d.ctx, &parent_session_dir, &d.request.id);
    let resume_key = select_resume_key(&d.request.id, d.request.resume_from.as_deref(), d.is_wake);
    let source_str = if resume_key.is_some() { "resumed" } else { "new" };

    // §5.4 step 4 — field lists mirror handle_request.rs:1164-1232
    let meta = SubagentMeta {
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        child_session_id: d.request.id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        description: d.request.description.clone(),
        prompt: d.request.prompt.clone(),
        status: "running".to_owned(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        error: None,
        effective_context_source: Some(source_str.to_owned()),
        context_normalized: false,
        fork_copy_error: None,
        persona: d.effective_runtime.persona.clone(),
        resumed_from: d.request.resume_from.clone(),
        child_cwd: Some(child_cwd_string.clone()),
        worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        snapshot_ref: None,
        effective_model_id: Some(effective_model_id.clone()),
    };
    let advertised_address = xai_message_delivery_core::advertised_address(
        d.agent_address.as_ref().map(|a| a.as_str()),
        d.request.owner.is_workflow(),
    )
    .map(str::to_owned);
    let spawned = SessionUpdate::SubagentSpawned {
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        child_session_id: d.request.id.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        parent_prompt_id: d.request.parent_prompt_id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        description: d.request.description.clone(),
        effective_context_source: Some(source_str.to_owned()),
        context_normalized: false,
        capability_mode: d.effective_runtime.capability_mode
            .and_then(|m| serde_json::to_value(m).ok())
            .and_then(|v| v.as_str().map(String::from)),
        persona: d.effective_runtime.persona.clone(),
        role: d.effective_runtime.role_name.clone(),
        model: Some(effective_model_id.clone()),
        resumed_from: d.request.resume_from.clone(),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        agent_address: advertised_address.clone(),
    };
    let boundary = if d.is_wake { PublicationBoundary::Started } else { PublicationBoundary::Prepared };
    let mut start_artifacts = StartArtifactPublication::new(
        boundary,
        (!d.is_wake).then(|| d.spawn_prepare_span.span().clone()),
        PreparedStartArtifacts {
            meta_dir: meta_dir.clone(),
            meta,
            spawned,
            advertised_address,
            reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
            role_name: d.effective_runtime.role_name.clone(),
            parent_prompt_id: d.request.parent_prompt_id.clone(),
        },
        d.gateway.clone(),
        d.reporter.clone(),
        &d.ctx,
    );
    start_artifacts.publish_at(PublicationBoundary::Prepared);
    if !d.is_wake {
        d.completion_data.mark_spawned_notification_emitted();
    }
    let start_artifacts = Arc::new(Mutex::new(start_artifacts));

    // §5.4 step 5
    let read_only = d.effective_runtime.capability_mode == Some(SubagentCapabilityMode::ReadOnly);
    let caps = &validated.config.capabilities;
    let pool = if caps.mcp_bridge && (!read_only || caps.mcp_in_read_only) {
        resolve_inherited_mcp_pool(d.ctx.parent_mcp_pool.take(), &d.definition.mcp_inheritance)
    } else {
        None
    };
    let tools = bridged_tools(
        pool.as_ref(),
        d.ctx.parent_tool_definitions.as_deref(),
        &validated.config.mcp_bridge.deny_servers,
    );
    let bridged_server_count = tools.iter().map(|t| t.server.as_str()).collect::<std::collections::HashSet<_>>().len();

    // §5.4 step 10 (launch half)
    xai_grok_telemetry::session_ctx::log_event(xai_grok_telemetry::events::SubagentLaunched {
        subagent_id: d.request.id.clone(),
        parent_session_id: d.request.parent_session_id.clone(),
        subagent_type: d.request.subagent_type.clone(),
        owner: telemetry_owner_kind(&d.request),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        queued_ms: d.queued_for.map(|q| u64::try_from(q.as_millis()).unwrap_or(u64::MAX)),
        session_running: u32::try_from(d.session_running).unwrap_or(u32::MAX),
        persona: d.request.runtime_overrides.persona.clone(),
        fork_context: false,
        resume_from: d.request.resume_from.clone(),
        isolated_worktree: d.worktree_path.is_some(),
        mcp_inherited_count: u32::try_from(bridged_server_count).unwrap_or(u32::MAX),
        mcp_owned_count: 0,
        skills_inherited_count: 0,
    });
    d.spawn_timer.record(
        xai_grok_telemetry::subagent_spawn::SubagentSpawnPhase::SpawnPrepare,
        d.start.elapsed(),
    );

    // §5.4 steps 6-8
    let host = Arc::new(ShellModHost {
        reporter: d.reporter.clone(),
        gateway: d.gateway.clone(),
        parent_cmd_tx: d.ctx.parent_cmd_tx.clone(),
        parent_session_id: d.ctx.parent_session_id.clone(),
        subagent_id: d.request.id.clone(),
        attempt_id: d.attempt_id.clone(),
        started_at: d.start,
        is_wake: d.is_wake,
        child_template: Mutex::new(Some(StartedChildTemplate {
            persona: d.effective_runtime.persona.clone(),
            resumed_from: d.request.resume_from.clone(),
            child_cwd: child_cwd_string.clone(),
            worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            effective_model_id: effective_model_id.clone(),
            definition_background: d.definition.background.unwrap_or(false),
        })),
        start_artifacts: Arc::clone(&start_artifacts),
        completion_data: d.completion_data.clone(),
        mcp_pool: pool,
    });
    let outcome = xai_grok_mods::run_mod_child(
        ModRunInput {
            validated,
            subagent_id: d.request.id.clone(),
            parent_session_id: d.ctx.parent_session_id.clone(),
            subagent_type: d.request.subagent_type.clone(),
            agent_name: d.definition.name.clone(),
            vendor_model,
            cwd: child_cwd,
            prompt,
            system_prompt: d.definition.prompt_body.clone(),
            resume_key,
            read_only,
            reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
            bridged_tools: tools,
            cancellation: d.cancel_token.clone(),
        },
        host,
    )
    .await;

    if !outcome.promoted {
        remove_unpromoted_worktree(&d).await;
    }
    warnings.extend(outcome.warnings.iter().cloned());
    if outcome.admission_uncertain {
        warnings.push("active-message admission did not settle before finalize; a late message may be dropped".to_owned());
    }

    let mut result = SubagentResult {
        success: outcome.error.is_none() && !outcome.cancelled,
        output: Arc::from(outcome.final_text.as_str()),
        error: outcome.error.clone(),
        cancelled: outcome.cancelled,
        subagent_id: d.request.id.clone(),
        child_session_id: d.request.id.clone(),
        tool_calls: outcome.tool_calls,
        turns: outcome.turns,
        duration_ms: u64::try_from(d.start.elapsed().as_millis()).unwrap_or(u64::MAX),
        tokens_used: outcome.tokens_used,
        output_tokens_used: 0,
        total_tokens_used: outcome.tokens_used,
        output_usage_incomplete: true,
        worktree_path: d.worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        backgrounded: false,
        warnings,
    };

    // §5.4 step 9
    let gcs_upload_ctx = GcsUploadContext {
        bucket_url: d.ctx.gcs_bucket_url.clone(),
        upload_method: d.ctx.gcs_upload_method.clone(),
        model_id: Some(effective_model_id.clone()),
        cwd: Some(child_cwd_string),
        reasoning_effort: d.effective_runtime.reasoning_effort.clone(),
        role_name: d.effective_runtime.role_name.clone(),
        parent_prompt_id: d.request.parent_prompt_id.clone(),
        auth_manager: d.ctx.auth_manager.clone(),
        isolation_mode: Some(format!("{:?}", d.effective_runtime.isolation)),
        capability_mode: d.effective_runtime.capability_mode.as_ref().map(|m| format!("{m:?}")),
        depth: d.child_depth,
    };
    let terminal_persistence_allowed = start_artifacts.lock().terminal_persistence_allowed();
    if terminal_persistence_allowed {
        d.completion_data.set_persisted_output_dir(persist_subagent_output(&meta_dir, &result));
        persist_subagent_completion(&meta_dir, &result, &gcs_upload_ctx);
    }
    d.completion_data.set_telemetry_tokens(result.tokens_used);
    let mut disposed = None;
    if outcome.promoted
        && terminal_persistence_allowed
        && d.ctx.resolve_subagent_worktree_snapshot_enabled()
        && let Some(wt) = d.worktree_path.as_deref()
    {
        let final_status = result.status().to_string();
        let disposal = super::handle_request::dispose_worktree_after_completion(
            wt,
            &resolve_subagent_source_repo(&d.ctx),
            &meta_dir,
            &final_status,
            &d.request.id,
        )
        .await;
        if disposal.worktree_removed() {
            result.worktree_path = None;
        }
        disposed = disposal.snapshot_ref().map(str::to_owned);
    }

    // §5.4 step 10 (completion half)
    let telemetry_outcome = if result.success {
        xai_grok_telemetry::events::Outcome::Completed
    } else if result.cancelled {
        xai_grok_telemetry::events::Outcome::Cancelled
    } else {
        xai_grok_telemetry::events::Outcome::Error
    };
    let mut completed = xai_grok_telemetry::events::SubagentCompleted {
        subagent_id: d.request.id.clone(),
        parent_session_id: d.request.parent_session_id.clone(),
        owner: telemetry_owner_kind(&d.request),
        workflow_run_id: d.request.owner.workflow_run_id().map(str::to_string),
        outcome: telemetry_outcome,
        duration_ms: result.duration_ms,
        tool_calls: result.tool_calls,
        tokens_used: (result.tokens_used > 0).then_some(result.tokens_used),
        queue_wait_ms: None,
        spawn_prepare_ms: None,
        session_bootstrap_ms: None,
        agent_build_ms: None,
        tool_setup_ms: None,
        ready_to_first_turn_ms: None,
    };
    d.spawn_timer.write_event_phases(&mut completed);
    xai_grok_telemetry::session_ctx::log_event(completed);

    child_run_output(result, d.completion_data, disposed)
}
