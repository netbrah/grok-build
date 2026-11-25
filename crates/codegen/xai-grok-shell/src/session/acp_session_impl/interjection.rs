//! Mid-turn interjection handling for `SessionActor`: buffer type, formatting, broadcast, and drain.
//! Also hosts `inject_synthetic_user_message`, the shared synthetic-user-message injector the permission-panel followup path reuses.

use super::*;

// Buffer, entry type, and formatting live in the shared xai-interjection-core crate so the server-side agent loop can adopt the same behaviour.
// The shell keeps arrival (ACP ext methods), persistence, and pager echo.
// Re-exported for `acp_session.rs`, which does `pub(crate) use interjection::*;`.
#[allow(unused_imports)]
pub(crate) use xai_interjection_core::{
    INTERRUPT_NOTE, InterjectionBuffer, drain_formatted, format_interjection, frame_user_turn,
};
use xai_interjection_core::events::{Occurrence, next_occurrence};

/// Shell instantiation of the shared entry type: images are ACP content.
pub(crate) type PendingInterjection = xai_interjection_core::PendingInterjection<acp::ImageContent>;

/// Prompt-id prefix for interjections that missed their turn and were converted into standalone prompt turns.
/// The prefix keeps the turn's user echo persist-only.
/// Every pane already rendered the text from the `x.ai/session/interjection` broadcast, so a live echo would duplicate it.
pub(crate) const INTERJECT_FALLBACK_PROMPT_PREFIX: &str = "interject-fallback-";

pub(crate) fn is_interject_fallback(prompt_id: &str) -> bool {
    prompt_id.starts_with(INTERJECT_FALLBACK_PROMPT_PREFIX)
}

impl SessionActor {
    /// An interjection is only merged into a *running* turn (`drain_pending_interjections`).
    /// One that arrives while the session is idle, or lands after the running turn's final drain, would sit in `pending_interjections` forever.
    /// The user's message would be silently lost (the pager already rendered it and said "Interjection sent").
    pub(super) async fn queue_interjection_fallback_prompt(
        &self,
        text: String,
        images: Vec<acp::ImageContent>,
        front: bool,
    ) {
        let prompt_id = format!("{INTERJECT_FALLBACK_PROMPT_PREFIX}{}", uuid::Uuid::now_v7());
        let mut prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(text))];
        prompt_blocks.extend(images.into_iter().map(acp::ContentBlock::Image));
        // Respect an active plan mode: the interjection was aimed at a turn that ran under it, so its fallback turn must not escape the gate
        let prompt_mode = if self.plan_mode.lock().is_active() {
            crate::session::plan_mode::PromptMode::Plan
        } else {
            crate::session::plan_mode::PromptMode::Agent
        };
        let (respond_to, _) = tokio::sync::oneshot::channel();
        // User message (skips queue_input); invalidate in-flight recap now.
        self.invalidate_side_calls_for_new_prompt();
        let item = InputItem {
            prompt_id,
            prompt_blocks,
            prompt_mode,
            trace_gcs_config: None,
            artifact_tracker: None,
            client_identifier: None,
            screen_mode: None,
            verbatim: false,
            json_schema: None,
            input_origin: InputOrigin::new(super::super::PromptOrigin::User),
            task_wake_fallback: None,
            tool_overrides_update: None,
            respond_to,
            persist_ack: None,
            parsed_prompt_tx: None,
            initial_child_prompt_ready: None,
            queue_meta: None,
            queue_mutation_policy: QueueMutationPolicy::hidden(),
            // Send-now placement (see doc): a later real send-now must not leapfrog this fallback in `queue_input`'s FIFO scan
            send_now: front,
            traceparent: None,
        };
        let mut state = self.state.lock().await;
        if front {
            // Never displace a running front (see doc): insert after it when the front row is the in-flight turn's own item
            let insert_at = usize::from(matches!(
                (state.pending_inputs.front(), state.running_prompt_id()),
                (Some(front_item), Some(running)) if front_item.prompt_id == running
            ));
            state.pending_inputs.insert(insert_at, item);
        } else {
            state.pending_inputs.push_back(item);
        }
        tracing::info!("Converted stranded interjection into a queued prompt turn");
    }

    /// Convert interjections that missed their turn's final drain into queued prompt turns, front of the queue in original order.
    /// Returns the count.
    pub(super) async fn flush_stranded_interjections(&self) -> usize {
        if self.retain_stranded_interjections_for_next_turn().await {
            return 0;
        }
        let stranded = self.pending_interjections.drain_all();
        let count = stranded.len();
        // Reversed push_fronts keep entry 0 front-most.
        for entry in stranded.into_iter().rev() {
            self.queue_interjection_fallback_prompt(entry.text, entry.attachments, true)
                .await;
        }
        count
    }
    /// Normalize interjection images for injection (shared pipeline above); notices append to `wrapped` (TEXT side only).
    /// Returns the images to attach structurally.
    /// Sessions whose template rejects inline images instead transcribe normalized survivors into the text via the describe pipeline.
    async fn prepare_interjection_images(
        &self,
        wrapped: &mut String,
        images: Vec<acp::ImageContent>,
    ) -> Vec<acp::ImageContent> {
        if images.is_empty() {
            return images;
        }
        let is_cursor = self.is_cursor_harness();
        let images = self
            .normalize_images_with_notices(wrapped, images, is_cursor)
            .await;
        if !is_cursor {
            return images;
        }
        if !images.is_empty() {
            match self.transcribe_user_images(wrapped.clone(), &images).await {
                Ok(new_text) => *wrapped = new_text,
                Err(e) => {
                    tracing::warn!(?e, "interjection image processing failed; dropping images");
                    wrapped.push_str(
                        "\n\n[Note: the user attached image(s) to this message, but they could \
                         not be processed in this session and were dropped.]",
                    );
                }
            }
        }
        Vec::new()
    }

    /// Broadcast a mid-turn interjection to every attached client.
    /// The originator uses `id` to claim its optimistic prompt block; other clients render the notification normally.
    pub(super) fn broadcast_interjection(&self, text: &str, id: Option<&str>) {
        let mut payload = serde_json::json!({
            "sessionId": self.session_info.id.0.as_ref(),
            "text": text,
        });
        if let Some(id) = id {
            payload["interjectionId"] = serde_json::json!(id);
        }
        if let Ok(params) = serde_json::value::to_raw_value(&payload) {
            self.notifications
                .gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    "x.ai/session/interjection",
                    params.into(),
                ));
        }
    }

    /// Persist and optionally notify the pager for a synthetic user message.
    pub(super) async fn persist_synthetic_user_message(
        &self,
        text: &str,
        notify_pager: bool,
        images: &[acp::ImageContent],
    ) {
        let model_id = self.current_model_id().await;
        self.persist_synthetic_user_message_with_model(text, None, images, &model_id);

        // Notify pager (skipped for interjections; the pager has a local block)
        if notify_pager {
            let user_chunk_meta = serde_json::json!({ "modelId": model_id })
                .as_object()
                .cloned();
            self.send_update(
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                        text.to_string(),
                    )))
                    .meta(user_chunk_meta),
                ),
                None,
            )
            .await;
        }
    }

    /// Persistence half of `persist_synthetic_user_message`, synchronous (model id.
    /// pre-fetched) so the interjection drain can keep the batch submit and its.
    /// persisted user chunks on the same side of any cancellation point.
    fn persist_synthetic_user_message_with_model(
        &self,
        text: &str,
        interjection_display_text: Option<&str>,
        images: &[acp::ImageContent],
        model_id: &str,
    ) {
        let mut user_chunk_meta = serde_json::Map::new();
        user_chunk_meta.insert("modelId".into(), serde_json::json!(model_id));
        let mut text_block = acp::TextContent::new(text.to_string());
        if let Some(display_text) = interjection_display_text {
            user_chunk_meta.insert(
                crate::session::storage::INTERJECTION_META_KEY.into(),
                serde_json::json!(true),
            );
            let mut text_meta = serde_json::Map::new();
            text_meta.insert("displayText".into(), serde_json::json!(display_text));
            text_block = text_block.meta(Some(text_meta));
        }
        let user_chunk_meta = Some(user_chunk_meta);

        // Persist to updates.jsonl: one UserMessageChunk per content block (text first, then any images; Image chunks already round-trip)
        let mut content_blocks = vec![acp::ContentBlock::Text(text_block)];
        content_blocks.extend(images.iter().cloned().map(acp::ContentBlock::Image));
        let notification_meta = self.build_notification_meta();
        for content_block in content_blocks {
            let update = acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(content_block).meta(user_chunk_meta.clone()),
            );
            let _ = self
                .notifications
                .persistence_tx
                .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                    acp::SessionNotification::new(self.session_info.id.clone(), update)
                        .meta(notification_meta.clone().as_object().cloned()),
                ))));
        }
    }

    /// Inject a synthetic user message: persist, optionally notify pager, and push to the conversation as a standalone synthetic user message.
    pub(super) async fn inject_synthetic_user_message(
        &self,
        text: &str,
        item: ConversationItem,
        notify_pager: bool,
        images: &[acp::ImageContent],
    ) {
        self.persist_synthetic_user_message(text, notify_pager, images)
            .await;
        self.chat_state_handle.push_user_message(item);
    }

    /// Expand skill slash references in interjection text into the `<skill_information>` envelope (loaded and substituted SKILL.md bodies).
    /// Interjections bypass turn-start slash resolution (`slash_commands::resolve`).
    /// Without this, a queued `/skill` row force-sent mid-turn, or a typed `/skill` interjection, reaches the model as a bare, unexpanded slash command.
    async fn interjection_skill_information(&self, text: &str) -> Option<String> {
        // Mirror turn-start gating (`parse_slash_prefix`): only a leading slash invokes skills
        // "don't run /commit yet" is steering text, not an invocation
        if !text.trim_start().starts_with('/') {
            return None;
        }
        let slash_skills = self.slash_skills_for_resolve().await;
        // Availability without `command_availability()`'s goal-reconciliation side effects; this runs mid-turn inside the drain
        let tool_names = self.registered_tool_names().await;
        let has_workflow_runs = !self.workflow_tracker().await.lock().list().is_empty();
        let availability = self.build_command_availability(&tool_names, has_workflow_runs);
        let parsed = slash_commands::parse_skill_references(text, &slash_skills, availability)?;
        // Deliberately lighter telemetry than turn start: no `skill.activated` span, `PluginUsed`, or `active_skill` stamp
        // Those attribute the turn, which this skill did not start
        // `SkillDispatched` still carries `plugin_source`, so dispatch counts stay complete
        for sk in &parsed {
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::SlashCommandUsed {
                    command: sk.name.clone(),
                    args_provided: !sk.args.is_empty(),
                },
            );
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::SkillDispatched {
                    skill_name: sk.name.clone(),
                    plugin_source: sk.plugin_name.clone(),
                    trigger: xai_grok_telemetry::events::SkillTrigger::SlashCommand,
                },
            );
        }
        slash_commands::build_skill_information_for_refs(
            &parsed,
            &slash_skills,
            &self.session_id_string(),
        )
        .await
    }

    /// When follow-up behavior is Steer, promote held queue rows into interjections, then drain.
    /// Call after a tool batch, at loop top, and before the turn returns to the user.
    /// Returns `true` if any interjections were drained (caller may `continue` so the model sees them next).
    pub(super) async fn drain_interjections_at_safe_point(&self) -> bool {
        // Queue (default) must not re-parse config on every tool, model, or turn-end drain
        // `follow_up_steer_enabled` is mtime-keyed on config.toml, so a live pager settings write is visible without restarting the shell agent
        // The pager is a separate process; an unchanged mtime is a cheap stat
        if crate::util::config::follow_up_steer_enabled().await {
            let has_held = {
                let state = self.state.lock().await;
                let running = state.running_prompt_id();
                // Only editable human rows are promotable; protected pins and queue-hidden fallbacks must not trigger steer promotion
                running.is_some()
                    && state.pending_inputs.iter().any(|item| {
                        item.is_queue_editable() && Some(item.prompt_id.as_str()) != running
                    })
            };
            if has_held {
                self.promote_queued_as_interjections().await;
            }
        }
        self.drain_admitted_messages_at_safe_point().await
    }

    /// Drain already-admitted human interjections and parent Steers.
    /// Does not promote held queue rows; use after turn-end bookkeeping so
    /// late admissions still inject into the running turn.
    pub(super) async fn drain_admitted_messages_at_safe_point(&self) -> bool {
        if self.hold_inputs_at_native_safe_point().await {
            return false;
        }
        let merging = {
            let state = self.state.lock().await;
            state.message_delivery.held_input.retaining()
        };
        let drained_human = self.drain_pending_interjections().await;
        self.state.lock().await.message_delivery.held_input.peer_origins.clear();
        if merging {
            return drained_human;
        }
        self.drain_parent_messages_at_safe_point().await || drained_human
    }
    pub(super) async fn drain_pending_interjections(&self) -> bool {
        // A native (v2) agent message flushed here is activity even when no
        // human interjection drained (provenance: open-grok@240c99c9
        // interjection.rs safe point).
        let native_activity = self.flush_native_agent_messages().await;
        // Manual drain (not `drain_formatted`): skill parsing needs the raw text.
        // Parsed after wrapping, the envelope's closing `</user_query>` tag would pollute the trailing skill's args.
        // The guard owns the drained entries until the batch is submitted: every await below is a.
        let merging = {
            let state = self.state.lock().await;
            state.message_delivery.held_input.retaining()
        };
        if merging {
            return self.release_native_held_inputs().await || native_activity;
        }
        let (occurrences, entries) = self
            .pending_interjections
            .drain_all_with_occurrences()
            .into_iter()
            .unzip();
        let guard = RestoreOnCancel {
            buffer: self.pending_interjections.clone(),
            entries,
            occurrences,
        };
        if guard.entries.is_empty() {
            return native_activity;
        }

        let mut prepared = Vec::with_capacity(guard.entries.len());
        for PendingInterjection { text, attachments } in &guard.entries {
            // The sanitizer rewrites `[Image #N: <path>]` to `[Image #N]` before the text reaches the model
            // It covers legacy-client raw text AND text harvested from queued rows sent as interjections
            // Wrapping and truncation stay in the shared crate (`format_interjection`)
            let sanitized = crate::session::placeholder_images::strip_paths_from_image_placeholders(
                text.clone(),
            );
            let skill_information = self.interjection_skill_information(&sanitized).await;
            let mut wrapped = format_interjection(sanitized.clone());
            // The pipeline consumes a clone; the guard keeps the original attachments restorable
            let images = self
                .prepare_interjection_images(&mut wrapped, attachments.clone())
                .await;
            // Model-visible text: <skill_information> follows the wrapped <user_query>, the same order as turn-start prompt assembly
            // It is appended after the image pipeline so the template-specific transcription rewrite cannot mangle the envelope
            // The persisted user chunk stays envelope-only (no SKILL.md body); the typed text rides in `displayText` for replay
            let model_text = match &skill_information {
                Some(skill_information) => {
                    tracing::info!("expanded skill references in mid-turn interjection");
                    format!("{wrapped}\n{skill_information}")
                }
                None => wrapped.clone(),
            };
            let mut item = ConversationItem::interjection(model_text);
            for img in &images {
                item.add_image(pick_user_image_url(img));
            }
            prepared.push((wrapped, sanitized, images, item));
        }
        // Last await before the submit; from here persistence is deliberately synchronous so no
        // cancellation point can separate the submitted batch from its persisted user chunks
        let model_id = self.current_model_id().await;
        let (persist_parts, chat_items): (Vec<_>, Vec<_>) = prepared
            .into_iter()
            .map(|(wrapped, typed, images, item)| ((wrapped, typed, images), item))
            .unzip();
        if self
            .chat_state_handle
            .try_push_user_messages_batch(chat_items)
            .is_err()
        {
            // The guard restores the entries so `flush_stranded_interjections` can still deliver them
            tracing::error!(
                session_id = %self.session_info.id.0,
                "interjection drain skipped: chat-state actor unavailable"
            );
            return native_activity;
        }
        // Persist only after the submit succeeded: on the failure/cancel paths the entries go back
        // to the buffer and the fallback-prompt turn persists them, so persisting here too would
        // duplicate the user chunks in updates.jsonl
        for (wrapped, typed, images) in persist_parts {
            self.persist_synthetic_user_message_with_model(
                &wrapped,
                Some(&typed),
                &images,
                &model_id,
            );
        }
        guard.defuse();
        tracing::info!("Injected mid-turn interjections as standalone synthetic user messages");
        // An interjection never cancels the turn, so it leaves no marker on the next user turn (that field is reserved for fatal aborts)
        // The interjection itself is recorded at enqueue time via `Event::Interjected` (carrying the shared `redirect_kind`)
        true
    }
}

/// Cancel-safety guard for `drain_pending_interjections`: a turn abort drops the drain future at one of its awaits (skill resolution, image pipeline, model-id fetch).
/// Entries already drained but not yet submitted to chat state would vanish — `flush_stranded_interjections` would find an empty buffer with nothing to convert into fallback prompts.
/// On drop, unsubmitted entries go back to the front of the buffer, ahead of anything pushed since, keeping arrival order.
#[must_use]
struct RestoreOnCancel {
    buffer: InterjectionBuffer<acp::ImageContent>,
    entries: Vec<PendingInterjection>,
    occurrences: Vec<Occurrence>,
}

impl RestoreOnCancel {
    /// The batch was submitted; the entries are chat state's responsibility now.
    fn defuse(mut self) {
        self.entries.clear();
    }

    fn defuse_occurrences(&mut self, submitted: &[Occurrence]) {
        let entries = std::mem::take(&mut self.entries);
        let occurrences = std::mem::take(&mut self.occurrences);
        for (key, entry) in occurrences.into_iter().zip(entries) {
            if !submitted.contains(&key) {
                self.occurrences.push(key);
                self.entries.push(entry);
            }
        }
    }
}

impl Drop for RestoreOnCancel {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        tracing::warn!(
            count = self.entries.len(),
            "interjection drain aborted before submit; restoring entries for a later drain or flush"
        );
        self.buffer.restore_front_with_occurrences(
            std::mem::take(&mut self.occurrences)
                .into_iter()
                .zip(std::mem::take(&mut self.entries))
                .collect(),
        );
    }
}
const HELD_MACHINE_ENTRIES: usize = 128;

type HeldTurnBinding = xai_message_delivery_core::TurnBinding<String, TurnEpoch>;

pub(super) struct HoldLatch {
    pub(super) binding: HeldTurnBinding,
}

#[derive(Clone)]
pub(super) struct HeldBindingMeta {
    pub(super) prompt_id: String,
    pub(super) epoch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeldSource {
    HumanInterjection,
    ParentSteer,
    MonitorEvent,
    DeferredFollowUp,
    StationarityReminder,
    SkillReminder,
    PlanActivation,
    PeerAgent,
    MediaResample,
    ToolFreeLength,
    LengthReminder,
}

impl HeldSource {
    fn tag(self) -> &'static str {
        match self {
            Self::HumanInterjection => "human_interjection",
            Self::ParentSteer => "parent_steer",
            Self::MonitorEvent => "monitor_event",
            Self::DeferredFollowUp => "deferred_followup",
            Self::StationarityReminder => "stationarity_reminder",
            Self::SkillReminder => "skill_reminder",
            Self::PlanActivation => "plan_activation",
            Self::PeerAgent => "peer_agent",
            Self::MediaResample => "media_resample",
            Self::ToolFreeLength => "tool_free_length",
            Self::LengthReminder => "length_reminder",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldRefusalReason {
    SteerSlotsFull,
    MonitorBandFull,
    DeferredBandFull,
    StationarityBandFull,
    SkillBandFull,
    PlanSlot,
    Terminal,
    PeerBandFull,
    MediaResampleBandFull,
    ToolFreeLengthBandFull,
    LengthReminderBandFull,
}

impl HoldRefusalReason {
    fn tag(self) -> &'static str {
        match self {
            Self::SteerSlotsFull => "steer_slots_full",
            Self::MonitorBandFull => "monitor_band_full",
            Self::DeferredBandFull => "deferred_band_full",
            Self::StationarityBandFull => "stationarity_band_full",
            Self::SkillBandFull => "skill_band_full",
            Self::PlanSlot => "plan_slot",
            Self::PeerBandFull => "peer_band_full",
            Self::MediaResampleBandFull => "media_resample_band_full",
            Self::ToolFreeLengthBandFull => "tool_free_length_band_full",
            Self::LengthReminderBandFull => "length_reminder_band_full",
            Self::Terminal => "terminal",
        }
    }
}

pub(super) struct HoldRefusal {
    pub(super) source: HeldSource,
    pub(super) reason: HoldRefusalReason,
    pub(super) key: u64,
    pub(super) observed_len: u64,
}

impl HoldRefusal {
    fn record(&self) -> serde_json::Value {
        serde_json::json!({"source":self.source.tag(),"reason":self.reason.tag(),"key":self.key,"observed_len":self.observed_len})
    }
}

pub(super) enum HeldBody {
    Human(std::sync::Arc<PendingInterjection>),
    Owned(ConversationItem),
    Parent(String),
    Monitor,
    Plan,
}

pub(super) struct HeldEntry {
    pub(super) key: Occurrence,
    pub(super) source: HeldSource,
    pub(super) body: HeldBody,
    pub(super) meta: serde_json::Value,
    pub(super) preparing: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HeldBand {
    pub(super) held: usize,
    pub(super) limit: Option<usize>,
    pub(super) refused_total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HeldBands {
    pub(super) human: HeldBand,
    pub(super) steer: HeldBand,
    pub(super) monitor: HeldBand,
    pub(super) deferred: HeldBand,
    pub(super) stationarity: HeldBand,
    pub(super) skill: HeldBand,
    pub(super) plan: HeldBand,
    pub(super) peer: HeldBand,
    pub(super) media_resample: HeldBand,
    pub(super) tool_free_length: HeldBand,
    pub(super) length_reminder: HeldBand,
    pub(super) parent_stamps: usize,
    pub(super) refused_by_reason: [u64; 11],
    pub(super) observed_payload_bytes: [Option<u64>; 11],
}

/// Local queue occupancy joined into this ledger. The global shared-control
/// owner is not credited by this landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SharedOwnerDebt {
    pub(super) local_entries: usize,
    pub(super) global_owner_credited: bool,
}

pub(super) struct HeldInputLedger {
    pub(super) latch: Option<HoldLatch>,
    pub(super) latch_meta: Option<HeldBindingMeta>,
    pub(super) entries: std::collections::VecDeque<HeldEntry>,
    pub(super) shared_owner_debt: SharedOwnerDebt,
    pub(super) parent_stamps: Vec<(String, Occurrence)>,
    pub(super) parent_owner: Option<HeldTurnBinding>,
    pub(super) parent_owner_meta: Option<HeldBindingMeta>,
    pub(super) parent_persisted: u32,
    pub(super) pin_owner: Option<Occurrence>,
    pub(super) pinned_in:
        Option<xai_grok_tools::implementations::grok_build::monitor::types::MonitorEventBuffer>,
    pub(super) terminal: bool,
    pub(super) refusal: Option<HoldRefusal>,
    pub(super) peer_origins: std::collections::BTreeSet<Occurrence>,
    refused_totals: [u64; 11],
}

impl Default for HeldInputLedger {
    fn default() -> Self {
        Self {
            latch: None,
            latch_meta: None,
            entries: Default::default(),
            shared_owner_debt: SharedOwnerDebt {
                local_entries: 0,
                global_owner_credited: false,
            },
            parent_stamps: Vec::new(),
            parent_owner: None,
            parent_owner_meta: None,
            parent_persisted: 0,
            pin_owner: None,
            pinned_in: None,
            terminal: false,
            refusal: None,
            peer_origins: Default::default(),
            refused_totals: [0; 11],
        }
    }
}

impl Drop for HeldInputLedger {
    fn drop(&mut self) {
        if let (Some(owner), Some(buffer)) = (self.pin_owner, self.pinned_in.as_ref()) {
            buffer.unpin_all(owner);
        }
    }
}

impl HeldInputLedger {
    pub(super) fn bands(&self, live_steer_slots: usize) -> HeldBands {
        let band = |source, limit| {
            let reason = match source {
                HeldSource::HumanInterjection => None,
                HeldSource::ParentSteer => Some(HoldRefusalReason::SteerSlotsFull),
                HeldSource::MonitorEvent => Some(HoldRefusalReason::MonitorBandFull),
                HeldSource::DeferredFollowUp => Some(HoldRefusalReason::DeferredBandFull),
                HeldSource::StationarityReminder => Some(HoldRefusalReason::StationarityBandFull),
                HeldSource::SkillReminder => Some(HoldRefusalReason::SkillBandFull),
                HeldSource::PlanActivation => Some(HoldRefusalReason::PlanSlot),
                HeldSource::PeerAgent => Some(HoldRefusalReason::PeerBandFull),
                HeldSource::MediaResample => Some(HoldRefusalReason::MediaResampleBandFull),
                HeldSource::ToolFreeLength => Some(HoldRefusalReason::ToolFreeLengthBandFull),
                HeldSource::LengthReminder => Some(HoldRefusalReason::LengthReminderBandFull),
            };
            HeldBand {
                held: self.entries.iter().filter(|entry| entry.source == source).count(),
                limit,
                refused_total: reason.map_or(0, |reason| self.refused_totals[reason as usize]),
            }
        };
        let monitor_pins = match (self.pin_owner, self.pinned_in.as_ref()) {
            (Some(owner), Some(buffer)) => buffer.release_view(owner, |_| false).iter().count(),
            _ => 0,
        };
        HeldBands {
            human: band(HeldSource::HumanInterjection, None),
            steer: HeldBand { held: live_steer_slots, ..band(HeldSource::ParentSteer, Some(32)) },
            monitor: HeldBand { held: monitor_pins, ..band(HeldSource::MonitorEvent, Some(128)) },
            deferred: band(HeldSource::DeferredFollowUp, Some(128)),
            stationarity: band(HeldSource::StationarityReminder, Some(128)),
            skill: band(HeldSource::SkillReminder, Some(128)),
            plan: band(HeldSource::PlanActivation, Some(1)),
            peer: HeldBand { held: self.peer_count(0), ..band(HeldSource::PeerAgent, Some(50)) },
            media_resample: band(HeldSource::MediaResample, Some(128)),
            tool_free_length: band(HeldSource::ToolFreeLength, Some(128)),
            length_reminder: band(HeldSource::LengthReminder, Some(128)),
            parent_stamps: self.parent_stamps.len(),
            refused_by_reason: self.refused_totals,
            observed_payload_bytes: [None; 11],
        }
    }

    // The fixed parent-delivery caller still reaches this method.
    pub(super) fn refresh_header(&mut self) {}

    pub(super) fn bind_parent_delivery(
        &mut self,
        binding: &HeldTurnBinding,
        meta: &HeldBindingMeta,
    ) -> Result<(), HeldCommitError> {
        if self.parent_owner.as_ref() != Some(binding) {
            self.parent_owner = Some(binding.clone());
            self.parent_owner_meta = Some(meta.clone());
            self.parent_persisted = 0;
        }
        Ok(())
    }

    pub(super) fn check_parent_progress(&mut self, _persisted: u32) -> Result<(), HeldCommitError> {
        Ok(())
    }

    pub(super) fn clear_parent_delivery(&mut self) {
        self.parent_owner = None;
        self.parent_owner_meta = None;
        self.parent_persisted = 0;
    }

    pub(super) fn retaining(&self) -> bool {
        self.latch.is_some() || !self.entries.is_empty()
    }

    pub(super) fn stamp_parent(&mut self, message_id: &str) {
        if self.terminal
            || self.parent_stamps.iter().any(|(id, _)| id == message_id)
        {
            return;
        }
        self.parent_stamps
            .push((message_id.to_owned(), next_occurrence()));
    }

    pub(super) fn parent_key(&self, message_id: &str) -> Option<Occurrence> {
        self.parent_stamps
            .iter()
            .find_map(|(id, key)| (id == message_id).then_some(*key))
    }

    fn has_key(&self, key: Occurrence) -> bool {
        self.entries.iter().any(|entry| entry.key == key)
    }

    pub(super) fn refusal(
        &mut self,
        source: HeldSource,
        reason: HoldRefusalReason,
        key: Occurrence,
        observed_len: usize,
    ) {
        self.refused_totals[reason as usize] = self.refused_totals[reason as usize].saturating_add(1);
        self.refusal = Some(HoldRefusal {
            source,
            reason,
            key: key.as_u64(),
            observed_len: observed_len as u64,
        });
        tracing::warn!(
            source = source.tag(),
            reason = reason.tag(),
            "native held input refused at admission"
        );
    }

    fn admit(
        &mut self,
        key: Occurrence,
        source: HeldSource,
        body: HeldBody,
        meta: serde_json::Value,
    ) -> Result<(), (HoldRefusalReason, HeldBody)> {
        if self.has_key(key) {
            self.record_shared_owner_debt();
            return Ok(());
        }
        let held = self.entries.iter().filter(|entry| entry.source == source).count();
        let reason = if self.terminal {
            Some(HoldRefusalReason::Terminal)
        } else {
            match source {
                HeldSource::HumanInterjection | HeldSource::ParentSteer => None,
                HeldSource::MonitorEvent if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::MonitorBandFull),
                HeldSource::DeferredFollowUp if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::DeferredBandFull),
                HeldSource::StationarityReminder if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::StationarityBandFull),
                HeldSource::SkillReminder if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::SkillBandFull),
                HeldSource::PlanActivation if held >= 1 => Some(HoldRefusalReason::PlanSlot),
                HeldSource::PeerAgent if held >= 50 => Some(HoldRefusalReason::PeerBandFull),
                HeldSource::MediaResample if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::MediaResampleBandFull),
                HeldSource::ToolFreeLength if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::ToolFreeLengthBandFull),
                HeldSource::LengthReminder if held >= HELD_MACHINE_ENTRIES => Some(HoldRefusalReason::LengthReminderBandFull),
                HeldSource::MonitorEvent | HeldSource::DeferredFollowUp | HeldSource::StationarityReminder
                | HeldSource::SkillReminder | HeldSource::PlanActivation | HeldSource::PeerAgent
                | HeldSource::MediaResample | HeldSource::ToolFreeLength | HeldSource::LengthReminder => None,
            }
        };
        if let Some(reason) = reason {
            self.refusal(source, reason, key, held);
            return Err((reason, body));
        }
        let preparing = matches!(&body, HeldBody::Human(_))
            .then(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        self.entries.push_back(HeldEntry { key, source, body, meta, preparing });
        self.record_shared_owner_debt();
        Ok(())
    }

    fn record_shared_owner_debt(&mut self) {
        self.shared_owner_debt = SharedOwnerDebt {
            local_entries: self.entries.len(),
            global_owner_credited: false,
        };
    }

    pub(super) fn commit_keys(&mut self, submitted: &[Occurrence]) {
        self.entries.retain(|entry| !submitted.contains(&entry.key));
    }

    fn clear_latch(&mut self) {
        self.latch = None;
        self.latch_meta = None;
    }
}

pub(super) struct HeldParentView {
    pub(super) key: Option<Occurrence>,
    pub(super) message_id: String,
    pub(super) text: std::sync::Arc<str>,
    pub(super) payload: serde_json::Value,
}

impl HeldParentView {
    pub(super) fn release_key(&self) -> Result<Occurrence, HeldCommitError> {
        self.key.ok_or(HeldCommitError::MissingParentStamp)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HeldParentSelection {
    All,
    None,
}

pub(super) enum HeldCommitError {
    PersistenceUnavailable,
    ChatStateUnavailable,
    Terminal,
    Capacity,
    MissingParentStamp,
}

struct PreparedHeldHuman {
    key: Occurrence,
    wrapped: String,
    typed: String,
    images: Vec<acp::ImageContent>,
    item: ConversationItem,
}

enum HeldReleaseContent {
    Item(ConversationItem),
    Monitor,
}

struct HeldReleaseItem {
    key: Occurrence,
    source: HeldSource,
    content: HeldReleaseContent,
}

struct HeldNativeObservation {
    live: bool,
    unavailable: bool,
    chat_closed: bool,
}

impl SessionActor {
    async fn held_native_observation(&self) -> Option<HeldNativeObservation> {
        if super::switch_projection::select_native_history()
            != xai_grok_sampling_types::conversation::native_search_history::NativeHistoryRequirement::Selected {
            return None;
        }
        self.latched_native_observation().await
    }

    async fn latched_native_observation(&self) -> Option<HeldNativeObservation> {
        let read = self.chat_state_handle.read_native_history().await;
        let live = read.as_ref().is_ok_and(|read| {
            read.checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.ledger.groups.iter().any(|group| {
                    group.state == "LivePending"
                        && checkpoint.source.producing_bindings.iter().any(|origin| {
                            origin.origin_ref == group.origin_ref
                                && checkpoint
                                    .ledger
                                    .lifecycle
                                    .exchanges
                                    .get(&origin.request)
                                    .map(String::as_str)
                                    == Some("LiveBound")
                        })
                })
            })
        });
        let unavailable = read.as_ref().map_or(true, |read| read.checkpoint.is_none());
        let chat_closed =
            read.is_err() && self.chat_state_handle.get_sampling_config().await.is_none();
        Some(HeldNativeObservation {
            live,
            unavailable,
            chat_closed,
        })
    }

    fn held_matches(state: &State, observation: Option<&HeldNativeObservation>) -> bool {
        let ledger = &state.message_delivery.held_input;
        if ledger.terminal {
            return true;
        }
        let Some(task) = state.running_task.as_ref() else {
            return ledger.retaining();
        };
        let Some(observation) = observation else {
            return false;
        };
        ledger.latch.as_ref().is_some_and(|latch| {
            latch.binding
                == xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch)
        }) && (observation.live || (observation.unavailable && !observation.chat_closed))
    }

    pub(super) async fn native_inputs_held(&self) -> bool {
        let observation = self.held_native_observation().await;
        let state = self.state.lock().await;
        Self::held_matches(&state, observation.as_ref())
    }

    pub(super) async fn hold_or_push_user_item(
        &self,
        item: ConversationItem,
        source: HeldSource,
    ) -> Result<(), acp::Error> {
        let observation = self.held_native_observation().await;
        let mut state = self.state.lock().await;
        if !Self::held_matches(&state, observation.as_ref()) {
            drop(state);
            self.chat_state_handle.push_user_message(item);
            return Ok(());
        }
        let key = next_occurrence();
        let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"content_references":1});
        state.message_delivery.held_input
            .admit(key, source, HeldBody::Owned(item), meta)
            .map_err(|(reason, _)| {
                crate::sampling::error::local_error(
                    "native_held_input_incomplete",
                    format!("held {} refused: {}; accepted prefix retained", source.tag(), reason.tag()),
                )
            })
    }

    pub(super) async fn hold_inputs_at_native_safe_point(&self) -> bool {
        let observation = self.held_native_observation().await;
        let mut state = self.state.lock().await;
        let holding = Self::held_matches(&state, observation.as_ref());
        #[cfg(test)]
        super::native_messages_replay_tests::u6_record_native_safe_point(
            "held admission",
            observation.as_ref().map(|value| (value.live, value.unavailable, value.chat_closed)),
            holding,
            state.message_delivery.held_input.retaining(),
        );
        if !holding {
            return false;
        }
        if state.message_delivery.held_input.terminal {
            return true;
        }
        let Some(task) = state.running_task.as_ref() else {
            return true;
        };
        let binding = xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
        let binding_meta = HeldBindingMeta {
            prompt_id: task.prompt_id.clone(),
            epoch: format!("{:?}", task.epoch),
        };
        let parents = self.held_parent_views(&state, &binding, &binding_meta);
        for parent in parents {
            let Some(key) = parent.key else {
                continue;
            };
            let ledger = &mut state.message_delivery.held_input;
            if ledger.has_key(key) {
                continue;
            }
            let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"resident_reference":1});
            let _ = ledger.admit(key, HeldSource::ParentSteer, HeldBody::Parent(parent.message_id), meta);
        }
        let peer_origins = std::mem::take(&mut state.message_delivery.held_input.peer_origins);
        let mut residents = self.pending_interjections.drain_all_with_occurrences().into_iter();
        while let Some((key, resident)) = residents.next() {
            let source = if peer_origins.contains(&key) { HeldSource::PeerAgent } else { HeldSource::HumanInterjection };
            let body = HeldBody::Human(std::sync::Arc::new(resident));
            let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"raw_references":1});
            let ledger = &mut state.message_delivery.held_input;
            if let Err((_, HeldBody::Human(resident))) = ledger.admit(key, source, body, meta) {
                let resident = std::sync::Arc::try_unwrap(resident)
                    .unwrap_or_else(|_| unreachable!("refused raw resident has one owner"));
                let mut refused = vec![(key, resident)];
                refused.extend(residents);
                ledger.peer_origins.extend(refused.iter().map(|(key, _)| *key).filter(|key| peer_origins.contains(key)));
                self.pending_interjections.restore_front_with_occurrences(refused);
                break;
            }
        }
        if let Some(buffer) = self.tool_context.monitor_event_buffer.as_ref() {
            let ledger = &mut state.message_delivery.held_input;
            let owner = ledger.pin_owner.unwrap_or_else(next_occurrence);
            let pins = buffer.release_view(owner, |_| false).iter().map(|(key, _)| key).collect::<Vec<_>>();
            ledger.entries.retain(|entry| entry.source != HeldSource::MonitorEvent || pins.contains(&entry.key));
            let session_id = self.session_info.id.0.as_ref();
            buffer.pin_matching(owner, |event| event.owner_session_id.as_deref().is_none_or(|id| id == session_id), |key, event| {
                let meta = serde_json::json!({"owner_session_id":event.owner_session_id,"task_id":event.task_id,"pin_owner":owner.as_u64()});
                if ledger.admit(key, HeldSource::MonitorEvent, HeldBody::Monitor, meta).is_ok() {
                    ledger.pin_owner = Some(owner);
                    ledger.pinned_in = Some(buffer.clone());
                    true
                } else {
                    false
                }
            });
        }
        true
    }

    pub(super) async fn flush_skills_at_native_safe_point(&self) {
        let observation = self.held_native_observation().await;
        let mut state = self.state.lock().await;
        if Self::held_matches(&state, observation.as_ref()) {
            if state.message_delivery.held_input.terminal {
                return;
            }
            let mut reminders = self.pending_skill_reminders.lock();
            let mut accepted = 0;
            for item in reminders.iter() {
                let key = next_occurrence();
                let ledger = &mut state.message_delivery.held_input;
                let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"content_references":1});
                if ledger.admit(key, HeldSource::SkillReminder, HeldBody::Owned(item.clone()), meta).is_err() {
                    break;
                }
                accepted += 1;
            }
            reminders.drain(..accepted);
            let plan = self.plan_mode.lock();
            let ledger = &mut state.message_delivery.held_input;
            if plan.pending_activation_text().is_some() {
                if !ledger.entries.iter().any(|entry| matches!(entry.body, HeldBody::Plan)) {
                    let key = next_occurrence();
                    let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"resident_reference":1});
                    let _ = ledger.admit(key, HeldSource::PlanActivation, HeldBody::Plan, meta);
                }
            } else {
                let keys: Vec<_> = ledger.entries.iter()
                    .filter(|entry| matches!(entry.body, HeldBody::Plan)).map(|entry| entry.key).collect();
                ledger.commit_keys(&keys);
            }
            return;
        }
        let retaining = state.message_delivery.held_input.retaining();
        drop(state);
        if !retaining {
            self.flush_pending_skill_reminders().await;
        }
    }
}

impl SessionActor {
    pub(super) async fn arm_native_hold_latch(&self) {
        let mut state = self.state.lock().await;
        if state.message_delivery.held_input.terminal {
            return;
        }
        let Some(task) = state.running_task.as_ref() else {
            return;
        };
        let binding =
            xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
        let meta = HeldBindingMeta {
            prompt_id: task.prompt_id.clone(),
            epoch: format!("{:?}", task.epoch),
        };
        let ledger = &mut state.message_delivery.held_input;
        ledger.latch = Some(HoldLatch { binding });
        ledger.latch_meta = Some(meta);
        ledger.refresh_header();
    }
}

impl SessionActor {
    async fn release_native_held_inputs(&self) -> bool {
        let (binding, binding_meta, custody) = {
            let mut state = self.state.lock().await;
            if state.message_delivery.held_input.terminal {
                return false;
            }
            let Some(task) = state.running_task.as_ref() else {
                return false;
            };
            let binding =
                xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
            let binding_meta = HeldBindingMeta {
                prompt_id: task.prompt_id.clone(),
                epoch: format!("{:?}", task.epoch),
            };
            let Ok(custody) = state.message_delivery.held_input.begin_preparation() else {
                return false;
            };
            (binding, binding_meta, custody)
        };
        let (occurrences, entries): (Vec<_>, Vec<_>) = self
            .pending_interjections
            .drain_all_with_occurrences()
            .into_iter()
            .unzip();
        let mut guard = RestoreOnCancel {
            buffer: self.pending_interjections.clone(),
            entries,
            occurrences,
        };
        let mut prepared = Vec::new();
        for (key, raw) in custody
            .entries
            .iter()
            .map(|entry| (entry.key, entry.raw.as_ref()))
            .chain(guard.occurrences.iter().copied().zip(guard.entries.iter()))
        {
            let typed = crate::session::placeholder_images::strip_paths_from_image_placeholders(
                raw.text.clone(),
            );
            let skill = self.interjection_skill_information(&typed).await;
            let mut wrapped = format_interjection(typed.clone());
            let images = self
                .prepare_interjection_images(&mut wrapped, raw.attachments.clone())
                .await;
            let model_text =
                skill.map_or_else(|| wrapped.clone(), |skill| format!("{wrapped}\n{skill}"));
            let mut item = ConversationItem::interjection(model_text);
            for image in &images {
                item.add_image(pick_user_image_url(image));
            }
            prepared.push(PreparedHeldHuman {
                key,
                wrapped,
                typed,
                images,
                item,
            });
        }
        let model_id = self.current_model_id().await;
        self.flush_native_agent_messages().await;
        let (respond_to, persisted) = oneshot::channel();
        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::FlushAndAck { respond_to })
            .is_err()
            || !matches!(persisted.await, Ok(Ok(())))
        {
            tracing::warn!("native held release skipped: persistence barrier unavailable");
            return false;
        }
        let observation = self.held_native_observation().await;
        let user_chunk_meta = serde_json::json!({"modelId":model_id}).as_object().cloned();
        let notification_meta = self.build_notification_meta();
        let mut submitted = Vec::new();
        let mut delivered_skill = false;
        let mut delivered_plan = false;
        let mut missing_stamp_report = None;
        let committed = {
            let mut state = self.state.lock().await;
            let current = state.running_task.as_ref().map(|task| {
                xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch)
            });
            #[cfg(test)]
            super::native_messages_replay_tests::u6_record_commit_observation(
                observation
                    .as_ref()
                    .map(|value| (value.live, value.unavailable, value.chat_closed)),
                Self::held_matches(&state, observation.as_ref()),
                state.message_delivery.held_input.terminal || current.as_ref() != Some(&binding),
            );
            if state.message_delivery.held_input.terminal
                || current.as_ref() != Some(&binding)
                || Self::held_matches(&state, observation.as_ref())
            {
                return false;
            }
            let resident_view = self
                .pending_interjections
                .release_view(next_occurrence(), |_| true);
            let mut cutoff = resident_view.iter().map(|(key, _)| key).min();
            for entry in &state.message_delivery.held_input.entries {
                if matches!(entry.body, HeldBody::Human(_))
                    && !prepared.iter().any(|human| human.key == entry.key)
                {
                    cutoff = Some(cutoff.map_or(entry.key, |key| key.min(entry.key)));
                }
            }
            let expired_parent_identities = state.message_delivery.held_input.entries.iter()
                .filter_map(|entry| match &entry.body {
                    HeldBody::Parent(identity) if !self.parent_identity_is_live(&state, identity) => Some(identity.clone()),
                    _ => None,
                }).collect::<Vec<_>>();
            let mut parents = self.held_parent_views(&state, &binding, &binding_meta);
            parents.extend(self.held_projecting_parent_views(&mut state, &binding, &binding_meta));
            let missing_parent_stamps = parents.iter().filter(|parent| parent.key.is_none()).count();
            if missing_parent_stamps > 0 {
                let mut record = HeldInputHostRecord {
                    reason: "missing_parent_stamp",
                    bands: self.held_input_bands(&state),
                    held_human: Vec::new(),
                }.record();
                record["missing_parent_stamps"] = serde_json::json!(missing_parent_stamps);
                missing_stamp_report = Some(record);
            }
            let parent_selection = if parents.iter().any(|parent| {
                parent.key.is_none_or(|key| cutoff.is_some_and(|limit| key >= limit))
            }) {
                if let Some(first) = parents.iter().filter_map(|parent| parent.key).min() {
                    cutoff = Some(cutoff.map_or(first, |key| key.min(first)));
                }
                HeldParentSelection::None
            } else {
                HeldParentSelection::All
            };
            let eligible = |key: Occurrence| cutoff.is_none_or(|limit| key < limit);
            let owner = state
                .message_delivery
                .held_input
                .pin_owner
                .unwrap_or_else(next_occurrence);
            let buffer = self.tool_context.monitor_event_buffer.as_ref();
            let mut view = buffer.map(|buffer| {
                buffer.release_view(owner, |event| {
                    event
                        .owner_session_id
                        .as_deref()
                        .is_none_or(|id| id == self.session_info.id.0.as_ref())
                })
            });
            let mut plan = self.plan_mode.lock();
            let mut skills = self.pending_skill_reminders.lock();
            self.commit_held_parent_batch(&mut state, &binding, &binding_meta, parent_selection, |parents, ledger| {
                if ledger.terminal { return Err(HeldCommitError::Terminal); }
                let mut items = Vec::new();
                let mut expired = Vec::new();
                for human in &prepared {
                    items.push(HeldReleaseItem { key: human.key, source: HeldSource::HumanInterjection,
                        content: HeldReleaseContent::Item(human.item.clone()) });
                }
                for entry in &ledger.entries {
                    match &entry.body {
                        HeldBody::Human(_) => {}
                        HeldBody::Parent(identity) => {
                            if expired_parent_identities.contains(identity) { expired.push(entry.key); }
                        }
                        HeldBody::Owned(item) => items.push(HeldReleaseItem { key: entry.key, source: entry.source,
                            content: HeldReleaseContent::Item(item.clone()) }),
                        HeldBody::Monitor => {
                            if view.as_ref().is_some_and(|view| view.iter().any(|(key, _)| key == entry.key)) {
                                items.push(HeldReleaseItem { key: entry.key, source: HeldSource::MonitorEvent, content: HeldReleaseContent::Monitor });
                            } else { expired.push(entry.key); }
                        }
                        HeldBody::Plan => {
                            if let Some(text) = plan.pending_activation_text() {
                                items.push(HeldReleaseItem { key: entry.key, source: HeldSource::PlanActivation,
                                    content: HeldReleaseContent::Item(ConversationItem::system_reminder(text.to_owned())) });
                            } else { expired.push(entry.key); }
                        }
                    }
                }
                for parent in parents {
                    items.push(HeldReleaseItem { key: parent.release_key()?, source: HeldSource::ParentSteer,
                        content: HeldReleaseContent::Item(ConversationItem::agent_message(parent.text.to_string())) });
                }
                if let Some(view) = &view {
                    for (key, _) in view.iter() {
                        if !items.iter().any(|item| item.key == key) {
                            items.push(HeldReleaseItem { key, source: HeldSource::MonitorEvent, content: HeldReleaseContent::Monitor });
                        }
                    }
                }
                if !items.iter().any(|item| item.source == HeldSource::PlanActivation)
                    && let Some(text) = plan.pending_activation_text() {
                    items.push(HeldReleaseItem { key: next_occurrence(), source: HeldSource::PlanActivation,
                        content: HeldReleaseContent::Item(ConversationItem::system_reminder(text.to_owned())) });
                }
                let resident_skill_keys: Vec<_> = skills.iter().map(|item| {
                    let key = next_occurrence();
                    items.push(HeldReleaseItem { key, source: HeldSource::SkillReminder, content: HeldReleaseContent::Item(item.clone()) });
                    key
                }).collect();
                items.retain(|item| eligible(item.key));
                items.sort_by_key(|item| item.key);
                let mut batch = Vec::new();
                let mut index = 0;
                while index < items.len() {
                    match &items[index].content {
                        HeldReleaseContent::Item(item) => { batch.push(item.clone()); submitted.push(items[index].key); index += 1; }
                        HeldReleaseContent::Monitor => {
                            let mut monitors = Vec::new();
                            while index < items.len() && matches!(items[index].content, HeldReleaseContent::Monitor) {
                                let key = items[index].key;
                                if let Some(event) = view.as_ref().and_then(|view| view.iter().find_map(|(candidate, event)| (candidate == key).then_some(event))) {
                                    monitors.push(event.clone()); submitted.push(key);
                                }
                                index += 1;
                            }
                            if let Some(text) = xai_grok_tools::reminders::task_completion::format_monitor_events(&monitors, Some(&self.tool_context.task_output_tool_name)) {
                                batch.push(ConversationItem::system_reminder(xai_grok_tools::reminders::wrap_reminder(&text)));
                            }
                        }
                    }
                }
                if batch.is_empty() {
                    ledger.commit_keys(&expired);
                    if cutoff.is_none() && missing_parent_stamps == 0 { ledger.clear_latch(); }
                    return Ok(());
                }
                Self::persist_held_parent_prefix(parents, ledger, |parent| {
                    let update = acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                        acp::ContentBlock::Text(acp::TextContent::new(parent.text.to_string()))).meta(user_chunk_meta.clone()));
                    self.notifications.persistence_tx.send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                        acp::SessionNotification::new(self.session_info.id.clone(), update).meta(notification_meta.clone().as_object().cloned())))))
                        .map_err(|_| HeldCommitError::PersistenceUnavailable)
                })?;
                self.chat_state_handle.try_push_user_messages_batch(batch).map_err(|_| HeldCommitError::ChatStateUnavailable)?;
                if let Some(view) = &mut view { view.commit_occurrences(&submitted); }
                delivered_plan = items.iter().any(|item| item.source == HeldSource::PlanActivation && submitted.contains(&item.key));
                if delivered_plan { plan.take_pending_activation(); }
                delivered_skill = items.iter().any(|item| item.source == HeldSource::SkillReminder && submitted.contains(&item.key));
                let skill_count = resident_skill_keys.iter().filter(|key| submitted.contains(key)).count();
                skills.drain(..skill_count);
                let mut removed = submitted.clone(); removed.extend(expired);
                ledger.commit_keys(&removed);
                if cutoff.is_none() && missing_parent_stamps == 0 { ledger.clear_latch(); }
                Ok(())
            })
        };
        #[cfg(test)]
        super::native_messages_replay_tests::u6_record_release_outcome(committed.is_ok());
        if let Some(record) = missing_stamp_report {
            self.send_hook_annotation(&record.to_string()).await;
        }
        if committed.is_err() {
            tracing::warn!(
                "native held release incomplete: downstream commit unavailable; custody retained"
            );
            return false;
        }
        guard.defuse_occurrences(&submitted);
        drop(guard);
        let persisted_humans = prepared.iter().any(|human| submitted.contains(&human.key));
        for human in prepared {
            if submitted.contains(&human.key) {
                self.persist_synthetic_user_message_with_model(
                    &human.wrapped,
                    Some(&human.typed),
                    &human.images,
                    &model_id,
                );
            }
        }
        if persisted_humans {
            // Fence the queued chat batch before flushing its persisted updates.
            let accepted = self.chat_state_handle.push_user_messages_batch_and_ack(Vec::new()).await;
            let (respond_to, persisted) = oneshot::channel();
            if accepted.is_none()
                || self.notifications.persistence_tx.send(PersistenceMsg::FlushAndAck { respond_to }).is_err()
                || !matches!(persisted.await, Ok(Ok(())))
            {
                tracing::warn!("native held user updates could not be flushed after submission");
            }
            if self.state.lock().await.message_delivery.held_input.terminal {
                return false;
            }
        }
        if delivered_plan {
            self.plan_mode.lock().record_reminder_injected();
            self.persist_plan_mode_state();
        }
        if delivered_skill {
            self.persist_announcement_state().await;
        }
        !submitted.is_empty()
    }
}

struct HeldPreparationEntry {
    key: Occurrence,
    raw: std::sync::Arc<PendingInterjection>,
    preparing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct HeldPreparation {
    entries: Vec<HeldPreparationEntry>,
}

impl Drop for HeldPreparation {
    fn drop(&mut self) {
        for entry in &self.entries {
            entry.preparing.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

impl HeldInputLedger {
    fn begin_preparation(&mut self) -> Result<HeldPreparation, ()> {
        let mut preparation = HeldPreparation::default();
        for entry in &self.entries {
            let HeldBody::Human(raw) = &entry.body else {
                continue;
            };
            let Some(preparing) = &entry.preparing else {
                return Err(());
            };
            // A raw entry has one preparation owner, independently of its size.
            if preparing.swap(true, std::sync::atomic::Ordering::AcqRel) {
                return Err(());
            }
            preparation.entries.push(HeldPreparationEntry {
                key: entry.key,
                raw: raw.clone(),
                preparing: preparing.clone(),
            });
        }
        Ok(preparation)
    }
}

#[derive(serde::Serialize)]
struct HeldHumanAtClose {
    key: u64,
    text: String,
    attachment_count: usize,
}

struct HeldInputHostRecord {
    reason: &'static str,
    bands: HeldBands,
    held_human: Vec<HeldHumanAtClose>,
}

impl HeldInputHostRecord {
    fn record(&self) -> serde_json::Value {
        let mut record = serde_json::json!({
            "kind": "held_input_host_record",
            "reason": self.reason,
            "bands": {
                "human_interjection": self.bands.human.held,
                "parent_steer": self.bands.steer.held,
                "monitor_event": self.bands.monitor.held,
                "deferred_followup": self.bands.deferred.held,
                "stationarity_reminder": self.bands.stationarity.held,
                "skill_reminder": self.bands.skill.held,
                "plan_activation": self.bands.plan.held,
                "peer_agent": self.bands.peer.held,
                "media_resample": self.bands.media_resample.held,
                "tool_free_length": self.bands.tool_free_length.held,
                "length_reminder": self.bands.length_reminder.held,
            },
        });
        if !self.held_human.is_empty() {
            record["held_human"] = serde_json::json!(self.held_human);
        }
        record
    }
}

impl SessionActor {
    async fn retain_stranded_interjections_for_next_turn(&self) -> bool {
        let retaining = self.state.lock().await.message_delivery.held_input.retaining();
        if !retaining {
            return false;
        }
        // Custody outlives the turn's native-selection context.
        let read = self.chat_state_handle.read_native_history().await;
        let unavailable = read.as_ref().map_or(true, |read| read.checkpoint.is_none());
        let chat_closed = read.is_err()
            && self.chat_state_handle.get_sampling_config().await.is_none();
        let report = {
            let mut state = self.state.lock().await;
            if !state.message_delivery.held_input.retaining() {
                return false;
            }
            if state.message_delivery.held_input.terminal {
                return true;
            }
            let peer_origins = std::mem::take(&mut state.message_delivery.held_input.peer_origins);
            let mut refused = Vec::new();
            for (key, resident) in self.pending_interjections.drain_all_with_occurrences() {
                let source = if peer_origins.contains(&key) { HeldSource::PeerAgent } else { HeldSource::HumanInterjection };
                let meta = serde_json::json!({
                    "owner_session_id": self.session_info.id.0.as_ref(),
                    "resident_reference": 1,
                });
                if let Err((_, HeldBody::Human(resident))) = state.message_delivery.held_input.admit(
                    key, source, HeldBody::Human(std::sync::Arc::new(resident)), meta,
                ) {
                    let resident = std::sync::Arc::try_unwrap(resident)
                        .unwrap_or_else(|_| unreachable!("refused raw resident has one owner"));
                    refused.push((key, resident));
                }
            }
            state.message_delivery.held_input.peer_origins.extend(refused.iter().map(|(key, _)| *key).filter(|key| peer_origins.contains(key)));
            self.pending_interjections.restore_front_with_occurrences(refused);
            if chat_closed {
                self.settle_held_inputs_terminal(&mut state)
            } else {
                unavailable.then(|| {
                    let record = HeldInputHostRecord {
                        reason: "retained_view_unavailable",
                        bands: self.held_input_bands(&state),
                        held_human: Vec::new(),
                    };
                    tracing::warn!(reason = record.reason, bands = ?record.bands, "Held input retained at turn end");
                    record.record()
                })
            }
        };
        if let Some(record) = report {
            self.send_hook_annotation(&record.to_string()).await;
        }
        true
    }
}

impl SessionActor {
    pub(super) fn settle_held_inputs_terminal(&self, state: &mut State) -> Option<serde_json::Value> {
        let bands = self.held_input_bands(state);
        let ledger = &mut state.message_delivery.held_input;
        if ledger.terminal {
            return None;
        }
        ledger.terminal = true;
        let record = ledger.retaining().then(|| {
            let mut held_human = ledger.entries.iter().filter_map(|entry| {
                if entry.source != HeldSource::HumanInterjection { return None; }
                let HeldBody::Human(human) = &entry.body else {
                    return None;
                };
                Some(HeldHumanAtClose {
                    key: entry.key.as_u64(),
                    text: human.text.clone(),
                    attachment_count: human.attachments.len(),
                })
            }).collect::<Vec<_>>();
            held_human.sort_by_key(|human| human.key);
            HeldInputHostRecord {
                reason: "settled_at_chat_close",
                bands,
                held_human,
            }
        });
        if let (Some(owner), Some(buffer)) = (ledger.pin_owner, ledger.pinned_in.as_ref()) {
            buffer.unpin_all(owner);
        }
        ledger.entries.clear();
        ledger.peer_origins.clear();
        ledger.parent_stamps.clear();
        ledger.clear_parent_delivery();
        ledger.clear_latch();
        ledger.pin_owner = None;
        ledger.pinned_in = None;
        record.map(|record| {
            tracing::warn!(reason = record.reason, bands = ?record.bands, "Held input settled at logical end");
            record.record()
        })
    }
}

impl HeldInputLedger {
    fn peer_count(&self, native_residents: usize) -> usize {
        self.entries.iter().filter(|entry| entry.source == HeldSource::PeerAgent).count()
            + self.peer_origins.len() + native_residents
    }

    fn peer_band_available(&mut self, native_residents: usize) -> bool {
        let count = self.peer_count(native_residents);
        let reason = if self.terminal {
            Some(HoldRefusalReason::Terminal)
        } else if count >= 50 {
            Some(HoldRefusalReason::PeerBandFull)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.refusal(HeldSource::PeerAgent, reason, next_occurrence(), count);
            return false;
        }
        true
    }
}

impl SessionActor {
    pub(super) async fn queue_active_peer_text(&self, text: String) -> bool {
        let observation = self.latched_native_observation().await;
        let mut state = self.state.lock().await;
        let holding = Self::held_matches(&state, observation.as_ref());
        let entry = PendingInterjection { text, attachments: Vec::new() };
        if holding {
            let native_residents = self.pending_native_agent_messages.lock().map_or(0, |items| items.len());
            let ledger = &mut state.message_delivery.held_input;
            if !ledger.peer_band_available(native_residents) {
                return false;
            }
            let key = self.pending_interjections.push_with_occurrence(entry);
            ledger.peer_origins.insert(key);
        } else {
            self.pending_interjections.push(entry);
        }
        true
    }

    pub(super) async fn queue_active_native_peer(&self, item: ConversationItem) {
        let observation = self.latched_native_observation().await;
        let mut state = self.state.lock().await;
        let holding = Self::held_matches(&state, observation.as_ref());
        if let Ok(mut buffered) = self.pending_native_agent_messages.lock()
            && (!holding || state.message_delivery.held_input.peer_band_available(buffered.len()))
        {
            buffered.push(item);
        }
    }

    fn held_input_bands(&self, state: &State) -> HeldBands {
        let mut bands = state.message_delivery.held_input.bands(self.live_parent_steer_slots(state));
        bands.peer.held += self.pending_native_agent_messages.lock().map_or(0, |items| items.len());
        bands
    }
}
