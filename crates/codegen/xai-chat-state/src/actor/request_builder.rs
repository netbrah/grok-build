//! ConversationRequest assembly — image compaction, pruning, repair, memory injection.

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
    ) -> ConversationRequest {
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
        let budgeted = apply_image_budget(self.state.conversation.clone());
        let ImageBudgetOutcome {
            body_bytes,
            body_bytes_after,
            inline_images,
            needs_image_compaction,
            evicted,
        } = budgeted.outcome;
        let mut items = budgeted.items;
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
        }
        if let Some(reminder) = memory_reminder {
            inject_memory_reminder(&mut items, &reminder);
        }
        items = crate::compaction_utils::ModelRequestHistory::from_raw(items).into_items();

        // Step 4: Assemble request
        ConversationRequest {
            items,
            tools: tool_definitions,
            hosted_tools: vec![],
            tool_choice: None,
            // MGW F5 (apex-ayl.114): row-resolved tool control threads onto
            // the request; the producer nests dptu into tool_choice and
            // places the per-tool cache breakpoint.
            disable_parallel_tool_use: self.state.sampling_config.disable_parallel_tool_use,
            tool_cache_breakpoint: self.state.sampling_config.tool_cache_breakpoint,
            model: Some(self.state.sampling_config.model.clone()),
            temperature: self.state.sampling_config.temperature,
            max_output_tokens: self.state.sampling_config.max_completion_tokens,
            cache_ttl: self.state.sampling_config.cache_ttl.clone(),
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
