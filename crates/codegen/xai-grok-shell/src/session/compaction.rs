//! This module contains all compaction methods: manual `/compact`, auto-compact threshold checks, and inline auto-compact with auto-continue.
//! It also holds error-recovery compaction, preflight overflow detection, and checkpoint persistence.
//! These methods form a second `impl SessionActor` block that lives alongside the primary one in `acp_session.rs`.
use super::SessionActor;
use super::is_project_instructions;
use crate::extensions::notification::MODEL_FAMILY_SWITCH_COMPACT_BANNER;
use crate::remote::DEFAULT_CONTEXT_WINDOW;
use crate::session::compaction_config::{
    AsyncCompactionCache, SUPPRESS_AUTH, SUPPRESS_NONE, SUPPRESS_STICKY, SUPPRESS_TURN,
    SUPPRESS_UNTIL_SUCCESS,
};
use crate::session::helpers::CompactionStateContext;
use crate::session::helpers::compaction_context::CompactionInputs;
use crate::session::helpers::compaction_context::to_system_reminder;
use crate::session::helpers::session_compact::{
    COMPACT_FAILED_PREFIX, CompactOutput, CompactionOutcome, build_compaction_prompt,
    generate_session_compact, is_context_length_error, retain_session_asset_files,
};
use crate::session::persistence::PersistenceMsg;
use crate::session::two_pass::{
    TWO_PASS_DEFAULT_SPLIT_FRACTION, build_two_pass_pass1_history, build_two_pass_pass2_history,
    note_for_two_pass_pass2, split_conversation_for_two_pass,
};
use agent_client_protocol as acp;
use std::sync::Arc;
use xai_chat_state::compaction_image_context::CompactionImageContext;
use xai_chat_state::compaction_utils::{
    CompactedHistoryInput, CompactionAttempt, build_codex_remote_compaction_v2_history,
    build_compacted_history, codex_remote_compaction_v2_interjections, is_degenerate_summary,
    prepare_conversation_for_verbatim_summarization, sanitize_compacted_history,
    validate_compacted_history,
};
use xai_grok_sampling_types::{
    build_messages_request, drop_model_bound_items, request_validation::RequestValidationError,
    ApiBackend, ConversationItem, ConversationRequest,
};
/// Prefix on the early-guard failure payloads below; the user-facing normalizer strips it (the renderer prepends its own headline).
const COMPACTION_FAILED_GUARD_PREFIX: &str = "Compaction failed: ";
/// Human-readable "next fire" for a scheduled loop in the compaction reminder.
fn format_loop_next_fire(
    next: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = next.signed_duration_since(now).num_seconds();
    if secs <= 0 {
        return "now".to_string();
    }
    if secs < 60 {
        return format!("in {secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("in {mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("in {hours}h");
    }
    next.format("%d %b %H:%M UTC").to_string()
}
/// Reserve for the prompt and generated summary (plus reasoning on Responses/Messages); 32_768 covers p99 of prod output (~20k p95).
const SUMMARY_BUDGET_RESERVE_TOKENS: u64 = 32_768;
/// Default percentage points below the auto-compact threshold at which prefire (background pass-1) starts.
/// The lead gives pass-1 runway to finish before the limit.
/// Override with `GROK_PREFIRE_LEAD_PERCENT`.
const DEFAULT_PREFIRE_LEAD_PERCENT: u64 = 10;
fn prefire_lead_percent() -> u64 {
    std::env::var("GROK_PREFIRE_LEAD_PERCENT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_PREFIRE_LEAD_PERCENT)
}
/// Cheap fingerprint of a conversation prefix for prefire NOTE₁ validity.
/// A mismatch means the prefix changed (edit / rewind / branch) since pass-1.
/// The cached NOTE₁ then no longer summarizes the current prefix and must be dropped.
fn fingerprint_prefix(items: &[ConversationItem]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    items.len().hash(&mut h);
    for it in items {
        let tag: u8 = match it {
            ConversationItem::System(_) => 0,
            ConversationItem::User(_) => 1,
            ConversationItem::Assistant(_) => 2,
            ConversationItem::ToolResult(_) => 3,
            ConversationItem::BackendToolCall(_) => 4,
            ConversationItem::Reasoning(_) => 5,
            // Tag 6 for the seventh variant. Two consequences to state plainly: every
            // fingerprint of a history that already contains a discovery item changes
            // when this variant ships (a one-off prefire NOTE₁ invalidation, never a
            // wire prefix change — this hash is local-only), and the hash below covers
            // `text_content()`, i.e. the bounded `text_summary()`, so two different
            // loaded-tool payloads under the same query fingerprint alike. That is
            // acceptable for a prefix-CHANGE detector — it answers "did the history
            // shape move?", not "is the payload the same?" — but it is why a discovery
            // item must never be the only evidence a cache key relies on.
            ConversationItem::Discovery { .. } => 6,
        };
        tag.hash(&mut h);
        it.text_content().hash(&mut h);
    }
    h.finish()
}
/// Prefix message for tool outputs that the Codex remote-compaction preflight had to
/// shrink to fit the provider's context window.
const CODEX_CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";
/// Match codex-rs's remote-compaction preflight: when the request itself has
/// crossed the effective context window, rewrite trailing tool outputs from
/// newest to oldest. Do not discard ordinary messages or arbitrary history;
/// if tool-output trimming cannot make the request fit, let the endpoint
/// return the provider error intact.
/// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:51 :: rewrite_codex_tool_outputs_to_fit_context_window (adapted: the worktree `ConversationItem` has no `CustomToolOutput` variant and the worktree `ToolResult` has no `ordered_content` field)
fn rewrite_codex_tool_outputs_to_fit_context_window(
    items: &mut [ConversationItem],
    input_budget: u64,
) -> usize {
    let mut rewritten = 0usize;
    for index in (0..items.len()).rev() {
        if xai_chat_state::estimate_conversation_tokens(items) <= input_budget {
            break;
        }
        let did_rewrite = match &mut items[index] {
            ConversationItem::ToolResult(output) => {
                output.content = Arc::<str>::from(CODEX_CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE);
                output.images.clear();
                true
            }
            // Halting here is the intended behaviour, not a defect: this reverse fit
            // only shrinks tool OUTPUTS, and any other item means it cannot make the
            // request fit, so the endpoint error is left intact (see the fn doc).
            // A discovery pair is never shrunk to make room — `raw` is opaque provider
            // state (wire invariant 6) and the pair is the loaded-tool-set record
            // (ruling apex-waj.18 A-26). Any future shrink must be a typed stub that
            // keeps both ids, never a byte edit of `raw`.
            ConversationItem::Discovery { .. }
            | ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::Assistant(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => false,
        };
        if !did_rewrite {
            break;
        }
        rewritten = rewritten.saturating_add(1);
    }
    rewritten
}
/// An in-flight Codex compaction may refresh auth once, and only while an
/// attempt remains (a refresh on the final attempt would buy no retry).
/// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:90 :: codex_compaction_auth_refresh_allowed (verbatim)
fn codex_compaction_auth_refresh_allowed(
    attempt: u32,
    max_attempts: u32,
    already_refreshed: bool,
) -> bool {
    !already_refreshed && attempt < max_attempts
}
/// Local two-pass prefire is redundant when the compaction itself goes to the
/// provider server: codex family + Responses backend + the v2 flag on.
/// Fresh-written predicate (no session-level v2 E2E exists in either source repo);
/// see `codex_remote_compaction_v2_tests`.
fn codex_remote_compaction_v2_excludes_two_pass_prefire(
    model_family: Option<&str>,
    api_backend: ApiBackend,
    remote_compaction_v2_enabled: bool,
) -> bool {
    model_family.is_some_and(|family| family == "codex")
        && api_backend == ApiBackend::Responses
        && remote_compaction_v2_enabled
}
/// Outcome of a background prefire pass-1 run, recorded on the `session.prefire_pass1` span as `compaction_prefire_outcome`.
/// [`PrefireOutcome::as_str`] values are stable telemetry keys (telemetry/dashboards key off them); don't rename the strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, strum::AsRefStr, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum PrefireOutcome {
    Cached,
    Disabled,
    DebugFailPass1,
    TooSmall,
    EmptySplit,
    SampleFailed,
    EmptyNote1,
}
/// Telemetry from one prefire pass-1 run; recorded onto the `session.prefire_pass1` span by [`SessionActor::run_prefire_pass1`].
/// A `None` field means the run exited before that stage.
struct PrefirePass1Run {
    outcome: PrefireOutcome,
    prefix_len: Option<usize>,
    prefix_est_tokens: Option<u64>,
    pass1_latency_ms: Option<u64>,
    note1_chars: Option<usize>,
}
impl From<PrefireOutcome> for PrefirePass1Run {
    fn from(outcome: PrefireOutcome) -> Self {
        Self {
            outcome,
            prefix_len: None,
            prefix_est_tokens: None,
            pass1_latency_ms: None,
            note1_chars: None,
        }
    }
}
#[cfg(test)]
#[path = "compaction_codex_remote_v2_tests.rs"]
mod codex_remote_compaction_v2_tests;
#[cfg(test)]
#[path = "compaction_two_pass_prefire_helper_tests.rs"]
mod two_pass_prefire_helper_tests;

/// Maps the fork `ResponseUsage` returned by a Codex remote compaction v2 completion into the
/// session ledger's `TokenUsage` (same fold path as the main loop: `record_model_call_usage`
/// → `UsageSummary::from_ledger` → `usage.json`).
fn codex_compaction_v2_token_usage(
    usage: &async_openai::types::responses::ResponseUsage,
) -> xai_grok_sampling_types::TokenUsage {
    xai_grok_sampling_types::TokenUsage {
        prompt_tokens: usage.input_tokens,
        completion_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        reasoning_tokens: usage.output_tokens_details.reasoning_tokens,
        cached_prompt_tokens: usage.input_tokens_details.cached_tokens,
        cache_creation_prompt_tokens: usage
            .input_tokens_details
            .cache_write_tokens
            .map(|v| v.max(0) as u32)
            .unwrap_or(0),
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    }
}
#[cfg(test)]
#[path = "compaction_verbatim_input_tests.rs"]
mod verbatim_input_tests;
impl SessionActor {
    /// Two-pass is active for this session when the flag resolved on at build and the agent is not one that keeps its single short self-summary.
    pub(crate) fn two_pass_active(&self) -> bool {
        let agent = self.agent.borrow();
        agent.compaction_policy().two_pass_enabled
    }
    /// The catalog model family of `model`, or `None` when the model is unlisted.
    /// The worktree has no per-config `provider` field, so the codex gate resolves the
    /// family from the catalog (precedent: `reconstruct_full_config`).
    pub(crate) fn codex_model_family(&self, model: &str) -> Option<String> {
        let models = self.models_manager.models();
        crate::agent::remote_config::resolve_catalog_key(
            &models,
            &acp::ModelId::new(model.to_owned()),
        )
        .and_then(|catalog_id| {
            models
                .get(catalog_id.0.as_ref())
                .and_then(|entry| entry.info.model_family.clone())
        })
    }
    /// The prompt is already embedded, so this bypasses the single-pass sampler and calls `generate_session_compact` directly.
    /// Agent `RefCell` borrows are only taken for synchronous snapshots (never held across `.await`).
    /// A long-lived borrow would race with turn/compact/cancel and panic on double-borrow.
    async fn two_pass_sample(&self, history: Vec<ConversationItem>) -> Option<CompactOutput> {
        let mut binding = self.capture_transport_binding_or_default().await;
        #[cfg(test)]
        binding_tests::after_capture(self, &binding).await;
        let sampling_config = self.reconstruct_full_config_from_binding(&binding).await;
        #[cfg(test)]
        binding_tests::record_reconstruction(&sampling_config);
        let client = match self
            .prepare_chat_completion_from_binding(&mut binding, false)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "two_pass: failed to prepare sampling client");
                return None;
            }
        };
        let tool_defs = self.prepare_tool_definitions().await;
        let tools = self.turn_base_tool_specs(&tool_defs);
        let compaction_tool_tokens = xai_chat_state::estimate_tool_specs_tokens(&tools);
        let wall_clock_budget_secs = self
            .agent
            .borrow()
            .compaction_policy()
            .wall_clock_budget_secs;
        let hosted_tools = self.hosted_tools_for_turn();
        #[cfg(test)]
        binding_tests::record_inputs(binding_tests::GeneratorInputs {
            site: "two-pass",
            history: history.clone(),
            tools: tools.clone(),
            hosted: hosted_tools.clone(),
            token_kind: "specs",
            token_input: serde_json::to_value(&tools).unwrap(),
            tool_tokens: compaction_tool_tokens,
            config: sampling_config.clone(),
            client_backend: client.api_backend(),
            idle_timeout: self.inference_idle_timeout,
            wall_clock_budget_secs,
            tool_choice: self.compaction.tool_choice,
        });
        let (cancel, _cancel_scope) = self.compaction.cancel.enter();
        match generate_session_compact(
            history,
            compaction_tool_tokens,
            tools,
            hosted_tools,
            client,
            self.session_info.id.clone(),
            &sampling_config,
            self.inference_idle_timeout,
            wall_clock_budget_secs,
            self.compaction.tool_choice,
            &cancel,
        )
        .await
        {
            Ok(out) => Some(out),
            Err(e) => {
                tracing::warn!(error = ?e, "two_pass: summarization sample failed");
                None
            }
        }
    }
    /// Per-turn prefire decision: usage has reached `threshold - lead` (so there is still runway before the hard auto-compact line at `threshold`).
    pub(crate) async fn should_prefire_two_pass(&self) -> bool {
        if self.compaction.is_suppressed() {
            return false;
        }
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        // Divergence from OG (documented): OG excludes prefire for every codex
        // session (its flag-off fallback is the v1 unary endpoint, which the
        // worktree does not model). Here the exclusion fires only when the v2
        // path actually applies — codex family + Responses + flag on — so flag
        // off (or a non-Responses codex model) stays on local compaction, whose
        // prefire remains valid.
        if let Some(cfg) = sampling_cfg.as_ref()
            && codex_remote_compaction_v2_excludes_two_pass_prefire(
                self.codex_model_family(cfg.model.as_str()).as_deref(),
                cfg.api_backend.clone(),
                self.agent.borrow().compaction_policy().remote_compaction_v2,
            )
        {
            return false;
        }
        let Some(cw) = sampling_cfg.as_ref().map(|c| c.context_window.get()) else {
            return false;
        };
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        let threshold = self.compaction.threshold_percent.get() as u64;
        let start_pct = threshold.saturating_sub(prefire_lead_percent());
        xai_token_estimation::exceeds_threshold(estimated_total, cw, start_pct as u8)
    }
    /// Background pass-1: summarize the ~95% prefix into NOTE₁ and cache it for a later pass-2 apply.
    /// Always releases the in-flight guard.
    /// Spawned via `spawn_local` from the turn loop; reads a conversation snapshot and does not mutate session state.
    #[tracing::instrument(
        name = "session.prefire_pass1",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            compaction_prefire_outcome = tracing::field::Empty,
            compaction_pass1_latency_ms = tracing::field::Empty,
            compaction_prefire_prefix_len = tracing::field::Empty,
            compaction_prefire_prefix_est_tokens = tracing::field::Empty,
            compaction_prefire_note1_chars = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_prefire_pass1(self: &Arc<Self>) {
        struct InFlightGuard<'a>(&'a crate::session::compaction_config::PrefireState);
        impl Drop for InFlightGuard<'_> {
            fn drop(&mut self) {
                self.0.finish();
            }
        }
        let _guard = InFlightGuard(&self.compaction.prefire);
        let run = self.run_prefire_pass1_inner().await;
        let span = tracing::Span::current();
        span.record("compaction_prefire_outcome", run.outcome.as_ref());
        if let Some(v) = run.prefix_len {
            span.record("compaction_prefire_prefix_len", v as i64);
        }
        if let Some(v) = run.prefix_est_tokens {
            span.record("compaction_prefire_prefix_est_tokens", v as i64);
        }
        if let Some(v) = run.pass1_latency_ms {
            span.record("compaction_pass1_latency_ms", v as i64);
        }
        if let Some(v) = run.note1_chars {
            span.record("compaction_prefire_note1_chars", v as i64);
        }
    }
    async fn run_prefire_pass1_inner(self: &Arc<Self>) -> PrefirePass1Run {
        if !self.two_pass_active() {
            return PrefireOutcome::Disabled.into();
        }
        if std::env::var("GROK_DEBUG_TWO_PASS_FAIL_PASS1")
            .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        {
            tracing::info!(
                target: "two_pass",
                "two_pass: DEBUG GROK_DEBUG_TWO_PASS_FAIL_PASS1 — prefire pass1 produces no cache"
            );
            return PrefireOutcome::DebugFailPass1.into();
        }
        let conversation = self.chat_state_handle.get_conversation().await;
        if conversation.len() < 4 {
            return PrefireOutcome::TooSmall.into();
        }
        let split = split_conversation_for_two_pass(&conversation, TWO_PASS_DEFAULT_SPLIT_FRACTION);
        if split.prefix.is_empty() || split.tail.is_empty() {
            return PrefireOutcome::EmptySplit.into();
        }
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        let strips = sampling_cfg
            .as_ref()
            .map(|c| c.api_backend == ApiBackend::Messages)
            .unwrap_or(false);
        let model_slug = sampling_cfg
            .as_ref()
            .map(|c| c.model.to_string())
            .unwrap_or_default();
        let prefix_prepared = apply_turn_image_budget_and_prune(
            &self.chat_state_handle,
            prepare_conversation_for_verbatim_summarization(split.prefix.to_vec(), strips),
        )
        .await;
        let prefix_est_tokens = prefix_prepared
            .iter()
            .map(xai_chat_state::estimate_item_tokens)
            .sum::<u64>();
        let prompt = build_compaction_prompt(None, false);
        let pass1_history = build_two_pass_pass1_history(&prefix_prepared, &prompt);
        let started = std::time::Instant::now();
        let out = self.two_pass_sample(pass1_history).await;
        let pass1_latency_ms = started.elapsed().as_millis() as u64;
        let attempted = |outcome: PrefireOutcome, note1_chars: Option<usize>| PrefirePass1Run {
            outcome,
            prefix_len: Some(split.split_idx),
            prefix_est_tokens: Some(prefix_est_tokens),
            pass1_latency_ms: Some(pass1_latency_ms),
            note1_chars,
        };
        let Some(out) = out else {
            return attempted(PrefireOutcome::SampleFailed, None);
        };
        let note1 = note_for_two_pass_pass2(&out.content);
        if note1.trim().is_empty() {
            return attempted(PrefireOutcome::EmptyNote1, None);
        }
        let note1_chars = note1.chars().count();
        let Some(prefix) = conversation.get(..split.split_idx) else {
            return attempted(PrefireOutcome::SampleFailed, None);
        };
        let cache = AsyncCompactionCache {
            note1,
            prefix_len: split.split_idx,
            fingerprint: fingerprint_prefix(prefix),
            model_slug,
            pass1_latency_ms,
        };
        tracing::info!(
            target: "two_pass",
            prefix_len = cache.prefix_len,
            pass1_latency_ms = cache.pass1_latency_ms,
            "two_pass: prefire pass1 cached NOTE1"
        );
        self.compaction.prefire.store(cache);
        attempted(PrefireOutcome::Cached, Some(note1_chars))
    }
    /// Runs only when a valid cached NOTE₁ exists for the current conversation.
    /// if pass-1 is still in flight we do add that await into `ttft_ms` (first token of the final summary), because the user is blocked on it;.
    /// `stream_ms` / `delta_count` / `itl_max_ms` are always pass-2 only (the only sample that streams the successor-visible summary).
    async fn try_two_pass_pass2_apply(
        &self,
        user_context: Option<&str>,
        strips_reasoning: bool,
    ) -> Option<CompactOutput> {
        if !self.two_pass_active() {
            return None;
        }
        let mut prefire_waited_ms = 0u64;
        if let Some(handle) = self.compaction.prefire.take_handle() {
            let was_in_flight = self.compaction.prefire.is_in_flight();
            let waited = std::time::Instant::now();
            let _ = handle.await;
            if was_in_flight {
                prefire_waited_ms = waited.elapsed().as_millis() as u64;
                tracing::Span::current()
                    .record("compaction_prefire_waited_ms", prefire_waited_ms as i64);
                tracing::info!(
                    target: "two_pass",
                    wait_ms = prefire_waited_ms,
                    "two_pass: waited for in-flight prefire pass1 before pass2"
                );
            }
        }
        let cache = self.compaction.prefire.take()?;
        let live = self.chat_state_handle.get_conversation().await;
        let model_slug = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .map(|c| c.model.to_string())
            .unwrap_or_default();
        let Some((prefix, tail)) = live.split_at_checked(cache.prefix_len) else {
            tracing::Span::current().record("compaction_prefire_stale", true);
            tracing::info!(
                target: "two_pass",
                "two_pass: cached NOTE1 stale or model changed; falling back to single-pass"
            );
            return None;
        };
        if cache.prefix_len == 0
            || cache.model_slug != model_slug
            || fingerprint_prefix(prefix) != cache.fingerprint
        {
            tracing::Span::current().record("compaction_prefire_stale", true);
            tracing::info!(
                target: "two_pass",
                "two_pass: cached NOTE1 stale or model changed; falling back to single-pass"
            );
            return None;
        }
        let prepared_tail =
            prepare_conversation_for_verbatim_summarization(tail.to_vec(), strips_reasoning);
        let prompt = build_compaction_prompt(user_context, false);
        let pass2_history =
            build_two_pass_pass2_history(prefix, &prepared_tail, &cache.note1, &prompt);
        let started = std::time::Instant::now();
        let mut out = self.two_pass_sample(pass2_history).await?;
        if is_degenerate_summary(&out.content) {
            tracing::Span::current().record("compaction_prefire_stale", true);
            tracing::info!(
                target: "two_pass",
                "two_pass: pass2 summary empty/degenerate; falling back to single-pass"
            );
            return None;
        }
        let pass2_latency_ms = started.elapsed().as_millis() as u64;
        if prefire_waited_ms > 0 {
            out.ttft_ms = Some(out.ttft_ms.unwrap_or(0).saturating_add(prefire_waited_ms));
        }
        let span = tracing::Span::current();
        span.record("compaction_two_pass_used", true);
        span.record("compaction_prefire_hit", true);
        span.record("compaction_pass2_latency_ms", pass2_latency_ms as i64);
        tracing::info!(
            target: "two_pass",
            prefix_len = cache.prefix_len,
            tail_len = tail.len(),
            prefire_waited_ms,
            pass2_latency_ms,
            pass1_bg_latency_ms = cache.pass1_latency_ms,
            "two_pass: pass2 applied cached NOTE1 (prefire hit)"
        );
        Some(out)
    }
}
pub(crate) struct AutoCompactTriggerInfo {
    pub tokens_used: u64,
    pub context_window: u64,
    pub percentage: u8,
    pub reason_override: Option<&'static str>,
}
/// The "always fits" lossy summarization budget (~70% of window, minus tool definitions); shared by the ladder's Lossy step and the cold Lossy start.
fn lossy_input_budget(context_window: u64, tool_tokens: u64) -> u64 {
    (context_window.saturating_mul(7) / 10).saturating_sub(tool_tokens)
}
/// Verbatim-fitted budget: leave room for the summary prompt plus tool-definition prefix.
fn fitted_input_budget(context_window: u64, tool_tokens: u64) -> u64 {
    context_window
        .saturating_sub(SUMMARY_BUDGET_RESERVE_TOKENS)
        .saturating_sub(tool_tokens)
}
/// Input ladder: verbatim, then verbatim fitted, then lossy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::AsRefStr, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum CompactInputStage {
    Verbatim,
    VerbatimFitted,
    Lossy,
}
/// Same image budget then tool-result prune a live turn request uses.
async fn apply_turn_image_budget_and_prune(
    chat_state: &xai_chat_state::ChatStateHandle,
    items: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    let max_request_bytes = chat_state
        .get_sampling_config()
        .await
        .and_then(|config| config.max_request_bytes);
    let items = xai_chat_state::image_budget::apply_image_budget(items, max_request_bytes).items;
    chat_state.apply_turn_request_pruning(items).await
}
/// Start fitted when the (already image-budgeted and pruned) estimate cannot leave room for tools + summary.
fn start_verbatim_compact_turns(
    turns: Vec<ConversationItem>,
    tool_tokens: u64,
    context_window: u64,
) -> (Vec<ConversationItem>, CompactInputStage) {
    let estimate = xai_chat_state::estimate_conversation_tokens(&turns);
    let budget = fitted_input_budget(context_window, tool_tokens);
    if estimate > budget {
        tracing::info!(
            estimate,
            budget,
            "verbatim compact estimate exceeds fitted budget; starting at verbatim_fitted"
        );
        (
            xai_chat_state::compaction_utils::fit_conversation_to_budget(turns, budget),
            CompactInputStage::VerbatimFitted,
        )
    } else {
        (turns, CompactInputStage::Verbatim)
    }
}
/// Why auto-compaction was suppressed after a deterministic failure.
/// [`SuppressReason::as_str`] is a stable telemetry value (BQ/OTLP/dashboards key off it); don't rename the strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, strum::AsRefStr, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum SuppressReason {
    CreditBlock,
    Size,
    Auth,
    Schema,
    Other,
}
impl SuppressReason {
    /// `size` and `schema` get [`SUPPRESS_STICKY`]: cleared only on a context-budget change.
    /// `credit_block` gets [`SUPPRESS_UNTIL_SUCCESS`]: wait for a model `200`.
    /// `auth` gets [`SUPPRESS_AUTH`]: cleared on login/token refresh, not on a `200` (an over-window session never gets one).
    fn suppress_state(self) -> u8 {
        match self {
            SuppressReason::Size | SuppressReason::Schema => SUPPRESS_STICKY,
            SuppressReason::CreditBlock => SUPPRESS_UNTIL_SUCCESS,
            SuppressReason::Auth => SUPPRESS_AUTH,
            SuppressReason::Other => SUPPRESS_TURN,
        }
    }
}
/// Splice the preserved prefix (`conversation[0..prefix_len]`) onto the compacted suffix, dropping the suffix's leading System.
/// If the prefix already has an AGENTS.md item, drop the suffix's re-injected AGENTS.md too (else the model sees it twice).
/// Returns `Err(compacted_history)` unchanged when `prefix_len` is 0 or out of range.
fn preserve_inherited_prefix(
    conversation: &[ConversationItem],
    compacted_history: Vec<ConversationItem>,
    prefix_len: usize,
) -> Result<Vec<ConversationItem>, Vec<ConversationItem>> {
    if prefix_len == 0 {
        return Err(compacted_history);
    }
    let Some(inherited) = conversation.get(..prefix_len) else {
        return Err(compacted_history);
    };
    let drop_reinjected_agents_md = inherited.iter().any(is_project_instructions);
    let mut preserved = inherited.to_vec();
    let child_items = compacted_history
        .into_iter()
        .skip_while(|i| matches!(i, ConversationItem::System(_)))
        .filter(|i| !(drop_reinjected_agents_md && is_project_instructions(i)));
    preserved.extend(child_items);
    Ok(preserved)
}
/// Project the token count a re-pinned (preserved) history would reseed to.
/// The release decision then compares against the same threshold the auto-compact trigger applies next turn.
/// The conversation only grows, so this under-estimates the reseed (a lower bound) and can lean toward preserve.
fn project_preserved_reseed_tokens(
    preserved_estimate: u64,
    tokens_before: u64,
    full_conv_estimate: u64,
) -> u64 {
    let ratio = tokens_before as f64 / full_conv_estimate.max(1) as f64;
    ((preserved_estimate as f64 * ratio).round() as u64).min(tokens_before)
}
impl SessionActor {
    /// Where the transcript would be, without asking the filesystem: callers on a hot path do the `exists()` themselves, off the actor's thread.
    pub(crate) fn transcript_path(&self) -> std::path::PathBuf {
        crate::session::persistence::session_dir(&self.session_info).join("updates.jsonl")
    }
    /// Path to the raw `updates.jsonl` transcript if it exists, else `None`.
    /// `pub(crate)` so the `Transcript`-mode dispatch in `compaction_segments` and transcript-location pointers can both reuse it.
    /// The `path.exists()` guard keeps the pointer safe when a session never wrote one.
    pub(crate) fn get_transcript_path(&self) -> Option<String> {
        let path = self.transcript_path();
        if path.exists() {
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        }
    }
    /// Increment the compaction counter and launch a pre-compaction memory flush.
    ///
    /// The counter is incremented before the flush check so the once-per-cycle guard does not suppress the first eligible flush.
    async fn maybe_pre_compaction_flush(
        self: &Arc<Self>,
        total_tokens: u64,
        context_window: u64,
        trigger: &'static str,
    ) {
        let compaction_count = self
            .compaction
            .count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if !self.agent.borrow().compaction_policy().memory_flush_enabled {
            return;
        }
        let last_flush = self
            .memory
            .last_flush_compaction
            .load(std::sync::atomic::Ordering::Relaxed);
        if xai_grok_memory::flush::should_flush(
            total_tokens,
            context_window,
            self.compaction.threshold_percent.get(),
            &self.memory.flush_config,
            last_flush,
            compaction_count,
        ) {
            let snapshot = self.snapshot_memory_flush_state().await;
            tokio::task::spawn_local({
                let session = self.clone();
                async move {
                    if session.run_memory_flush(trigger, Some(snapshot)).await {
                        session
                            .memory
                            .last_flush_compaction
                            .store(compaction_count, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            });
        }
    }
    /// Tag the current `session.compact` span with `mode` (and `detail`, for `segments`), the A/B variant key for grouping outcomes in telemetry.
    fn record_compaction_variant(&self) {
        let mode = self.compaction.compaction_mode;
        let span = tracing::Span::current();
        span.record("mode", tracing::field::display(mode));
        if let Some(detail) = mode.segment_detail() {
            span.record("detail", tracing::field::display(detail));
        }
    }
    /// Handle a manual `/compact`: summarize the conversation so the context shrinks.
    #[tracing::instrument(
        name = "session.compact",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            trigger = "manual",
            mode = tracing::field::Empty,
            detail = tracing::field::Empty,
            pre_tokens = tracing::field::Empty,
            post_tokens = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_compact(self: &Arc<Self>) -> Result<(), acp::Error> {
        let (_cancel, _cancel_scope) = self.compaction.cancel.enter();
        self.record_compaction_variant();
        let total_tokens = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("pre_tokens", total_tokens as i64);
        let sampling_config = self.chat_state_handle.get_sampling_config().await;
        let context_window = sampling_config
            .as_ref()
            .map(|c| c.context_window.get())
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        self.maybe_pre_compaction_flush(total_tokens, context_window, "pre_compaction")
            .await;
        if let Err(e) = self
            .run_compact_inner(
                None,
                xai_grok_telemetry::events::CompactionTrigger::Manual,
                false,
            )
            .await
        {
            let span = tracing::Span::current();
            span.record("success", false);
            span.record("error", e.to_string().as_str());
            let kind = crate::session::helpers::session_compact::compact_error_kind(&e)
                .unwrap_or(crate::session::helpers::session_compact::CompactErrorKind::Failed);
            let detail =
                Self::user_facing_compact_error(&crate::sampling::error::acp_error_message(&e));
            return Err(e.data(
                crate::session::helpers::session_compact::compact_error_data(kind, &detail),
            ));
        }
        use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
        let tokens_after = self.chat_state_handle.get_total_tokens().await;
        let span = tracing::Span::current();
        span.record("post_tokens", tokens_after as i64);
        span.record("success", true);
        self.send_xai_notification(XaiSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(total_tokens),
            tokens_after,
            elapsed_ms: None,
            summary_preview: None,
        })
        .await;
        self.emit_status_snapshot_detached();
        Ok(())
    }
    async fn emit_compact_cancelled(&self, auto_trigger: bool) -> Result<(), acp::Error> {
        if auto_trigger {
            use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
            self.send_xai_notification(XaiSessionUpdate::AutoCompactCancelled {
                reason: crate::extensions::notification::AutoCompactCancelReason::UserCancelled,
            })
            .await;
        }
        Err(crate::session::helpers::session_compact::CompactFailure::cancelled_error())
    }
    /// Suppress AUTO compaction after a deterministic failure.
    /// Emits telemetry and one notification per transition; manual `/compact` is exempt.
    /// Only `Other` shows `detail`; the canned reasons are already actionable.
    async fn suppress_auto_compaction(
        &self,
        reason: SuppressReason,
        detail: &str,
        estimated_tokens: u64,
        context_window: u64,
    ) {
        let new_state = reason.suppress_state();
        if self
            .compaction
            .auto_compact_suppressed
            .compare_exchange(
                SUPPRESS_NONE,
                new_state,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            tracing::warn!(
                suppress_reason = reason.as_ref(),
                estimated_tokens,
                context_window,
                "auto-compaction suppressed after deterministic compaction failure"
            );
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::AutoCompactSuppressed {
                    reason: reason.into(),
                    estimated_tokens,
                    context_window,
                },
            );
            self.send_xai_notification(
                crate::extensions::notification::SessionUpdate::AutoCompactFailed {
                    error: Self::suppress_notification_message(reason, detail),
                },
            )
            .await;
        }
    }
    /// Text for the suppression notification, canned per reason; `Other` carries the real error.
    /// Starts lowercase; the renderer prepends its own headline.
    fn suppress_notification_message(reason: SuppressReason, detail: &str) -> String {
        match reason {
            SuppressReason::CreditBlock => {
                "out of credits or over your spending limit. Add credits and retry.".to_string()
            }
            SuppressReason::Auth => {
                "authentication problem — re-authenticate using /login and retry.".to_string()
            }
            SuppressReason::Size => "this conversation is too large to compact.".to_string(),
            SuppressReason::Schema => "this conversation can't be summarized.".to_string(),
            SuppressReason::Other => Self::failure_with_retry_guidance(detail),
        }
    }
    /// Retry/new-session guidance headline, normalized detail on its own line.
    /// Shared by the `Other` suppress arm and the transient failure; both retry on the next turn.
    fn failure_with_retry_guidance(detail: &str) -> String {
        const RETRY_GUIDANCE: &str =
            "it'll retry on the next turn, or start a new session using /new.";
        let detail = Self::user_facing_compact_error(detail);
        if detail.is_empty() {
            RETRY_GUIDANCE.to_string()
        } else {
            format!("{RETRY_GUIDANCE}\n{detail}")
        }
    }
    /// Map a deterministic failure's error text to a fixed, content-free [`SuppressReason`] (drives telemetry and the sticky-vs-per-turn scope).
    fn classify_suppress_reason(error_msg: &str) -> SuppressReason {
        let m = error_msg.to_ascii_lowercase();
        if m.contains("spending-limit")
            || m.contains("spending limit")
            || m.contains("out of credits")
            || m.contains("usage balance exhausted")
            || m.contains("usage limit reached")
        {
            SuppressReason::CreditBlock
        } else if is_context_length_error(&m) {
            SuppressReason::Size
        } else if m.contains("status 401") || m.contains("unauthorized") {
            SuppressReason::Auth
        } else if m.contains("invalid_request_error") {
            SuppressReason::Schema
        } else {
            SuppressReason::Other
        }
    }
    /// Strip the known internal prefixes (wrappers nest), then apply the shared normalize sequence ([`normalize_compact_detail`]).
    fn user_facing_compact_error(raw: &str) -> String {
        use crate::session::helpers::session_compact::normalize_compact_detail;
        const INTERNAL_PREFIXES: &[&str] = &[
            COMPACT_FAILED_PREFIX,
            COMPACTION_FAILED_GUARD_PREFIX,
            xai_grok_compaction::sampler::SAMPLER_BUILD_FAILED_PREFIX,
            xai_grok_compaction::sampler::SAMPLER_START_FAILED_PREFIX,
        ];
        let mut rest = raw.trim();
        loop {
            let Some(prefix) = INTERNAL_PREFIXES.iter().find(|p| {
                rest.as_bytes()
                    .get(..p.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(p.as_bytes()))
            }) else {
                break;
            };
            let Some(stripped) = rest.get(prefix.len()..) else {
                break;
            };
            rest = stripped.trim_start();
        }
        normalize_compact_detail(rest)
    }
    /// True for an auth/401 compact failure; callers abort the turn for a reauth resubmit instead of sampling oversized.
    pub(crate) fn is_auth_compact_error(err: &acp::Error) -> bool {
        matches!(
            Self::classify_suppress_reason(&crate::sampling::error::acp_error_message(err)),
            SuppressReason::Auth
        )
    }
    /// Terminal auth compact failure: emit RetryState auth (reauth stash) and auth_required.
    /// Separate from `AutoCompactFailed` (user-facing); this aborts the turn.
    pub(crate) async fn surface_compact_auth_failure(&self, err: acp::Error) -> acp::Error {
        use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
        use crate::extensions::notification::UNAUTHORIZED_NEEDLE;
        let detailed = crate::sampling::error::acp_error_message(&err);
        let message = if detailed.to_ascii_lowercase().contains("unauthorized") {
            detailed
        } else {
            format!(
                "{UNAUTHORIZED_NEEDLE}: compaction failed — re-authenticate with /login \
                 and retry. ({detailed})"
            )
        };
        tracing::warn!(
            session_id = %self.session_info.id.0,
            error = %message,
            "auto-compact auth failure: aborting turn for re-auth"
        );
        xai_grok_telemetry::unified_log::warn(
            "auto-compact auth failure: aborting turn for re-auth",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "message": crate::util::truncate(&message, 300),
            })),
        );
        self.send_xai_notification(XaiSessionUpdate::RetryState(
            crate::extensions::notification::RetryState::Failed {
                error_type: "auth".to_string(),
                message: message.clone(),
            },
        ))
        .await;
        acp::Error::auth_required().data(crate::sampling::error::terminal_error_data(
            message,
            Some(401),
            xai_grok_sampler::SamplingErrorKind::Auth,
        ))
    }
    /// Clear [`SUPPRESS_AUTH`] on login/token refresh (credit suppress waits for a 200).
    pub(crate) fn clear_auth_compact_suppression(&self) {
        let _ = self.compaction.auto_compact_suppressed.compare_exchange(
            SUPPRESS_AUTH,
            SUPPRESS_NONE,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    /// Credit or auth suppress; a model switch cannot clear these.
    fn is_account_state_suppressed(&self) -> bool {
        matches!(
            self.compaction
                .auto_compact_suppressed
                .load(std::sync::atomic::Ordering::Relaxed),
            SUPPRESS_UNTIL_SUCCESS | SUPPRESS_AUTH
        )
    }
    /// Choose the post-compaction history for a forked session.
    /// Re-pin the inherited prefix, or release it when re-pinning would leave the fork at/over the auto-compact threshold.
    /// A release also sets the sticky flag and records the release span field (this runs within the `run_compact_inner` span).
    async fn resolve_forked_compacted_history(
        &self,
        compacted_history: Vec<ConversationItem>,
        prefix_len: usize,
        tokens_before: u64,
        context_window: u64,
    ) -> Vec<ConversationItem> {
        let full_conv = self.chat_state_handle.get_conversation().await;
        let compacted_len = compacted_history.len();
        let release_candidate = compacted_history.clone();
        match preserve_inherited_prefix(&full_conv, compacted_history, prefix_len) {
            Ok(preserved) => {
                let projected_preserved = project_preserved_reseed_tokens(
                    xai_chat_state::estimate_conversation_tokens(&preserved),
                    tokens_before,
                    xai_chat_state::estimate_conversation_tokens(&full_conv),
                );
                if xai_token_estimation::exceeds_threshold(
                    projected_preserved,
                    context_window,
                    self.compaction.threshold_percent.get(),
                ) {
                    self.compaction
                        .prefix_released
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::Span::current().record("compaction_prefix_released", true);
                    tracing::info!(
                        session_id = %self.session_info.id.0,
                        prefix_len,
                        projected_preserved,
                        "compaction: releasing inherited prefix under pressure"
                    );
                    release_candidate
                } else {
                    tracing::info!(
                        session_id = %self.session_info.id.0,
                        prefix_len,
                        compacted_len,
                        "Preserving inherited prefix across compaction"
                    );
                    preserved
                }
            }
            Err(original) => {
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    prefix_len,
                    conversation_len = full_conv.len(),
                    "Inherited prefix invalid, using compacted history as-is"
                );
                original
            }
        }
    }
    /// Codex remote compaction v2: the provider server compacts the conversation over the
    /// Responses wire and we splice its opaque replacement history back in. Retries with a
    /// single auth refresh inside the attempt budget; an auto trigger that still fails
    /// suppresses further auto compaction.
    /// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:819 :: run_codex_remote_compact_request (adapted: v2-only — the worktree has no v1 unary endpoint, no `codex_turn_state` (DROP 2), and auth refresh goes through `refresh_token_if_expired` via `prepare_chat_completion`; prompt-cache affinity falls back to the session id, DROP 1)
    #[allow(clippy::too_many_arguments)]
    async fn run_codex_remote_compact(
        &self,
        provider_conversation: &[ConversationItem],
        sampling_config: &xai_grok_sampler::SamplerConfig,
        binding: &mut xai_chat_state::RequestTransportBinding,
        mut client: xai_grok_sampler::SamplingClient,
        user_context: Option<&str>,
        auto_trigger: bool,
        estimated_input_tokens: u64,
        context_window: u64,
        compaction_tools: Vec<xai_grok_sampling_types::ToolSpec>,
        compaction_hosted_tools: Vec<xai_grok_sampling_types::HostedTool>,
        compaction_tool_tokens: u64,
        segment_messages: &[ConversationItem],
        tokens_before: u64,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        compact_source: &'static str,
        cancel: tokio_util::sync::CancellationToken,
        compaction: xai_grok_telemetry::events::CompactionScope,
        pre_compaction_ms: u64,
    ) -> Result<(), acp::Error> {
        const MAX_ATTEMPTS: u32 = 3;
        let mut refreshed_auth = false;
        // COMPACT-BOUNDARM-1 (apex-ayl.82): the model-bound strip-once budget
        // (one pair-atomic strip per run; a second model-bound failure
        // reports-and-stops — never replay a byte-identical failing request,
        // the compact-400 storm class) + whether the retry must omit the
        // trailing `compaction_trigger` item (the proxy rejected the item
        // itself — Family 9).
        let mut model_bound_stripped = false;
        let mut suppress_compaction_trigger = false;
        let base_instruction_count = provider_conversation
            .iter()
            .take_while(|item| matches!(item, ConversationItem::System(_)))
            .count();
        let system_items = provider_conversation[..base_instruction_count].to_vec();
        let mut instructions = system_items
            .iter()
            .map(ConversationItem::text_content)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        // Only the leading system prefix represents base instructions.
        // Preserve any later system/reminder item in its original history
        // position instead of moving it ahead of prior conversation.
        let mut compact_input = provider_conversation[base_instruction_count..].to_vec();
        if let Some(context) = user_context.filter(|value| !value.trim().is_empty()) {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(&format!(
                "<compaction_context>\n{context}\n</compaction_context>"
            ));
        }
        let cross_provider_fallback =
            xai_grok_sampling_types::codex_cross_provider_fallback(&compact_input, 32_000);
        // codex-rs evaluates remote-compaction input against the model's
        // effective context window and rewrites only trailing function
        // outputs when necessary. Base instructions and tools also occupy
        // that request budget even though they are not history items.
        let compact_input_budget = context_window
            .saturating_sub(xai_token_estimation::estimate_tokens(&instructions))
            .saturating_sub(compaction_tool_tokens);
        let rewritten_outputs = rewrite_codex_tool_outputs_to_fit_context_window(
            &mut compact_input,
            compact_input_budget,
        );
        if rewritten_outputs > 0 {
            tracing::info!(
                session_id = %self.session_info.id.0,
                rewritten_outputs,
                "rewrote trailing tool outputs before Codex remote compaction"
            );
        }
        let mut request = xai_grok_sampling_types::ConversationRequest::from_items(compact_input)
            .with_model(sampling_config.model.clone())
            .with_tools(compaction_tools);
        request.hosted_tools = compaction_hosted_tools;
        request.reasoning_effort = sampling_config.reasoning_effort;
        request.x_grok_session_id = Some(self.session_info.id.0.to_string());
        #[cfg(test)]
        binding_tests::record_remote_request(&request);
        // `prompt_cache_key` falls back to `x_grok_session_id` in the v2 body
        // builder; the worktree has no `prompt_cache_affinity_id` (DROP 1).
        let model_wait_start = std::time::Instant::now();
        for attempt in 1..=MAX_ATTEMPTS {
            let attempt_started = std::time::Instant::now();
            let result = client
                .compact_codex_conversation_v2(
                    request.clone(),
                    &instructions,
                    !suppress_compaction_trigger,
                )
                .await
                .map(|result| {
                    let usage = result.usage.as_ref();
                    tracing::info!(
                        session_id = %self.session_info.id.0,
                        response_id = %result.response_id,
                        input_tokens = usage.map(|usage| usage.input_tokens),
                        output_tokens = usage.map(|usage| usage.output_tokens),
                        cached_tokens = usage
                            .map(|usage| usage.input_tokens_details.cached_tokens),
                        "Codex remote compaction v2 stream completed"
                    );
                    if let Some(usage) = usage {
                        let usage = codex_compaction_v2_token_usage(usage);
                        let api_duration_ms = u64::try_from(attempt_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX);
                        self.chat_state_handle.record_model_call_usage(
                            request.model.clone(),
                            usage.clone(),
                            Some(api_duration_ms),
                            None,
                        );
                        self.signals_handle()
                            .record_token_usage(usage.completion_tokens, usage.reasoning_tokens);
                    }
                    build_codex_remote_compaction_v2_history(&request.items, result.compaction_item)
                });
            match result {
                Ok(items) => {
                    return self
                        .install_codex_remote_compacted_history(
                            items,
                            cross_provider_fallback,
                            system_items,
                            provider_conversation,
                            segment_messages,
                            tokens_before,
                            auto_continue,
                            compact_source,
                            attempt,
                            cancel,
                            auto_trigger,
                            compaction,
                            xai_grok_telemetry::events::CompactionTiming {
                                model_wait_ms: Some(model_wait_start.elapsed().as_millis() as u64),
                                pre_compaction_ms: Some(pre_compaction_ms),
                                post_compaction_ms: None,
                            },
                        )
                        .await;
                }
                Err(error) => {
                    if error.is_auth_error()
                        && codex_compaction_auth_refresh_allowed(
                            attempt,
                            MAX_ATTEMPTS,
                            refreshed_auth,
                        )
                    {
                        refreshed_auth = true;
                        // Worktree adaptation of OG's `codex_auth::force_refresh` +
                        // `update_codex_chat_credentials`: refresh the token, then
                        // rebuild the client from the reconstructed config.
                        client = self
                            .prepare_chat_completion_from_binding(binding, false)
                            .await?;
                        continue;
                    }
                    // COMPACT-BOUNDARM-1 (apex-ayl.82): the model-bound arm.
                    // A model-bound 400 (the proxy's
                    // `Unsupported Responses API input item type:
                    // "compaction_trigger"` rejection, an encrypted-
                    // continuation or strict-schema item-shape rejection) is
                    // not fixed by re-sending the same shape — but stripping
                    // the model-bound state ONCE (pair-atomic) and re-issuing
                    // can. One strip per run: a second model-bound failure
                    // reports-and-stops (retry-budget stop; the incident
                    // storm retried the same failing compact shape 15x).
                    if error.is_model_bound_history_error() && !model_bound_stripped {
                        model_bound_stripped = true;
                        let stripped_items = drop_model_bound_items(&mut request.items);
                        // The item itself is the offending shape: the retry
                        // must omit the trailing trigger item (the Codex-
                        // native protocol marker a proxy does not model). Any
                        // OTHER model-bound 400 keeps the trigger.
                        suppress_compaction_trigger |=
                            error.is_compaction_trigger_item_rejection();
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            attempt,
                            stripped_items,
                            suppress_compaction_trigger,
                            error = %error,
                            "model-bound compact 400: stripping model-bound state once (pair-atomic) before the retry — a second model-bound failure stops (no byte-identical replay)"
                        );
                        continue;
                    }
                    let retry_after = match &error {
                        xai_grok_sampling_types::SamplingError::Api {
                            retry_after_secs, ..
                        } => *retry_after_secs,
                        _ => None,
                    };
                    if attempt < MAX_ATTEMPTS && error.is_retryable() {
                        let delay = retry_after.unwrap_or(3).min(120);
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            attempt,
                            delay,
                            error = %error,
                            "retrying Codex remote compaction request"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        continue;
                    }
                    if auto_trigger {
                        let reason = Self::classify_suppress_reason(&error.to_string());
                        self.suppress_auto_compaction(
                            reason,
                            &error.to_string(),
                            estimated_input_tokens,
                            context_window,
                        )
                        .await;
                    }
                    return Err(self.to_acp_error(error));
                }
            }
        }
        unreachable!("Codex compact retry loop returns on every terminal attempt")
    }
    /// Installs the provider's replacement history for Codex remote compaction v2:
    /// reattach the local system prefix, keep any in-flight user interjections,
    /// persist the segment/checkpoint, swap the conversation, and reset the
    /// compaction-adjacent session state.
    /// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:959 :: install_codex_remote_compacted_history (adapted: v2-only; the worktree has no `mark_history_compacted` experience ledger and uses `rearm_failed_server_announcements` for announcement state)
    #[allow(clippy::too_many_arguments)]
    async fn install_codex_remote_compacted_history(
        &self,
        mut compacted_history: Vec<ConversationItem>,
        cross_provider_fallback: String,
        system_items: Vec<ConversationItem>,
        conversation_snapshot: &[ConversationItem],
        segment_messages: &[ConversationItem],
        tokens_before: u64,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        compact_source: &'static str,
        attempts: u32,
        cancel: tokio_util::sync::CancellationToken,
        auto_trigger: bool,
        compaction: xai_grok_telemetry::events::CompactionScope,
        mut timing: xai_grok_telemetry::events::CompactionTiming,
    ) -> Result<(), acp::Error> {
        let post_compaction_start = std::time::Instant::now();
        if compacted_history.is_empty() {
            return Err(acp::Error::internal_error()
                .data("Codex remote compaction v2 returned no replacement history"));
        }
        if !cross_provider_fallback.is_empty() {
            for item in compacted_history.iter_mut().rev() {
                let ConversationItem::BackendToolCall(item) = item else {
                    continue;
                };
                let xai_grok_sampling_types::BackendToolKind::CodexRawInput(raw) = &mut item.kind
                else {
                    continue;
                };
                let item_type = raw.raw.get("type").and_then(serde_json::Value::as_str);
                if matches!(item_type, Some("compaction" | "context_compaction")) {
                    raw.cross_provider_fallback = Some(cross_provider_fallback);
                    break;
                }
            }
        }
        let compacted_history_chars = compacted_history
            .iter()
            .map(|item| match item {
                ConversationItem::BackendToolCall(item) => item.estimated_content_len(),
                // The real payload, not `text_content()`: for a discovery item the
                // bounded summary understates a multi-KB `tool_search_output` by orders
                // of magnitude (tool_search.rs), and this number feeds the
                // `compaction_summary_chars` telemetry a post-compaction cap check is
                // judged against.
                ConversationItem::Discovery { item } => item.estimated_model_visible_len(),
                ConversationItem::System(_)
                | ConversationItem::User(_)
                | ConversationItem::Assistant(_)
                | ConversationItem::ToolResult(_)
                | ConversationItem::Reasoning(_) => item.text_content().chars().count(),
            })
            .sum::<usize>();
        // Base instructions are request controls in codex-rs rather than part
        // of replacement history. The unified conversation stores them as
        // leading System items, so reattach them locally while keeping every
        // provider output item exact and ordered after that prefix.
        let mut replacement = system_items;
        replacement.append(&mut compacted_history);
        let current = self.chat_state_handle.get_conversation().await;
        let interjections = codex_remote_compaction_v2_interjections(
            conversation_snapshot,
            &current,
        )
        .ok_or_else(|| {
            acp::Error::internal_error().data(
                "conversation changed while Codex remote compaction v2 was in flight; refusing stale replacement history",
            )
        })?;
        if !interjections.is_empty() {
            tracing::info!(
                session_id = %self.session_info.id.0,
                interjection_count = interjections.len(),
                "preserving user interjections received during Codex remote compaction v2"
            );
            replacement.extend(interjections);
        }
        let segments_queued = u32::from(self.persist_compaction_segment(
            segment_messages,
            "[OpenAI server-side compacted context]",
        ));
        let prompt_index_at_compaction = self.chat_state_handle.get_prompt_index().await;
        let original_user_info = self
            .chat_state_handle
            .get_conversation_item_at(1)
            .await
            .and_then(|item| match item {
                ConversationItem::User(parts) => {
                    parts
                        .content
                        .into_iter()
                        .next()
                        .and_then(|part| match part {
                            xai_grok_sampling_types::ContentPart::Text { text } => {
                                Some(text.as_ref().to_owned())
                            }
                            // The first part of a stored `User` row is whatever the harness
                            // put there; a non-text part is not the user_info text.
                            _ => None,
                        })
                }
                // Stored index 1 is the `User(user_info)` row. Anything else — including a
                // discovery row, which is provider state and is never the user_info source —
                // simply has no user_info text (cut review WAJ21R2-13: named so this arm is a
                // decision and not a silent catch-all over the seventh variant).
                ConversationItem::Discovery { .. }
                | ConversationItem::System(_)
                | ConversationItem::Assistant(_)
                | ConversationItem::ToolResult(_)
                | ConversationItem::BackendToolCall(_)
                | ConversationItem::Reasoning(_) => None,
            });
        if cancel.is_cancelled() {
            return self.emit_compact_cancelled(auto_trigger).await;
        }
        self.chat_state_handle
            .record_compaction_at(prompt_index_at_compaction);
        self.persist_compaction_checkpoint(
            &replacement,
            prompt_index_at_compaction,
            auto_continue,
            original_user_info,
        );
        if self.startup_hints.inherited_prefix_len.is_some() {
            // The server compacted the complete fork history into its opaque
            // replacement item. Re-pinning the old prefix would defeat that
            // compaction and immediately retrigger the threshold.
            self.compaction
                .prefix_released
                .store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::Span::current().record("compaction_prefix_released", true);
        }
        let new_len = replacement.len();
        self.chat_state_handle
            .replace_conversation_for_compaction(replacement);
        self.compaction
            .auto_compact_suppressed
            .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
        self.last_idle_flush_conversation_len
            .store(new_len, std::sync::atomic::Ordering::Relaxed);
        self.memory
            .context_injected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::PlanState(
                crate::tools::todo::TodoState::default(),
            ));
        self.agent
            .borrow()
            .tool_bridge()
            .on_agents_md_compaction()
            .await;
        self.agent
            .borrow()
            .tool_bridge()
            .on_skill_discovery_compaction()
            .await;
        self.rearm_failed_server_announcements().await;
        self.plan_mode.lock().reset_after_compaction();
        self.persist_plan_mode_state();
        self.dispatch_hook(
            xai_grok_hooks::event::HookEventName::PostCompact,
            xai_grok_hooks::event::HookPayload::PostCompact {
                source: compact_source.into(),
            },
            None,
        )
        .await;
        let tokens_after = self.chat_state_handle.get_total_tokens().await;
        let span = tracing::Span::current();
        span.record("compaction_tokens_after", tokens_after as i64);
        span.record("compaction_summary_chars", compacted_history_chars as i64);
        span.record("compaction_attempts", attempts as i64);
        span.record("compaction_degenerate_rejections", 0i64);
        span.record("compaction_input_overflow_rejections", 0i64);
        span.record("compaction_deterministic_rejections", 0i64);
        span.record("compaction_transient_rejections", 0i64);
        span.record("compaction_stop_reason", "responses_compaction_v2");
        span.record("compaction_outcome", CompactionOutcome::Success.as_ref());
        span.record("compaction_delta_count", 0i64);
        timing.post_compaction_ms = Some(post_compaction_start.elapsed().as_millis() as u64);
        compaction.complete(
            xai_grok_telemetry::events::CompactionCompleteStats {
                tokens_after,
                two_pass_used: false,
                segments_queued,
                degenerate_retries: 0,
                input_overflow_retries: 0,
            },
            timing,
        );
        tracing::info!(
            session_id = %self.session_info.id.0,
            tokens_before,
            tokens_after,
            attempts,
            "installed Codex remote-compaction v2 replacement history"
        );
        Ok(())
    }
    // apex-ayl.89 (C1-lite): project the candidate history to its final
    // Messages form and run the N3 per-item caps. Items + the session's
    // CURRENT model + the session's cache_ttl (tools are uniform across
    // every turn of the session — not a compaction-specific vector; an
    // over-cap tool definition bricks every turn regardless of
    // compaction — pre-existing, out of scope). model + cache_ttl come
    // from the SAME source the turn's request uses (request_builder.rs:85
    // — chat-state actor turn-request source; sampler_turn.rs:777 — shell
    // client-config source; both read sampling_config.cache_ttl, and
    // get_sampling_config() here is the same state as the actor side —
    // fix-pass 2 R5; the compaction summarization call at :1404 is the
    // precedent) so R8 thinking suppression and the head system block's
    // ttl field project identically (fix-pass 1 m1/m2).
    async fn projected_caps_ok(
        &self,
        history: &[ConversationItem],
    ) -> Result<(), RequestValidationError> {
        let cfg = self.chat_state_handle.get_sampling_config().await;
        let request = ConversationRequest {
            items: history.to_vec(),
            model: cfg.as_ref().map(|c| c.model.clone()),
            cache_ttl: cfg.and_then(|c| c.cache_ttl),
            thinking_replay: None,
            ..Default::default() // conversation.rs:778 derives Default (fix-pass 1 m3)
        };
        let projected = build_messages_request(&request);
        xai_grok_sampling_types::request_validation::check_messages_request_caps(&projected)
    }

    /// apex-ayl.89 (C1-lite): the fail-loud contract for a pre-install
    /// per-item cap failure — nothing is persisted, nothing replaced.
    /// The ordering below is BINDING (fix-pass 1 M1): on the AUTO trigger
    /// the `AutoCompactFailed` notification goes out BEFORE the sticky
    /// suppression store — the `run_compact_only` Err arm's
    /// `!cancelled && !is_suppressed()` gate (compaction_config.rs:189)
    /// would otherwise silence it. Manual trigger: no notification — the
    /// Err already surfaces through the slash path, which never consults
    /// the suppression gate (no double-emit anywhere: by the time any Err
    /// reaches that arm, the sticky store has run).
    async fn reject_over_cap_install(
        &self,
        e: &RequestValidationError,
        auto_trigger: bool,
    ) -> acp::Error {
        use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
        tracing::error!(
            session_id = %self.session_info.id,
            error = %e,
            "compaction: candidate history violates the wire per-item cap — \
             install rejected, session not compacted"
        );
        let detail = format!(
            "compaction produced a history that violates the wire per-item cap \
             ({e}); session not compacted — rewind to before compaction (reliable \
             escape); retry /compact only if the overage was coalescing — an \
             intrinsic over-cap piece re-fails deterministically"
        );
        let acp_error = acp::Error::internal_error().data(detail);
        if auto_trigger {
            self.send_xai_notification(XaiSessionUpdate::AutoCompactFailed {
                error: Self::failure_with_retry_guidance(
                    &crate::sampling::error::acp_error_message(&acp_error),
                ),
            })
            .await;
        }
        self.compaction
            .auto_compact_suppressed
            .store(SUPPRESS_STICKY, std::sync::atomic::Ordering::Relaxed);
        acp_error
    }
    /// Inner implementation of compaction that supports an optional `auto_continue` payload for the checkpoint.
    #[tracing::instrument(
        name = "session.compact_inner",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            compaction_tokens_before = tracing::field::Empty,
            compaction_tokens_after = tracing::field::Empty,
            compaction_summary_chars = tracing::field::Empty,
            compaction_degenerate_rejections = tracing::field::Empty,
            compaction_input_overflow_rejections = tracing::field::Empty,
            compaction_deterministic_rejections = tracing::field::Empty,
            compaction_transient_rejections = tracing::field::Empty,
            compaction_attempts = tracing::field::Empty,
            compaction_trigger = tracing::field::Empty,
            compaction_trigger_pct = tracing::field::Empty,
            compaction_threshold_pct = tracing::field::Empty,
            compaction_outcome = tracing::field::Empty,
            compaction_failure_reason = tracing::field::Empty,
            compaction_stop_reason = tracing::field::Empty,
            compaction_ttft_ms = tracing::field::Empty,
            compaction_stream_ms = tracing::field::Empty,
            compaction_delta_count = tracing::field::Empty,
            compaction_itl_max_ms = tracing::field::Empty,
            compaction_two_pass_used = tracing::field::Empty,
            compaction_prefire_hit = tracing::field::Empty,
            compaction_pass2_latency_ms = tracing::field::Empty,
            compaction_prefire_waited_ms = tracing::field::Empty,
            compaction_prefire_stale = tracing::field::Empty,
            compaction_prefix_released = tracing::field::Empty,
        )
    )]
    async fn run_compact_inner(
        &self,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        trigger: xai_grok_telemetry::events::CompactionTrigger,
        lossy_input: bool,
    ) -> Result<(), acp::Error> {
        let (cancel, _cancel_scope) = self.compaction.cancel.enter();
        let tokens_before = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("compaction_tokens_before", tokens_before as i64);
        self.signals_handle().record_compaction(tokens_before);
        let trigger_str = match trigger {
            xai_grok_telemetry::events::CompactionTrigger::Manual => "manual",
            xai_grok_telemetry::events::CompactionTrigger::Auto => "auto",
        };
        let mut binding = self.capture_transport_binding_or_default().await;
        #[cfg(test)]
        binding_tests::after_capture(self, &binding).await;
        let sampling_config = &binding.config;
        let context_window = sampling_config.context_window.get();
        {
            let span = tracing::Span::current();
            let trigger_pct = if context_window == 0 {
                0
            } else {
                ((tokens_before as f64 / context_window as f64) * 100.0).round() as i64
            };
            span.record("compaction_trigger_pct", trigger_pct);
            span.record(
                "compaction_threshold_pct",
                self.compaction.threshold_percent.get() as i64,
            );
            span.record("compaction_trigger", trigger_str);
        }
        let summary_strips_reasoning = sampling_config.api_backend == ApiBackend::Messages;
        let model_id = sampling_config.model.clone();
        let compaction = xai_grok_telemetry::events::CompactionScope::begin(
            xai_grok_telemetry::events::CompactionBeginParams {
                trigger,
                tokens_used: tokens_before,
                context_window,
                model_id: model_id.clone(),
                compaction_mode: match self.compaction.compaction_mode {
                    xai_chat_state::CompactionMode::Summary => {
                        xai_grok_telemetry::events::CompactionModeLabel::Summary
                    }
                    xai_chat_state::CompactionMode::Transcript => {
                        xai_grok_telemetry::events::CompactionModeLabel::Transcript
                    }
                    xai_chat_state::CompactionMode::Segments(_) => {
                        xai_grok_telemetry::events::CompactionModeLabel::Segments
                    }
                },
                two_pass_enabled: self.two_pass_active(),
                is_subagent: self.startup_hints.is_subagent,
            },
        );
        let user_context = self.goal_compaction_user_context();
        let compact_source = trigger_str;
        self.dispatch_hook(
            xai_grok_hooks::event::HookEventName::PreCompact,
            xai_grok_hooks::event::HookPayload::PreCompact {
                source: compact_source.into(),
            },
            None,
        )
        .await;
        let max_retries = 3u32;
        let retry_delay_secs = 3u64;
        let (conv_len, system_message, full_conversation) = tokio::join!(
            self.chat_state_handle.get_conversation_len(),
            self.chat_state_handle.get_system_message(),
            self.chat_state_handle.get_conversation(),
        );
        // The Codex v2 branch below operates on the raw provider conversation
        // (leading system prefix included); `simplified_messages` is the separate
        // summarization copy built just after.
        let codex_provider_conversation = full_conversation.clone();
        let assembly_start = std::time::Instant::now();
        let segment_messages = if self.compaction.compaction_mode.writes_segments() {
            xai_chat_state::compaction_utils::prepare_conversation_for_segment(
                full_conversation.clone(),
            )
        } else {
            Vec::new()
        };
        let verbatim_input_enabled = self.compaction.verbatim_input && !lossy_input;
        let mut simplified_messages = if verbatim_input_enabled {
            xai_chat_state::compaction_utils::prepare_conversation_for_verbatim_summarization(
                full_conversation,
                summary_strips_reasoning,
            )
        } else {
            xai_chat_state::compaction_utils::prepare_conversation_for_summarization(
                full_conversation,
            )
        };
        if verbatim_input_enabled {
            simplified_messages =
                apply_turn_image_budget_and_prune(&self.chat_state_handle, simplified_messages)
                    .await;
        }
        let pre_compaction_ms = assembly_start.elapsed().as_millis() as u64;
        if conv_len == 0 {
            tracing::error!(
                session_id = %self.session_info.id.0,
                "Compaction failed: conversation is empty (ChatStateActor may have died)"
            );
            return Err(acp::Error::internal_error().data(format!(
                "{COMPACTION_FAILED_GUARD_PREFIX}conversation is empty"
            )));
        }
        let system_message = match system_message {
            Some(msg) => msg,
            None => {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    conversation_len = conv_len,
                    "Compaction failed: no system message in conversation history"
                );
                return Err(acp::Error::internal_error().data(format!(
                    "{COMPACTION_FAILED_GUARD_PREFIX}no system message in conversation history"
                )));
            }
        };
        if simplified_messages.is_empty() {
            tracing::error!(
                session_id = %self.session_info.id.0,
                conversation_len = conv_len,
                "Compaction failed: simplified conversation is empty"
            );
            return Err(acp::Error::internal_error().data(format!(
                "{COMPACTION_FAILED_GUARD_PREFIX}simplified conversation is empty"
            )));
        }
        if !simplified_messages
            .iter()
            .any(|msg| matches!(msg, ConversationItem::System(_)))
        {
            tracing::error!(
                session_id = %self.session_info.id.0,
                conversation_len = conv_len,
                simplified_len = simplified_messages.len(),
                "Compaction failed: no system message in simplified conversation"
            );
            return Err(acp::Error::internal_error().data(format!(
                "{COMPACTION_FAILED_GUARD_PREFIX}no system message in simplified conversation"
            )));
        }
        let sampling_config = self.reconstruct_full_config_from_binding(&binding).await;
        #[cfg(test)]
        binding_tests::record_reconstruction(&sampling_config);
        let sampling_client = self
            .prepare_chat_completion_from_binding(&mut binding, false)
            .await?;
        let backend_search_active = self.backend_search_active();
        let effective_tool_defs: Vec<xai_grok_sampling_types::ToolDefinition> = self
            .prepare_tool_definitions()
            .await
            .into_iter()
            .filter(|td| !backend_search_active || td.function.name != "web_search")
            .collect();
        let compaction_tool_tokens =
            xai_chat_state::estimate_tool_definitions_tokens(&effective_tool_defs);
        #[cfg(test)]
        let compaction_token_input = serde_json::to_value(&effective_tool_defs).unwrap();
        let compaction_tools: Vec<xai_grok_sampling_types::ToolSpec> = effective_tool_defs
            .into_iter()
            .map(xai_grok_sampling_types::ToolSpec::from)
            .collect();
        let compaction_hosted_tools: Vec<xai_grok_sampling_types::HostedTool> =
            self.hosted_tools_for_turn();
        #[cfg(test)]
        binding_tests::record_inputs(binding_tests::GeneratorInputs {
            site: "full",
            history: simplified_messages.clone(),
            tools: compaction_tools.clone(),
            hosted: compaction_hosted_tools.clone(),
            token_kind: "definitions",
            token_input: compaction_token_input,
            tool_tokens: compaction_tool_tokens,
            config: sampling_config.clone(),
            client_backend: sampling_client.api_backend(),
            idle_timeout: self.inference_idle_timeout,
            wall_clock_budget_secs: self
                .agent
                .borrow()
                .compaction_policy()
                .wall_clock_budget_secs,
            tool_choice: self.compaction.tool_choice,
        });
        if lossy_input {
            simplified_messages = xai_chat_state::compaction_utils::fit_conversation_to_budget(
                simplified_messages,
                lossy_input_budget(context_window, compaction_tool_tokens),
            );
        }
        tracing::info!(
            num_tools = compaction_tools.len(),
            tool_tokens = compaction_tool_tokens,
            "Running compact with model '{}' (user model: '{}')",
            &sampling_config.model,
            &sampling_config.model
        );
        let mut last_error: Option<acp::Error> = None;
        let mut last_failure_outcome = CompactionOutcome::Failed;
        let mut last_failure_reason: Option<SuppressReason> = None;
        let mut input_stage = if verbatim_input_enabled {
            let (turns, stage) = start_verbatim_compact_turns(
                simplified_messages,
                compaction_tool_tokens,
                context_window,
            );
            simplified_messages = turns;
            stage
        } else {
            CompactInputStage::Lossy
        };
        let use_short_prompt = false;
        let started_at = chrono::Utc::now().to_rfc3339();
        let estimated_input_tokens =
            xai_chat_state::estimate_conversation_tokens(&simplified_messages);
        let auto_trigger = matches!(trigger, xai_grok_telemetry::events::CompactionTrigger::Auto);
        // Codex remote compaction v2 (provider-side): default-on for the codex family
        // on the Responses backend; the policy flag or a non-codex model stays local.
        if self.agent.borrow().compaction_policy().remote_compaction_v2
            && sampling_config.api_backend == ApiBackend::Responses
            && let Some("codex") = self.codex_model_family(&sampling_config.model).as_deref()
        {
            return self
                .run_codex_remote_compact(
                    &codex_provider_conversation,
                    &sampling_config,
                    &mut binding,
                    sampling_client,
                    user_context.as_deref(),
                    auto_trigger,
                    estimated_input_tokens,
                    context_window,
                    compaction_tools,
                    compaction_hosted_tools,
                    compaction_tool_tokens,
                    &segment_messages,
                    tokens_before,
                    auto_continue,
                    compact_source,
                    cancel,
                    compaction,
                    pre_compaction_ms,
                )
                .await;
        }
        let wall_clock_budget_secs = self
            .agent
            .borrow()
            .compaction_policy()
            .wall_clock_budget_secs;
        let sampler = crate::session::helpers::full_replace_compaction::ShellCompactionSampler::new(
            use_short_prompt,
            user_context.clone(),
            compaction_tools.clone(),
            compaction_hosted_tools.clone(),
            compaction_tool_tokens,
            sampling_client,
            self.session_info.id.clone(),
            sampling_config.clone(),
            self.inference_idle_timeout,
            wall_clock_budget_secs,
            self.compaction.tool_choice,
            cancel.clone(),
        );
        let observer =
            crate::session::helpers::full_replace_compaction::ShellFullReplaceObserver::new(
                trigger,
                context_window,
                compaction.compaction_id.clone(),
                self.session_info.id.0.to_string(),
                estimated_input_tokens,
                retry_delay_secs,
            );
        let fr_config = xai_grok_compaction::FullReplaceConfig {
            max_attempts: max_retries,
            retry_delay_secs,
            sampling_timeout_secs: 0,
        };
        let mut request_turns = simplified_messages.clone();
        let mut input_overflow_rejections: u32 = 0;
        // COMPACT-BOUNDARM-1 (apex-ayl.82): the model-bound strip-once
        // budget — at most ONE pair-atomic strip per compact run; a second
        // model-bound failure reports-and-stops (never replay a
        // byte-identical failing request — the compact-400 storm class).
        let mut model_bound_stripped = false;
        let two_pass_output = self
            .try_two_pass_pass2_apply(user_context.as_deref(), summary_strips_reasoning)
            .await;
        let two_pass_used = two_pass_output.is_some();
        let mut compact_summary: Option<String> =
            two_pass_output.as_ref().map(|o| o.content.clone());
        while compact_summary.is_none() {
            match xai_grok_compaction::sample_full_replace_summary(
                &sampler,
                &request_turns,
                user_context.as_deref(),
                &fr_config,
                &observer,
            )
            .await
            {
                Ok(summary) => {
                    compact_summary = Some(summary.summary);
                    break;
                }
                Err(xai_grok_compaction::FullReplaceError::NothingToCompact) => {
                    last_error = Some(
                        acp::Error::internal_error()
                            .data(format!("{COMPACT_FAILED_PREFIX}nothing to compact")),
                    );
                    break;
                }
                Err(xai_grok_compaction::FullReplaceError::EmptyResponse) => {
                    last_failure_outcome = if observer.degenerate_seen() {
                        CompactionOutcome::Degenerate
                    } else {
                        CompactionOutcome::Transient
                    };
                    last_error = Some(acp::Error::internal_error().data(
                        observer.last_error_message().unwrap_or_else(|| {
                            format!("{COMPACT_FAILED_PREFIX}model returned empty response")
                        }),
                    ));
                    break;
                }
                Err(xai_grok_compaction::FullReplaceError::Sampler {
                    message,
                    deterministic,
                    context_overflow,
                    model_bound,
                }) => {
                    if cancel.is_cancelled()
                        || message.contains(
                            crate::session::helpers::session_compact::COMPACT_CANCELLED_MSG,
                        )
                    {
                        return self.emit_compact_cancelled(auto_trigger).await;
                    }
                    // COMPACT-BOUNDARM-1 (apex-ayl.82): the model-bound arm.
                    // A model-bound 400 (encrypted continuation, strict-schema
                    // item shape, unsupported compaction-trigger item, 401
                    // tags-config) is not fixed by re-sending the same shape,
                    // but stripping the model-bound state ONCE (pair-atomic)
                    // and re-issuing can — the same arm the ordinary turn path
                    // takes. One strip per run: a second model-bound failure
                    // on the stripped input reports-and-stops (retry-budget
                    // stop; the compact-400 storm retried the same failing
                    // shape 15x).
                    if model_bound {
                        if model_bound_stripped {
                            tracing::warn!(
                                session_id = %self.session_info.id.0,
                                error = %message,
                                "model-bound compact failure after the one strip: the stripped input still trips the boundary — reporting and stopping (no byte-identical replay)"
                            );
                        } else {
                            model_bound_stripped = true;
                            let stripped = drop_model_bound_items(&mut request_turns);
                            if stripped == 0 {
                                // Fail-closed: this pass's input carries no
                                // model-bound state — the retry would be
                                // byte-identical; report-and-stop.
                                tracing::warn!(
                                    session_id = %self.session_info.id.0,
                                    error = %message,
                                    "model-bound compact rejection with nothing model-bound in the compact input — re-sending cannot help; stopping"
                                );
                                last_failure_outcome = CompactionOutcome::Deterministic;
                                if auto_trigger {
                                    let reason = Self::classify_suppress_reason(&message);
                                    self.suppress_auto_compaction(
                                        reason,
                                        &message,
                                        estimated_input_tokens,
                                        context_window,
                                    )
                                    .await;
                                }
                                last_error = Some(acp::Error::internal_error().data(message));
                                break;
                            }
                            xai_grok_telemetry::session_ctx::log_event(
                                xai_grok_telemetry::events::CompactionRetryDegraded {
                                    trigger,
                                    reason: "model_bound_strip",
                                    from_stage: Some(input_stage.into()),
                                    to_stage: Some(input_stage.into()),
                                    summary_chars: None,
                                    attempt: observer.attempt_count(),
                                    context_window,
                                    compaction_id: compaction.compaction_id.clone(),
                                },
                            );
                            tracing::warn!(
                                session_id = %self.session_info.id.0,
                                stripped,
                                error = %message,
                                "model-bound compact 400: stripped {stripped} model-bound item(s) once (pair-atomic); retrying the stripped input — a second model-bound failure stops"
                            );
                            continue;
                        }
                        last_failure_outcome = CompactionOutcome::Deterministic;
                        if auto_trigger {
                            let reason = Self::classify_suppress_reason(&message);
                            self.suppress_auto_compaction(
                                reason,
                                &message,
                                estimated_input_tokens,
                                context_window,
                            )
                            .await;
                        }
                        last_error = Some(acp::Error::internal_error().data(message));
                        break;
                    }
                    if context_overflow {
                        let next_stage = match input_stage {
                            CompactInputStage::Verbatim => Some(CompactInputStage::VerbatimFitted),
                            CompactInputStage::VerbatimFitted => Some(CompactInputStage::Lossy),
                            CompactInputStage::Lossy => None,
                        };
                        if let Some(stage) = next_stage {
                            input_overflow_rejections += 1;
                            xai_grok_telemetry::session_ctx::log_event(
                                xai_grok_telemetry::events::CompactionRetryDegraded {
                                    trigger,
                                    reason: "input_overflow",
                                    from_stage: Some(input_stage.into()),
                                    to_stage: Some(stage.into()),
                                    summary_chars: None,
                                    attempt: observer.attempt_count(),
                                    context_window,
                                    compaction_id: compaction.compaction_id.clone(),
                                },
                            );
                            tracing::warn!(
                                session_id = %self.session_info.id.0,
                                ?stage,
                                error = %message,
                                "Compaction input overflowed deterministically; stepping down the input ladder to avoid an incompactable state"
                            );
                            let conv = self.chat_state_handle.get_conversation().await;
                            request_turns = match stage {
                                CompactInputStage::VerbatimFitted => {
                                    let verbatim = xai_chat_state::compaction_utils::prepare_conversation_for_verbatim_summarization(
                                        conv,
                                        summary_strips_reasoning,
                                    );
                                    let pruned = apply_turn_image_budget_and_prune(
                                            &self.chat_state_handle,
                                            verbatim,
                                        )
                                        .await;
                                    xai_chat_state::compaction_utils::fit_conversation_to_budget(
                                        pruned,
                                        fitted_input_budget(context_window, compaction_tool_tokens),
                                    )
                                }
                                CompactInputStage::Lossy => {
                                    xai_chat_state::compaction_utils::fit_conversation_to_budget(
                                        xai_chat_state::compaction_utils::prepare_conversation_for_summarization(
                                            conv,
                                        ),
                                        lossy_input_budget(context_window, compaction_tool_tokens),
                                    )
                                }
                                CompactInputStage::Verbatim => {
                                    unreachable!("ladder only steps forward")
                                }
                            };
                            input_stage = stage;
                            continue;
                        }
                        last_failure_outcome = CompactionOutcome::Deterministic;
                        last_failure_reason = Some(SuppressReason::Size);
                        if auto_trigger {
                            self.suppress_auto_compaction(
                                SuppressReason::Size,
                                &message,
                                estimated_input_tokens,
                                context_window,
                            )
                            .await;
                        }
                        last_error = Some(acp::Error::internal_error().data(message));
                        break;
                    }
                    if deterministic {
                        last_failure_outcome = CompactionOutcome::Deterministic;
                        let reason = Self::classify_suppress_reason(&message);
                        last_failure_reason = Some(reason);
                        if auto_trigger {
                            self.suppress_auto_compaction(
                                reason,
                                &message,
                                estimated_input_tokens,
                                context_window,
                            )
                            .await;
                        }
                        last_error = Some(acp::Error::internal_error().data(message));
                        break;
                    }
                    last_failure_outcome = CompactionOutcome::Transient;
                    last_error = Some(acp::Error::internal_error().data(message));
                    break;
                }
            }
        }
        let telemetry = observer.into_telemetry();
        if two_pass_output.is_none()
            && let Some(request_chat_history) = sampler.take_last_attempted_items()
        {
            self.persist_compaction_request_artifact(
                request_chat_history,
                compaction_tools,
                use_short_prompt,
                &sampling_config.model,
                trigger,
                compact_summary
                    .as_deref()
                    .or(telemetry.last_rejected_summary.as_deref()),
                last_error.as_ref(),
                telemetry.attempts,
                telemetry.attempt_details,
                started_at,
            );
        }
        let compact_output = match compact_summary {
            Some(_) => match two_pass_output {
                Some(tp) => tp,
                None => sampler
                    .take_last_success()
                    .expect("a successful full-replace sample stashes its CompactOutput"),
            },
            None => {
                let span = tracing::Span::current();
                span.record("compaction_attempts", telemetry.attempts as i64);
                span.record(
                    "compaction_degenerate_rejections",
                    telemetry.degenerate_rejections as i64,
                );
                span.record(
                    "compaction_input_overflow_rejections",
                    input_overflow_rejections as i64,
                );
                span.record(
                    "compaction_deterministic_rejections",
                    telemetry.deterministic_rejections as i64,
                );
                span.record(
                    "compaction_transient_rejections",
                    telemetry.transient_rejections as i64,
                );
                span.record("compaction_outcome", last_failure_outcome.as_ref());
                if let Some(reason) = last_failure_reason {
                    span.record("compaction_failure_reason", reason.as_ref());
                }
                return Err(last_error.unwrap_or_else(|| {
                    acp::Error::internal_error().data("compaction failed: unknown error")
                }));
            }
        };
        let generate_session_compact = compact_output.content.clone();
        let user_message_prefix = self.build_user_message_prefix().await;
        let conversation = self.chat_state_handle.get_conversation().await;
        let (discovered_agents_md, all_skills_for_compaction, _edited_paths, mut state_context) =
            if use_short_prompt {
                let empty_edited: std::collections::BTreeSet<String> = Default::default();
                let ctx = CompactionStateContext::build(
                    &conversation,
                    CompactionInputs {
                        goal_objective: self.goal_objective_for_compaction(),
                        ..Default::default()
                    },
                )
                .await;
                (Vec::<std::path::PathBuf>::new(), vec![], empty_edited, ctx)
            } else {
                let agents_md: Vec<std::path::PathBuf> = self
                    .agent
                    .borrow()
                    .tool_bridge()
                    .agents_md_reminded_paths()
                    .await
                    .into_iter()
                    .collect();
                let skills = self.slash_skills_for_resolve().await;
                let edited_paths = self.chat_state_handle.get_agent_edited_paths().await;
                let ctx = {
                    let bridge_tasks = self
                        .agent
                        .borrow()
                        .tool_bridge()
                        .list_background_tasks()
                        .await;
                    let pending_tasks: Vec<_> =
                        bridge_tasks.into_iter().filter(|t| !t.completed).collect();
                    let (execute_tool_name, monitor_tool_name) = if pending_tasks.is_empty() {
                        (None, None)
                    } else {
                        let agent_ref = self.agent.borrow();
                        let bridge = agent_ref.tool_bridge();
                        let empty = serde_json::json!({});
                        let execute = bridge
                            .render_prompt("${{ tools.by_kind.execute }}", &empty)
                            .await
                            .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                        let monitor = bridge
                            .render_prompt("${{ tools.by_kind.monitor }}", &empty)
                            .await
                            .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                        (execute, monitor)
                    };
                    let running_tasks: Vec<_> = pending_tasks
                        .into_iter()
                        .map(|t| {
                            let tool_name = match t.kind {
                                xai_grok_tools::computer::types::TaskKind::Monitor => {
                                    monitor_tool_name.clone()
                                }
                                xai_grok_tools::computer::types::TaskKind::Bash => {
                                    execute_tool_name.clone()
                                }
                            };
                            CompactionStateContext::task_summary(
                                t.task_id, t.command, "running", tool_name,
                            )
                        })
                        .collect();
                    let running_subagents = if let Some(ref event_tx) =
                        self.tool_context.subagent_event_tx
                    {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        use xai_grok_tools::implementations::grok_build::task::types::{
                            SubagentEvent, SubagentListActiveRequest,
                        };
                        let _ =
                            event_tx.send(SubagentEvent::ListActive(SubagentListActiveRequest {
                                parent_session_id: self.session_id_string(),
                                respond_to: tx,
                            }));
                        rx.await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| crate::session::helpers::compaction_context::RunningSubagentSummary {
                            subagent_id: s.subagent_id,
                            subagent_type: s.subagent_type,
                            description: s.description,
                            elapsed_ms: s.elapsed_ms,
                        })
                        .collect()
                    } else {
                        vec![]
                    };
                    let connected_mcp_servers = {
                        use crate::session::helpers::compaction_context::CompactionServerSummary;
                        use xai_grok_tools::implementations::search_tool::{
                            sanitize_description, truncate_description,
                        };
                        self.connected_server_summaries()
                            .into_iter()
                            .map(|s| {
                                let desc = s
                                    .description
                                    .map(|d| truncate_description(&sanitize_description(&d)))
                                    .filter(|d| !d.is_empty());
                                CompactionServerSummary {
                                    name: s.name,
                                    tool_count: s.tool_count,
                                    description: desc,
                                }
                            })
                            .collect()
                    };
                    let todos = {
                        use crate::session::helpers::compaction_context::{
                            TodoSummary, TodoSummaryStatus,
                        };
                        use crate::tools::todo::{TodoState, TodoStatus};
                        use xai_grok_tools::types::resources::State;
                        let bridge = self.agent.borrow().tool_bridge().clone();
                        bridge
                            .read_resource::<State<TodoState>>()
                            .await
                            .map(|s| {
                                s.0.todo_items_with_ids()
                                    .map(|(id, item)| TodoSummary {
                                        id: id.clone(),
                                        content: item.content.clone(),
                                        status: match item.status {
                                            TodoStatus::Pending => TodoSummaryStatus::Pending,
                                            TodoStatus::InProgress => TodoSummaryStatus::InProgress,
                                            TodoStatus::Completed => TodoSummaryStatus::Completed,
                                            TodoStatus::Cancelled => TodoSummaryStatus::Cancelled,
                                        },
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    let scheduled_loops = {
                        use crate::session::helpers::compaction_context::ScheduledLoopSummary;
                        let now = chrono::Utc::now();
                        let bridge = self.agent.borrow().tool_bridge().clone();
                        bridge
                        .list_scheduled_tasks()
                        .await
                        .into_iter()
                        .filter(|t| t.pending_fire_at(now).is_some())
                        .map(|t| {
                            let next_fire_at = format_loop_next_fire(
                                t.next_fire_at(),
                                now,
                            );
                            ScheduledLoopSummary {
                                task_id: t.id,
                                interval: xai_grok_tools::implementations::grok_build::scheduler::interval::interval_to_human(
                                    t.interval_secs,
                                ),
                                next_fire_at,
                                prompt: t.prompt,
                                recurring: t.recurring,
                                durable: t.durable,
                            }
                        })
                        .collect()
                    };
                    let workflows = {
                        use crate::session::helpers::compaction_context::WorkflowRunSummary;
                        let tracker = self.workflow_tracker().await;
                        let guard = tracker.lock();
                        guard
                            .list()
                            .into_iter()
                            .filter(|r| !r.status.is_terminal())
                            .map(|r| WorkflowRunSummary {
                                elapsed_ms: guard.elapsed_ms(&r.run_id),
                                name: r.name,
                                run_id: r.run_id,
                                status: r.status.as_ref().to_string(),
                                objective: r.objective,
                                current_phase: r.current_phase,
                                agents_used: r.agents_used,
                                agent_budget: r.agent_budget,
                            })
                            .collect::<Vec<_>>()
                    };
                    let workflow_tool_name = if workflows.is_empty() {
                        None
                    } else {
                        use xai_grok_tools::types::tool::ToolKind;
                        self.agent
                            .borrow()
                            .tool_bridge()
                            .tool_for_kind(ToolKind::Workflow)
                            .await
                            .filter(|s| !s.is_empty())
                    };
                    CompactionStateContext::build(
                        &conversation,
                        CompactionInputs {
                            running_tasks,
                            running_subagents,
                            agent_edited_paths: edited_paths.clone(),
                            connected_mcp_servers,
                            todos,
                            scheduled_loops,
                            workflows,
                            workflow_tool_name,
                            goal_objective: self.goal_objective_for_compaction(),
                            ..Default::default()
                        },
                    )
                    .await
                };
                (agents_md, skills, edited_paths, ctx)
            };
        if self.is_cursor_harness() {
            state_context.images = CompactionImageContext::default();
        }
        let harvested_paths = std::mem::take(&mut state_context.images.attached_paths);
        let (kept, dropped_paths) = if harvested_paths.is_empty() {
            (Vec::new(), 0)
        } else {
            match crate::session::persistence::ensure_owner_only_session_dir(&self.session_info) {
                Ok(session_dir) => {
                    retain_session_asset_files(
                        harvested_paths,
                        &crate::session::image_describe::session_assets_dir(&session_dir),
                    )
                    .await
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "compaction: session dir unavailable; attached image paths dropped"
                    );
                    (Vec::new(), harvested_paths.len())
                }
            }
        };
        state_context.images.attached_paths = kept;
        if dropped_paths > 0 {
            tracing::debug!(
                session_id = %self.session_info.id.0,
                dropped_paths,
                "compaction: dropped attached image paths that are not session asset files"
            );
        }
        use crate::session::helpers::compaction_context::SubagentToolNames;
        let subagent_tool_names: Option<SubagentToolNames> =
            if use_short_prompt || state_context.running_subagents.is_empty() {
                None
            } else {
                let agent_ref = self.agent.borrow();
                let bridge = agent_ref.tool_bridge();
                let empty = serde_json::json!({});
                let poll_name = bridge
                    .render_prompt("${{ tools.by_kind.background_task_action }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                let cancel_name = bridge
                    .render_prompt("${{ tools.by_kind.kill_task_action }}", &empty)
                    .await
                    .filter(|s| !s.is_empty() && !s.contains("by_kind"));
                match (poll_name, cancel_name) {
                    (Some(poll), Some(cancel)) => Some(SubagentToolNames { poll, cancel }),
                    (poll, cancel) => {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            poll_resolved = poll.is_some(),
                            cancel_resolved = cancel.is_some(),
                            "could not resolve subagent tool names, \
                             omitting subagent reminder from compacted conversation"
                        );
                        None
                    }
                }
            };
        use crate::session::helpers::compaction_context::McpToolNames;
        // The legacy hint has its own render site here, independent of the turn path's
        // `acp_session_impl/mcp.rs:722`, so the same predicate is applied here too: an admitted row
        // must not be told to call `search_tool` after a `/compress` it natively declares. Only the
        // sentence is gated — `helpers/compaction_context.rs:237` gates the server list separately,
        // and `:244` is this value's sole consumer.
        let mcp_tool_names: Option<McpToolNames> = if use_short_prompt
            || state_context.connected_mcp_servers.is_empty()
            || self
                .models_manager
                .model_supports_search_tool(self.current_model_id().await.as_str())
        {
            None
        } else {
            let agent_ref = self.agent.borrow();
            let bridge = agent_ref.tool_bridge();
            let empty = serde_json::json!({});
            let search_name = bridge
                .render_prompt("${{ tools.by_kind.search_tool }}", &empty)
                .await
                .filter(|s| !s.is_empty() && !s.contains("by_kind"));
            let call_name = bridge
                .render_prompt("${{ tools.by_kind.use_tool }}", &empty)
                .await
                .filter(|s| !s.is_empty() && !s.contains("by_kind"));
            match (search_name, call_name) {
                (Some(search), Some(call)) => Some(McpToolNames { search, call }),
                _ => None,
            }
        };
        let memory_backend_impl = {
            let g = self.memory.storage.borrow();
            g.as_ref()
                .zip(self.memory.backend_params.as_ref())
                .map(|(storage, params)| {
                    crate::session::memory::MemoryBackendImpl::from_session_params(
                        storage.clone(),
                        &crate::session::memory::MemoryBackendParams {
                            search_source:
                                crate::session::memory::MemorySearchSource::CompactionRecovery,
                            ..params.clone()
                        },
                    )
                })
        };
        let memory_opt_out = false;
        let memory_ref: Option<&dyn xai_grok_tools::types::memory_backend::MemoryBackend> =
            if memory_opt_out {
                None
            } else {
                memory_backend_impl
                    .as_ref()
                    .map(|b| b as &dyn xai_grok_tools::types::memory_backend::MemoryBackend)
            };
        let suppress_state_reminder = false;
        let workflow_listing = self.workflow_listing_for_prompt();
        let system_reminder = if suppress_state_reminder {
            workflow_listing.as_deref().map(|listing| {
                let tag = self.reminder_wrapper_tag();
                format!("<{tag}>\n## Available Workflows\n{listing}\n</{tag}>")
            })
        } else {
            to_system_reminder(
                &state_context,
                &discovered_agents_md,
                &all_skills_for_compaction,
                memory_ref,
                subagent_tool_names.as_ref(),
                mcp_tool_names.as_ref(),
                workflow_listing.as_deref(),
            )
            .await
        };
        let v2_memory_context = if self.memory.can_expose_v2() {
            if let Some(storage) = self.memory.storage() {
                let compact_index = self.memory.v2_config.compact_index_enabled;
                match tokio::task::spawn_blocking(move || {
                    crate::session::helpers::memory_context::format_v2_memory_context(
                        &storage,
                        compact_index,
                    )
                })
                .await
                {
                    Ok(Ok(context)) => {
                        let injected_bytes = context.content.len() as u64;
                        self.memory.record_injected_bytes(injected_bytes);
                        crate::session::memory_observation::log_memory_injection(
                            self.session_info.id.to_string(),
                            xai_grok_telemetry::memory_telemetry::MemoryInjectionOutcome::Results,
                            crate::session::memory_observation::MemoryInjectionMetrics {
                                result_count: context
                                    .global_entry_count
                                    .saturating_add(context.workspace_entry_count),
                                injected_bytes,
                                estimated_tokens: xai_token_estimation::estimate_tokens(
                                    &context.content,
                                ),
                                global_entry_count: context.global_entry_count,
                                workspace_entry_count: context.workspace_entry_count,
                                compact_index,
                                ..Default::default()
                            },
                        );
                        Some(context.content)
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            target: xai_grok_telemetry::memory_log::TARGET,
                            %error,
                            "MEMORY_COMPACTION_RECOVERY: failed to refresh v2 manifests"
                        );
                        crate::session::memory_observation::log_memory_injection(
                            self.session_info.id.to_string(),
                            xai_grok_telemetry::memory_telemetry::MemoryInjectionOutcome::Error,
                            crate::session::memory_observation::MemoryInjectionMetrics {
                                compact_index,
                                ..Default::default()
                            },
                        );
                        None
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: xai_grok_telemetry::memory_log::TARGET,
                            %error,
                            "MEMORY_COMPACTION_RECOVERY: v2 manifest task failed"
                        );
                        crate::session::memory_observation::log_memory_injection(
                            self.session_info.id.to_string(),
                            xai_grok_telemetry::memory_telemetry::MemoryInjectionOutcome::Error,
                            crate::session::memory_observation::MemoryInjectionMetrics {
                                compact_index,
                                ..Default::default()
                            },
                        );
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let system_reminder = match (system_reminder, v2_memory_context) {
            (Some(mut reminder), Some(context)) => {
                reminder.push_str("\n\n");
                reminder.push_str(&context);
                Some(reminder)
            }
            (None, Some(context)) => Some(context),
            (reminder, None) => reminder,
        };
        let system_reminder = {
            let plan_path = {
                let guard = self.plan_mode.lock();
                guard
                    .is_active()
                    .then(|| guard.plan_file_path().to_path_buf())
            };
            if let Some(plan_path) = plan_path {
                let plan_has_content =
                    crate::session::plan_mode::plan_file_has_content(&plan_path).await;
                let template = crate::session::plan_mode::plan_mode_reminder_full_template();
                let wrapper = self.reminder_wrapper_tag();
                let rendered = self
                    .render_plan_template(template, &plan_path, plan_has_content)
                    .await;
                match (system_reminder, rendered) {
                    (Some(mut existing), Some(plan_section)) => {
                        if let Some(pos) = existing.rfind("</system-reminder>") {
                            existing.insert_str(pos, &format!("\n\n{}\n", plan_section));
                        } else {
                            existing.push_str("\n\n");
                            existing.push_str(&plan_section);
                        }
                        Some(existing)
                    }
                    (None, Some(plan_section)) => Some(format!(
                        "<{tag}>\n{body}\n</{tag}>",
                        tag = wrapper,
                        body = plan_section,
                    )),
                    (existing, None) => {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            "compaction: plan mode active but template render failed"
                        );
                        existing
                    }
                }
            } else {
                system_reminder
            }
        };
        let system_reminder = if let Some(goal_section) = self.compaction_goal_section().await {
            use crate::session::acp_session::splice_goal_section;
            match system_reminder {
                Some(existing) => Some(splice_goal_section(&existing, &goal_section)),
                None => {
                    let tag = self.reminder_wrapper_tag();
                    Some(format!("<{tag}>\n{goal_section}\n</{tag}>"))
                }
            }
        } else {
            system_reminder
        };
        if let Some(ref recovery_backend) = memory_backend_impl {
            let n = recovery_backend
                .search_counter
                .load(std::sync::atomic::Ordering::Relaxed);
            if n > 0 {
                self.memory
                    .compaction_recovery_count
                    .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!(
                    target: xai_grok_telemetry::memory_log::TARGET,
                    count = n,
                    "MEMORY_COMPACTION_RECOVERY: {} search(es) performed",
                    n,
                );
            }
        }
        let agents_md_reminder = self.agent.borrow().agents_md_user_reminder();
        let compaction_context = state_context.for_compaction();
        let compaction_state_context: &CompactionStateContext = &compaction_context;
        tracing::debug!(
            session_id = %self.session_info.id.0,
            has_last_user_query = compaction_state_context.last_user_query.is_some(),
            last_turn_image_parts = compaction_state_context.images.last_turn_image_parts.len(),
            has_last_turn_image_files = compaction_state_context
                .images
                .last_turn_image_files
                .is_some(),
            "compaction: last-turn image context"
        );
        let transcript_hint = self.transcript_hint();
        let summary_count = self
            .compaction
            .count
            .load(std::sync::atomic::Ordering::Relaxed);
        let apply_start = std::time::Instant::now();
        let raw_compacted = build_compacted_history(CompactedHistoryInput {
            system_message: system_message.clone(),
            user_message_prefix: user_message_prefix.clone(),
            agents_md_reminder: agents_md_reminder.clone(),
            state_context: compaction_state_context,
            compaction_summary: generate_session_compact.clone(),
            system_reminder: system_reminder.clone(),
            summary_before_recent: use_short_prompt,
            transcript_hint: transcript_hint.clone(),
            summary_count,
        });
        let sanitize_result = sanitize_compacted_history(raw_compacted);
        let compacted_history = if sanitize_result.stripped_tool_call_ids.is_empty() {
            sanitize_result.items
        } else {
            tracing::warn!(
                session_id = %self.session_info.id,
                stripped_count = sanitize_result.stripped_tool_call_ids.len(),
                stripped_ids = ?sanitize_result.stripped_tool_call_ids,
                "compaction: stripped orphaned ToolResults from compacted history"
            );
            sanitize_result.items
        };
        let remaining_violations = validate_compacted_history(&compacted_history);
        let compacted_history = if remaining_violations.is_empty() {
            compacted_history
        } else {
            tracing::error!(
                session_id = %self.session_info.id,
                violation_count = remaining_violations.len(),
                violation_ids = ?remaining_violations,
                "compaction: sanitized history still has invalid ToolResults -- \
                 falling back to minimal compacted history (no recent_messages)"
            );
            build_compacted_history(CompactedHistoryInput {
                system_message,
                user_message_prefix,
                agents_md_reminder,
                state_context: &state_context.for_compaction(),
                compaction_summary: generate_session_compact.clone(),
                system_reminder,
                summary_before_recent: use_short_prompt,
                transcript_hint,
                summary_count,
            })
        };
        // apex-ayl.89 (C1-lite, Site A): validate the final-form
        // projection of the candidate history BEFORE any persistence or
        // replace — an over-cap install bricks every subsequent turn with
        // a local pre-HTTP rejection. Fail loud; nothing is persisted,
        // nothing replaced.
        if let Err(e) = self.projected_caps_ok(&compacted_history).await {
            return Err(self.reject_over_cap_install(&e, auto_trigger).await);
        }
        let post_compaction_ms = apply_start.elapsed().as_millis() as u64;
        let prompt_index_at_compaction = self.chat_state_handle.get_prompt_index().await;
        let original_user_info = self
            .chat_state_handle
            .get_conversation_item_at(1)
            .await
            .and_then(|item| match item {
                ConversationItem::User(parts) => {
                    parts.content.into_iter().next().and_then(|p| match p {
                        xai_grok_sampling_types::ContentPart::Text { text } => {
                            Some(text.as_ref().to_owned())
                        }
                        _ => None,
                    })
                }
                // Same arm as the one in `install_codex_remote_compacted_history`: index 1
                // is the `User(user_info)` row and a discovery row is never that (WAJ21R2-13).
                ConversationItem::Discovery { .. }
                | ConversationItem::System(_)
                | ConversationItem::Assistant(_)
                | ConversationItem::ToolResult(_)
                | ConversationItem::BackendToolCall(_)
                | ConversationItem::Reasoning(_) => None,
            });
        if cancel.is_cancelled() {
            return self.emit_compact_cancelled(auto_trigger).await;
        }
        let segments_queued = u32::from(
            self.persist_compaction_segment(&segment_messages, &generate_session_compact),
        );
        self.chat_state_handle
            .record_compaction_at(prompt_index_at_compaction);
        self.persist_compaction_checkpoint(
            &compacted_history,
            prompt_index_at_compaction,
            auto_continue,
            original_user_info,
        );
        let prefix_len = if self
            .compaction
            .prefix_released
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            0
        } else {
            self.startup_hints.inherited_prefix_len.unwrap_or(0)
        };
        let compacted_history = if prefix_len == 0 {
            compacted_history
        } else {
            self.resolve_forked_compacted_history(
                compacted_history,
                prefix_len,
                tokens_before,
                context_window,
            )
            .await
        };
        // apex-ayl.89 (C1-lite, Site B): re-check the FINAL post-fork
        // value — fork resolution can change the projection (preserving an
        // inherited prefix). Documented residual: the checkpoint was
        // already persisted with the base shape (which passed Site A); the
        // live conversation is unchanged and the next successful
        // compaction overwrites the checkpoint.
        if let Err(e) = self.projected_caps_ok(&compacted_history).await {
            return Err(self.reject_over_cap_install(&e, auto_trigger).await);
        }
        let new_len = compacted_history.len();
        self.chat_state_handle
            .replace_conversation_for_compaction(compacted_history);
        self.reseed_active_goal_after_compaction().await;
        let new_len = self.chat_state_handle.get_conversation_len().await;
        if self.startup_hints.inherited_prefix_len.is_some() {
            let post_replace_tokens = self.chat_state_handle.get_total_tokens().await;
            if xai_token_estimation::exceeds_threshold(
                post_replace_tokens,
                context_window,
                self.compaction.threshold_percent.get(),
            ) {
                self.compaction
                    .auto_compact_suppressed
                    .store(SUPPRESS_STICKY, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    session_id = %self.session_info.id.0,
                    post_replace_tokens,
                    context_window,
                    "compaction: released history still over threshold; suppressing AUTO to avoid a re-loop"
                );
            } else {
                self.compaction
                    .auto_compact_suppressed
                    .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
            }
        } else {
            self.compaction
                .auto_compact_suppressed
                .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
        }
        self.last_idle_flush_conversation_len
            .store(new_len, std::sync::atomic::Ordering::Relaxed);
        self.memory
            .context_injected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if self.memory.is_enabled() {
            tracing::info!(target: xai_grok_telemetry::memory_log::TARGET, "MEMORY_COMPACT: post-compaction reset, next turn re-checks injection (search only if no block persisted)");
        }
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::PlanState(
                crate::tools::todo::TodoState::default(),
            ));
        self.agent
            .borrow()
            .tool_bridge()
            .on_agents_md_compaction()
            .await;
        self.agent
            .borrow()
            .tool_bridge()
            .on_skill_discovery_compaction()
            .await;
        self.rearm_failed_server_announcements().await;
        if self
            .tool_bridge_handle()
            .toolset()
            .tool_name_for_kind(xai_grok_tools::types::tool::ToolKind::UseTool)
            .is_some()
            && let Some(hint) = self.rendered_mcp_hint().await
        {
            self.push_system_reminder_with_tag(&hint, self.reminder_wrapper_tag());
        }
        self.plan_mode.lock().reset_after_compaction();
        self.persist_plan_mode_state();
        self.dispatch_hook(
            xai_grok_hooks::event::HookEventName::PostCompact,
            xai_grok_hooks::event::HookPayload::PostCompact {
                source: compact_source.into(),
            },
            None,
        )
        .await;
        let tokens_after = self.chat_state_handle.get_total_tokens().await;
        {
            let span = tracing::Span::current();
            span.record("compaction_tokens_after", tokens_after as i64);
            span.record(
                "compaction_summary_chars",
                compact_output.content.chars().count() as i64,
            );
            span.record("compaction_attempts", telemetry.attempts as i64);
            span.record(
                "compaction_degenerate_rejections",
                telemetry.degenerate_rejections as i64,
            );
            span.record(
                "compaction_input_overflow_rejections",
                input_overflow_rejections as i64,
            );
            span.record(
                "compaction_deterministic_rejections",
                telemetry.deterministic_rejections as i64,
            );
            span.record(
                "compaction_transient_rejections",
                telemetry.transient_rejections as i64,
            );
            let stop_reason = compact_output.stop_reason.as_deref().unwrap_or("stop");
            span.record("compaction_stop_reason", stop_reason);
            let outcome = if compact_output.truncated {
                CompactionOutcome::Truncated
            } else {
                CompactionOutcome::Success
            };
            span.record("compaction_outcome", outcome.as_ref());
            span.record("compaction_delta_count", compact_output.delta_count as i64);
            if let Some(ms) = compact_output.ttft_ms {
                span.record("compaction_ttft_ms", ms as i64);
            }
            if let Some(ms) = compact_output.stream_ms {
                span.record("compaction_stream_ms", ms as i64);
            }
            if let Some(ms) = compact_output.itl_max_ms {
                span.record("compaction_itl_max_ms", ms as i64);
            }
        }
        compaction.complete(
            xai_grok_telemetry::events::CompactionCompleteStats {
                tokens_after,
                two_pass_used,
                segments_queued,
                degenerate_retries: telemetry.degenerate_rejections,
                input_overflow_retries: input_overflow_rejections,
            },
            xai_grok_telemetry::events::CompactionTiming {
                model_wait_ms: compact_output.model_wait_ms(),
                pre_compaction_ms: Some(pre_compaction_ms),
                post_compaction_ms: Some(post_compaction_ms),
            },
        );
        Ok(())
    }
    pub(crate) fn should_auto_compact(
        &self,
        total_tokens: u64,
        context_window: std::num::NonZeroU64,
    ) -> Option<AutoCompactTriggerInfo> {
        let cw = context_window.get();
        if xai_token_estimation::exceeds_threshold(
            total_tokens,
            cw,
            self.compaction.threshold_percent.get(),
        ) {
            let percentage = xai_token_estimation::usage_percentage_u8(total_tokens, cw);
            Some(AutoCompactTriggerInfo {
                tokens_used: total_tokens,
                context_window: cw,
                percentage,
                reason_override: None,
            })
        } else {
            None
        }
    }
    /// Returns true if the error response indicates tokens exceed the model's context window.
    /// Inspects only the model-metadata portion of the [`SamplingErrorInfo`] (the `context_window` field) against the tracked token estimate.
    /// Called from `handle_sampling_failure` with the `SamplingErrorInfo` the sampler hands back.
    pub(crate) async fn should_compact_on_error(
        &self,
        err: &xai_grok_sampler::SamplingErrorInfo,
    ) -> bool {
        if self.compaction.is_suppressed() {
            return false;
        }
        self.estimate_exceeds_error_context_window(err).await
    }
    /// The request's token estimate exceeds the failed response's reported context window.
    /// This probable-overflow signal is shared by compact-and-resubmit and the mid-salvage truncated-complete arm.
    /// The latter must see overflows even while compaction is suppressed.
    pub(crate) async fn estimate_exceeds_error_context_window(
        &self,
        err: &xai_grok_sampler::SamplingErrorInfo,
    ) -> bool {
        let Some(ref metadata) = err.model_metadata else {
            return false;
        };
        let Some(context_window) = metadata.context_window else {
            return false;
        };
        if context_window == 0 {
            return false;
        }
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        estimated_total > context_window
    }
    /// Pre-sampling compaction check.
    /// Uses `get_estimated_total_tokens()` (exact prior count plus a byte-estimate of items since last response) so tool results are accounted for.
    /// Returns `None` when `is_flushing`.
    pub(crate) async fn check_auto_compact_needed(&self) -> Option<AutoCompactTriggerInfo> {
        if self
            .memory
            .is_flushing
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let sampling_cfg = self.chat_state_handle.get_sampling_config().await;
        let context_window = sampling_cfg.as_ref().map(|c| c.context_window)?;
        let cw = context_window.get();
        let model = sampling_cfg
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default();
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        self.signals_handle()
            .update_context_usage(estimated_total, cw);
        if self.compaction.is_suppressed() {
            return None;
        }
        if self
            .compaction
            .force_compact
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            let percentage = xai_token_estimation::usage_percentage_u8(estimated_total, cw);
            tracing::info!(
                "Forced auto-compact trigger (debug): model={model}, \
                 {percentage}% full ({estimated_total}/{cw} tokens)",
            );
            return Some(AutoCompactTriggerInfo {
                tokens_used: estimated_total,
                context_window: cw,
                percentage,
                reason_override: None,
            });
        }
        if let Some(trigger_info) = self.should_auto_compact(estimated_total, context_window) {
            tracing::info!(
                "Pre-sampling auto-compact trigger: model={model}, \
                 {}% full ({}/{} tokens)",
                trigger_info.percentage,
                trigger_info.tokens_used,
                trigger_info.context_window,
            );
            return Some(trigger_info);
        }
        None
    }
    /// Returns `Some` when tool call outputs have pushed the estimated token count past the context window, so pre-emptive compaction is needed.
    pub(crate) async fn check_preflight_overflow(&self) -> Option<AutoCompactTriggerInfo> {
        if self.compaction.is_suppressed() {
            return None;
        }
        let estimated_total = self.chat_state_handle.get_estimated_total_tokens().await;
        let cfg = self.chat_state_handle.get_sampling_config().await?;
        let cw = cfg.context_window.get();
        if estimated_total <= cw {
            return None;
        }
        let overflow = estimated_total.saturating_sub(cw);
        let percentage = xai_token_estimation::usage_percentage_u8(estimated_total, cw);
        tracing::warn!(
            estimated_total,
            context_window = cw,
            overflow,
            model = %cfg.model,
            "CONTEXT_OVERFLOW_PREFLIGHT: estimated tokens exceed context window \
             after tool call outputs"
        );
        Some(AutoCompactTriggerInfo {
            tokens_used: estimated_total,
            context_window: cw,
            percentage,
            reason_override: None,
        })
    }
    /// On model change: clear sticky/other suppress and compact if the window shrank.
    /// Leaves credit/auth suppress (a switch can't fix those) and short-circuits.
    /// Auth compact failures abort the turn (same as pre-sampling/preflight).
    pub(crate) async fn maybe_compact_on_model_switch(self: &Arc<Self>) -> Result<(), acp::Error> {
        self.refresh_token_if_expired().await;
        let Some(prev) = self.compaction.previous_model.take() else {
            return Ok(());
        };
        let Some(cfg) = self.chat_state_handle.get_sampling_config().await else {
            return Ok(());
        };
        if cfg.model == prev.model_slug {
            return Ok(());
        }
        if self.is_account_state_suppressed() {
            return Ok(());
        }
        self.compaction
            .auto_compact_suppressed
            .store(SUPPRESS_NONE, std::sync::atomic::Ordering::Relaxed);
        if prev.context_window <= cfg.context_window.get() {
            return Ok(());
        }
        let total_tokens = self.chat_state_handle.get_estimated_total_tokens().await;
        let Some(trigger_info) = self.should_auto_compact(total_tokens, cfg.context_window) else {
            return Ok(());
        };
        tracing::info!(
            "Proactive model-switch compact: {} ({}) -> {} ({}), {}% full",
            prev.model_slug,
            prev.context_window,
            cfg.model,
            cfg.context_window.get(),
            trigger_info.percentage,
        );
        if let Err(e) = self.run_compact_only(trigger_info, false).await {
            tracing::error!(error = %e, "Model-switch compaction failed");
            if Self::is_auth_compact_error(&e) {
                return Err(self.surface_compact_auth_failure(e).await);
            }
        }
        Ok(())
    }
    /// Record the current model for model-switch detection on the next turn.
    pub(crate) async fn record_turn_model(&self) {
        if let Some(cfg) = self.chat_state_handle.get_sampling_config().await {
            self.compaction.previous_model.set(Some(
                crate::session::compaction_config::PreviousModelInfo {
                    model_slug: cfg.model.clone(),
                    context_window: cfg.context_window.get(),
                },
            ));
        }
    }
    /// Compact without auto-continue. The outer turn loop rebuilds and retries.
    /// Emits telemetry (`auto_compact_fired`) and UI notifications automatically.
    #[tracing::instrument(
        name = "session.compact",
        skip_all,
        fields(
            session_id = %self.session_info.id.0,
            trigger = "auto",
            mode = tracing::field::Empty,
            detail = tracing::field::Empty,
            pre_tokens = tracing::field::Empty,
            post_tokens = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub(crate) async fn run_compact_only(
        self: &Arc<Self>,
        trigger_info: AutoCompactTriggerInfo,
        lossy_input: bool,
    ) -> Result<(), acp::Error> {
        use crate::extensions::notification::SessionUpdate as XaiSessionUpdate;
        let (_cancel, _cancel_scope) = self.compaction.cancel.enter();
        let _compaction_phase = self.turn_phases.begin_compaction();
        self.record_compaction_variant();
        let tokens_before = self.chat_state_handle.get_total_tokens().await;
        tracing::Span::current().record("pre_tokens", tokens_before as i64);
        xai_grok_telemetry::session_ctx::log_event(xai_grok_telemetry::events::AutoCompactFired {
            tokens_before: trigger_info.tokens_used,
            percentage: trigger_info.percentage,
        });
        self.signals_handle()
            .record_compaction(trigger_info.tokens_used);
        let reason = if trigger_info.reason_override == Some(MODEL_FAMILY_SWITCH_COMPACT_BANNER) {
            MODEL_FAMILY_SWITCH_COMPACT_BANNER.to_string()
        } else {
            format!("Context window {}% full", trigger_info.percentage)
        };
        self.send_xai_notification(XaiSessionUpdate::AutoCompactStarted {
            tokens_used: trigger_info.tokens_used,
            context_window: trigger_info.context_window,
            percentage: trigger_info.percentage,
            reason,
        })
        .await;
        self.maybe_pre_compaction_flush(
            trigger_info.tokens_used,
            trigger_info.context_window,
            "pre_compact_on_error",
        )
        .await;
        let compact_start = std::time::Instant::now();
        let result = self
            .run_compact_inner(
                None,
                xai_grok_telemetry::events::CompactionTrigger::Auto,
                lossy_input,
            )
            .await;
        let elapsed_ms = compact_start.elapsed().as_millis() as i64;
        match result {
            Ok(()) => {
                let tokens_after = self.chat_state_handle.get_total_tokens().await;
                let span = tracing::Span::current();
                span.record("post_tokens", tokens_after as i64);
                span.record("success", true);
                self.send_xai_notification(XaiSessionUpdate::AutoCompactCompleted {
                    tokens_before: Some(trigger_info.tokens_used),
                    tokens_after,
                    elapsed_ms: Some(elapsed_ms),
                    summary_preview: None,
                })
                .await;
                self.emit_status_snapshot_detached();
                Ok(())
            }
            Err(e) => {
                let span = tracing::Span::current();
                span.record("success", false);
                span.record("error", e.to_string().as_str());
                let cancelled = self.compaction.cancel.is_cancelled()
                    || matches!(
                        crate::session::helpers::session_compact::compact_error_kind(&e),
                        Some(crate::session::helpers::session_compact::CompactErrorKind::Cancelled)
                    )
                    || e.data.as_ref().and_then(|d| d.as_str()).is_some_and(|s| {
                        s.contains(crate::session::helpers::session_compact::COMPACT_CANCELLED_MSG)
                    });
                if !cancelled && !self.compaction.is_suppressed() {
                    self.send_xai_notification(XaiSessionUpdate::AutoCompactFailed {
                        error: Self::failure_with_retry_guidance(
                            &crate::sampling::error::acp_error_message(&e),
                        ),
                    })
                    .await;
                }
                Err(e)
            }
        }
    }
    /// `created_at` is taken from the caller-supplied `started_at` (captured before the retry loop) rather than `Utc::now()` here.
    /// Transient retries then don't skew the timestamp away from when the call actually started.
    /// Best-effort: send-failures are logged at `warn` and never shown to the user, because the artifact is purely for offline analysis.
    #[allow(clippy::too_many_arguments)]
    fn persist_compaction_request_artifact(
        &self,
        chat_history: Vec<ConversationItem>,
        tools: Vec<xai_grok_sampling_types::ToolSpec>,
        use_short_prompt: bool,
        model: &str,
        trigger: xai_grok_telemetry::events::CompactionTrigger,
        summary: Option<&str>,
        error: Option<&acp::Error>,
        attempts: u32,
        attempt_details: Vec<CompactionAttempt>,
        started_at: String,
    ) {
        use crate::extensions::notification::CompactionRequestFile;
        let request_id = uuid::Uuid::new_v4().to_string();
        let trigger_str = match trigger {
            xai_grok_telemetry::events::CompactionTrigger::Manual => "manual",
            xai_grok_telemetry::events::CompactionTrigger::Auto => "auto",
        };
        let prompt_variant = if use_short_prompt {
            "short"
        } else {
            "detailed"
        };
        let error_str = error.map(|e| {
            e.data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or("<no error data>")
                .to_owned()
        });
        let artifact = CompactionRequestFile {
            schema_version: 2,
            request_id,
            created_at: started_at,
            trigger: trigger_str.to_owned(),
            prompt_variant: prompt_variant.to_owned(),
            model: model.to_owned(),
            chat_history,
            tools,
            summary: summary.map(str::to_owned),
            error: error_str,
            attempts,
            attempt_details,
        };
        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CompactionRequest(artifact))
            .is_err()
        {
            tracing::warn!(
                session_id = %self.session_info.id.0,
                "Failed to send compaction request artifact to persistence channel"
            );
        }
    }
    /// Persist a compaction checkpoint.
    /// Writes the compacted history to a separate file and records a `CompactionCheckpoint` marker in `updates.jsonl`.
    /// `auto_continue` should be `Some` when this compaction was triggered by auto-compact and an auto-continue prompt will follow.
    fn persist_compaction_checkpoint(
        &self,
        compacted_history: &[ConversationItem],
        prompt_index_at_compaction: usize,
        auto_continue: Option<crate::extensions::notification::AutoContinueInfo>,
        original_user_info: Option<String>,
    ) {
        use crate::extensions::notification::{
            CompactionCheckpointFile, CompactionCheckpointInfo, SessionUpdate as XaiSessionUpdate,
        };
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let checkpoint_file = format!("compaction_checkpoints/{checkpoint_id}.json");
        let created_at = chrono::Utc::now().to_rfc3339();
        let file_data = CompactionCheckpointFile {
            checkpoint_id: checkpoint_id.clone(),
            prompt_index_at_compaction,
            compacted_history: compacted_history.to_vec(),
            schema_version: 1,
            created_at: created_at.clone(),
            original_user_info,
            reread_file_paths: vec![],
        };
        if self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::CompactionCheckpoint(file_data))
            .is_err()
        {
            tracing::warn!("Failed to send compaction checkpoint file to persistence channel");
        }
        let info = CompactionCheckpointInfo {
            checkpoint_id,
            prompt_index_at_compaction,
            checkpoint_file,
            auto_continue,
            schema_version: 1,
            created_at,
        };
        self.persist_xai_update_only(XaiSessionUpdate::CompactionCheckpoint(Box::new(info)));
        tracing::info!(
            prompt_index_at_compaction,
            "Persisted compaction checkpoint"
        );
    }
}
#[cfg(test)]
#[path = "compaction_inline_auto_compact_flow_tests.rs"]
mod inline_auto_compact_flow_tests;

#[cfg(test)]
pub(super) fn record_compaction_refresh(site: &'static str, model: &str, base_url: &str) {
    binding_tests::record_refresh(site, model, base_url);
}

#[cfg(test)]
mod binding_tests {
    use super::super::{ModelAuthMemo, support};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xai_chat_state::RequestTransportBinding;
    use xai_grok_sampling_types::{HostedTool, ToolDefinition, ToolExposure, ToolSpec};
    use xai_grok_test_support::{MockInferenceServer, ScriptedResponse};

    pub(super) struct GeneratorInputs {
        pub site: &'static str,
        pub history: Vec<ConversationItem>,
        pub tools: Vec<ToolSpec>,
        pub hosted: Vec<HostedTool>,
        pub token_kind: &'static str,
        pub token_input: serde_json::Value,
        pub tool_tokens: u64,
        pub config: xai_grok_sampler::SamplerConfig,
        pub client_backend: ApiBackend,
        pub idle_timeout: std::time::Duration,
        pub wall_clock_budget_secs: u64,
        pub tool_choice: crate::util::config::CompactionToolChoice,
    }

    struct Trace {
        next: Option<RequestTransportBinding>,
        captured: Vec<RequestTransportBinding>,
        reconstruction: Option<xai_grok_sampler::SamplerConfig>,
        reconstruction_refreshes: usize,
        inputs: Option<GeneratorInputs>,
        before_send_refreshes: usize,
        remote_request: Option<ConversationRequest>,
        refreshes: Vec<(&'static str, String, String)>,
        refresh_count: Arc<AtomicUsize>,
    }

    thread_local! {
        static TRACE: std::cell::RefCell<Option<Trace>> = const { std::cell::RefCell::new(None) };
    }

    struct TraceGuard;

    impl TraceGuard {
        fn new(next: Option<RequestTransportBinding>, refresh_count: Arc<AtomicUsize>) -> Self {
            TRACE.with(|trace| {
                assert!(trace.borrow().is_none());
                *trace.borrow_mut() = Some(Trace {
                    next,
                    captured: Vec::new(),
                    reconstruction: None,
                    reconstruction_refreshes: 0,
                    inputs: None,
                    before_send_refreshes: 0,
                    remote_request: None,
                    refreshes: Vec::new(),
                    refresh_count,
                });
            });
            Self
        }

        fn take(&self) -> Trace {
            TRACE.with(|trace| trace.borrow_mut().take().unwrap())
        }
    }

    impl Drop for TraceGuard {
        fn drop(&mut self) {
            TRACE.with(|trace| *trace.borrow_mut() = None);
        }
    }

    pub(super) async fn after_capture(actor: &SessionActor, binding: &RequestTransportBinding) {
        let next = TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            let trace = trace.as_mut()?;
            assert!(trace.captured.is_empty(), "one binding capture per sender");
            trace.captured.push(binding.clone());
            trace.next.take()
        });
        if let Some(next) = next {
            actor
                .chat_state_handle
                .install_transport_binding(next)
                .await
                .unwrap();
        }
    }

    pub(super) fn record_reconstruction(config: &xai_grok_sampler::SamplerConfig) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.reconstruction.is_none());
                trace.reconstruction = Some(config.clone());
                trace.reconstruction_refreshes = trace.refresh_count.load(Ordering::SeqCst);
            }
        });
    }

    pub(super) fn record_inputs(inputs: GeneratorInputs) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.inputs.is_none());
                trace.before_send_refreshes = trace.refresh_count.load(Ordering::SeqCst);
                trace.inputs = Some(inputs);
            }
        });
    }

    pub(super) fn record_remote_request(request: &ConversationRequest) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.remote_request.is_none());
                trace.remote_request = Some(request.clone());
            }
        });
    }

    struct RefreshFailure(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl xai_grok_login::refresh::TokenRefresher for RefreshFailure {
        async fn refresh(
            &self,
            reason: xai_grok_login::refresh::RefreshReason,
        ) -> xai_grok_login::refresh::RefreshOutcome {
            assert_eq!(reason, xai_grok_login::refresh::RefreshReason::PreRequest);
            self.0.fetch_add(1, Ordering::SeqCst);
            xai_grok_login::refresh::RefreshOutcome::TransientFailure {
                message: "u7 synthetic refresh unavailable".into(),
            }
        }
    }

    // Effective configuration caches its home, so isolate before the first lookup.
    fn isolated(name: &str) -> bool {
        const CHILD: &str = "GROK_TEST_U7_COMPACTION_CHILD";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return false;
        }
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env(CHILD, name)
            .env("GROK_HOME", home.path())
            .env_remove("GROK_CONFIG")
            .env_remove("GROK_CONFIG_PATH")
            .env_remove("GROK_CAMPAIGNS")
            .env_remove("GROK_CAMPAIGNS_OVERRIDE")
            .env_remove("XAI_API_KEY")
            .env_remove("APEX_LLM_API_KEY")
            .args(["--exact", name, "--nocapture"])
            .stdin(std::process::Stdio::null());
        xai_tty_utils::detach_std_command(&mut command);
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("running 1 test"),
            "isolated selector must select one test"
        );
        assert!(
            output.status.success(),
            "isolated compaction assertion failed: {stderr}"
        );
        assert!(stdout.contains(&format!("test {name} ... ok")));
        true
    }

    struct Case {
        backend: ApiBackend,
        admitted: bool,
        session_token: bool,
        remote: bool,
    }

    struct Fixture {
        actor: Arc<SessionActor>,
        a: RequestTransportBinding,
        b: RequestTransportBinding,
        server_a: MockInferenceServer,
        server_b: MockInferenceServer,
        history: Vec<ConversationItem>,
        refresh_count: Arc<AtomicUsize>,
        _home: tempfile::TempDir,
        _persistence: tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
    }

    async fn fixture(case: &Case) -> Fixture {
        use xai_grok_sampling_types::ReasoningEffort;
        use xai_grok_tools::implementations::grok_build::{
            update_goal::UpdateGoalTool, web_search::WebSearchTool,
        };
        use xai_grok_tools::registry::types::ToolConfig;
        let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        support::drain_gateway(gateway_rx);
        let (persistence_tx, persistence) = tokio::sync::mpsc::unbounded_channel();
        let mut actor =
            support::create_test_actor(0, 384_000, 85, gateway_tx, persistence_tx).await;
        actor.auth_method_id = support::test_auth_method_id("xai.api_key");
        actor.inference_idle_timeout = std::time::Duration::from_secs(19);
        let agent = support::test_agent_with_tools(vec![
            ToolConfig::for_tool::<UpdateGoalTool>(),
            ToolConfig::for_tool::<WebSearchTool>(),
        ])
        .await;
        let bridge = Arc::clone(agent.tool_bridge());
        support::register_stub(&bridge, "mcp__u7__lookup").await;
        let mut policy = agent.compaction_policy().clone();
        policy.two_pass_enabled = false;
        policy.remote_compaction_v2 = case.remote;
        policy.wall_clock_budget_secs = 43;
        *actor.agent.borrow_mut() = xai_grok_agent::Agent::new(
            agent.definition().clone(),
            agent.prompt_context().clone(),
            agent.system_prompt().to_string(),
            bridge,
            agent.reminder_policy().clone(),
            policy,
            vec![HostedTool::WebSearch { options: None }],
            true,
        );
        actor.supports_backend_search.set(true);
        let (manager, home) = support::manager_with_entries();
        actor.models_manager = manager;
        let defs = actor.prepare_tool_definitions().await;
        assert!(defs.iter().any(|d| d.function.name == "web_search"));
        let marked = defs
            .iter()
            .find(|d| d.function.name != "web_search")
            .unwrap()
            .function
            .name
            .clone();
        actor.models_manager.insert_test_entry(
            "u7-operator",
            support::entry_with_deferred_tools("u7-operator", true, &[&marked]),
        );
        actor
            .models_manager
            .set_current_model_id(acp::ModelId::new("u7-operator"));
        let mut entry = support::entry_with_search_flag("u7-source-a", case.admitted);
        if case.remote {
            entry.info.model_family = Some("codex".into());
        }
        actor.models_manager.insert_test_entry("u7-source-a", entry);
        actor.models_manager.insert_test_entry(
            "u7-installed-b",
            support::entry_with_search_flag("u7-installed-b", !case.admitted),
        );
        let server_a = MockInferenceServer::start().await.unwrap();
        let server_b = MockInferenceServer::start().await.unwrap();
        for server in [&server_a, &server_b] {
            server.set_response("<summary>Retain the request-scoped transport, complete tool schemas, and compaction state. ".to_owned() + "Continue the original user task with the recorded constraints. ".repeat(20).as_str() + "</summary>");
        }
        let mut a = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap();
        a.config.model = "u7-source-a".into();
        a.config.base_url = server_a.url();
        a.config.api_backend = case.backend.clone();
        a.config.context_window = std::num::NonZeroU64::new(384_000).unwrap();
        a.config.reasoning_effort = Some(ReasoningEffort::High);
        a.config.ultra_wire_effort = Some(ReasoningEffort::Medium);
        a.config.supports_search_tool = case.admitted;
        a.config.max_completion_tokens = Some(8192);
        a.config.temperature = Some(0.375);
        a.config.top_p = Some(0.625);
        a.config.max_retries = Some(0);
        a.config.extra_headers = indexmap::indexmap! { "x-u7-row".into() => "a".into() };
        a.credentials = xai_chat_state::Credentials {
            api_key: Some("u7-a-key".into()),
            auth_type: xai_chat_state::AuthType::ApiKey,
            alpha_test_key: Some("u7-a-alpha".into()),
            client_version: Some("u7-a-version".into()),
        };
        let mut b = a.clone();
        b.config.model = "u7-installed-b".into();
        b.config.base_url = server_b.url();
        b.config.api_backend = if case.backend == ApiBackend::Messages {
            ApiBackend::Responses
        } else {
            ApiBackend::Messages
        };
        b.config.context_window = std::num::NonZeroU64::new(96_000).unwrap();
        b.config.reasoning_effort = Some(ReasoningEffort::None);
        b.config.ultra_wire_effort = None;
        b.config.supports_search_tool = !case.admitted;
        b.config.extra_headers = indexmap::indexmap! { "x-u7-row".into() => "b".into() };
        b.credentials.api_key = Some("u7-b-key".into());
        b.credentials.alpha_test_key = Some("u7-b-alpha".into());
        b.credentials.client_version = Some("u7-b-version".into());
        let refresh_count = Arc::new(AtomicUsize::new(0));
        if case.session_token {
            let manager = Arc::new(xai_grok_login::AuthManager::new(
                home.path(),
                xai_grok_login::GrokComConfig::default(),
            ));
            manager.hot_swap(xai_grok_login::GrokAuth {
                key: "u7-expired-bearer".into(),
                auth_mode: xai_grok_login::AuthMode::Oidc,
                refresh_token: Some("u7-refresh-token".into()),
                expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                ..xai_grok_login::GrokAuth::test_default()
            });
            manager.set_refresher(Arc::new(RefreshFailure(refresh_count.clone())));
            actor.auth_manager = Some(manager);
            actor.auth_method_id = support::test_auth_method_id("cached_token");
            actor.model_auth_memo.replace(Some(ModelAuthMemo {
                model_id: a.config.model.clone(),
                facts: crate::agent::config::ModelAuthFacts {
                    byok: crate::agent::auth_method::ModelByok::NotByok,
                    auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
                },
                provider: None,
            }));
        }
        actor
            .chat_state_handle
            .install_transport_binding(a.clone())
            .await
            .unwrap();
        let history = vec![
            ConversationItem::system("u7 system instructions"),
            ConversationItem::user("original task ".to_owned() + "x".repeat(24_000).as_str()),
            ConversationItem::assistant("prior work ".to_owned() + "y".repeat(16_000).as_str()),
            ConversationItem::user("continue the original task"),
        ];
        actor
            .chat_state_handle
            .replace_conversation(history.clone());
        actor.chat_state_handle.get_conversation().await;
        Fixture {
            actor: Arc::new(actor),
            a,
            b,
            server_a,
            server_b,
            history,
            refresh_count,
            _home: home,
            _persistence: persistence,
        }
    }

    struct LegacySurface {
        tools: Vec<ToolSpec>,
        hosted: Vec<HostedTool>,
        token_kind: &'static str,
        token_input: serde_json::Value,
        tokens: u64,
    }

    async fn legacy_surface(actor: &SessionActor, two_pass: bool) -> LegacySurface {
        let bridge = actor.agent.borrow().tool_bridge().clone();
        assert!(
            bridge
                .tool_definitions()
                .await
                .iter()
                .any(|d| d.function.name == "mcp__u7__lookup"),
            "registered MCP exclusion control must exist"
        );
        let defs = actor.prepare_tool_definitions().await;
        assert!(!defs.iter().any(|d| d.function.name == "mcp__u7__lookup"));
        let hosted = actor.hosted_tools_for_turn();
        assert!(!hosted.is_empty());
        let active = actor.backend_search_active();
        let full_defs: Vec<ToolDefinition> = defs
            .iter()
            .filter(|d| !active || d.function.name != "web_search")
            .cloned()
            .collect();
        let full: Vec<ToolSpec> = full_defs.iter().cloned().map(ToolSpec::from).collect();
        let prepared = actor.turn_base_tool_specs(&defs);
        assert!(
            prepared
                .iter()
                .any(|t| t.exposure == ToolExposure::Deferred)
        );
        assert!(full.iter().all(|t| t.exposure == ToolExposure::Immediate));
        assert_ne!(
            serde_json::to_value(&prepared).unwrap(),
            serde_json::to_value(&full).unwrap(),
            "legacy conversions must be distinguishable"
        );
        if two_pass {
            LegacySurface {
                tokens: xai_chat_state::estimate_tool_specs_tokens(&prepared),
                token_kind: "specs",
                token_input: serde_json::to_value(&prepared).unwrap(),
                tools: prepared,
                hosted,
            }
        } else {
            LegacySurface {
                tokens: xai_chat_state::estimate_tool_definitions_tokens(&full_defs),
                token_kind: "definitions",
                token_input: serde_json::to_value(&full_defs).unwrap(),
                tools: full,
                hosted,
            }
        }
    }

    fn assert_binding(actual: &RequestTransportBinding, expected: &RequestTransportBinding) {
        assert_eq!(
            serde_json::to_value(&actual.config).unwrap(),
            serde_json::to_value(&expected.config).unwrap()
        );
        assert_eq!(actual.credentials, expected.credentials);
    }

    fn assert_inputs(
        fixture: &Fixture,
        case: &Case,
        trace: &Trace,
        surface: &LegacySurface,
        expected_config: &xai_grok_sampler::SamplerConfig,
        two_pass: bool,
    ) {
        assert_eq!(trace.captured.len(), 1, "one atomic capture per sender");
        assert_binding(&trace.captured[0], &fixture.a);
        let inputs = trace
            .inputs
            .as_ref()
            .expect("production generator boundary reached");
        assert_eq!(inputs.site, if two_pass { "two-pass" } else { "full" });
        assert!(
            !inputs.tools.iter().any(|t| t.name == "mcp__u7__lookup"),
            "compaction must exclude registered MCP tools"
        );
        assert_eq!(
            (serde_json::to_value(&inputs.tools).unwrap(), &inputs.hosted),
            (
                serde_json::to_value(&surface.tools).unwrap(),
                &surface.hosted
            ),
            "each compaction sender retains its own complete legacy tool conversion"
        );
        assert_eq!(
            (inputs.token_kind, &inputs.token_input, inputs.tool_tokens),
            (surface.token_kind, &surface.token_input, surface.tokens),
            "token accounting consumes the exact separate legacy surface"
        );
        let expected_history = if two_pass {
            two_pass_history(&fixture.history)
        } else {
            prepare_conversation_for_verbatim_summarization(
                fixture.history.clone(),
                case.backend == ApiBackend::Messages,
            )
        };
        assert_eq!(
            serde_json::to_value(&inputs.history).unwrap(),
            serde_json::to_value(&expected_history).unwrap(),
            "sender preserves its history preparation"
        );
        assert_eq!(
            serde_json::to_value(&inputs.config).unwrap(),
            serde_json::to_value(expected_config).unwrap(),
            "generator config remains completely on captured A"
        );
        let reconstructed = trace.reconstruction.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(reconstructed).unwrap(),
            serde_json::to_value(expected_config).unwrap()
        );
        assert!(Arc::ptr_eq(
            inputs.config.header_injector.as_ref().unwrap(),
            reconstructed.header_injector.as_ref().unwrap()
        ));
        match (
            &inputs.config.bearer_resolver,
            &reconstructed.bearer_resolver,
        ) {
            (Some(a), Some(b)) => assert!(Arc::ptr_eq(a, b)),
            (None, None) => assert!(!case.session_token),
            _ => panic!("sender lost live resolver identity"),
        }
        assert_eq!(
            inputs.client_backend, case.backend,
            "client and generator use the same captured backend"
        );
        assert_eq!(
            (
                inputs.idle_timeout,
                inputs.wall_clock_budget_secs,
                format!("{:?}", inputs.tool_choice)
            ),
            (
                std::time::Duration::from_secs(19),
                43,
                format!("{:?}", fixture.actor.compaction.tool_choice)
            )
        );
        let counts = if case.session_token {
            (1, 3, 3)
        } else {
            (0, 0, 0)
        };
        assert_eq!(
            (
                trace.reconstruction_refreshes,
                trace.before_send_refreshes,
                fixture.refresh_count.load(Ordering::SeqCst)
            ),
            counts,
            "retain standalone gated auth plus one preflight and one gated auth at bound preparation"
        );
        if case.remote {
            let request = trace
                .remote_request
                .as_ref()
                .expect("actual remote compact request reached");
            let mut expected = ConversationRequest::from_items(fixture.history[1..].to_vec())
                .with_model(fixture.a.config.model.clone())
                .with_tools(surface.tools.clone());
            expected.hosted_tools = surface.hosted.clone();
            expected.reasoning_effort = fixture.a.config.reasoning_effort;
            expected.x_grok_session_id = Some(fixture.actor.session_info.id.0.to_string());
            assert_eq!(
                format!("{request:?}"),
                format!("{expected:?}"),
                "remote helper retains the complete legacy compact request"
            );
        } else {
            assert!(trace.remote_request.is_none());
        }
    }

    fn two_pass_history(history: &[ConversationItem]) -> Vec<ConversationItem> {
        let mut items = history.to_vec();
        items.push(ConversationItem::user(build_two_pass_compaction_prompt(
            None,
        )));
        items
    }

    async fn run_sender(fixture: &Fixture, case: &Case, two_pass: bool) -> bool {
        if case.remote {
            for server in [&fixture.server_a, &fixture.server_b] {
                server.enqueue_response("/v1/responses", ScriptedResponse::json(400, serde_json::json!({"error": {"type": "invalid_request_error", "message": "u7 terminal fixture rejection"}})));
            }
        }
        if two_pass {
            fixture
                .actor
                .two_pass_sample(two_pass_history(&fixture.history))
                .await
                .is_some()
        } else {
            fixture.actor.run_compact(None).await.is_ok()
        }
    }

    async fn exercise_sender(case: Case, two_pass: bool) {
        let fixture = fixture(&case).await;
        let surface = legacy_surface(&fixture.actor, two_pass).await;
        let mut baseline_tools = None;
        for interleave in [false, true] {
            fixture
                .actor
                .chat_state_handle
                .install_transport_binding(fixture.a.clone())
                .await
                .unwrap();
            fixture
                .actor
                .chat_state_handle
                .replace_conversation(fixture.history.clone());
            fixture.actor.chat_state_handle.get_conversation().await;
            fixture.refresh_count.store(0, Ordering::SeqCst);
            let expected_config = fixture
                .actor
                .assemble_sampler_config_without_refresh(&fixture.a)
                .await;
            let guard = TraceGuard::new(
                interleave.then(|| fixture.b.clone()),
                fixture.refresh_count.clone(),
            );
            let succeeded = run_sender(&fixture, &case, two_pass).await;
            let trace = guard.take();
            assert!(
                fixture.server_b.requests().is_empty(),
                "{} transport must never use installed B",
                if two_pass { "two-pass" } else { "full" }
            );
            let requests = fixture.server_a.requests();
            assert_eq!(
                requests.len(),
                if interleave { 2 } else { 1 },
                "one actual send per compaction invocation"
            );
            let request = requests.last().unwrap();
            assert_eq!(
                request.path,
                if case.backend == ApiBackend::Messages {
                    "/v1/messages"
                } else {
                    "/v1/responses"
                }
            );
            assert_eq!(
                request.authorization.as_deref(),
                if case.session_token {
                    None
                } else {
                    Some("Bearer u7-a-key")
                },
                "captured source credentials reach the actual send"
            );
            assert_eq!(request.header("x-u7-row"), Some("a"));
            let body = request.body.as_ref().unwrap();
            assert_eq!(body["model"], fixture.a.config.model);
            assert_eq!(
                succeeded, !case.remote,
                "local send succeeds; remote fixture terminates without an auth retry"
            );
            assert_inputs(
                &fixture,
                &case,
                &trace,
                &surface,
                &expected_config,
                two_pass,
            );
            if let Some(expected) = &baseline_tools {
                assert_eq!(
                    &body["tools"], expected,
                    "actual complete wire tools retain the sender's no-switch baseline"
                );
            } else {
                baseline_tools = Some(body["tools"].clone());
            }
            if interleave {
                assert_binding(
                    &fixture
                        .actor
                        .chat_state_handle
                        .capture_transport_binding()
                        .await
                        .unwrap(),
                    &fixture.b,
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_pass_sender_keeps_bound_transport_and_legacy_tools() {
        const NAME: &str = "session::acp_session::compaction::binding_tests::two_pass_sender_keeps_bound_transport_and_legacy_tools";
        if isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(async {
                for (backend, admitted) in [
                    (ApiBackend::Messages, true),
                    (ApiBackend::Messages, false),
                    (ApiBackend::Responses, true),
                ] {
                    exercise_sender(
                        Case {
                            backend,
                            admitted,
                            session_token: false,
                            remote: false,
                        },
                        true,
                    )
                    .await;
                }
                exercise_sender(
                    Case {
                        backend: ApiBackend::Responses,
                        admitted: true,
                        session_token: true,
                        remote: false,
                    },
                    true,
                )
                .await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_sender_keeps_bound_transport_and_legacy_tools() {
        const NAME: &str = "session::acp_session::compaction::binding_tests::full_sender_keeps_bound_transport_and_legacy_tools";
        if isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(async {
                for (backend, admitted) in [
                    (ApiBackend::Messages, true),
                    (ApiBackend::Messages, false),
                    (ApiBackend::Responses, true),
                ] {
                    exercise_sender(
                        Case {
                            backend,
                            admitted,
                            session_token: false,
                            remote: false,
                        },
                        false,
                    )
                    .await;
                }
                exercise_sender(
                    Case {
                        backend: ApiBackend::Responses,
                        admitted: true,
                        session_token: true,
                        remote: false,
                    },
                    false,
                )
                .await;
                exercise_sender(
                    Case {
                        backend: ApiBackend::Responses,
                        admitted: true,
                        session_token: false,
                        remote: true,
                    },
                    false,
                )
                .await;
            })
            .await;
    }

    pub(super) fn record_refresh(site: &'static str, model: &str, base_url: &str) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                assert!(trace.refreshes.len() < 32);
                trace
                    .refreshes
                    .push((site, model.to_owned(), base_url.to_owned()));
            }
        });
    }

    fn take_refreshes() -> Vec<(&'static str, String, String)> {
        TRACE.with(|trace| std::mem::take(&mut trace.borrow_mut().as_mut().unwrap().refreshes))
    }

    #[derive(Clone, Copy, Debug)]
    enum RemoteRetryCase {
        TerminalFailure,
        SecondAuthFailure,
        FinalAttemptAuthFailure,
        Cancelled,
        RotatedBearer,
    }

    fn retry_error(status: u16) -> ScriptedResponse {
        let mut response = ScriptedResponse::json(
            status,
            serde_json::json!({
                "error": {"type": "u8_fixture_error", "message": "u8 terminal fixture rejection"}
            }),
        );
        response.headers.push(("retry-after".into(), "0".into()));
        response
    }

    fn compact_success() -> ScriptedResponse {
        use xai_grok_test_support::SseEvent;
        ScriptedResponse::sse(vec![
            SseEvent::with_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done", "output_index": 0,
                    "item": {"type": "compaction", "encrypted_content": "u8-opaque-summary"}
                })
                .to_string(),
            ),
            SseEvent::with_event(
                "response.completed",
                serde_json::json!({
                    "type": "response.completed", "response": {"id": "u8-compact-response"}
                })
                .to_string(),
            ),
        ])
    }

    fn assert_refreshes(
        actual: &[(&'static str, String, String)],
        sites: &[&'static str],
        binding: &RequestTransportBinding,
        message: &str,
    ) {
        let expected: Vec<_> = sites
            .iter()
            .map(|site| {
                (
                    *site,
                    binding.config.model.clone(),
                    binding.config.base_url.clone(),
                )
            })
            .collect();
        assert_eq!(actual, expected, "{message}");
    }

    async fn exercise_remote_auth_retry(session_token: bool, scenario: RemoteRetryCase) {
        use xai_grok_test_support::{InferenceEndpoint, InferenceRequestMatcher};
        let case = Case {
            backend: ApiBackend::Responses,
            admitted: true,
            session_token,
            remote: true,
        };
        let mut fixture = fixture(&case).await;
        fixture.b.config.api_backend = ApiBackend::Responses;
        let mut entry = support::entry_with_search_flag(&fixture.b.config.model, false);
        entry.info.model_family = Some("codex".into());
        fixture
            .actor
            .models_manager
            .insert_test_entry(&fixture.b.config.model, entry);
        let surface = legacy_surface(&fixture.actor, false).await;
        let expected_config = fixture
            .actor
            .assemble_sampler_config_without_refresh(&fixture.a)
            .await;
        assert!(surface.tools.len() + surface.hosted.len() >= 2);
        let matcher = InferenceRequestMatcher::foreground(InferenceEndpoint::Responses);
        let last_attempt = matches!(scenario, RemoteRetryCase::FinalAttemptAuthFailure);
        let mut first = fixture.server_a.expect_response_blocked(
            "u8-first-remote-response",
            matcher,
            retry_error(if last_attempt { 429 } else { 401 }),
        );
        let mut cancelled = if matches!(scenario, RemoteRetryCase::Cancelled) {
            Some(fixture.server_a.expect_response_blocked(
                "u8-cancel-before-install",
                matcher,
                compact_success(),
            ))
        } else {
            let status = if last_attempt {
                429
            } else if matches!(scenario, RemoteRetryCase::SecondAuthFailure) {
                401
            } else {
                400
            };
            fixture
                .server_a
                .enqueue_response("/v1/responses", retry_error(status));
            fixture
                .server_a
                .enqueue_response("/v1/responses", retry_error(401));
            None
        };
        fixture
            .server_b
            .enqueue_response("/v1/responses", retry_error(400));
        let guard = TraceGuard::new(None, fixture.refresh_count.clone());
        let actor = fixture.actor.clone();
        let task = tokio::task::spawn_local(async move { actor.run_compact(None).await });
        tokio::time::timeout(std::time::Duration::from_secs(10), first.wait_blocked())
            .await
            .expect("first remote compact response must reach its owned barrier");
        assert_eq!(fixture.server_a.requests().len(), 1);
        let initial_refreshes = take_refreshes();
        assert_refreshes(
            &initial_refreshes,
            if session_token {
                &["gated_auth", "preflight", "gated_auth"]
            } else {
                &["preflight"]
            },
            &fixture.a,
            "U8 preserves the full sender's initial refresh sites",
        );
        assert_eq!(
            fixture.refresh_count.load(Ordering::SeqCst),
            if session_token { 3 } else { 0 }
        );
        fixture
            .actor
            .chat_state_handle
            .install_transport_binding(fixture.b.clone())
            .await
            .unwrap();
        if matches!(scenario, RemoteRetryCase::RotatedBearer) {
            fixture
                .actor
                .auth_manager
                .as_ref()
                .unwrap()
                .hot_swap(xai_grok_login::GrokAuth {
                    key: "u8-live-rotated-bearer".into(),
                    auth_mode: xai_grok_login::AuthMode::Oidc,
                    refresh_token: Some("u8-refresh-token".into()),
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                    ..xai_grok_login::GrokAuth::test_default()
                });
        }
        first.release();
        if let Some(expectation) = cancelled.as_mut() {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                expectation.wait_blocked(),
            )
            .await
            .expect("retried remote compact must reach the cancellation barrier");
            fixture.actor.compaction.cancel.request_cancel();
            expectation.release();
        }
        let error = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("bounded remote auth retry must complete")
            .unwrap()
            .expect_err("scripted terminal outcome");
        first.assert_satisfied();
        if let Some(expectation) = cancelled.as_ref() {
            expectation.assert_satisfied();
        }
        let trace = guard.take();
        assert!(
            fixture.server_b.requests().is_empty(),
            "U8 retry transport must never use installed B"
        );
        let requests = fixture.server_a.requests();
        assert_eq!(
            requests.len(),
            if last_attempt { 3 } else { 2 },
            "U8 auth retry must stay inside the one-refresh attempt budget"
        );
        let preflights: Vec<_> = trace
            .refreshes
            .iter()
            .filter(|(site, _, _)| *site == "preflight")
            .cloned()
            .collect();
        assert_refreshes(
            &preflights,
            if last_attempt { &[] } else { &["preflight"] },
            &fixture.a,
            "U8 retry must perform exactly one captured-row preflight, or none on the final attempt",
        );
        assert_refreshes(
            &trace.refreshes,
            if last_attempt {
                &[]
            } else if session_token {
                &["preflight", "gated_auth"]
            } else {
                &["preflight"]
            },
            &fixture.a,
            "U8 retry counts gated auth separately from source-row preflight",
        );
        let authority_calls = if !session_token {
            0
        } else if last_attempt || matches!(scenario, RemoteRetryCase::RotatedBearer) {
            3
        } else {
            5
        };
        assert_eq!(
            fixture.refresh_count.load(Ordering::SeqCst),
            authority_calls,
            "U8 fake authority calls are independent of named preflight and send counts"
        );
        assert_eq!(trace.captured.len(), 1);
        assert_binding(&trace.captured[0], &fixture.a);
        assert_binding(
            &fixture
                .actor
                .chat_state_handle
                .capture_transport_binding()
                .await
                .unwrap(),
            &fixture.b,
        );
        let inputs = trace.inputs.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(&inputs.config).unwrap(),
            serde_json::to_value(&expected_config).unwrap()
        );
        assert_eq!(
            (
                serde_json::to_value(&inputs.tools).unwrap(),
                &inputs.hosted,
                inputs.token_kind,
                &inputs.token_input,
                inputs.tool_tokens
            ),
            (
                serde_json::to_value(&surface.tools).unwrap(),
                &surface.hosted,
                surface.token_kind,
                &surface.token_input,
                surface.tokens
            ),
            "U8 keeps the full sender's complete legacy tool and token surface",
        );
        let mut expected_request = ConversationRequest::from_items(fixture.history[1..].to_vec())
            .with_model(fixture.a.config.model.clone())
            .with_tools(surface.tools);
        expected_request.hosted_tools = surface.hosted;
        expected_request.reasoning_effort = fixture.a.config.reasoning_effort;
        expected_request.x_grok_session_id = Some(fixture.actor.session_info.id.0.to_string());
        assert_eq!(
            format!("{:?}", trace.remote_request.unwrap()),
            format!("{expected_request:?}")
        );
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.path, "/v1/responses");
            assert_eq!(request.header("x-u7-row"), Some("a"));
            let expected_key = if !session_token {
                Some("Bearer u7-a-key")
            } else if index > 0 && matches!(scenario, RemoteRetryCase::RotatedBearer) {
                Some("Bearer u8-live-rotated-bearer")
            } else {
                None
            };
            assert_eq!(
                request.authorization.as_deref(),
                expected_key,
                "U8 retry retains source credentials and live first-party bearer resolution"
            );
            let body = request.body.as_ref().unwrap();
            assert_eq!(body["model"], fixture.a.config.model);
            assert_eq!(body["reasoning"]["effort"], "high");
            assert_eq!(body["store"], false);
            assert_eq!(
                request.body, requests[0].body,
                "U8 retry preserves the complete initial A wire body"
            );
        }
        if matches!(scenario, RemoteRetryCase::Cancelled) {
            assert_eq!(
                crate::session::helpers::session_compact::compact_error_kind(&error),
                Some(crate::session::helpers::session_compact::CompactErrorKind::Cancelled)
            );
        } else {
            let message = crate::sampling::error::acp_error_message(&error);
            if matches!(
                scenario,
                RemoteRetryCase::SecondAuthFailure | RemoteRetryCase::FinalAttemptAuthFailure
            ) {
                assert_eq!(error.code, acp::Error::auth_required().code);
                assert_eq!(
                    message,
                    if session_token {
                        crate::agent::auth_method::AUTH_ERROR_SESSION_EXPIRED
                    } else {
                        crate::agent::auth_method::AUTH_ERROR_API_KEY
                    },
                    "second or final auth failure retains its method-specific terminal outcome"
                );
            } else {
                assert!(
                    message.contains("u8 terminal fixture rejection"),
                    "{message}"
                );
            }
        }
        assert_eq!(
            serde_json::to_value(fixture.actor.chat_state_handle.get_conversation().await).unwrap(),
            serde_json::to_value(&fixture.history).unwrap(),
            "U8 failure or cancellation must not install replacement history",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_auth_retry_uses_bound_row_and_one_preflight() {
        const NAME: &str = "session::acp_session::compaction::binding_tests::remote_auth_retry_uses_bound_row_and_one_preflight";
        if isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(async {
                for session_token in [false, true] {
                    for scenario in [
                        RemoteRetryCase::TerminalFailure,
                        RemoteRetryCase::SecondAuthFailure,
                        RemoteRetryCase::FinalAttemptAuthFailure,
                        RemoteRetryCase::Cancelled,
                    ] {
                        exercise_remote_auth_retry(session_token, scenario).await;
                    }
                }
                exercise_remote_auth_retry(true, RemoteRetryCase::RotatedBearer).await;
            })
            .await;
    }

    async fn candidate_builtin_surface(actor: &SessionActor, two_pass: bool) -> LegacySurface {
        let defs = actor.prepare_tool_definitions().await;
        assert!(
            defs.iter()
                .any(|definition| definition.function.name == "web_search")
        );
        assert!(
            !defs
                .iter()
                .any(|definition| definition.function.name == "mcp__u7__lookup")
        );
        let active = actor.backend_search_active();
        let filtered: Vec<_> = defs
            .into_iter()
            .filter(|definition| !active || definition.function.name != "web_search")
            .collect();
        let prepared = actor.turn_base_tool_specs(&filtered);
        let ordinary: Vec<_> = filtered.iter().cloned().map(ToolSpec::from).collect();
        assert!(
            prepared
                .iter()
                .any(|spec| spec.exposure == ToolExposure::Deferred)
        );
        assert!(
            ordinary
                .iter()
                .all(|spec| spec.exposure == ToolExposure::Immediate)
        );
        assert_ne!(
            serde_json::to_value(&prepared).unwrap(),
            serde_json::to_value(&ordinary).unwrap(),
            "U5 operator exposure distinguishes the two landed conversions"
        );
        assert_eq!(
            ordinary.iter().any(|spec| spec.name == "web_search"),
            !active
        );
        let hosted = actor.hosted_tools_for_turn();
        assert_eq!(hosted.is_empty(), !active);
        if two_pass {
            LegacySurface {
                token_kind: "specs",
                token_input: serde_json::to_value(&prepared).unwrap(),
                tokens: xai_chat_state::estimate_tool_specs_tokens(&prepared),
                tools: prepared,
                hosted,
            }
        } else {
            LegacySurface {
                token_kind: "definitions",
                token_input: serde_json::to_value(&filtered).unwrap(),
                tokens: xai_chat_state::estimate_tool_definitions_tokens(&filtered),
                tools: ordinary,
                hosted,
            }
        }
    }

    async fn register_compaction_candidate(actor: &SessionActor) {
        use xai_grok_tools::types::tool::ToolNamespace;
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}},
            "required": ["query"],
            "additionalProperties": false
        });
        let bridge = actor.agent.borrow().tool_bridge().clone();
        bridge
            .register_mcp_tools(
                "u5__lookup".into(),
                support::StubMcpTool("u5__lookup"),
                Some(schema.clone()),
            )
            .unwrap();
        assert!(xai_grok_mcp::servers::parse_mcp_qualified_name("u5__lookup").is_some());
        let typed: Vec<_> = bridge
            .toolset()
            .tools_namespaced()
            .into_iter()
            .filter(|(definition, _)| definition.function.name == "u5__lookup")
            .collect();
        assert_eq!(typed.len(), 1);
        assert_eq!(typed[0].1, ToolNamespace::MCP);
        let expected = vec![ToolSpec {
            name: "u5__lookup".into(),
            description: Some("stub MCP tool".into()),
            parameters: schema,
            exposure: ToolExposure::Immediate,
        }];
        let (prepared, _) = actor.prepare_native_tool_candidates_timed().await;
        assert_eq!(
            serde_json::to_value(&prepared.mcp_specs).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "U5 has a real parser-qualified candidate distinct from the inherited invalid stub"
        );
        let mut row = actor
            .chat_state_handle
            .capture_transport_binding()
            .await
            .unwrap()
            .config;
        row.api_backend = ApiBackend::Messages;
        row.supports_search_tool = true;
        let selected = actor
            .turn_native_tool_candidates(&prepared.builtin_defs, &prepared.mcp_specs)
            .select_for_row(&row);
        let selected_mcp: Vec<_> = selected
            .into_iter()
            .filter(|spec| spec.name == "u5__lookup")
            .collect();
        let mut expected = expected;
        expected[0].exposure = ToolExposure::Deferred;
        assert_eq!(
            serde_json::to_value(&selected_mcp).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }

    async fn exercise_candidate_surface(case: Case, two_pass: bool, backend_search: bool) {
        let baseline = fixture(&case).await;
        let candidate = fixture(&case).await;
        for fixture in [&baseline, &candidate] {
            fixture.actor.supports_backend_search.set(backend_search);
        }
        let surface = candidate_builtin_surface(&baseline.actor, two_pass).await;
        register_compaction_candidate(&candidate.actor).await;
        let mut baseline_wire_tools = None;
        for (index, (fixture, interleave)) in
            [(&baseline, false), (&candidate, false), (&candidate, true)]
                .into_iter()
                .enumerate()
        {
            fixture
                .actor
                .chat_state_handle
                .install_transport_binding(fixture.a.clone())
                .await
                .unwrap();
            fixture
                .actor
                .chat_state_handle
                .replace_conversation(fixture.history.clone());
            fixture.actor.chat_state_handle.get_conversation().await;
            fixture.refresh_count.store(0, Ordering::SeqCst);
            let config = fixture
                .actor
                .assemble_sampler_config_without_refresh(&fixture.a)
                .await;
            let guard = TraceGuard::new(
                interleave.then(|| fixture.b.clone()),
                fixture.refresh_count.clone(),
            );
            let succeeded = run_sender(fixture, &case, two_pass).await;
            let trace = guard.take();
            assert!(succeeded, "U5 actual local compaction send completes");
            let inputs = trace
                .inputs
                .as_ref()
                .expect("U5 actual generator boundary reached");
            assert_eq!(
                (serde_json::to_value(&inputs.tools).unwrap(), &inputs.hosted),
                (
                    serde_json::to_value(&surface.tools).unwrap(),
                    &surface.hosted
                ),
                "U5 complete compaction surface equals its isolated builtin baseline"
            );
            assert_eq!(
                (inputs.token_kind, &inputs.token_input, inputs.tool_tokens),
                (surface.token_kind, &surface.token_input, surface.tokens),
                "U5 compaction token accounting keeps its distinct builtin input"
            );
            assert_inputs(fixture, &case, &trace, &surface, &config, two_pass);
            assert!(
                fixture.server_b.requests().is_empty(),
                "U5 sender never selects the installed B transport"
            );
            let requests = fixture.server_a.requests();
            assert_eq!(requests.len(), if index == 2 { 2 } else { 1 });
            let request = requests.last().unwrap();
            assert_eq!(
                request.path,
                if case.backend == ApiBackend::Messages {
                    "/v1/messages"
                } else {
                    "/v1/responses"
                }
            );
            assert_eq!(request.authorization.as_deref(), Some("Bearer u7-a-key"));
            assert_eq!(request.header("x-u7-row"), Some("a"));
            let body = request.body.as_ref().unwrap();
            assert_eq!(body["model"], fixture.a.config.model);
            if let Some(expected) = &baseline_wire_tools {
                assert_eq!(
                    &body["tools"], expected,
                    "U5 complete sent tools add no candidate-induced MCP or native entry"
                );
            } else {
                baseline_wire_tools = Some(body["tools"].clone());
            }
            assert_binding(
                &fixture
                    .actor
                    .chat_state_handle
                    .capture_transport_binding()
                    .await
                    .unwrap(),
                if interleave { &fixture.b } else { &fixture.a },
            );
        }
    }

    async fn exercise_candidate_rows(two_pass: bool) {
        for (backend, admitted) in [
            (ApiBackend::Messages, true),
            (ApiBackend::Messages, false),
            (ApiBackend::Responses, true),
            (ApiBackend::Responses, false),
        ] {
            for backend_search in [false, true] {
                exercise_candidate_surface(
                    Case {
                        backend: backend.clone(),
                        admitted,
                        session_token: false,
                        remote: false,
                    },
                    two_pass,
                    backend_search,
                )
                .await;
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_pass_candidates_do_not_change_builtin_surface() {
        const NAME: &str = "session::acp_session::compaction::binding_tests::two_pass_candidates_do_not_change_builtin_surface";
        if isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(exercise_candidate_rows(true))
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_compaction_candidates_do_not_change_builtin_surface() {
        const NAME: &str = "session::acp_session::compaction::binding_tests::full_compaction_candidates_do_not_change_builtin_surface";
        if isolated(NAME) {
            return;
        }
        tokio::task::LocalSet::new()
            .run_until(exercise_candidate_rows(false))
            .await;
    }
}
