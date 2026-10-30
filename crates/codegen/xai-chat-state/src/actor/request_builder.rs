//! ConversationRequest assembly — image compaction, pruning, repair, memory injection.

use xai_grok_sampling_types::conversation::native_search_history::{
    NativeHistoryBoundary, NativeHistoryMappingEntry, NativeHistoryTransform,
    NativeHistoryTransformMapping, NativeUnavailable,
};
use xai_grok_sampling_types::{ConversationItem, ConversationRequest, ToolSpec, TraceContext};

use super::ChatStateActor;
use crate::events::ChatStateEvent;
use crate::image_budget::{ImageBudgetOutcome, apply_image_budget};
use crate::types::PruningConfig;

/// Placeholder inserted when a tool result is hard-cleared.
/// `pub(super)` so `mutations.rs` can use the same string on the retained conversation.
pub(super) const HARD_CLEAR_PLACEHOLDER: &str = "[Tool result omitted — too old]";

/// Separator inserted between head and tail in soft-trimmed results.
const SOFT_TRIM_SEPARATOR: &str = "\n\n[…trimmed…]\n\n";

impl ChatStateActor {
    /// Build a `ConversationRequest` from current actor state (image eviction, prune, memory reminder).
    /// The command handler already ran integrity repair on the actor conversation before this clone.
    /// Do not re-run dangling/dedup repair on the clone — those would be O(n) no-ops.
    pub(super) fn build_conversation_request(
        &mut self,
        tool_definitions: Vec<ToolSpec>,
        memory_reminder: Option<String>,
        persist_memory_reminder: bool,
        trace: Option<Box<dyn TraceContext>>,
        conv_id: String,
        req_id: String,
        native_capture: NativeBuildCapture,
    ) -> BuiltRequest {
        let capturing = matches!(native_capture, NativeBuildCapture::Selected);
        let needs_prune = should_prune(
            self.state.total_tokens,
            self.state.sampling_config.context_window,
        );
        let mut memory_reminder = memory_reminder;
        if let Some(reminder) = memory_reminder.as_deref()
            && persist_memory_reminder
        {
            // A live in-place inject can prepend a `System` item, shifting indices
            // under an active capture; snapshot + rebase like the other mutators.
            self.snapshot_turn_slice();
            let injected = inject_memory_reminder(&mut self.state.conversation, reminder);
            if injected {
                self.persistence.replace_history(&self.state.conversation);
                memory_reminder = None;
            }
            self.rebase_turn_capture_offset();
        }
        // `D_pre` (design r4 §4.1): the retained history is frozen here — after
        // this build's durable repairs and reminder effects and before any
        // request-only transform. Nothing below mutates `self.state.conversation`.
        let durable = capturing
            .then(|| crate::persistence::native::history_boundary(&self.state.conversation).ok())
            .flatten();
        let mut ledger = capturing
            .then(|| NativeMappingLedger::new(&self.state.conversation))
            .flatten();
        let budgeted = apply_image_budget(self.state.conversation.clone());
        let ImageBudgetOutcome {
            body_bytes,
            body_bytes_after,
            inline_images,
            needs_image_compaction,
            evicted,
        } = budgeted.outcome;
        let mut items = budgeted.items;
        if let Some(ledger) = ledger.as_mut() {
            ledger.apply_in_place(NativeHistoryTransform::ImageBudgetCompacted, None, &items);
        }
        if inline_images > 0 {
            self.send_event(ChatStateEvent::ImageBudget {
                body_bytes,
                trigger_bytes: crate::image_budget::IMAGE_COMPACT_TRIGGER_BYTES,
                reclaim_target_bytes: crate::image_budget::IMAGE_COMPACT_RECLAIM_TARGET_BYTES,
                inline_images,
                needs_image_compaction,
                evicted,
                body_bytes_after,
            });
        }
        if needs_prune {
            prune_conversation(&mut items, &self.pruning_config);
            if let Some(ledger) = ledger.as_mut() {
                ledger.apply_in_place(
                    NativeHistoryTransform::PrunedSoftTrim,
                    Some(HARD_CLEAR_PLACEHOLDER),
                    &items,
                );
            }
        }
        if let Some(reminder) = memory_reminder {
            inject_memory_reminder(&mut items, &reminder);
            if let Some(ledger) = ledger.as_mut() {
                ledger.apply_memory_reminder(&reminder, &items);
            }
        }
        items = crate::compaction_utils::ModelRequestHistory::from_raw(items).into_items();
        if let Some(ledger) = ledger.as_mut() {
            ledger.apply_in_place(
                NativeHistoryTransform::AgentMessageLabelRepaired,
                None,
                &items,
            );
        }

        // Step 4: Assemble request
        // apex-waj.35 requirement 4: the admission rides the REQUEST. The row half is
        // `sampling_config.supports_search_tool` — the flag of the config that names this request's
        // row, read off that config instead of a copy of it, because the config is what boot, resume
        // and switch all move in one message (ruling `map/RULINGS-o1o5.md` §"O3, run 5"): a session
        // that BOOTS on a flagged row is therefore admitted on its very first request, with no
        // `SetSessionModel` and no second, racy admission message behind it. The surface half is
        // computed here over the tools this request actually declares, because a tool-bridge
        // snapshot taken at any earlier point goes stale against an MCP connect/disconnect or
        // a preset change. That is also what makes a tool-less auxiliary request un-admitted
        // on its own: the producer, not a hand-set literal, closes the surface half. Rebuilding
        // through `for_row` keeps `has_searchable_tools` the single producer ruling D1 requires.
        let search_admission = Some(xai_grok_sampling_types::SearchAdmission::for_row(
            self.state.sampling_config.supports_search_tool,
            &tool_definitions,
        ));
        let request = ConversationRequest {
            items,
            tools: tool_definitions,
            hosted_tools: vec![],
            search_admission,
            native_eligible_inputs: None,
            tool_choice: None,
            // MGW F5 (apex-ayl.114): row-resolved tool control threads onto
            // the request; the producer nests dptu into tool_choice and
            // places the per-tool cache breakpoint.
            disable_parallel_tool_use: self.state.sampling_config.disable_parallel_tool_use,
            tool_cache_breakpoint: self.state.sampling_config.tool_cache_breakpoint,
            // MGW F1 (apex-ayl.115): config-selected server-tool surface
            // threads onto the request (the producer emits the members
            // after the client tools).
            server_tools: self.state.sampling_config.server_tools.clone(),
            mcp_servers: self.state.sampling_config.mcp_servers.clone(),
            mcp_toolset_server: self.state.sampling_config.mcp_toolset_server.clone(),
            model: Some(self.state.sampling_config.model.clone()),
            temperature: self.state.sampling_config.temperature,
            max_output_tokens: self.state.sampling_config.max_completion_tokens,
            cache_ttl: self.state.sampling_config.cache_ttl.clone(),
            thinking_replay: self.state.sampling_config.thinking_replay.clone(),
            top_p: self.state.sampling_config.top_p,
            // MGW F2 (apex-ayl.113): row-resolved sampling knobs thread onto
            // the wire; user_id is the process-global OS-user hash (FIX-PASS 4).
            top_k: self.state.sampling_config.top_k,
            stop_sequences: self.state.sampling_config.stop_sequences.clone(),
            user_id: msgw_user_id_hash(),
            x_grok_conv_id: Some(conv_id),
            x_grok_req_id: Some(req_id),
            x_grok_session_id: None,
            x_grok_turn_idx: None,
            x_grok_transient_retry: None,
            x_grok_agent_id: None,
            x_grok_deployment_id: None,
            x_grok_user_id: None,
            trace,
            prompt_cache_key: None,
            reasoning_effort: self.state.sampling_config.reasoning_effort,
            ultra_wire_effort: self.state.sampling_config.ultra_wire_effort,
            json_schema: None,
            // Execute completed tool calls on a Length-truncated turn instead
            // of failing it; text-only salvage stays behind `CompletePartial`.
            length_policy: xai_grok_sampling_types::LengthPolicy::CompleteToolCalls,
            // The retry actor (sampler) owns the cached messages-wire carrier
            // (REQVALID-1 47b D-5); chat-state always starts a request fresh.
            encoded: None,
            native_history: None,
            native_bound_preparation: None,
        };
        // The relation is only trustworthy if what the seams predicted is what
        // the builder actually emitted, so the finished request is the check.
        let mapping = match (ledger, durable.clone()) {
            (Some(ledger), Some(durable)) => ledger.finish(durable, &request.items).ok(),
            _ => None,
        };
        BuiltRequest {
            request,
            mapping,
            durable,
        }
    }
}

// ============================================================================
// Pruning (standalone functions, no actor state needed)
// ============================================================================

/// Deterministic opaque hash of the OS user identity for wire
/// `metadata.user_id` (MGW F2, apex-ayl.113; apex-ayl.109 as amended by
/// FIX-PASS 4: OS-user hash, always-emit on the main-turn actor path).
///
/// Identity: unix — `getpwuid_r(geteuid())` → `pw_name` (fallback env `USER`,
/// then `LOGNAME`); windows — env `USERNAME`, prefixed `USERDOMAIN + "\"`
/// when `USERDOMAIN` is set (the Windows identity is domain-scoped).
///
/// Hash (binding input spec, versioned): SHA-256 over
/// `grok-responses:metadata-user-id:v1:<identity>` → 64-char lowercase hex.
/// Deterministic across sessions AND machines for the same OS identity.
/// Unresolvable identity ⇒ `None` ⇒ `metadata` omitted (pre-cut body parity).
/// Process-memoized.
fn msgw_user_id_hash() -> Option<String> {
    static MEMO: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    MEMO
        .get_or_init(|| {
            fn identity() -> Option<String> {
                #[cfg(unix)]
                {
                    let mut buf = vec![0u8; 1024];
                    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
                    let mut result: *mut libc::passwd = std::ptr::null_mut();
                    let rc = unsafe {
                        libc::getpwuid_r(
                            libc::geteuid(),
                            &mut pwd,
                            buf.as_mut_ptr() as *mut libc::c_char,
                            buf.len(),
                            &mut result,
                        )
                    };
                    if rc == 0 && !pwd.pw_name.is_null() {
                        if let Ok(name) =
                            unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }.to_str()
                        {
                            return Some(name.to_owned());
                        }
                    }
                    std::env::var("USER").ok().or_else(|| std::env::var("LOGNAME").ok())
                }
                #[cfg(not(unix))]
                {
                    let name = std::env::var("USERNAME").ok()?;
                    Some(match std::env::var("USERDOMAIN") {
                        Ok(domain) => format!("{domain}\\{name}"),
                        Err(_) => name,
                    })
                }
            }
            identity().map(|identity| {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(format!("grok-responses:metadata-user-id:v1:{identity}"));
                format!("{:x}", hasher.finalize())
            })
        })
        .clone()
}

/// Check whether pruning should run based on context utilization.
///
/// Returns `true` when `total_tokens` exceeds 50% of `context_window`.
pub(crate) fn should_prune(total_tokens: u64, context_window: std::num::NonZeroU64) -> bool {
    total_tokens > context_window.get() / 2
}

/// Prune old, large tool results from the conversation in place.
/// Turn age is estimated by walking backward and counting `User` items.
pub(crate) fn prune_conversation(conversation: &mut [ConversationItem], config: &PruningConfig) {
    if !config.enabled {
        return;
    }

    let mut turn_from_end: usize = 0;
    let mut seen_first_user = false;

    for i in (0..conversation.len()).rev() {
        if matches!(&conversation[i], ConversationItem::User(_)) {
            if seen_first_user {
                turn_from_end += 1;
            }
            seen_first_user = true;
            continue;
        }

        let ConversationItem::ToolResult(tool_result) = &mut conversation[i] else {
            continue;
        };

        // Never prune recent turns.
        if turn_from_end < config.keep_last_n_turns {
            continue;
        }

        // Hard clear: very old tool results → replace entirely.
        if turn_from_end >= config.hard_clear_age_turns {
            if tool_result.content.as_ref() != HARD_CLEAR_PLACEHOLDER {
                tool_result.content = std::sync::Arc::<str>::from(HARD_CLEAR_PLACEHOLDER);
            }
            continue;
        }

        // Soft trim: large tool results → keep head + tail.
        let content_len = tool_result.content.chars().count();
        if content_len > config.soft_trim_threshold {
            let head = safe_char_slice(&tool_result.content, 0, config.soft_trim_head);
            let tail = safe_char_slice_tail(&tool_result.content, config.soft_trim_tail);
            tool_result.content =
                std::sync::Arc::<str>::from(format!("{head}{SOFT_TRIM_SEPARATOR}{tail}"));
        }
    }
}

// ============================================================================
// Memory reminder injection
// ============================================================================

use crate::types::MEMORY_CONTEXT_OPEN_TAG;

/// Upsert a memory reminder into the conversation's system message.
/// Replaces a prior reminder section in-place, or prepends a `System` item if none exists.
/// Returns `true` when the conversation was changed.
pub(super) fn inject_memory_reminder(items: &mut Vec<ConversationItem>, reminder: &str) -> bool {
    let reminder = reminder.trim();
    if reminder.is_empty() {
        return false;
    }

    if let Some(ConversationItem::System(sys)) = items.first_mut() {
        upsert_memory_reminder_text(&mut sys.content, reminder)
    } else {
        items.insert(0, ConversationItem::system(reminder));
        true
    }
}

fn upsert_memory_reminder_text(system_prompt: &mut std::sync::Arc<str>, reminder: &str) -> bool {
    let existing_start = system_prompt
        .find(MEMORY_CONTEXT_OPEN_TAG)
        .map(|idx| system_prompt[..idx].trim_end_matches('\n').len());

    let updated: String = if let Some(prefix_len) = existing_start {
        let prefix = system_prompt[..prefix_len].trim_end_matches('\n');
        if prefix.is_empty() {
            reminder.to_string()
        } else {
            format!("{prefix}\n\n{reminder}")
        }
    } else if system_prompt.trim_end() == reminder {
        system_prompt.as_ref().to_owned()
    } else if system_prompt.is_empty() {
        reminder.to_string()
    } else {
        format!("{}\n\n{reminder}", system_prompt.trim_end_matches('\n'))
    };

    if system_prompt.as_ref() == updated.as_str() {
        false
    } else {
        *system_prompt = std::sync::Arc::<str>::from(updated);
        true
    }
}

// ============================================================================
// String helpers
// ============================================================================

fn safe_char_slice(s: &str, start: usize, count: usize) -> String {
    s.chars().skip(start).take(count).collect()
}

fn safe_char_slice_tail(s: &str, count: usize) -> String {
    let total = s.chars().count();
    if count >= total {
        return s.to_string();
    }
    s.chars().skip(total - count).collect()
}

// ============================================================================
// Bound-build durable→send mapping (design r4 §4.1; waj2-U8)
// ============================================================================

/// Whether this builder run must also produce the durable→send relation a
/// Selected bound preparation carries.
pub(super) enum NativeBuildCapture {
    /// The ordinary route: the pipeline runs exactly as before and derives no
    /// relation.
    Legacy,
    /// A Selected bound build: freeze `D_pre`, follow every request-only seam,
    /// and verify the resulting relation against the items actually sent.
    Selected,
}

/// What one builder run produced for its caller.
pub(super) struct BuiltRequest {
    pub request: ConversationRequest,
    /// `Some` only for a [`NativeBuildCapture::Selected`] build whose relation
    /// verified; `None` means the caller must refuse a Selected preparation.
    pub mapping: Option<NativeHistoryTransformMapping>,
    /// The frozen `D_pre`: retained history after this build's durable repairs
    /// and reminder effects, before any request-only transform.
    pub durable: Option<NativeHistoryBoundary>,
}

/// The per-item half of the digest pair a mapping entry carries.
///
/// Same byte recipe as
/// [`crate::persistence::native::history_boundary`] (canonical JSON through
/// `serde_json`) applied to one item instead of the whole slice.
fn native_item_digest(item: &ConversationItem) -> Option<String> {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_vec(item).ok()?;
    Some(format!("{:x}", Sha256::digest(encoded)))
}

fn tool_result_content_equals(item: &ConversationItem, marker: &str) -> bool {
    matches!(item, ConversationItem::ToolResult(result) if result.content.as_ref() == marker)
}

/// One `D_pre` occurrence tracked through the request-only transforms.
struct NativeMappingSlot {
    durable_index: Option<u32>,
    before_digest: String,
    digest: String,
    transforms: Vec<NativeHistoryTransform>,
    reminder_input: Option<String>,
}

impl NativeMappingSlot {
    fn record(&mut self, observed: NativeHistoryTransform) {
        if self.transforms == [NativeHistoryTransform::Kept] {
            self.transforms = vec![observed];
        } else if !self.transforms.contains(&observed) {
            self.transforms.push(observed);
        }
    }
}

/// The occurrence-indexed ledger between `D_pre` and `S_pre`.
///
/// Every request-only step below is index-preserving: the image budget evicts
/// image parts in place, pruning rewrites `ToolResult.content` in place, and
/// `ModelRequestHistory::from_raw` is a length-preserving repair. Only the
/// request-only memory reminder may insert one item at the front. Any other
/// shape is a relation this code cannot derive, so the ledger marks itself
/// ambiguous and [`Self::finish`] refuses instead of guessing a mapping.
struct NativeMappingLedger {
    slots: Vec<NativeMappingSlot>,
    ambiguous: bool,
}

impl NativeMappingLedger {
    fn new(items: &[ConversationItem]) -> Option<Self> {
        let mut slots = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let digest = native_item_digest(item)?;
            slots.push(NativeMappingSlot {
                durable_index: Some(u32::try_from(index).ok()?),
                before_digest: digest.clone(),
                digest,
                transforms: vec![NativeHistoryTransform::Kept],
                reminder_input: None,
            });
        }
        Some(Self {
            slots,
            ambiguous: false,
        })
    }

    /// Records an index-preserving transform stage. `hard_clear_marker` names
    /// the placeholder that distinguishes a prune hard clear from a soft trim.
    fn apply_in_place(
        &mut self,
        transform: NativeHistoryTransform,
        hard_clear_marker: Option<&str>,
        after: &[ConversationItem],
    ) {
        if self.ambiguous || self.slots.len() != after.len() {
            self.ambiguous = true;
            return;
        }
        for (slot, item) in self.slots.iter_mut().zip(after) {
            let Some(digest) = native_item_digest(item) else {
                self.ambiguous = true;
                return;
            };
            if digest == slot.digest {
                continue;
            }
            let observed = match (transform, hard_clear_marker) {
                (NativeHistoryTransform::PrunedSoftTrim, Some(marker))
                    if tool_result_content_equals(item, marker) =>
                {
                    NativeHistoryTransform::PrunedHardClear
                }
                (transform, _) => transform,
            };
            slot.record(observed);
            slot.digest = digest;
        }
    }

    /// Records the request-only memory-reminder step, which upserts into a
    /// leading `System` item or inserts a new leading item.
    fn apply_memory_reminder(&mut self, reminder: &str, after: &[ConversationItem]) {
        if self.ambiguous {
            return;
        }
        let Some(first) = after.first() else {
            self.ambiguous = true;
            return;
        };
        let Some(digest) = native_item_digest(first) else {
            self.ambiguous = true;
            return;
        };
        if after.len() == self.slots.len() + 1 {
            self.slots.insert(
                0,
                NativeMappingSlot {
                    durable_index: None,
                    before_digest: digest.clone(),
                    digest,
                    transforms: vec![NativeHistoryTransform::RequestOnlyReminder],
                    reminder_input: Some(reminder.to_string()),
                },
            );
            return;
        }
        if after.len() != self.slots.len() {
            self.ambiguous = true;
            return;
        }
        if digest == self.slots[0].digest {
            // The reminder text was already present: no effect to map.
            return;
        }
        let slot = &mut self.slots[0];
        slot.record(NativeHistoryTransform::RequestOnlyReminderUpsert);
        slot.digest = digest;
        slot.reminder_input = Some(reminder.to_string());
    }

    /// Verifies the predicted digests against the items the builder actually
    /// produced and returns the checked relation.
    fn finish(
        self,
        durable: NativeHistoryBoundary,
        sent: &[ConversationItem],
    ) -> Result<NativeHistoryTransformMapping, NativeUnavailable> {
        if self.ambiguous || self.slots.len() != sent.len() {
            return Err(NativeUnavailable::Incomplete);
        }
        let mut entries = Vec::with_capacity(self.slots.len());
        for (slot, item) in self.slots.into_iter().zip(sent) {
            let after_digest = native_item_digest(item).ok_or(NativeUnavailable::Fidelity)?;
            if after_digest != slot.digest {
                // The builder emitted bytes the seams did not predict; the
                // relation would be a guess.
                return Err(NativeUnavailable::Fidelity);
            }
            let send_transforms = slot.transforms.clone();
            entries.push(match slot.durable_index {
                Some(durable_index) => NativeHistoryMappingEntry::durable(
                    durable_index,
                    slot.before_digest,
                    after_digest,
                    send_transforms,
                    slot.reminder_input,
                ),
                None => NativeHistoryMappingEntry::request_only_insertion(
                    after_digest,
                    send_transforms,
                    slot.reminder_input,
                ),
            });
        }
        let send = crate::persistence::native::history_boundary(sent)?;
        NativeHistoryTransformMapping::checked(durable, send, entries)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn should_prune_gating() {
        use std::num::NonZeroU64;
        let cw = NonZeroU64::new(10000).unwrap();
        assert!(!should_prune(1000, cw)); // 10%
        assert!(should_prune(6000, cw)); // 60%
        assert!(!should_prune(5000, cw)); // 50% exact (> not >=)
    }

    #[test]
    fn prune_disabled_is_noop() {
        let mut conv = vec![ConversationItem::tool_result("c1", "x".repeat(10_000))];
        let config = PruningConfig {
            enabled: false,
            ..Default::default()
        };
        prune_conversation(&mut conv, &config);
        if let ConversationItem::ToolResult(ref tr) = conv[0] {
            assert_eq!(tr.content.len(), 10_000);
        }
    }

    #[test]
    fn inject_memory_into_existing_system() {
        let mut items = vec![
            ConversationItem::system("You are helpful."),
            ConversationItem::user("hi"),
        ];
        inject_memory_reminder(&mut items, "Remember: user likes rust");
        if let ConversationItem::System(ref sys) = items[0] {
            assert!(sys.content.contains("Remember: user likes rust"));
            assert!(sys.content.starts_with("You are helpful."));
        }
        assert_eq!(items.len(), 2); // no new item added
    }

    #[test]
    fn inject_memory_prepends_when_no_system() {
        let mut items = vec![ConversationItem::user("hi")];
        inject_memory_reminder(&mut items, "Remember: user likes rust");
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], ConversationItem::System(_)));
    }

    // MGW F2 (apex-ayl.113, FIX-PASS 4): the expected `metadata.user_id`
    // value, re-derived in-test from the versioned spec input (hermetic:
    // no hard-coded username). Shared with the actor test rig (tests.rs).
    #[cfg(test)]
    pub(crate) fn mgw_f2_expected_user_id_hash() -> Option<String> {
        fn identity() -> Option<String> {
            #[cfg(unix)]
            {
                let mut buf = vec![0u8; 1024];
                let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
                let mut result: *mut libc::passwd = std::ptr::null_mut();
                let rc = unsafe {
                    libc::getpwuid_r(
                        libc::geteuid(),
                        &mut pwd,
                        buf.as_mut_ptr() as *mut libc::c_char,
                        buf.len(),
                        &mut result,
                    )
                };
                if rc == 0 && !pwd.pw_name.is_null() {
                    if let Ok(name) = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }.to_str()
                    {
                        return Some(name.to_owned());
                    }
                }
                std::env::var("USER").ok().or_else(|| std::env::var("LOGNAME").ok())
            }
            #[cfg(not(unix))]
            {
                let name = std::env::var("USERNAME").ok()?;
                Some(match std::env::var("USERDOMAIN") {
                    Ok(domain) => format!("{domain}\\{name}"),
                    Err(_) => name,
                })
            }
        }
        identity().map(|identity| {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(format!("grok-responses:metadata-user-id:v1:{identity}"));
            format!("{:x}", hasher.finalize())
        })
    }

    #[test]
    fn mgw_f2_user_id_hash_is_deterministic_spec_input_sha256() {
        // U-RED-3b: process-memoized, 64 lowercase hex, and equal to the
        // SHA-256 of the §3.5 spec input for THIS process's OS identity.
        let first = msgw_user_id_hash();
        let second = msgw_user_id_hash();
        assert_eq!(first, second, "process-memoized: stable across calls");
        let hash = first
            .as_deref()
            .expect("identity resolves on any CI box (passwd/env fallback)");
        assert_eq!(hash.len(), 64, "SHA-256 hex is 64 chars: {hash:?}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex only: {hash:?}"
        );
        let expected = mgw_f2_expected_user_id_hash();
        assert_eq!(
            first, expected,
            "must equal SHA-256 of the versioned spec input for the \
             test process's own uid"
        );
    }
}
