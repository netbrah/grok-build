//! Length-salvage policy: budget resolution, the continue reminder, and the per-turn continue/exhaust state machine.

use super::*;

/// Matches the agent implementation's `MAX_RETRY_ITERATIONS`.
const CURSOR_LENGTH_CONTINUE_BUDGET: u32 = 5;

const DEFAULT_LENGTH_CONTINUE_BUDGET: u32 = 2;

/// ANTHROPIC-WIRE-2 (cut 4): default continue budget for subagent turns —
/// the qwen-code `MAX_OUTPUT_RECOVERY_ATTEMPTS = 3` recovery tier
/// (geminiChat.ts:563, .41.1 WAVE-3). Subagent turns default salvage on with
/// this budget; every explicit tier (cursor, env, remote) and both kill
/// switches still outrank it.
const SUBAGENT_LENGTH_CONTINUE_BUDGET: u32 = 3;

/// This reminder is injected once per turn on the first continue, wrapped in `SessionActor::reminder_wrapper_tag`.
/// The trailing clause keeps a stranded copy from hijacking the user's next prompt.
pub(super) const LENGTH_CONTINUE_REMINDER_BODY: &str = "Your previous response exceeded the output token \
     limit and was cut off. Continue from exactly where it stopped — or if a newer user \
     message follows this note, answer that instead.";

/// Pure form of [`SessionActor::length_salvage_budget`].
/// Kill switches are absolute and outrank every tier, including the always-on cursor one: an explicit `GROK_LENGTH_SALVAGE=0` locally, and the remote `length_salvage_budget = 0` fleet-wide.
/// Otherwise the precedence is cursor, then env opt-in, then remote budget, then the subagent default, then off.
pub(super) fn resolve_length_salvage_budget(
    is_cursor: bool,
    env: Option<bool>,
    remote: Option<u32>,
    is_subagent: bool,
) -> Option<u32> {
    if env == Some(false) || remote == Some(0) {
        return None;
    }
    if is_cursor {
        return Some(CURSOR_LENGTH_CONTINUE_BUDGET);
    }
    if env == Some(true) {
        return Some(DEFAULT_LENGTH_CONTINUE_BUDGET);
    }
    // ANTHROPIC-WIRE-2 (cut 4): subagent turns default on below every explicit
    // tier and above off — an explicit fleet budget (even 1) still wins, and
    // the kill switches above already returned.
    remote.or_else(|| is_subagent.then_some(SUBAGENT_LENGTH_CONTINUE_BUDGET))
}

impl SessionActor {
    /// `Some(budget)` salvages Length truncations (partial commit and bounded continues); `None` hard-fails.
    /// Always on when [`SessionActor::is_cursor_agent`]; otherwise the `GROK_LENGTH_SALVAGE` env var (debug override), then the `length_salvage_budget` remote setting.
    pub(super) fn length_salvage_budget(&self) -> Option<u32> {
        resolve_length_salvage_budget(
            self.is_cursor_agent(),
            xai_grok_config::env_bool("GROK_LENGTH_SALVAGE"),
            self.length_salvage_remote_budget,
            self.startup_hints.is_subagent,
        )
    }
}

/// The turn loop's next step for a `Length`-stopped response.
pub(super) enum SalvageStep {
    /// Retry the step; inject the once-per-turn reminder when set.
    Continue { inject_reminder: bool },
    /// Budget just ran out: log once, then complete the turn truncated.
    Exhaust,
    /// Only the truncation mark (already exhausted, or salvage disabled).
    None,
}

/// Per-turn Length-salvage state.
pub(super) struct LengthSalvage {
    budget: Option<u32>,
    continues: u32,
    /// True while the next sample is a salvage continuation; cleared when its response arrives.
    awaiting_continuation: bool,
    /// Set while the next continue should inject the reminder; cleared on injection (the reminder stays in context for the rest of the run).
    /// Set again at an answer boundary so a second truncation run in the same prompt gets its own cue.
    reminder_armed: bool,
    /// The latest answer is known to be cut off, so the turn reports `MaxTokens` and the TodoGate disengages.
    /// Cleared at a round boundary (stop-hook feedback, goal directive, recovery prompt).
    /// A fresh round that finishes the cut work cleanly reports `EndTurn`.
    truncated: bool,
    /// Sticky for the whole prompt: the exhaustion event fires once even when later rounds spend the already-empty budget again.
    exhaustion_reported: bool,
    /// ANTHROPIC-WIRE-2 (cut 4): the escalated output cap for this turn —
    /// max(64K, the alias-aware R5 table row), i.e. the W1
    /// `responses_budget_fallback`. `None` unless salvage is on for the turn;
    /// the bump applies to every continuation sample from the first Length
    /// stop on (outside the continue budget).
    escalation_target: Option<u32>,
    /// ANTHROPIC-WIRE-2 (cut 4): set by the first Length stop that took the
    /// free escalation retry; the bumped cap stays in force for the rest of
    /// the prompt (qwen-code runs its recovery loop at the escalated limit).
    escalated: bool,
    /// ANTHROPIC-WIRE-2 (cut 4): the cap the last-sampled request carried
    /// (post-clamp), so the first Length stop can skip a no-op bump —
    /// qwen-code skips a no-op escalation and runs recovery instead.
    sampled_cap: Option<u32>,
}

impl LengthSalvage {
    pub(super) fn new(budget: Option<u32>) -> Self {
        Self {
            // `Some(0)` is the rollout flag's explicit off switch
            budget: budget.filter(|b| *b > 0),
            continues: 0,
            awaiting_continuation: false,
            reminder_armed: true,
            truncated: false,
            exhaustion_reported: false,
            escalation_target: None,
            escalated: false,
            sampled_cap: None,
        }
    }

    /// ANTHROPIC-WIRE-2 (cut 4): arm the escalation bump for this turn
    /// (max(64K, the alias-aware R5 table row)); `None` keeps the
    /// pre-escalation behavior exactly.
    pub(super) fn with_escalation_target(mut self, target: Option<u32>) -> Self {
        self.escalation_target = target;
        self
    }

    /// ANTHROPIC-WIRE-2 (cut 4): the escalated cap the continuations must
    /// carry once the escalation fired; `None` when never armed.
    pub(super) fn escalation_target(&self) -> Option<u32> {
        self.escalation_target
    }

    /// ANTHROPIC-WIRE-2 (cut 4): true once the escalation retry fired.
    pub(super) fn escalated(&self) -> bool {
        self.escalated
    }

    /// ANTHROPIC-WIRE-2 (cut 4): remember the cap the last-sampled request
    /// carried (post-clamp) for the no-op escalation check.
    pub(super) fn note_sample_cap(&mut self, cap: Option<u32>) {
        self.sampled_cap = cap;
    }

    pub(super) fn enabled(&self) -> bool {
        self.budget.is_some()
    }

    pub(super) fn budget(&self) -> u32 {
        self.budget.unwrap_or(0)
    }

    pub(super) fn continues(&self) -> u32 {
        self.continues
    }

    /// True once any continue ran: the answer spans multiple segments.
    pub(super) fn any_continues(&self) -> bool {
        self.continues > 0
    }

    pub(super) fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// True while a salvage continuation is in flight (its response has not arrived), so its failure can complete the turn instead of erroring.
    pub(super) fn awaiting_continuation(&self) -> bool {
        self.awaiting_continuation
    }

    /// The in-flight sample produced a response (or its slot was abandoned).
    pub(super) fn response_arrived(&mut self) {
        self.awaiting_continuation = false;
    }

    /// An answer boundary (a tool step or a failed continuation) ended the current run.
    /// A later truncation starts a new run and gets its own reminder; the previous one is stale or fell out of context.
    pub(super) fn step_boundary(&mut self) {
        self.reminder_armed = true;
    }

    /// A round boundary (stop-hook feedback, goal directive, recovery prompt, drained interjection) starts a fresh answer.
    /// Clearing the mark lets a round that finishes the cut work cleanly report `EndTurn` and re-engage the TodoGate.
    /// The budget stays spent and `exhaustion_reported` stays set.
    pub(super) fn round_boundary(&mut self) {
        self.step_boundary();
        self.truncated = false;
    }

    /// Advance the state machine for a `Length`-stopped response.
    pub(super) fn on_length_stop(&mut self) -> SalvageStep {
        // ANTHROPIC-WIRE-2 (cut 4): the first Length stop takes the one free
        // escalation retry — the bumped cap applies from this point on,
        // OUTSIDE the continue budget (qwen-code escalates once, outside the
        // retry loop). A no-op bump (cap already at/above the target, or an
        // unknown cap that fills to the fallback) takes no free retry.
        if self.enabled()
            && let Some(target) = self.escalation_target
            && !self.escalated
            && self.sampled_cap.map_or(true, |cap| cap < target)
        {
            self.escalated = true;
            self.awaiting_continuation = true;
            let inject_reminder = self.reminder_armed;
            self.reminder_armed = false;
            return SalvageStep::Continue { inject_reminder };
        }
        if self.continues < self.budget() {
            self.continues += 1;
            self.awaiting_continuation = true;
            let inject_reminder = self.reminder_armed;
            self.reminder_armed = false;
            return SalvageStep::Continue { inject_reminder };
        }
        // Report once per prompt; a leaked Length with salvage off is not an exhaustion
        let report_exhaustion = !self.exhaustion_reported && self.enabled();
        self.truncated = true;
        self.exhaustion_reported = true;
        if report_exhaustion {
            SalvageStep::Exhaust
        } else {
            SalvageStep::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_agent_always_gets_the_cursor_budget() {
        assert_eq!(
            resolve_length_salvage_budget(true, None, None, false),
            Some(CURSOR_LENGTH_CONTINUE_BUDGET)
        );
        assert_eq!(
            resolve_length_salvage_budget(true, Some(true), None, false),
            Some(CURSOR_LENGTH_CONTINUE_BUDGET),
            "cursor budget wins over the env opt-in"
        );
        assert_eq!(
            resolve_length_salvage_budget(true, None, Some(3), false),
            Some(CURSOR_LENGTH_CONTINUE_BUDGET),
            "a nonzero remote budget does not shrink the cursor tier"
        );
    }

    #[test]
    fn explicit_env_false_kills_every_tier() {
        assert_eq!(
            resolve_length_salvage_budget(true, Some(false), None, false),
            None,
            "the kill switch outranks the always-on cursor tier"
        );
        assert_eq!(
            resolve_length_salvage_budget(false, Some(false), None, false),
            None
        );
        assert_eq!(
            resolve_length_salvage_budget(false, Some(false), Some(3), false),
            None,
            "the env kill outranks a remote budget"
        );
    }

    #[test]
    fn remote_zero_kills_every_tier_including_cursor() {
        assert_eq!(
            resolve_length_salvage_budget(true, None, Some(0), false),
            None,
            "the remote kill is the server-side off switch for cursor"
        );
        assert_eq!(
            resolve_length_salvage_budget(false, None, Some(0), false),
            None
        );
        assert_eq!(
            resolve_length_salvage_budget(true, Some(true), Some(0), false),
            None,
            "the remote kill outranks the env opt-in and the cursor tier"
        );
        assert_eq!(
            resolve_length_salvage_budget(false, Some(true), Some(0), false),
            None,
            "the remote kill outranks the env opt-in"
        );
    }

    #[test]
    fn env_override_beats_a_nonzero_remote_budget() {
        assert_eq!(
            resolve_length_salvage_budget(false, Some(true), Some(9), false),
            Some(DEFAULT_LENGTH_CONTINUE_BUDGET)
        );
    }

    #[test]
    fn remote_budget_enables_default_agents() {
        assert_eq!(
            resolve_length_salvage_budget(false, None, Some(3), false),
            Some(3)
        );
    }

    #[test]
    fn env_gate_enables_the_default_budget() {
        assert_eq!(
            resolve_length_salvage_budget(false, Some(true), None, false),
            Some(2)
        );
    }

    #[test]
    fn disabled_without_cursor_or_env() {
        assert_eq!(
            resolve_length_salvage_budget(false, None, None, false),
            None,
        );
    }

    #[test]
    fn subagent_tier_defaults_the_budget_when_nothing_else_is_set() {
        assert_eq!(
            resolve_length_salvage_budget(false, None, None, true),
            Some(SUBAGENT_LENGTH_CONTINUE_BUDGET),
            "subagent turns default on with the qwen-code 3-attempt recovery budget"
        );
        assert_eq!(
            resolve_length_salvage_budget(false, None, None, false),
            None,
            "default agents stay off without a tier"
        );
    }

    #[test]
    fn subagent_tier_loses_to_every_explicit_tier_and_kill() {
        // Cursor, env opt-in, and an explicit remote budget all outrank the
        // subagent default...
        assert_eq!(
            resolve_length_salvage_budget(true, None, None, true),
            Some(CURSOR_LENGTH_CONTINUE_BUDGET)
        );
        assert_eq!(
            resolve_length_salvage_budget(false, Some(true), None, true),
            Some(DEFAULT_LENGTH_CONTINUE_BUDGET)
        );
        assert_eq!(
            resolve_length_salvage_budget(false, None, Some(7), true),
            Some(7),
            "an explicit fleet budget is operator-visible and wins"
        );
        // ...and both kill switches outrank it.
        assert_eq!(
            resolve_length_salvage_budget(false, Some(false), None, true),
            None
        );
        assert_eq!(
            resolve_length_salvage_budget(false, None, Some(0), true),
            None,
            "the remote kill zero turns subagent salvage off too"
        );
    }

    // --- ANTHROPIC-WIRE-2 (cut 4): salvage escalation ---------------------

    #[test]
    fn escalation_first_length_stop_is_a_free_continue_outside_the_budget() {
        let mut s = LengthSalvage::new(Some(1)).with_escalation_target(Some(128_000));
        s.note_sample_cap(Some(4_096));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: true
            }
        ));
        assert_eq!(
            s.continues(),
            0,
            "the escalation retry is outside the continue budget"
        );
        assert!(s.escalated());
        // The budgeted continue still follows, at the escalated cap.
        s.response_arrived();
        s.note_sample_cap(Some(128_000));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: false
            }
        ));
        assert_eq!(s.continues(), 1);
        s.response_arrived();
        assert!(matches!(s.on_length_stop(), SalvageStep::Exhaust));
        assert!(s.is_truncated());
    }

    #[test]
    fn escalation_skips_a_no_op_bump_and_keeps_the_budget() {
        let mut s = LengthSalvage::new(Some(1)).with_escalation_target(Some(64_000));
        s.note_sample_cap(Some(128_000));
        assert!(matches!(s.on_length_stop(), SalvageStep::Continue { .. }));
        assert_eq!(
            s.continues(),
            1,
            "a no-op bump must not burn a free retry (qwen-code skips it)"
        );
        assert!(!s.escalated());
        s.response_arrived();
        assert!(matches!(s.on_length_stop(), SalvageStep::Exhaust));
    }

    #[test]
    fn escalation_never_fires_without_a_target() {
        let mut s = LengthSalvage::new(Some(1));
        s.note_sample_cap(Some(4_096));
        assert!(matches!(s.on_length_stop(), SalvageStep::Continue { .. }));
        assert_eq!(
            s.continues(),
            1,
            "no target: the first continue is budgeted, exactly as today"
        );
    }

    #[test]
    fn disabled_salvage_never_escalates() {
        let mut s = LengthSalvage::new(None).with_escalation_target(Some(128_000));
        s.note_sample_cap(Some(4_096));
        assert!(matches!(s.on_length_stop(), SalvageStep::None));
        assert!(s.is_truncated());
        assert!(!s.escalated());
        assert_eq!(s.continues(), 0);
    }

    #[test]
    fn continues_until_budget_then_exhausts_once() {
        let mut s = LengthSalvage::new(Some(2));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: true
            }
        ));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: false
            }
        ));
        assert!(!s.is_truncated());
        assert!(matches!(s.on_length_stop(), SalvageStep::Exhaust));
        assert!(s.is_truncated());
        // Truncation is sticky within the round and exhaustion reports once.
        assert!(matches!(s.on_length_stop(), SalvageStep::None));
        assert!(s.is_truncated());
        assert!(s.any_continues());
    }

    #[test]
    fn round_boundary_clears_the_mark_but_not_the_spent_budget() {
        let mut s = LengthSalvage::new(Some(1));
        assert!(matches!(s.on_length_stop(), SalvageStep::Continue { .. }));
        assert!(matches!(s.on_length_stop(), SalvageStep::Exhaust));
        assert!(s.is_truncated());
        // A stop-hook, goal, or recovery round that finishes the cut work cleanly must report EndTurn again...
        s.round_boundary();
        assert!(!s.is_truncated());
        // ...but the budget stays spent and the exhaustion event stays reported: a new cut re-marks silently
        assert!(matches!(s.on_length_stop(), SalvageStep::None));
        assert!(s.is_truncated());
    }

    #[test]
    fn step_boundary_rearms_the_reminder_for_a_new_run() {
        let mut s = LengthSalvage::new(Some(3));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: true
            }
        ));
        assert!(matches!(
            s.on_length_stop(),
            SalvageStep::Continue {
                inject_reminder: false
            }
        ));
        s.step_boundary();
        assert!(
            matches!(
                s.on_length_stop(),
                SalvageStep::Continue {
                    inject_reminder: true
                }
            ),
            "a second truncation run gets its own reminder"
        );
    }

    #[test]
    fn awaiting_continuation_tracks_the_in_flight_sample() {
        let mut s = LengthSalvage::new(Some(2));
        assert!(!s.awaiting_continuation());
        assert!(matches!(s.on_length_stop(), SalvageStep::Continue { .. }));
        assert!(s.awaiting_continuation());
        s.response_arrived();
        assert!(!s.awaiting_continuation(), "served continuations clear it");
    }

    #[test]
    fn zero_budget_is_explicit_off() {
        let s = LengthSalvage::new(Some(0));
        assert!(!s.enabled(), "Some(0) must not opt requests into salvage");
    }

    #[test]
    fn disabled_leak_marks_truncated_without_exhaustion_report() {
        let mut s = LengthSalvage::new(None);
        assert!(!s.enabled());
        assert!(matches!(s.on_length_stop(), SalvageStep::None));
        assert!(s.is_truncated());
        assert!(!s.any_continues());
    }
}
