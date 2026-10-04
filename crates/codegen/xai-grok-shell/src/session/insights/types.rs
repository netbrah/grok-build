//! The facet TYPE SPINE (SDD §5; item 1's second cycle): the shared pure
//! types §4's diagram names in `insights/types.rs` — `CappedBy` and the six
//! closed facet variant sets, with one `X_VARIANTS` const per set as the
//! single source of truth. §7's JSON Schema `enum` arrays are built from
//! the consts rather than typed beside the prompt, so the prompt and the
//! type cannot drift; T-16 (SDD §12) asserts the closure, the order and
//! the wire round-trip.
//!
//! Pure types only — no I/O, no orchestration. `SessionFacets` (item 8's
//! `label.rs`) and `Aggregate`'s `friction`/`outcomes` fields (item 2b)
//! consume these enums; neither is declared here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which ceiling bound a run (SDD §5): the four members §11's fence comment
/// enumerates. The `capped_by` field on `Totals` (item 3's) is the sorted,
/// deduped `Vec<CappedBy>` that mirrors this one source into the footer and
/// the `insights_generated` event; the TYPE is this cycle's, the field is
/// item 3's.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CappedBy {
    Calls,
    ChunkPerSession,
    ChunkPerRun,
    Cost,
}

// The six closed sets (SDD §5). All six carry the same derive line and the
// same serde attribute; `rename_all` is what makes the Rust spelling and the
// wire spelling agree, so `FullyAchieved` is `fully_achieved` on the wire
// and in §7's schema without a rename per variant.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    FullyAchieved,
    MostlyAchieved,
    PartiallyAchieved,
    NotAchieved,
    UnclearFromTranscript,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Helpfulness {
    Unhelpful,
    SlightlyHelpful,
    ModeratelyHelpful,
    VeryHelpful,
    Essential,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    SingleTask,
    MultiTask,
    IterativeRefinement,
    Exploration,
    QuickQuestion,
}

/// 8: the prompt's 7 plus `handled_complexity`, which the label map knows and the prompt does not.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SuccessMode {
    None,
    FastAccurateSearch,
    CorrectCodeEdits,
    GoodExplanations,
    ProactiveHelp,
    MultiFileChanges,
    HandledComplexity,
    GoodDebugging,
}

/// 8, not the prose's 5: the label map also carries `unsure`, `neutral` and `delighted`, and `Dt`
/// charts `unsure`. `Dt`'s 6 are a subset of the label map's 8, so the union is the map's.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Satisfaction {
    Frustrated,
    Dissatisfied,
    LikelySatisfied,
    Satisfied,
    Happy,
    Unsure,
    Neutral,
    Delighted,
}

/// 12, not the prose's 5. This is the widest of the six and the reason §5's "enums, not free
/// strings" departure needed the sets written down before anyone could implement it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Friction {
    MisunderstoodRequest,
    WrongApproach,
    BuggyCode,
    UserRejectedAction,
    ClaudeGotBlocked,
    UserStoppedEarly,
    WrongFileOrLocation,
    ExcessiveChanges,
    SlowOrVerbose,
    ToolFailed,
    UserUnclear,
    ExternalIssue,
}

// The `X_VARIANTS` consts, one per set, are the single source of truth: §7's JSON Schema `enum`
// arrays are built from them rather than typed beside the prompt, so prompt and type cannot
// drift, and T-16 asserts against them. Each lists the FULL closed variant list of its set, in
// §5 declaration order. (No const for `CappedBy`: it is not a facet variant set.)

/// `Outcome` variants, in §5 declaration order.
pub const OUTCOME_VARIANTS: &[Outcome] = &[
    Outcome::FullyAchieved,
    Outcome::MostlyAchieved,
    Outcome::PartiallyAchieved,
    Outcome::NotAchieved,
    Outcome::UnclearFromTranscript,
];

/// `Helpfulness` variants, in §5 declaration order.
pub const HELPFULNESS_VARIANTS: &[Helpfulness] = &[
    Helpfulness::Unhelpful,
    Helpfulness::SlightlyHelpful,
    Helpfulness::ModeratelyHelpful,
    Helpfulness::VeryHelpful,
    Helpfulness::Essential,
];

/// `SessionType` variants, in §5 declaration order.
pub const SESSION_TYPE_VARIANTS: &[SessionType] = &[
    SessionType::SingleTask,
    SessionType::MultiTask,
    SessionType::IterativeRefinement,
    SessionType::Exploration,
    SessionType::QuickQuestion,
];

/// `SuccessMode` variants, in §5 declaration order.
pub const SUCCESS_MODE_VARIANTS: &[SuccessMode] = &[
    SuccessMode::None,
    SuccessMode::FastAccurateSearch,
    SuccessMode::CorrectCodeEdits,
    SuccessMode::GoodExplanations,
    SuccessMode::ProactiveHelp,
    SuccessMode::MultiFileChanges,
    SuccessMode::HandledComplexity,
    SuccessMode::GoodDebugging,
];

/// `Satisfaction` variants, in §5 declaration order.
pub const SATISFACTION_VARIANTS: &[Satisfaction] = &[
    Satisfaction::Frustrated,
    Satisfaction::Dissatisfied,
    Satisfaction::LikelySatisfied,
    Satisfaction::Satisfied,
    Satisfaction::Happy,
    Satisfaction::Unsure,
    Satisfaction::Neutral,
    Satisfaction::Delighted,
];

/// `Friction` variants, in §5 declaration order.
pub const FRICTION_VARIANTS: &[Friction] = &[
    Friction::MisunderstoodRequest,
    Friction::WrongApproach,
    Friction::BuggyCode,
    Friction::UserRejectedAction,
    Friction::ClaudeGotBlocked,
    Friction::UserStoppedEarly,
    Friction::WrongFileOrLocation,
    Friction::ExcessiveChanges,
    Friction::SlowOrVerbose,
    Friction::ToolFailed,
    Friction::UserUnclear,
    Friction::ExternalIssue,
];

/// The budget + control knobs item 8's functions consume (SOT §6; 8a's
/// R-budget). One pure home — 8b/8c consume the SAME struct (they do not split
/// it). Item 5 (deferred) later populates it from the `[insights.*]` CLI
/// values; 8a's tests construct it directly with known values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsightsBudget {
    /// The total model-call ceiling (facet + chunk, the shared guard counter). 208.
    pub max_calls: usize,
    /// The per-session chunk-call ceiling (the digest's `ChunkCapHit`). 4.
    pub max_chunk_calls_per_session: usize,
    /// The run-level chunk-call backstop. 160.
    pub max_chunk_calls_per_run: usize,
    /// The cost ceiling in USD ticks (≈ 3.29e10, §6 :940-990).
    pub max_cost_ticks: i64,
    /// The session-label ceiling (`label.max_sessions`). 40.
    pub max_sessions: usize,
    /// The shared model-output cap (`narrate.max_tokens`). 4096.
    pub max_tokens: u32,
    /// The in-flight bound (`label.concurrency`) — the cost-overshoot bound. 8.
    pub concurrency: usize,
    /// The warm-run floor; must not be tuned below it. 8.
    pub confirm_above_calls: usize,
}

impl Default for InsightsBudget {
    fn default() -> Self {
        Self {
            max_calls: 208,
            max_chunk_calls_per_session: 4,
            max_chunk_calls_per_run: 160,
            max_cost_ticks: 32_900_000_000,
            max_sessions: 40,
            max_tokens: 4096,
            concurrency: 8,
            confirm_above_calls: 8,
        }
    }
}

/// The aggregate half of the two-tier narrate payload (R-narrate-stats — one
/// pure home in the type spine, the same treatment as 8a's `InsightsBudget`):
/// the 13 keys §7's payload names verbatim (`commits` DROPPED — nothing
/// produces a commit count). `top_tools` / `top_goals` never exceed 8
/// entries in the payload (the builder caps them). 8c's orchestrator fills
/// it from the real `Aggregate` + the digests; 8b's tests construct it
/// directly.
#[derive(Debug, Clone, PartialEq)]
pub struct NarrateStats {
    pub sessions: u32,
    pub analyzed: u32,
    pub date_range: String,
    pub messages: u32,
    pub hours: f64,
    pub cost_usd: f64,
    pub top_tools: Vec<(String, u32)>,
    pub top_goals: Vec<(String, u32)>,
    pub outcomes: BTreeMap<Outcome, u32>,
    pub satisfaction: BTreeMap<Satisfaction, u32>,
    pub friction: BTreeMap<Friction, u32>,
    pub success: BTreeMap<SuccessMode, u32>,
    pub languages: BTreeMap<String, u32>,
}

#[cfg(test)]
mod tests {
    //! T-16 (SDD §12): the six facet variant sets are closed and are exactly
    //! what §5 declares — each `X_VARIANTS` const equals §5's table (same
    //! count, same members, same order as its enum), every variant
    //! round-trips through the `rename_all = "snake_case"` spelling the wire
    //! and §7's schema both use, and a reply carrying a value outside the
    //! set is refused by validation rather than dropped or drawn as a
    //! phantom bar.

    use super::*;
    use serde::{Serialize, de::DeserializeOwned};
    use std::fmt::Debug;

    /// (a)+(b): `variants` must be exactly §5's table for this set — same
    /// count, same members, same order as the enum — and each variant must
    /// round-trip through its snake_case wire spelling.
    fn assert_variant_spine<T>(name: &str, variants: &[T], wire: &[&str])
    where
        T: Serialize + DeserializeOwned + Copy + Debug + PartialEq,
    {
        assert_eq!(
            variants.len(),
            wire.len(),
            "{name}: the const must list exactly §5's variants"
        );
        for (variant, spelling) in variants.iter().zip(wire.iter()) {
            let encoded = serde_json::to_string(variant)
                .unwrap_or_else(|e| panic!("{name}: serializing {spelling}: {e}"));
            assert_eq!(
                encoded,
                format!("\"{spelling}\""),
                "{name}: the const's member here must serialize to {spelling} (got {encoded}) — §5's order is load-bearing"
            );
            let decoded: T = serde_json::from_str(&encoded)
                .unwrap_or_else(|e| panic!("{name}: deserializing {encoded}: {e}"));
            assert_eq!(
                decoded, *variant,
                "{name}: {spelling} did not round-trip to the same variant"
            );
        }
    }

    /// (c): a value outside the set is refused by validation — no catch-all
    /// variant, no silent default — so the session gets badged (§9 rule 3)
    /// instead of a phantom bar.
    fn assert_out_of_set_refused<T: DeserializeOwned>(name: &str, reply: &str) {
        let err = serde_json::from_str::<T>(reply).err().unwrap_or_else(|| {
            panic!("{name}: out-of-set reply {reply} was accepted — validation must refuse it")
        });
        assert!(
            err.to_string().contains("unknown variant"),
            "{name}: {reply} was refused, but not as an unknown variant: {err}"
        );
    }

    /// (d) — reviewer-minor follow-up: (a)+(b) pin each const's length and
    /// each const member's serde spelling, but nothing exhausts an enum, so
    /// a mutant that adds a variant to an enum without adding it to its
    /// `X_VARIANTS` const slips through green — the const is merely shorter,
    /// still length-consistent and correctly spelled. These six exhaustive
    /// matches make that direction a COMPILE ERROR (E0004; no wildcard
    /// arms): the compiler is the lock the const's type alone does not
    /// give.
    fn outcome_wire(v: Outcome) -> &'static str {
        match v {
            Outcome::FullyAchieved => "fully_achieved",
            Outcome::MostlyAchieved => "mostly_achieved",
            Outcome::PartiallyAchieved => "partially_achieved",
            Outcome::NotAchieved => "not_achieved",
            Outcome::UnclearFromTranscript => "unclear_from_transcript",
        }
    }

    fn helpfulness_wire(v: Helpfulness) -> &'static str {
        match v {
            Helpfulness::Unhelpful => "unhelpful",
            Helpfulness::SlightlyHelpful => "slightly_helpful",
            Helpfulness::ModeratelyHelpful => "moderately_helpful",
            Helpfulness::VeryHelpful => "very_helpful",
            Helpfulness::Essential => "essential",
        }
    }

    fn session_type_wire(v: SessionType) -> &'static str {
        match v {
            SessionType::SingleTask => "single_task",
            SessionType::MultiTask => "multi_task",
            SessionType::IterativeRefinement => "iterative_refinement",
            SessionType::Exploration => "exploration",
            SessionType::QuickQuestion => "quick_question",
        }
    }

    fn success_mode_wire(v: SuccessMode) -> &'static str {
        match v {
            SuccessMode::None => "none",
            SuccessMode::FastAccurateSearch => "fast_accurate_search",
            SuccessMode::CorrectCodeEdits => "correct_code_edits",
            SuccessMode::GoodExplanations => "good_explanations",
            SuccessMode::ProactiveHelp => "proactive_help",
            SuccessMode::MultiFileChanges => "multi_file_changes",
            SuccessMode::HandledComplexity => "handled_complexity",
            SuccessMode::GoodDebugging => "good_debugging",
        }
    }

    fn satisfaction_wire(v: Satisfaction) -> &'static str {
        match v {
            Satisfaction::Frustrated => "frustrated",
            Satisfaction::Dissatisfied => "dissatisfied",
            Satisfaction::LikelySatisfied => "likely_satisfied",
            Satisfaction::Satisfied => "satisfied",
            Satisfaction::Happy => "happy",
            Satisfaction::Unsure => "unsure",
            Satisfaction::Neutral => "neutral",
            Satisfaction::Delighted => "delighted",
        }
    }

    fn friction_wire(v: Friction) -> &'static str {
        match v {
            Friction::MisunderstoodRequest => "misunderstood_request",
            Friction::WrongApproach => "wrong_approach",
            Friction::BuggyCode => "buggy_code",
            Friction::UserRejectedAction => "user_rejected_action",
            Friction::ClaudeGotBlocked => "claude_got_blocked",
            Friction::UserStoppedEarly => "user_stopped_early",
            Friction::WrongFileOrLocation => "wrong_file_or_location",
            Friction::ExcessiveChanges => "excessive_changes",
            Friction::SlowOrVerbose => "slow_or_verbose",
            Friction::ToolFailed => "tool_failed",
            Friction::UserUnclear => "user_unclear",
            Friction::ExternalIssue => "external_issue",
        }
    }

    /// (d): drive each set's exhaustive match over its const, in §5 order —
    /// every const member is a real enum variant whose match arm spells it
    /// to the same snake_case literal the wire and §7's schema use. (a)+(b)
    /// lock serde's spelling for the const members; this locks the match
    /// arms themselves, so a mutant that rewrites an arm's literal is red
    /// here rather than only at the wire.
    fn assert_const_members_spell<T>(
        name: &str,
        variants: &[T],
        wire: &[&str],
        spell: fn(T) -> &'static str,
    ) where
        T: Copy + Debug,
    {
        assert_eq!(
            variants.len(),
            wire.len(),
            "{name}: the const and the §5 order list must have the same length"
        );
        for (variant, spelling) in variants.iter().zip(wire.iter()) {
            assert_eq!(
                spell(*variant),
                *spelling,
                "{name}: the exhaustive match must spell the const member {spelling}"
            );
        }
    }

    #[test]
    fn t16_facet_variant_sets_are_closed_and_round_trip() {
        // (a)+(b) for every variant of every set, against §5's table.
        assert_variant_spine(
            "Outcome",
            OUTCOME_VARIANTS,
            &[
                "fully_achieved",
                "mostly_achieved",
                "partially_achieved",
                "not_achieved",
                "unclear_from_transcript",
            ],
        );
        assert_variant_spine(
            "Helpfulness",
            HELPFULNESS_VARIANTS,
            &[
                "unhelpful",
                "slightly_helpful",
                "moderately_helpful",
                "very_helpful",
                "essential",
            ],
        );
        assert_variant_spine(
            "SessionType",
            SESSION_TYPE_VARIANTS,
            &[
                "single_task",
                "multi_task",
                "iterative_refinement",
                "exploration",
                "quick_question",
            ],
        );
        assert_variant_spine(
            "SuccessMode",
            SUCCESS_MODE_VARIANTS,
            &[
                "none",
                "fast_accurate_search",
                "correct_code_edits",
                "good_explanations",
                "proactive_help",
                "multi_file_changes",
                "handled_complexity",
                "good_debugging",
            ],
        );
        assert_variant_spine(
            "Satisfaction",
            SATISFACTION_VARIANTS,
            &[
                "frustrated",
                "dissatisfied",
                "likely_satisfied",
                "satisfied",
                "happy",
                "unsure",
                "neutral",
                "delighted",
            ],
        );
        assert_variant_spine(
            "Friction",
            FRICTION_VARIANTS,
            &[
                "misunderstood_request",
                "wrong_approach",
                "buggy_code",
                "user_rejected_action",
                "claude_got_blocked",
                "user_stopped_early",
                "wrong_file_or_location",
                "excessive_changes",
                "slow_or_verbose",
                "tool_failed",
                "user_unclear",
                "external_issue",
            ],
        );

        // (c): item-2b keys BTreeMaps on Friction and Outcome, so the
        // refusal is asserted there first, then spot-checked on the other
        // four sets — plus the case-sensitivity arm, which keeps a
        // case-insensitive deserializer from being a false green.
        assert_out_of_set_refused::<Friction>("Friction", r#""not_a_friction""#);
        assert_out_of_set_refused::<Outcome>("Outcome", r#""not_an_outcome""#);
        assert_out_of_set_refused::<Helpfulness>("Helpfulness", r#""not_a_helpfulness""#);
        assert_out_of_set_refused::<SessionType>("SessionType", r#""not_a_session_type""#);
        assert_out_of_set_refused::<SuccessMode>("SuccessMode", r#""not_a_success_mode""#);
        assert_out_of_set_refused::<Satisfaction>("Satisfaction", r#""not_a_satisfaction""#);
        assert_out_of_set_refused::<Outcome>("Outcome (case)", r#""FullyAchieved""#);
    }

    /// (d): the enum-contains-const direction of T-16's closure claim — a
    /// variant added to an enum without its const entry does not compile
    /// (the six exhaustive matches above, no wildcard arms), and every
    /// const member is a real, spellable variant in §5 order.
    #[test]
    fn t16_facet_enum_const_closure_is_exhaustively_matched() {
        assert_const_members_spell(
            "Outcome",
            OUTCOME_VARIANTS,
            &[
                "fully_achieved",
                "mostly_achieved",
                "partially_achieved",
                "not_achieved",
                "unclear_from_transcript",
            ],
            outcome_wire,
        );
        assert_const_members_spell(
            "Helpfulness",
            HELPFULNESS_VARIANTS,
            &[
                "unhelpful",
                "slightly_helpful",
                "moderately_helpful",
                "very_helpful",
                "essential",
            ],
            helpfulness_wire,
        );
        assert_const_members_spell(
            "SessionType",
            SESSION_TYPE_VARIANTS,
            &[
                "single_task",
                "multi_task",
                "iterative_refinement",
                "exploration",
                "quick_question",
            ],
            session_type_wire,
        );
        assert_const_members_spell(
            "SuccessMode",
            SUCCESS_MODE_VARIANTS,
            &[
                "none",
                "fast_accurate_search",
                "correct_code_edits",
                "good_explanations",
                "proactive_help",
                "multi_file_changes",
                "handled_complexity",
                "good_debugging",
            ],
            success_mode_wire,
        );
        assert_const_members_spell(
            "Satisfaction",
            SATISFACTION_VARIANTS,
            &[
                "frustrated",
                "dissatisfied",
                "likely_satisfied",
                "satisfied",
                "happy",
                "unsure",
                "neutral",
                "delighted",
            ],
            satisfaction_wire,
        );
        assert_const_members_spell(
            "Friction",
            FRICTION_VARIANTS,
            &[
                "misunderstood_request",
                "wrong_approach",
                "buggy_code",
                "user_rejected_action",
                "claude_got_blocked",
                "user_stopped_early",
                "wrong_file_or_location",
                "excessive_changes",
                "slow_or_verbose",
                "tool_failed",
                "user_unclear",
                "external_issue",
            ],
            friction_wire,
        );
    }
}
