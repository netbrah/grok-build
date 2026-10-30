//! ChatStateActor — runs in a dedicated tokio task and owns all chat state.
//!
//! This module is organized into submodules by responsibility:
//! - `state`: Internal state types (ChatState)
//! - `mutations`: State mutation handlers (push_user_message, replace_conversation, etc.)
//! - `queries`: Read-only query handlers (get_conversation, snapshot, etc.)

mod mutations;
mod queries;
pub(crate) mod request_builder;
pub mod state;

#[cfg(test)]
mod tests;

use tokio::sync::mpsc;
use tracing::debug;

use crate::commands::{ChatStateCommand, StrictAppendAck};
use crate::events::ChatStateEvent;
use crate::handle::ChatStateHandle;
use crate::persistence::ChatPersistence;
use crate::types::{PruningConfig, TurnCapture};

use request_builder::NativeBuildCapture;
use state::ChatState;
use xai_grok_sampling_types::{ConversationItem, SamplingConfig, TraceContext};

/// The actor that owns all chat state.
/// Runs in a dedicated tokio task and processes commands sequentially.
/// Mints one distinct nonce per actor instance, so a seal from a dead actor
/// instance can never be replayed against a live one.
static ACTOR_NONCE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct ChatStateActor {
    /// Internal state — conversation, tokens, config, etc.
    state: ChatState,
    /// Pruning configuration for tool-result trimming.
    pruning_config: PruningConfig,
    /// Persistence implementation — owned exclusively, called with `&mut self`.
    persistence: Box<dyn ChatPersistence>,
    /// Channel to receive commands from handles.
    cmd_rx: mpsc::UnboundedReceiver<ChatStateCommand>,
    /// Channel to send events to the session main loop.
    event_tx: mpsc::UnboundedSender<ChatStateEvent>,
    /// Cancellation token for graceful shutdown.
    cancellation_token: tokio_util::sync::CancellationToken,
    /// Monotonic per-build sequence for bound builds; issuance evidence only.
    native_build_sequence: u64,
    /// Per-actor-instance nonce, minted once at spawn.
    actor_nonce: u64,
}

impl ChatStateActor {
    /// One bound build: check the captured entry, then select and build exactly
    /// once. `None` means the request was refused before any side effect.
    ///
    /// The Inactive route returns no preparation, matching the legacy build byte
    /// for byte; only Selected carries the tentative preparation, whose counters
    /// are actor-local issuance evidence rather than durable readiness (waj2-U9).
    fn bound_build(
        &mut self,
        tool_candidates: crate::NativeToolCandidates,
        memory_reminder: Option<String>,
        persist_memory_reminder: bool,
        trace: Option<Box<dyn TraceContext>>,
        conv_id: String,
        req_id: String,
    ) -> Option<(
        xai_grok_sampling_types::ConversationRequest,
        crate::RequestTransportBinding,
    )> {
        use xai_grok_sampling_types::conversation::native_search_history::{
            NativeHistoryRequirement, NativePreparationGrant,
        };
        if tool_candidates.history_entry().is_err() {
            return None;
        }
        let captured_entry = tool_candidates.history_entry().ok()?;
        if let crate::NativeHistoryEntryIntent::OwningPrimary(token)
        | crate::NativeHistoryEntryIntent::SwitchStore(token) = captured_entry.intent()
        {
            if !self.native_owner_is_current(token) {
                return None;
            }
            let target = self.state.native.owner_target.as_ref()?;
            if target.model != self.state.sampling_config.model
                || target.backend != self.state.sampling_config.api_backend
            {
                return None;
            }
        }
        self.ensure_conversation_integrity();
        let before_builder = if captured_entry.requirement() == NativeHistoryRequirement::Selected {
            Some(crate::persistence::native::history_boundary(&self.state.conversation).ok()?)
        } else {
            None
        };
        let binding = crate::RequestTransportBinding {
            config: self.state.sampling_config.clone(),
            credentials: self.state.credentials.clone(),
        };
        let (specs, native_eligible_inputs, entry) = tool_candidates
            .select_for_bound_build(&binding.config)
            .ok()?;
        self.native_build_sequence = self.native_build_sequence.checked_add(1)?;
        let build_sequence = self.native_build_sequence;
        // The Selected preparation is captured from the one build's own output:
        // `D_pre` must include this build's durable repairs and memory-reminder
        // effects, so nothing about the retained history is frozen before
        // `build_conversation_request` has run.
        let built = self.build_conversation_request(
            specs,
            memory_reminder,
            persist_memory_reminder,
            trace,
            conv_id,
            req_id.clone(),
            match entry.requirement() {
                NativeHistoryRequirement::Inactive => NativeBuildCapture::Legacy,
                NativeHistoryRequirement::Selected => NativeBuildCapture::Selected,
            },
        );
        let mut request = built.request;
        if built
            .durable
            .as_ref()
            .is_some_and(|history| Some(history) != before_builder.as_ref())
        {
            self.state.native.history_revision =
                self.state.native.history_revision.checked_add(1)?;
        }
        let preparation = match entry.requirement() {
            // Inactive parity: the returned preparation stays `None`, so the
            // bound request Debug-matches the legacy one exactly. The caller
            // retains its own captured entry and never reads this absence as a
            // mode decision.
            NativeHistoryRequirement::Inactive => None,
            NativeHistoryRequirement::Selected => {
                let grant = match entry.intent() {
                    crate::types::NativeHistoryEntryIntent::SendOnly => {
                        NativePreparationGrant::SendOnly
                    }
                    crate::types::NativeHistoryEntryIntent::OwningPrimary(_) => {
                        NativePreparationGrant::OwningPrimary
                    }
                    crate::types::NativeHistoryEntryIntent::SwitchStore(_) => {
                        NativePreparationGrant::SwitchStore
                    }
                };
                // A Selected preparation without its frozen durable boundary or
                // a verified durable->send mapping is refused here rather than
                // carried, because either one missing makes the later plan a
                // guess about history the builder never related.
                let history = built.durable?;
                let mapping = built.mapping?;
                let mut preparation = xai_grok_sampling_types::conversation::native_search_history::NativeBoundPreparation::issued(
                    entry.requirement(), grant, req_id.clone(), history.clone(), mapping,
                    Some(self.state.native.history_revision), Some(self.state.native.install_generation),
                    build_sequence, self.actor_nonce,
                );
                if grant != NativePreparationGrant::SendOnly {
                    use xai_grok_sampling_types::conversation::native_search_history::*;
                    let token = self.state.native.owner.as_ref()?;
                    let checkpoint = match self.state.native.checkpoint.clone() {
                        Some(checkpoint) => checkpoint,
                        None => std::sync::Arc::new(
                            crate::persistence::native::NativeCheckpoint::empty_projection_input(
                                token.lineage().into(),
                                &self.state.conversation,
                            )
                            .ok()?,
                        ),
                    };
                    let target = self.state.native.owner_target.clone()?;
                    self.state.native.binding_generation =
                        self.state.native.binding_generation.checked_add(1)?;
                    let capture = NativeHistoryCapture {
                        history,
                        history_revision: self.state.native.history_revision,
                        lineage: checkpoint.record.lineage.clone(),
                        native_generation: checkpoint.record.generation,
                        high_water: checkpoint.record.high_water,
                        projection_revision: checkpoint.active.projection_revision,
                        strict_append_sequence: 0,
                        binding_generation: self.state.native.binding_generation,
                        install_generation: self.state.native.install_generation,
                        issued_request: req_id.clone(),
                        target,
                        execution: if binding.config.api_backend
                            == xai_grok_sampling_types::ApiBackend::Messages
                        {
                            NativeHistoryExecution::Server
                        } else {
                            NativeHistoryExecution::Client
                        },
                        original_admission: request.search_admission?,
                        versions: NativeHistoryVersions {
                            schema: checkpoint.record.schema_version,
                            policy: 1,
                            projection: 1,
                            reader: checkpoint.record.reader_floor,
                            limits: checkpoint.record.limits_revision,
                        },
                    };
                    let authority = match grant {
                        NativePreparationGrant::OwningPrimary => {
                            NativeHistoryWriteAuthority::OwningPrimary {
                                lineage: capture.lineage.id.clone(),
                                binding_generation: capture.binding_generation,
                                request: req_id.clone(),
                            }
                        }
                        NativePreparationGrant::SwitchStore => {
                            NativeHistoryWriteAuthority::SwitchStore {
                                lineage: capture.lineage.id.clone(),
                                binding_generation: capture.binding_generation,
                            }
                        }
                        NativePreparationGrant::SendOnly => unreachable!(),
                    };
                    let seal = self
                        .state
                        .native
                        .issuer
                        .issue_bound(token.seal(), capture.clone())
                        .ok()?;
                    let context = NativeHistoryRequestContext::captured(
                        entry.requirement(),
                        capture,
                        authority,
                    );
                    preparation = preparation
                        .with_issued_context(
                            context,
                            seal,
                            serde_json::to_vec(&self.state.conversation).ok()?.into(),
                        )
                        .ok()?;
                    self.state.native.issued = Some(preparation.clone());
                }
                Some(preparation)
            }
        };
        request.native_eligible_inputs = native_eligible_inputs;
        request.native_bound_preparation = preparation;
        Some((request, binding))
    }

    /// Send an event to subscribers, logging if the channel is closed.
    fn send_event(&self, event: ChatStateEvent) {
        if self.event_tx.send(event).is_err() {
            debug!("ChatState event channel closed, event dropped");
        }
    }

    /// Spawn the actor and return a handle to communicate with it.
    pub fn spawn(
        initial_conversation: Vec<ConversationItem>,
        sampling_config: SamplingConfig,
        persistence: Box<dyn ChatPersistence>,
        event_tx: mpsc::UnboundedSender<ChatStateEvent>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> ChatStateHandle {
        Self::spawn_with_pruning(
            initial_conversation,
            sampling_config,
            PruningConfig::default(),
            persistence,
            event_tx,
            cancellation_token,
        )
    }

    /// Spawn the actor with a custom pruning config.
    pub fn spawn_with_pruning(
        initial_conversation: Vec<ConversationItem>,
        sampling_config: SamplingConfig,
        pruning_config: PruningConfig,
        persistence: Box<dyn ChatPersistence>,
        event_tx: mpsc::UnboundedSender<ChatStateEvent>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> ChatStateHandle {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let actor_nonce = ACTOR_NONCE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let actor = ChatStateActor {
            state: ChatState::new(initial_conversation, sampling_config),
            pruning_config,
            persistence,
            cmd_rx,
            event_tx,
            cancellation_token,
            native_build_sequence: 0,
            actor_nonce,
        };

        let registration_key = actor.state.native.issuer.registration_key();
        tokio::spawn(actor.run());

        ChatStateHandle::new(cmd_tx).with_native_registration_key(registration_key)
    }

    /// Main actor loop — processes commands until shutdown or cancellation.
    async fn run(mut self) {
        self.bootstrap_native_history().await;
        loop {
            tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    debug!("ChatStateActor shutting down via cancellation");
                    break;
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        debug!("ChatStateActor shutting down: all handles dropped");
                        break;
                    };
                    self.handle_command(cmd).await;
                }
            }
        }
    }

    /// Dispatch a command to the appropriate mutation or query handler.
    async fn handle_command(&mut self, cmd: ChatStateCommand) {
        match cmd {
            ChatStateCommand::ProjectNativeSwitchHistory {
                candidates,
                target_model,
                target_pin,
                route,
                reply,
            } => {
                let result = self
                    .project_native_switch_history(candidates, target_model, target_pin, route)
                    .await;
                let _ = reply.send(result);
            }
            ChatStateCommand::ReadNativeLoadedAuthority { reply } => {
                let _ = reply.send(self.state.native.loaded.clone());
            }
            ChatStateCommand::ReconcileNativeHistoryProjection { reply } => {
                let result = self.reconcile_native_projection().await;
                let _ = reply.send(result);
            }
            ChatStateCommand::RegisterNativeHistoryOwner {
                key,
                lineage,
                request,
                target,
                owner_class,
                run_active,
                reply,
            } => {
                let result = self
                    .register_native_history_owner(
                        key,
                        lineage,
                        request,
                        target,
                        owner_class,
                        run_active,
                    )
                    .await;
                let _ = reply.send(result);
            }
            ChatStateCommand::CommitNativeHistoryProjection {
                preparation,
                request,
                route,
                reply,
            } => {
                let result = self
                    .commit_native_history_projection(preparation, *request, route)
                    .await;
                let _ = reply.send(result);
            }

            // ═══ Mutations ═══
            ChatStateCommand::PushUserMessage { item } => {
                self.push_user_message(item);
            }
            ChatStateCommand::PushUserMessagesBatch { items } => {
                for item in items {
                    self.push_user_message(item);
                }
            }
            ChatStateCommand::PushUserMessagesBatchAndAck { items, reply } => {
                for item in items {
                    self.push_user_message(item);
                }
                let _ = reply.send(());
            }
            ChatStateCommand::PushUserMessageAndAck { item, reply } => {
                self.push_user_message(item);
                let _ = reply.send(());
            }
            ChatStateCommand::AppendWorkingDirectorySwitchAndAck {
                content,
                cwd_generation,
                reply,
            } => {
                let generation = cwd_generation.get();
                let candidate = ConversationItem::working_directory_switch(content, generation);
                // Pop a crash-stranded reminder BEFORE the strict append: the
                // pop rewrites history from the in-memory image, which must
                // not yet contain the acked append, or the rewrite erases it.
                self.pop_stranded_continue_reminder();
                let persist_rx = self
                    .persistence
                    .persist_working_directory_switch_and_ack(&candidate);
                let result = persist_rx.await.unwrap_or_else(|_| {
                    Err(crate::commands::StrictAppendError::Indeterminate(
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "working-directory switch acknowledgement dropped; retry by generation",
                        ),
                    ))
                });
                let authoritative = match &result {
                    Ok(StrictAppendAck::Appended)
                    | Err(crate::commands::StrictAppendError::Committed {
                        acknowledgement: StrictAppendAck::Appended,
                        ..
                    }) => Some(&candidate),
                    Ok(StrictAppendAck::AlreadyPresent(authoritative))
                    | Err(crate::commands::StrictAppendError::Committed {
                        acknowledgement: StrictAppendAck::AlreadyPresent(authoritative),
                        ..
                    }) => Some(authoritative),
                    Err(_) => None,
                };
                if let Some(authoritative) = authoritative {
                    self.converge_working_directory_switch(generation, authoritative.clone());
                }
                let _ = reply.send(result);
            }
            ChatStateCommand::BeginNativeCapture {
                lineage,
                request,
                turn,
                reply,
            } => {
                let result = self
                    .begin_native_capture(
                        lineage,
                        request,
                        turn,
                        None,
                        crate::persistence::native::NativePrimaryPublication::Legacy,
                    )
                    .map(|lease| crate::persistence::native::NativeCaptureReply {
                        lease: Some(lease),
                    });
                if let Err(Ok(mut abandoned)) = reply.send(result)
                    && let Some(lease) = abandoned.lease.take()
                {
                    lease.finish();
                    self.state.native.leases.remove(&lease.ordinal());
                }
            }
            ChatStateCommand::BeginNativeCaptureWithIdentity {
                lineage,
                request,
                turn,
                inputs,
                publication,
                reply,
            } => {
                let result = self
                    .begin_native_capture(lineage, request, turn, inputs, publication)
                    .map(|lease| crate::persistence::native::NativeCaptureReply {
                        lease: Some(lease),
                    });
                if let Err(Ok(mut abandoned)) = reply.send(result)
                    && let Some(lease) = abandoned.lease.take()
                {
                    lease.finish();
                    self.state.native.leases.remove(&lease.ordinal());
                }
            }
            ChatStateCommand::AppendNativeEvidence {
                request,
                lease,
                event,
                reply,
            } => {
                let result = self.append_native_evidence(request, lease, event).await;
                let _ = reply.send(result);
            }
            ChatStateCommand::ResolveNativePrimary {
                lease,
                decision,
                reply,
            } => {
                let result = self.resolve_native_primary(lease, decision).await;
                let _ = reply.send(result);
            }
            ChatStateCommand::ReadNativeHistory { reply } => {
                let result = if let Some(reason) = self.state.native.unavailable {
                    Err(reason)
                } else if let Some(checkpoint) = &self.state.native.checkpoint
                    && let Err(reason) = checkpoint.matches_history(&self.state.conversation)
                {
                    Err(reason)
                } else if let Some(lineage) = &self.state.native.lineage {
                    Ok(crate::persistence::native::NativeRead {
                        lineage: lineage.clone(),
                        checkpoint: self.state.native.checkpoint.clone(),
                    })
                } else {
                    Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence)
                };
                let _ = reply.send(result);
            }
            ChatStateCommand::PushUserMessageWithRepairReason { item, reason } => {
                self.push_user_message_with_repair_reason(item, reason);
            }
            ChatStateCommand::PushAssistantResponse { item } => {
                self.push_message(item);
            }
            ChatStateCommand::PushToolResult { item } => {
                self.push_message(item);
            }
            ChatStateCommand::PushModelOutput { item } => {
                self.push_model_output(item);
            }
            ChatStateCommand::PushUnreportedModelOutput { item } => {
                self.push_unreported_model_output(item);
            }
            ChatStateCommand::RecordTokenUsage { total_tokens } => {
                self.record_token_usage(total_tokens);
            }
            ChatStateCommand::RecordLastTurnUsage { usage } => {
                self.record_last_turn_usage(usage);
            }
            ChatStateCommand::RecordModelCallUsage {
                model_id,
                usage,
                api_duration_ms,
                cost_usd_ticks,
            } => {
                self.record_model_call_usage(model_id, &usage, api_duration_ms, cost_usd_ticks);
            }
            ChatStateCommand::RecordSubagentUsage {
                by_model,
                attribute_to_prompt,
                incomplete,
                reply,
            } => {
                self.record_subagent_usage(&by_model, attribute_to_prompt, incomplete);
                let _ = reply.send(());
            }
            ChatStateCommand::MarkUsageIncomplete {
                prompt,
                session,
                reply,
            } => {
                self.mark_usage_incomplete(prompt, session);
                let _ = reply.send(());
            }
            ChatStateCommand::IncrementPromptIndex => {
                self.increment_prompt_index();
            }
            ChatStateCommand::InstallTransportBinding { binding, reply } => {
                self.state.sampling_config = binding.config;
                self.state.native.install_generation =
                    self.state.native.install_generation.saturating_add(1);
                self.state.credentials = binding.credentials;
                let _ = reply.send(());
            }
            ChatStateCommand::CaptureTransportBinding { reply } => {
                let _ = reply.send(crate::RequestTransportBinding {
                    config: self.state.sampling_config.clone(),
                    credentials: self.state.credentials.clone(),
                });
            }
            ChatStateCommand::ReplaceExpectedTransportBinding {
                expected,
                binding,
                reply,
            } => {
                let applied = self.state.sampling_config.model == expected.config.model
                    && self.state.sampling_config.base_url == expected.config.base_url
                    && self.state.sampling_config.api_backend == expected.config.api_backend
                    && self.state.credentials == expected.credentials;
                if applied {
                    self.state.sampling_config = binding.config;
                    self.state.native.install_generation =
                        self.state.native.install_generation.saturating_add(1);
                    self.state.credentials = binding.credentials;
                }
                let _ = reply.send(applied);
            }
            ChatStateCommand::PatchApiKey {
                expected,
                api_key,
                reply,
            } => {
                let applied = self.state.sampling_config.model == expected.config.model
                    && self.state.sampling_config.base_url == expected.config.base_url
                    && self.state.sampling_config.api_backend == expected.config.api_backend
                    && self.state.credentials == expected.credentials;
                if applied {
                    self.state.credentials.api_key = api_key;
                }
                let _ = reply.send(applied);
            }
            ChatStateCommand::PatchSamplingFields {
                expected,
                fields,
                reply,
            } => {
                let config = &mut self.state.sampling_config;
                let applied = config.model == expected.config.model
                    && config.base_url == expected.config.base_url
                    && config.api_backend == expected.config.api_backend
                    && self.state.credentials == expected.credentials
                    && (fields.context_window.is_none()
                        || config.context_window == expected.config.context_window)
                    && (fields.max_completion_tokens.is_none()
                        || config.max_completion_tokens == expected.config.max_completion_tokens)
                    && (fields.reasoning_effort.is_none()
                        || config.reasoning_effort == expected.config.reasoning_effort)
                    && (fields.ultra_wire_effort.is_none()
                        || config.ultra_wire_effort == expected.config.ultra_wire_effort)
                    && (fields.supports_search_tool.is_none()
                        || config.supports_search_tool == expected.config.supports_search_tool);
                if applied {
                    if let Some(value) = fields.context_window {
                        config.context_window = value;
                    }
                    if let Some(value) = fields.max_completion_tokens {
                        config.max_completion_tokens = value;
                    }
                    if let Some(value) = fields.model {
                        config.model = value;
                    }
                    if let Some(value) = fields.reasoning_effort {
                        config.reasoning_effort = value;
                    }
                    if let Some(value) = fields.ultra_wire_effort {
                        config.ultra_wire_effort = value;
                    }
                    if let Some(value) = fields.supports_search_tool {
                        config.supports_search_tool = value;
                    }
                }
                let _ = reply.send(applied);
            }
            ChatStateCommand::UpdateSamplingConfig { config } => {
                // apex-waj.35 (ruling `map/RULINGS-o1o5.md` §"O3, run 5"): the row's
                // `supports_search_tool` flag rides the config that names the row, so moving the
                // row and refreshing its admission is ONE message. There is nothing to invalidate
                // and nothing to race: whichever production caller reaches this command —
                // `handle_set_session_model`, `handle_set_reasoning_effort`,
                // `SessionCommand::OverrideModelName`, a response-metadata refresh — the flag it
                // ends up with is the one that came with the row now installed, and a caller that
                // rewrites `model` on a config it read back must re-derive the flag with it (the
                // shell's two such sites: `acp_session_impl/model_switch.rs`'s effort router and
                // `acp_session_impl/run_loop.rs`'s `OverrideModelName`).
                self.state.sampling_config = *config;
                self.state.native.install_generation =
                    self.state.native.install_generation.saturating_add(1);
            }
            ChatStateCommand::RecordAgentEditedPath { path } => {
                self.state.agent_edited_paths.insert(path);
            }
            ChatStateCommand::RecordStreamStart { timestamp_ms } => {
                self.state.stream_start_ms = Some(timestamp_ms);
            }
            ChatStateCommand::RecordTurnStart { timestamp_ms } => {
                self.state.turn_start_ms = Some(timestamp_ms);
            }
            ChatStateCommand::ReplaceConversation {
                items,
                is_compaction,
            } => {
                self.replace_conversation(items, is_compaction);
            }
            ChatStateCommand::RepairHistory {
                dry_run,
                turn_active,
                reply,
            } => {
                // Checked here so refusal and mutation are serialized; a
                // `false` at processing time means pre-turn state (see the
                // command's doc).
                let blocked = turn_active
                    .as_ref()
                    .map(|f| f.load(std::sync::atomic::Ordering::SeqCst))
                    .unwrap_or(false);
                let result = if blocked {
                    Err(crate::commands::RepairHistoryBlocked)
                } else {
                    Ok(self.repair_history(dry_run))
                };
                let _ = reply.send(result);
            }
            ChatStateCommand::StripConversationImages { urls, reply } => {
                match self.strip_conversation_images(&urls) {
                    None => {
                        let _ = reply.send(crate::StripOutcome::NoMatch);
                    }
                    Some((stripped, ack_rx)) => {
                        // Await the disk ack off-actor: the persistence channel already
                        // orders the write, and blocking here would stall reads behind an fsync.
                        tokio::spawn(async move {
                            let outcome = match ack_rx.await {
                                Ok(Ok(())) => crate::StripOutcome::Applied { stripped },
                                Ok(Err(_)) | Err(_) => {
                                    crate::StripOutcome::WriteFailed { stripped }
                                }
                            };
                            let _ = reply.send(outcome);
                        });
                    }
                }
            }
            ChatStateCommand::StripModelBoundHistory { reply } => {
                match self.strip_model_bound_history() {
                    None => {
                        let _ = reply.send(crate::StripOutcome::NoMatch);
                    }
                    Some((stripped, ack_rx)) => {
                        // Same off-actor ack discipline as StripConversationImages.
                        tokio::spawn(async move {
                            let outcome = match ack_rx.await {
                                Ok(Ok(())) => crate::StripOutcome::Applied { stripped },
                                Ok(Err(_)) | Err(_) => {
                                    crate::StripOutcome::WriteFailed { stripped }
                                }
                            };
                            let _ = reply.send(outcome);
                        });
                    }
                }
            }
            ChatStateCommand::ProjectSwitchHistory {
                target_model,
                target_pin,
                route,
                reply,
            } => {
                match self.project_switch_history(&target_model, target_pin.as_deref(), &route) {
                    None => {
                        let _ = reply.send(crate::StripOutcome::NoMatch);
                    }
                    Some((changed, ack_rx)) => {
                        // Same off-actor ack discipline as StripModelBoundHistory.
                        tokio::spawn(async move {
                            let outcome = match ack_rx.await {
                                Ok(Ok(())) => crate::StripOutcome::Applied { stripped: changed },
                                Ok(Err(_)) | Err(_) => {
                                    crate::StripOutcome::WriteFailed { stripped: changed }
                                }
                            };
                            let _ = reply.send(outcome);
                        });
                    }
                }
            }
            ChatStateCommand::ReplaceSystemHead { prompt, reply } => {
                let changed = self.replace_system_head(&prompt);
                let _ = reply.send(changed);
            }
            ChatStateCommand::CachePromptText { text } => {
                self.state.prompt_texts.push(text);
            }
            ChatStateCommand::RecordCompactionAt { prompt_index } => {
                self.state.last_compaction_prompt_index = Some(prompt_index);
            }
            ChatStateCommand::Flush => {
                self.persistence.flush();
            }
            ChatStateCommand::UpdateCredentials { credentials } => {
                self.state.credentials = credentials;
            }
            ChatStateCommand::RestoreSnapshot(snapshot) => {
                self.restore_snapshot(*snapshot);
            }
            ChatStateCommand::BeginTurnCapture => {
                self.state.turn_capture = Some(state::TurnCaptureState {
                    turn_start_offset: self.state.conversation.len(),
                    pre_replacement_messages: Vec::new(),
                    compaction_occurred: false,
                });
            }
            ChatStateCommand::AppendHarnessTraceItems { items } => {
                self.state.harness_trace_buffer.extend(items);
            }
            ChatStateCommand::FlushHarnessTraceTurn => {
                self.state.seal_harness_trace_turn();
            }
            ChatStateCommand::RepairDanglingAfterHarnessHalt { class } => {
                self.repair_dangling_after_harness_halt(class);
            }
            ChatStateCommand::PopStrandedContinueReminder => {
                self.pop_stranded_continue_reminder();
            }

            // Queries are pure reads — repair only at write boundaries
            // (`ChatState::new`, `push_user_message`).
            // `BuildConversationRequest` keeps the guard: it runs between turns, never from background tasks.
            ChatStateCommand::BuildConversationRequest {
                tool_definitions,
                memory_reminder,
                persist_memory_reminder,
                trace,
                conv_id,
                req_id,
                reply,
            } => {
                self.ensure_conversation_integrity();
                let request = self
                    .build_conversation_request(
                        tool_definitions,
                        memory_reminder,
                        persist_memory_reminder,
                        trace,
                        conv_id,
                        req_id,
                        NativeBuildCapture::Legacy,
                    )
                    .request;
                let _ = reply.send(request);
            }
            ChatStateCommand::BuildConversationRequestWithBinding {
                tool_candidates,
                memory_reminder,
                persist_memory_reminder,
                trace,
                conv_id,
                req_id,
                reply,
            } => {
                let built = self.bound_build(
                    tool_candidates,
                    memory_reminder,
                    persist_memory_reminder,
                    trace,
                    conv_id,
                    req_id,
                );
                // A refusal drops the reply: the handle's query resolves to
                // None, so the unchanged public signature reports it without a
                // new error type.
                match built {
                    Some(built) => {
                        let _ = reply.send(built);
                    }
                    None => drop(reply),
                }
            }
            ChatStateCommand::GetConversation { reply } => {
                tracing::debug!(
                    conversation_len = self.state.conversation.len(),
                    "ChatState: cloning full conversation for GetConversation"
                );
                let _ = reply.send(self.state.conversation.clone());
            }
            ChatStateCommand::GetPromptIndex { reply } => {
                let _ = reply.send(self.state.prompt_index);
            }
            ChatStateCommand::GetLastCompactionPromptIndex { reply } => {
                let _ = reply.send(self.state.last_compaction_prompt_index);
            }
            ChatStateCommand::GetTotalTokens { reply } => {
                let _ = reply.send(self.state.total_tokens);
            }
            ChatStateCommand::GetLastTurnUsage { reply } => {
                let _ = reply.send(self.state.last_turn_usage.clone());
            }
            ChatStateCommand::GetPromptUsage { reply } => {
                let _ = reply.send(self.state.prompt_usage.clone());
            }
            ChatStateCommand::GetSessionUsage { reply } => {
                let _ = reply.send(self.state.session_usage.clone());
            }
            ChatStateCommand::GetEstimatedTotalTokens { reply } => {
                let _ =
                    reply.send(self.state.total_tokens + self.state.estimated_tokens_since_model);
            }
            ChatStateCommand::GetSamplingConfig { reply } => {
                let _ = reply.send(self.state.sampling_config.clone());
            }
            ChatStateCommand::GetAgentEditedPaths { reply } => {
                let _ = reply.send(self.state.agent_edited_paths.clone());
            }
            ChatStateCommand::GetNotificationMeta { reply } => {
                let _ = reply.send(self.get_notification_meta());
            }
            ChatStateCommand::Snapshot { reply } => {
                tracing::debug!(
                    conversation_len = self.state.conversation.len(),
                    "ChatState: cloning full state for Snapshot"
                );
                let _ = reply.send(self.snapshot());
            }
            ChatStateCommand::TruncateToPromptIndex {
                target_prompt_index,
                reply,
            } => {
                self.truncate_to_prompt_index(target_prompt_index);
                self.state.turn_capture = None;
                self.state.prompt_usage = None;
                // `harness_trace_buffer` / `harness_trace_turns` survive a rewind: the
                // subagents genuinely ran, so their sealed traces stay uploadable even
                // when the live turn that triggered them is undone.
                let _ = reply.send(());
            }
            ChatStateCommand::CheckAutoCompactNeeded {
                threshold_percent,
                reply,
            } => {
                let _ = reply.send(self.check_auto_compact_needed(threshold_percent));
            }
            ChatStateCommand::GetCredentials { reply } => {
                let _ = reply.send(self.state.credentials.clone());
            }
            ChatStateCommand::GetLastModelMetadata { reply } => {
                let _ = reply.send(self.get_last_model_metadata());
            }
            ChatStateCommand::TakeTurnMessages { reply } => {
                let result = self.state.turn_capture.take().map(|cap| {
                    let mut messages = cap.pre_replacement_messages;
                    messages.extend(
                        Self::turn_tail(&self.state.conversation, cap.turn_start_offset)
                            .iter()
                            .cloned(),
                    );
                    TurnCapture {
                        messages,
                        compaction_occurred: cap.compaction_occurred,
                    }
                });
                let _ = reply.send(result);
            }
            ChatStateCommand::TakeHarnessTraceTurns { reply } => {
                // Defensive seal: a phase that recorded items but never flushed
                // still rides its own turn rather than stranding.
                self.state.seal_harness_trace_turn();
                let _ = reply.send(std::mem::take(&mut self.state.harness_trace_turns));
            }

            // ─── Narrow targeted queries ──────────────────────────────────
            ChatStateCommand::GetConversationLen { reply } => {
                let _ = reply.send(self.get_conversation_len());
            }
            ChatStateCommand::HasDanglingToolCalls { reply } => {
                let _ = reply.send(self.has_dangling_tool_calls());
            }
            ChatStateCommand::GetLastAssistantText { reply } => {
                let _ = reply.send(self.get_last_assistant_text());
            }
            ChatStateCommand::GetTrailingAssistantReport { reply } => {
                let _ = reply.send(self.get_trailing_assistant_report());
            }
            ChatStateCommand::GetLastAssistantTextInTurn { reply } => {
                let _ = reply.send(self.get_last_assistant_text_in_turn());
            }
            ChatStateCommand::GetAssistantTextInTurn { reply } => {
                let _ = reply.send(self.get_assistant_text_in_turn());
            }
            ChatStateCommand::GetFirstUserText { reply } => {
                let _ = reply.send(self.get_first_user_text());
            }
            ChatStateCommand::GetConversationItemAt { index, reply } => {
                let _ = reply.send(self.get_conversation_item_at(index));
            }
            ChatStateCommand::GetLastUserQueryText { reply } => {
                let _ = reply.send(self.get_last_user_query_text());
            }
            ChatStateCommand::GetConversationCounts { reply } => {
                let _ = reply.send(self.get_conversation_counts());
            }
            ChatStateCommand::GetSystemMessage { reply } => {
                let _ = reply.send(self.get_system_message());
            }
            ChatStateCommand::GetEstimatedMessagesTokens { reply } => {
                let _ = reply.send(state::estimate_messages_tokens(&self.state.conversation));
            }
        }
    }
}

impl ChatStateActor {
    async fn register_native_history_owner(
        &mut self,
        key: xai_grok_sampling_types::conversation::native_search_history::NativeHistoryRegistrationKey,
        lineage: String,
        request: String,
        target: xai_grok_sampling_types::conversation::native_search_history::NativeReplayDomain,
        owner_class: xai_grok_sampling_types::conversation::native_search_history::NativeProjectionOwner,
        run_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<
        crate::NativeHistoryOwnerGuard,
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    > {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        if !self.state.native.issuer.accepts_registration_key(&key) {
            return Err(NativeUnavailable::Incomplete);
        }
        if request.is_empty()
            || (owner_class == NativeProjectionOwner::OwningPrimary)
                != run_active.load(std::sync::atomic::Ordering::Acquire)
            || self
                .state
                .native
                .owner
                .as_ref()
                .is_some_and(|owner| owner.seal().is_live())
        {
            return Err(NativeUnavailable::Incomplete);
        }
        self.reconcile_native_projection().await?;
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.persistence.read_native_history(),
        )
        .await
        .map_err(|_| NativeUnavailable::Indeterminate)?
        .map_err(|_| NativeUnavailable::Indeterminate)??;
        if read.lineage != lineage {
            return Err(NativeUnavailable::StaleHistory);
        }
        if let Some(checkpoint) = &read.checkpoint {
            checkpoint.matches_history(&self.state.conversation)?;
        }
        self.state.native.lineage = Some(read.lineage);
        self.state.native.checkpoint = read.checkpoint;
        NativeProjectionDomain::from(&target).validate()?;
        if target.model != self.state.sampling_config.model
            || target.backend != self.state.sampling_config.api_backend
            || !target
                .endpoint
                .starts_with(self.state.sampling_config.base_url.trim_end_matches('/'))
        {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        self.state.native.next_run = self
            .state
            .native
            .next_run
            .checked_add(1)
            .ok_or(NativeUnavailable::Capacity)?;
        let purpose = match owner_class {
            NativeProjectionOwner::OwningPrimary => NativeHistorySealPurpose::OwningPrimary,
            NativeProjectionOwner::SwitchStore => NativeHistorySealPurpose::SwitchStore,
        };
        let seal = self.state.native.issuer.issue_owner(
            purpose,
            self.actor_nonce,
            lineage,
            self.state.native.next_run,
        )?;
        let token = crate::NativeHistoryOwnerToken::new(seal);
        self.state.native.owner = Some(token.clone());
        self.state.native.owner_target = Some(target);
        self.state.native.run_active = Some(run_active);
        Ok(crate::NativeHistoryOwnerGuard::new(token))
    }

    fn native_owner_is_current(&self, token: &crate::NativeHistoryOwnerToken) -> bool {
        self.state.native.owner.as_ref() == Some(token)
            && self.state.native.issuer.owns(token.seal())
            && token.actor_instance() == self.actor_nonce
            && self.state.native.lineage.as_deref() == Some(token.lineage())
            && self.state.native.run_active.as_ref().is_some_and(|active| {
                let live = active.load(std::sync::atomic::Ordering::Acquire);
                match token.seal().purpose() {
                    xai_grok_sampling_types::conversation::native_search_history::NativeHistorySealPurpose::SwitchStore => !live,
                    xai_grok_sampling_types::conversation::native_search_history::NativeHistorySealPurpose::OwningPrimary => live,
                    _ => false,
                }
            })
    }

    async fn reconcile_native_projection(
        &mut self,
    ) -> Result<(), xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable>
    {
        use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
        let Some(pending) = self.state.native.pending_projection.clone() else {
            return Ok(());
        };
        let acknowledgement = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.persistence
                .persist_native_projection_and_ack(pending.stage.write.clone()),
        )
        .await
        .map_err(|_| NativeUnavailable::Indeterminate)?
        .map_err(|_| NativeUnavailable::Indeterminate)??;
        let expected = &pending.stage.write;
        if acknowledgement.transaction() != expected.commit.transaction
            || acknowledgement.generation() != expected.commit.high_water
            || acknowledgement.checkpoint().digest
                != crate::persistence::native::digest(&expected.checkpoint.bytes)
            || acknowledgement.authority_digest()
                != crate::persistence::native::digest(&expected.authority_bytes)
            || acknowledgement.before() != &expected.commit.before.boundary
            || acknowledgement.after() != &expected.checkpoint.record.history_boundary
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        self.snapshot_turn_slice();
        self.state.conversation = pending.stage.durable_after;
        self.rebase_turn_capture_offset();
        self.state.native.history_revision = self
            .state
            .native
            .history_revision
            .checked_add(1)
            .ok_or(NativeUnavailable::Capacity)?;
        self.state.native.checkpoint = Some(expected.checkpoint.clone());
        self.state.native.loaded = pending.stage.send_core.loaded.clone();
        self.state.native.pending_projection = None;
        Ok(())
    }

    async fn commit_native_history_projection(
        &mut self,
        preparation: xai_grok_sampling_types::conversation::native_search_history::NativeBoundPreparation,
        request: xai_grok_sampling_types::ConversationRequest,
        route: xai_grok_sampling_types::conversation::projection::TargetRoute,
    ) -> Result<
        crate::persistence::native::NativeProjectionCompletion,
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    > {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let token = self
            .state
            .native
            .owner
            .clone()
            .ok_or(NativeUnavailable::Incomplete)?;
        if !self.native_owner_is_current(&token)
            || self.state.native.issued.as_ref() != Some(&preparation)
            || !preparation
                .seal()
                .is_some_and(|seal| self.state.native.issuer.owns(seal))
            || preparation.history_revision() != Some(self.state.native.history_revision)
            || crate::persistence::native::history_boundary(&self.state.conversation)?
                != *preparation.history()
        {
            return Err(NativeUnavailable::StaleHistory);
        }
        let checkpoint = match self.state.native.checkpoint.clone() {
            Some(checkpoint) => checkpoint,
            None => std::sync::Arc::new(
                crate::persistence::native::NativeCheckpoint::empty_projection_input(
                    token.lineage().into(),
                    &self.state.conversation,
                )?,
            ),
        };
        let context = preparation.context().ok_or(NativeUnavailable::Incomplete)?;
        if checkpoint.record.generation != context.capture().native_generation
            || checkpoint.record.high_water != context.capture().high_water
        {
            return Err(NativeUnavailable::StaleHistory);
        }
        let stage = checkpoint.prepare_projection(&preparation, &request, &route)?;
        self.commit_native_prepared_projection(preparation, stage)
            .await
    }

    async fn commit_native_prepared_projection(
        &mut self,
        preparation: xai_grok_sampling_types::conversation::native_search_history::NativeBoundPreparation,
        stage: crate::persistence::native::NativePreparedProjection,
    ) -> Result<
        crate::persistence::native::NativeProjectionCompletion,
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    > {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let token = self
            .state
            .native
            .owner
            .clone()
            .ok_or(NativeUnavailable::Incomplete)?;
        if !self.native_owner_is_current(&token)
            || self.state.native.issued.as_ref() != Some(&preparation)
            || crate::persistence::native::history_boundary(&self.state.conversation)?
                != *preparation.history()
        {
            return Err(NativeUnavailable::StaleHistory);
        }
        self.state.native.pending_projection =
            Some(crate::persistence::native::NativePendingProjection {
                preparation: preparation.clone(),
                stage: stage.clone(),
            });
        let first = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.persistence
                .persist_native_projection_and_ack(stage.write.clone()),
        )
        .await
        .map_err(|_| NativeUnavailable::Indeterminate)
        .and_then(|result| result.map_err(|_| NativeUnavailable::Indeterminate))
        .and_then(|result| result);
        let acknowledgement = match first {
            Ok(acknowledgement) => acknowledgement,
            Err(NativeUnavailable::Indeterminate) if self.native_owner_is_current(&token) => {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.persistence
                        .persist_native_projection_and_ack(stage.write.clone()),
                )
                .await
                .map_err(|_| NativeUnavailable::Indeterminate)?
                .map_err(|_| NativeUnavailable::Indeterminate)??
            }
            Err(reason) => return Err(reason),
        };
        if acknowledgement.transaction() != stage.write.commit.transaction
            || acknowledgement.generation() != stage.write.commit.high_water
            || acknowledgement.authority_digest()
                != crate::persistence::native::digest(&stage.write.authority_bytes)
            || acknowledgement.checkpoint().digest
                != crate::persistence::native::digest(&stage.write.checkpoint.bytes)
            || acknowledgement.before() != preparation.history()
            || acknowledgement.after() != &stage.write.checkpoint.record.history_boundary
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        self.snapshot_turn_slice();
        self.state.conversation = stage.durable_after.clone();
        self.rebase_turn_capture_offset();
        self.state.native.history_revision = self
            .state
            .native
            .history_revision
            .checked_add(1)
            .ok_or(NativeUnavailable::Capacity)?;
        self.state.native.checkpoint = Some(stage.write.checkpoint.clone());
        self.state.native.loaded = stage.send_core.loaded.clone();
        self.state.native.pending_projection = None;
        if !self.native_owner_is_current(&token) {
            return Err(NativeUnavailable::Incomplete);
        }
        let certificate = self.state.native.issuer.confirm_compound(
            &preparation,
            acknowledgement,
            &stage.write.checkpoint.record.history_boundary,
        )?;
        Ok(crate::persistence::native::NativeProjectionCompletion {
            preparation,
            certificate,
            send_record: stage.send_record,
            send_ledger_bytes: stage.send_ledger_bytes,
            send_before: stage.send_before,
            send_after: stage.send_after,
            core: stage.send_core,
        })
    }
}

impl ChatStateActor {
    async fn project_native_switch_history(
        &mut self,
        candidates: crate::NativeToolCandidates,
        target_model: String,
        target_pin: Option<String>,
        route: xai_grok_sampling_types::conversation::projection::TargetRoute,
    ) -> Result<
        Option<crate::persistence::native::NativeProjectionCompletion>,
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    > {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let entry = candidates.history_entry()?.clone();
        let crate::NativeHistoryEntryIntent::SwitchStore(token) = entry.intent() else {
            return Err(NativeUnavailable::Incomplete);
        };
        if entry.requirement() != NativeHistoryRequirement::Selected
            || !self.native_owner_is_current(token)
            || self.state.sampling_config.model != target_model
            || self.state.sampling_config.api_backend != route.backend()
        {
            return Err(NativeUnavailable::StaleHistory);
        }
        let (tools, native_eligible_inputs, _) =
            candidates.select_for_bound_build(&self.state.sampling_config)?;
        let admission = xai_grok_sampling_types::SearchAdmission::for_row(
            self.state.sampling_config.supports_search_tool,
            &tools,
        );
        if admission != route.admission() {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        let history = crate::persistence::native::history_boundary(&self.state.conversation)?;
        let checkpoint = match self.state.native.checkpoint.clone() {
            Some(checkpoint) => checkpoint,
            None => std::sync::Arc::new(
                crate::persistence::native::NativeCheckpoint::empty_projection_input(
                    token.lineage().into(),
                    &self.state.conversation,
                )?,
            ),
        };
        let mut entries = Vec::with_capacity(self.state.conversation.len());
        for (index, item) in self.state.conversation.iter().enumerate() {
            let digest = crate::persistence::native::digest(
                &serde_json::to_vec(item).map_err(|_| NativeUnavailable::Fidelity)?,
            );
            entries.push(NativeHistoryMappingEntry::durable(
                u32::try_from(index).map_err(|_| NativeUnavailable::Capacity)?,
                digest.clone(),
                digest,
                vec![NativeHistoryTransform::Kept],
                None,
            ));
        }
        let mapping =
            NativeHistoryTransformMapping::checked(history.clone(), history.clone(), entries)?;
        self.native_build_sequence = self
            .native_build_sequence
            .checked_add(1)
            .ok_or(NativeUnavailable::Capacity)?;
        self.state.native.binding_generation = self
            .state
            .native
            .binding_generation
            .checked_add(1)
            .ok_or(NativeUnavailable::Capacity)?;
        let request_id = format!(
            "switch:{}:{}",
            self.state.native.install_generation, target_model
        );
        let capture = NativeHistoryCapture {
            history: history.clone(),
            history_revision: self.state.native.history_revision,
            lineage: checkpoint.record.lineage.clone(),
            native_generation: checkpoint.record.generation,
            high_water: checkpoint.record.high_water,
            projection_revision: checkpoint.active.projection_revision,
            strict_append_sequence: 0,
            binding_generation: self.state.native.binding_generation,
            install_generation: self.state.native.install_generation,
            issued_request: request_id.clone(),
            target: self
                .state
                .native
                .owner_target
                .clone()
                .ok_or(NativeUnavailable::Incomplete)?,
            execution: if route.backend() == xai_grok_sampling_types::ApiBackend::Messages {
                NativeHistoryExecution::Server
            } else {
                NativeHistoryExecution::Client
            },
            original_admission: admission,
            versions: NativeHistoryVersions {
                schema: checkpoint.record.schema_version,
                policy: 1,
                projection: 1,
                reader: checkpoint.record.reader_floor,
                limits: checkpoint.record.limits_revision,
            },
        };
        let seal = self
            .state
            .native
            .issuer
            .issue_bound(token.seal(), capture.clone())?;
        let context = NativeHistoryRequestContext::captured(
            NativeHistoryRequirement::Selected,
            capture,
            NativeHistoryWriteAuthority::SwitchStore {
                lineage: token.lineage().into(),
                binding_generation: self.state.native.binding_generation,
            },
        );
        let preparation = NativeBoundPreparation::issued(
            NativeHistoryRequirement::Selected,
            NativePreparationGrant::SwitchStore,
            request_id,
            history,
            mapping,
            Some(self.state.native.history_revision),
            Some(self.state.native.install_generation),
            self.native_build_sequence,
            self.actor_nonce,
        )
        .with_issued_context(
            context,
            seal,
            serde_json::to_vec(&self.state.conversation)
                .map_err(|_| NativeUnavailable::Fidelity)?
                .into(),
        )?
        .with_switch_projection(NativeSwitchProjection::captured(
            target_model.clone(),
            target_pin,
            &route,
        ))?;
        self.state.native.issued = Some(preparation.clone());
        let request = xai_grok_sampling_types::ConversationRequest {
            items: self.state.conversation.clone(),
            tools,
            search_admission: Some(admission),
            native_eligible_inputs,
            model: Some(target_model),
            native_bound_preparation: Some(preparation.clone()),
            ..Default::default()
        };
        let stage = checkpoint.prepare_projection(&preparation, &request, &route)?;
        if let Some(previous) = &checkpoint.compound_proof {
            let metadata: NativeProjectionMetadata =
                serde_json::from_slice(&stage.write.metadata_bytes)
                    .map_err(|_| NativeUnavailable::InvalidReference)?;
            if previous.metadata() == &metadata
                && stage.write.commit.before.boundary
                    == stage.write.checkpoint.record.history_boundary
                && checkpoint.active.dispositions == stage.write.checkpoint.active.dispositions
                && checkpoint.active.loaded_invalidations
                    == stage.write.checkpoint.active.loaded_invalidations
            {
                return Ok(None);
            }
        }
        self.commit_native_prepared_projection(preparation, stage)
            .await
            .map(Some)
    }
}
