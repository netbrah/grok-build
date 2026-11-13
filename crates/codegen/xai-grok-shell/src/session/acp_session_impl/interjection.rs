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
const HELD_INPUT_ENTRIES: usize = 128;
const HELD_INPUT_BYTES: u32 = 262_144;
const HELD_ENTRY_BYTES: u32 = 32_768;
const HELD_PARENT_STAMPS: usize = 32;
const HELD_STAMP_ID_JSON_BYTES: usize = 128;

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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldRefusalReason {
    Records,
    EntryBytes,
    TotalBytes,
    Accounting,
    Terminal,
}

impl HoldRefusalReason {
    fn tag(self) -> &'static str {
        match self {
            Self::Records => "records",
            Self::EntryBytes => "entry_bytes",
            Self::TotalBytes => "total_bytes",
            Self::Accounting => "accounting",
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
    pub(super) charged: u32,
    pub(super) prepared_charge: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HeldOccupancy {
    pub(super) records: usize,
    pub(super) header_bytes: u32,
    pub(super) entry_bytes: u32,
    pub(super) reserve_bytes: u32,
}

pub(super) struct HeldInputLedger {
    pub(super) latch: Option<HoldLatch>,
    pub(super) latch_meta: Option<HeldBindingMeta>,
    pub(super) entries: std::collections::VecDeque<HeldEntry>,
    pub(super) parent_stamps: Vec<(String, Occurrence)>,
    pub(super) parent_owner: Option<HeldTurnBinding>,
    pub(super) parent_owner_meta: Option<HeldBindingMeta>,
    pub(super) parent_persisted: u32,
    pub(super) pin_owner: Option<Occurrence>,
    pub(super) pinned_in:
        Option<xai_grok_tools::implementations::grok_build::monitor::types::MonitorEventBuffer>,
    pub(super) terminal: bool,
    pub(super) refusal: Option<HoldRefusal>,
    header_bytes: u32,
    entry_bytes: u32,
    reserve_bytes: u32,
}

impl Default for HeldInputLedger {
    fn default() -> Self {
        let mut ledger = Self {
            latch: None,
            latch_meta: None,
            entries: Default::default(),
            parent_stamps: Vec::new(),
            parent_owner: None,
            parent_owner_meta: None,
            parent_persisted: 0,
            pin_owner: None,
            pinned_in: None,
            terminal: false,
            refusal: None,
            header_bytes: 0,
            entry_bytes: 0,
            reserve_bytes: held_refusal_reserve(),
        };
        ledger.refresh_header();
        ledger
    }
}

impl Drop for HeldInputLedger {
    fn drop(&mut self) {
        if let (Some(owner), Some(buffer)) = (self.pin_owner, self.pinned_in.as_ref()) {
            buffer.unpin_all(owner);
        }
    }
}

fn held_compact_bytes(value: &serde_json::Value) -> u32 {
    u32::try_from(value.to_string().len()).unwrap_or(u32::MAX)
}

fn held_refusal_reserve() -> u32 {
    let sources = [
        HeldSource::HumanInterjection,
        HeldSource::ParentSteer,
        HeldSource::MonitorEvent,
        HeldSource::DeferredFollowUp,
        HeldSource::StationarityReminder,
        HeldSource::SkillReminder,
        HeldSource::PlanActivation,
    ];
    let reasons = [
        HoldRefusalReason::Records,
        HoldRefusalReason::EntryBytes,
        HoldRefusalReason::TotalBytes,
        HoldRefusalReason::Accounting,
        HoldRefusalReason::Terminal,
    ];
    sources
        .into_iter()
        .flat_map(|source| {
            reasons.into_iter().map(move |reason| {
                held_compact_bytes(
                    &HoldRefusal {
                        source,
                        reason,
                        key: u64::MAX,
                        observed_len: u64::MAX,
                    }
                    .record(),
                )
            })
        })
        .max()
        .unwrap_or(0)
}

fn held_binding_record(meta: Option<&HeldBindingMeta>) -> serde_json::Value {
    meta.map_or(
        serde_json::Value::Null,
        |meta| serde_json::json!({"prompt_id":meta.prompt_id,"epoch":meta.epoch}),
    )
}

fn held_stamp_reservation() -> u32 {
    let slot =
        serde_json::json!({"message_id":"x".repeat(HELD_STAMP_ID_JSON_BYTES - 2),"key":u64::MAX});
    held_compact_bytes(&slot) * HELD_PARENT_STAMPS as u32
        + HELD_PARENT_STAMPS.saturating_sub(1) as u32
}

fn held_reference(body: &HeldBody, key: Occurrence) -> serde_json::Value {
    match body {
        HeldBody::Human(_) | HeldBody::Owned(_) => serde_json::Value::Null,
        HeldBody::Parent(message_id) => serde_json::json!({"message_id":message_id}),
        HeldBody::Monitor => serde_json::json!({"occurrence":key.as_u64()}),
        HeldBody::Plan => serde_json::json!({"activation_slot":1}),
    }
}

fn held_entry_charge(
    key: Occurrence,
    source: HeldSource,
    body: &HeldBody,
    meta: &serde_json::Value,
    payload: &serde_json::Value,
) -> Result<u32, HoldRefusalReason> {
    let mut charged = 0;
    for _ in 0..12 {
        let value = serde_json::json!({"key":key.as_u64(),"source":source.tag(),"ref":held_reference(body, key),
            "meta":meta,"payload":payload,"preparation_control":if matches!(body, HeldBody::Human(_)) {
                serde_json::json!({"key":key.as_u64(),"maximum_charge":u32::MAX})
            } else { serde_json::Value::Null },"charged":charged});
        let next = held_compact_bytes(&value);
        if next == charged {
            return Ok(charged);
        }
        if next == u32::MAX {
            return Err(HoldRefusalReason::Accounting);
        }
        charged = next;
    }
    Err(HoldRefusalReason::Accounting)
}

impl HeldInputLedger {
    pub(super) fn occupancy(&self) -> HeldOccupancy {
        HeldOccupancy {
            records: self.entries.len(),
            header_bytes: self.header_bytes,
            entry_bytes: self.entry_bytes.saturating_add(
                self.entries
                    .iter()
                    .filter_map(|entry| entry.prepared_charge.as_ref())
                    .fold(0u32, |sum, charge| {
                        sum.saturating_add(charge.load(std::sync::atomic::Ordering::SeqCst))
                    }),
            ),
            reserve_bytes: self.reserve_bytes,
        }
    }

    fn header_charge(&self, pin_owner: Option<Occurrence>, pinned: bool) -> u32 {
        self.header_charge_with_delivery(
            pin_owner,
            pinned,
            self.parent_owner_meta.as_ref(),
            self.parent_persisted,
        )
    }

    fn header_charge_with_delivery(
        &self,
        pin_owner: Option<Occurrence>,
        pinned: bool,
        owner: Option<&HeldBindingMeta>,
        persisted: u32,
    ) -> u32 {
        held_compact_bytes(&serde_json::json!({
            "latch":held_binding_record(self.latch_meta.as_ref()),
            "latch_metadata":held_binding_record(self.latch_meta.as_ref()),
            "delivery_owner":held_binding_record(owner),
            "delivery_owner_metadata":held_binding_record(owner),
            "parent_persisted":persisted,
            "pin_owner":pin_owner.map(Occurrence::as_u64),
            "pin_handle":if pinned { serde_json::json!("monitor_event_buffer") } else { serde_json::Value::Null },
            "terminal":u8::from(self.terminal),"parent_stamps":[],
        })).saturating_add(held_stamp_reservation())
    }

    pub(super) fn refresh_header(&mut self) {
        self.header_bytes = self.header_charge(self.pin_owner, self.pinned_in.is_some());
    }

    fn check_parent_header(&mut self, header_bytes: u32) -> Result<(), HeldCommitError> {
        let occupancy = self.occupancy();
        if u64::from(header_bytes)
            + u64::from(occupancy.entry_bytes)
            + u64::from(occupancy.reserve_bytes)
            > u64::from(HELD_INPUT_BYTES)
        {
            self.refusal(
                HeldSource::ParentSteer,
                HoldRefusalReason::TotalBytes,
                next_occurrence(),
                header_bytes,
            );
            return Err(HeldCommitError::Capacity);
        }
        Ok(())
    }

    pub(super) fn bind_parent_delivery(
        &mut self,
        binding: &HeldTurnBinding,
        meta: &HeldBindingMeta,
    ) -> Result<(), HeldCommitError> {
        if self.parent_owner.as_ref() != Some(binding) {
            let header = self.header_charge_with_delivery(
                self.pin_owner,
                self.pinned_in.is_some(),
                Some(meta),
                0,
            );
            self.check_parent_header(header)?;
            self.parent_owner = Some(binding.clone());
            self.parent_owner_meta = Some(meta.clone());
            self.parent_persisted = 0;
            self.header_bytes = header;
        }
        Ok(())
    }

    pub(super) fn check_parent_progress(&mut self, persisted: u32) -> Result<(), HeldCommitError> {
        let header = self.header_charge_with_delivery(
            self.pin_owner,
            self.pinned_in.is_some(),
            self.parent_owner_meta.as_ref(),
            persisted,
        );
        self.check_parent_header(header)
    }

    pub(super) fn clear_parent_delivery(&mut self) {
        self.parent_owner = None;
        self.parent_owner_meta = None;
        self.parent_persisted = 0;
        self.refresh_header();
    }

    pub(super) fn retaining(&self) -> bool {
        self.latch.is_some() || !self.entries.is_empty()
    }

    pub(super) fn stamp_parent(&mut self, message_id: &str) {
        if self.terminal
            || self.parent_stamps.len() >= HELD_PARENT_STAMPS
            || self.parent_stamps.iter().any(|(id, _)| id == message_id)
            || serde_json::json!(message_id).to_string().len() > HELD_STAMP_ID_JSON_BYTES
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

    fn refusal(
        &mut self,
        source: HeldSource,
        reason: HoldRefusalReason,
        key: Occurrence,
        observed_len: u32,
    ) {
        self.refusal = Some(HoldRefusal {
            source,
            reason,
            key: key.as_u64(),
            observed_len: u64::from(observed_len),
        });
        tracing::warn!(
            source = source.tag(),
            reason = reason.tag(),
            "native held input refused before growth"
        );
    }

    fn admit(
        &mut self,
        key: Occurrence,
        source: HeldSource,
        body: HeldBody,
        meta: serde_json::Value,
        payload: serde_json::Value,
        proposed_header: u32,
    ) -> Result<(), (HoldRefusalReason, HeldBody)> {
        if self.has_key(key) {
            return Ok(());
        }
        let charged = match held_entry_charge(key, source, &body, &meta, &payload) {
            Ok(charged) => charged,
            Err(reason) => {
                self.refusal(source, reason, key, u32::MAX);
                return Err((reason, body));
            }
        };
        let occupancy = self.occupancy();
        let reason = if self.terminal {
            Some(HoldRefusalReason::Terminal)
        } else if occupancy.records + 1 > HELD_INPUT_ENTRIES {
            Some(HoldRefusalReason::Records)
        } else if charged > HELD_ENTRY_BYTES {
            Some(HoldRefusalReason::EntryBytes)
        } else if u64::from(proposed_header)
            + u64::from(occupancy.entry_bytes)
            + u64::from(charged)
            + u64::from(occupancy.reserve_bytes)
            > u64::from(HELD_INPUT_BYTES)
        {
            Some(HoldRefusalReason::TotalBytes)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.refusal(source, reason, key, charged);
            return Err((reason, body));
        }
        self.header_bytes = proposed_header;
        self.entry_bytes += charged;
        let prepared_charge = matches!(&body, HeldBody::Human(_))
            .then(|| std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)));
        self.entries.push_back(HeldEntry {
            key,
            source,
            body,
            meta,
            charged,
            prepared_charge,
        });
        Ok(())
    }

    pub(super) fn commit_keys(&mut self, submitted: &[Occurrence]) {
        self.entries.retain(|entry| !submitted.contains(&entry.key));
        self.entry_bytes = self.entries.iter().map(|entry| entry.charged).sum();
    }

    fn clear_latch(&mut self) {
        self.latch = None;
        self.latch_meta = None;
        self.refresh_header();
    }
}

pub(super) struct HeldParentView {
    pub(super) key: Option<Occurrence>,
    pub(super) message_id: String,
    pub(super) text: std::sync::Arc<str>,
    pub(super) payload: serde_json::Value,
}

impl HeldParentView {
    pub(super) fn release_key(&self) -> Occurrence {
        self.key.unwrap_or_else(next_occurrence)
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
        let payload = serde_json::to_value(&item).map_err(|_| {
            crate::sampling::error::local_error(
                "native_held_input_incomplete",
                "held user input accounting failed",
            )
        })?;
        let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"content_references":1});
        let ledger = &mut state.message_delivery.held_input;
        let header = ledger.header_bytes;
        ledger
            .admit(key, source, HeldBody::Owned(item), meta, payload, header)
            .map_err(|(reason, _)| {
                crate::sampling::error::local_error(
                    "native_held_input_incomplete",
                    format!(
                        "held {} refused: {}; accepted prefix retained",
                        source.tag(),
                        reason.tag()
                    ),
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
            observation
                .as_ref()
                .map(|value| (value.live, value.unavailable, value.chat_closed)),
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
        let binding =
            xai_message_delivery_core::TurnBinding::new(task.prompt_id.clone(), task.epoch);
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
            let _ = ledger.admit(
                key,
                HeldSource::ParentSteer,
                HeldBody::Parent(parent.message_id),
                meta,
                parent.payload,
                ledger.header_bytes,
            );
        }
        let mut residents = self
            .pending_interjections
            .drain_all_with_occurrences()
            .into_iter();
        while let Some((key, resident)) = residents.next() {
            let payload =
                serde_json::json!({"text":resident.text,"attachments":resident.attachments});
            let body = HeldBody::Human(std::sync::Arc::new(resident));
            let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"raw_references":1});
            let ledger = &mut state.message_delivery.held_input;
            if let Err((_, HeldBody::Human(resident))) = ledger.admit(
                key,
                HeldSource::HumanInterjection,
                body,
                meta,
                payload,
                ledger.header_bytes,
            ) {
                let resident = std::sync::Arc::try_unwrap(resident)
                    .unwrap_or_else(|_| unreachable!("refused raw resident has one owner"));
                let mut refused = vec![(key, resident)];
                refused.extend(residents);
                self.pending_interjections
                    .restore_front_with_occurrences(refused);
                break;
            }
        }
        if let Some(buffer) = self.tool_context.monitor_event_buffer.as_ref() {
            let ledger = &mut state.message_delivery.held_input;
            let owner = ledger.pin_owner.unwrap_or_else(next_occurrence);
            let header = ledger.header_charge(Some(owner), true);
            let session_id = self.session_info.id.0.as_ref();
            buffer.pin_matching(owner, |event| event.owner_session_id.as_deref().is_none_or(|id| id == session_id), |key, event| {
                let meta = serde_json::json!({"owner_session_id":event.owner_session_id,"task_id":event.task_id,"pin_owner":owner.as_u64()});
                let payload = serde_json::json!({"owner_session_id":event.owner_session_id,"task_id":event.task_id,"event_text":event.event_text,"slot_pin":owner.as_u64()});
                if ledger.admit(key, HeldSource::MonitorEvent, HeldBody::Monitor, meta, payload, header).is_ok() {
                    ledger.pin_owner = Some(owner);
                    ledger.pinned_in = Some(buffer.clone());
                    true
                } else { false }
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
                let Ok(payload) = serde_json::to_value(item) else {
                    break;
                };
                let ledger = &mut state.message_delivery.held_input;
                let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"content_references":1});
                if ledger
                    .admit(
                        key,
                        HeldSource::SkillReminder,
                        HeldBody::Owned(item.clone()),
                        meta,
                        payload,
                        ledger.header_bytes,
                    )
                    .is_err()
                {
                    break;
                }
                accepted += 1;
            }
            reminders.drain(..accepted);
            let plan = self.plan_mode.lock();
            let ledger = &mut state.message_delivery.held_input;
            if let Some(text) = plan.pending_activation_text() {
                if !ledger
                    .entries
                    .iter()
                    .any(|entry| matches!(entry.body, HeldBody::Plan))
                {
                    let key = next_occurrence();
                    let payload = serde_json::json!({"text":text,"prior_state_bits":1});
                    let meta = serde_json::json!({"owner_session_id":self.session_info.id.0.as_ref(),"resident_reference":1});
                    let _ = ledger.admit(
                        key,
                        HeldSource::PlanActivation,
                        HeldBody::Plan,
                        meta,
                        payload,
                        ledger.header_bytes,
                    );
                }
            } else {
                let keys: Vec<_> = ledger
                    .entries
                    .iter()
                    .filter(|entry| matches!(entry.body, HeldBody::Plan))
                    .map(|entry| entry.key)
                    .collect();
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
            let owned = custody.entries.iter().any(|entry| entry.key == key);
            if owned
                && !self
                    .price_held_preparation(
                        key,
                        serde_json::json!({"key":key.as_u64(),"typed":typed}),
                    )
                    .await
            {
                return false;
            }
            let skill = self.interjection_skill_information(&typed).await;
            let mut wrapped = format_interjection(typed.clone());
            if owned
                && !self
                    .price_held_preparation(
                        key,
                        serde_json::json!({"key":key.as_u64(),"typed":typed,
                "wrapped":wrapped,"skill":skill,"input_images":raw.attachments}),
                    )
                    .await
            {
                return false;
            }
            let images = self
                .prepare_interjection_images(&mut wrapped, raw.attachments.clone())
                .await;
            let model_text =
                skill.map_or_else(|| wrapped.clone(), |skill| format!("{wrapped}\n{skill}"));
            let mut item = ConversationItem::interjection(model_text);
            for image in &images {
                item.add_image(pick_user_image_url(image));
            }
            if owned
                && !self
                    .price_held_preparation(
                        key,
                        serde_json::json!({"key":key.as_u64(),"wrapped":wrapped,
                "typed":typed,"images":images,"item":item}),
                    )
                    .await
            {
                return false;
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
            let pending_parents = self.held_parent_views(&state, &binding, &binding_meta);
            let parent_selection = if cutoff.is_some_and(|limit| {
                pending_parents
                    .iter()
                    .any(|parent| parent.key.is_some_and(|key| key >= limit))
            }) {
                if let Some(first) = pending_parents.iter().filter_map(|parent| parent.key).min() {
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
                        HeldBody::Human(_) | HeldBody::Parent(_) => {}
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
                    items.push(HeldReleaseItem { key: parent.release_key(), source: HeldSource::ParentSteer,
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
                    if cutoff.is_none() { ledger.clear_latch(); }
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
                if cutoff.is_none() { ledger.clear_latch(); }
                Ok(())
            })
        };
        #[cfg(test)]
        super::native_messages_replay_tests::u6_record_release_outcome(committed.is_ok());
        if committed.is_err() {
            tracing::warn!(
                "native held release incomplete: downstream commit unavailable; custody retained"
            );
            return false;
        }
        guard.defuse_occurrences(&submitted);
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
    charge: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

#[derive(Default)]
struct HeldPreparation {
    entries: Vec<HeldPreparationEntry>,
}

impl Drop for HeldPreparation {
    fn drop(&mut self) {
        for entry in &self.entries {
            entry.charge.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

fn held_preparation_charge(key: Occurrence, payload: &serde_json::Value) -> u32 {
    let mut charged = 0;
    for _ in 0..12 {
        let record = serde_json::json!({"key":key.as_u64(),"raw_reference":key.as_u64(),
            "charge_reference":key.as_u64(),"prepared":payload,"charged":charged});
        let next = held_compact_bytes(&record);
        if next == charged {
            return charged;
        }
        if next == u32::MAX {
            return u32::MAX;
        }
        charged = next;
    }
    u32::MAX
}

impl HeldInputLedger {
    fn begin_preparation(&mut self) -> Result<HeldPreparation, ()> {
        let mut preparation = HeldPreparation::default();
        for index in 0..self.entries.len() {
            let entry = &self.entries[index];
            let HeldBody::Human(raw) = &entry.body else {
                continue;
            };
            let key = entry.key;
            let Some(charge) = &entry.prepared_charge else {
                return Err(());
            };
            if charge.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                return Err(());
            }
            let bytes = held_preparation_charge(key, &serde_json::Value::Null);
            let occupancy = self.occupancy();
            if u64::from(occupancy.header_bytes)
                + u64::from(occupancy.entry_bytes)
                + u64::from(bytes)
                + u64::from(occupancy.reserve_bytes)
                > u64::from(HELD_INPUT_BYTES)
            {
                self.refusal(
                    HeldSource::HumanInterjection,
                    HoldRefusalReason::TotalBytes,
                    key,
                    bytes,
                );
                return Err(());
            }
            charge.store(bytes, std::sync::atomic::Ordering::SeqCst);
            preparation.entries.push(HeldPreparationEntry {
                key,
                raw: raw.clone(),
                charge: charge.clone(),
            });
        }
        Ok(preparation)
    }
}

impl SessionActor {
    async fn price_held_preparation(&self, key: Occurrence, payload: serde_json::Value) -> bool {
        let bytes = held_preparation_charge(key, &payload);
        let mut state = self.state.lock().await;
        let ledger = &mut state.message_delivery.held_input;
        let Some(charge) = ledger
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.prepared_charge.as_ref())
        else {
            return false;
        };
        let previous = charge.load(std::sync::atomic::Ordering::SeqCst);
        let occupancy = ledger.occupancy();
        if u64::from(occupancy.header_bytes) + u64::from(occupancy.entry_bytes)
            - u64::from(previous)
            + u64::from(bytes)
            + u64::from(occupancy.reserve_bytes)
            > u64::from(HELD_INPUT_BYTES)
        {
            ledger.refusal(
                HeldSource::HumanInterjection,
                HoldRefusalReason::TotalBytes,
                key,
                bytes,
            );
            return false;
        }
        charge.store(bytes, std::sync::atomic::Ordering::SeqCst);
        true
    }
}
