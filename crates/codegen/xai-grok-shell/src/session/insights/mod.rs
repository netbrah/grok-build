//! Session insights: per-session digest → aggregate → labelled report (SDD §4).
//!
//! Module map:
//! - `digest.rs` — transcript → [`SessionDigest`] (pure) — item 1
//! - `aggregate.rs` — digests → `Aggregate` (pure) — item 2
//! - `report.rs` — HTML render (pure) — item 3
//! - `scope.rs` — session selection, limits and the storage reads — item 4
//! - `cache.rs` — read-through cache — item 4
//! - `label.rs` — facet calls + validation — item 7
//! - `narrate.rs` — section fan-out + at-a-glance — item 8
//!
//! Item 1 shipped the pure half (the §5 types, the §5 consts, `digest_one`
//! and `is_user_turn`); item 8c adds the control plane, the
//! `insights_generated` telemetry record and the `handle_insights`
//! orchestration to this module — item 5's command entry builds the request
//! and calls it. `project()` / the tag neutraliser still land in item 4
//! (SDD §13). No file I/O in this tree yet.

pub mod aggregate;
pub mod digest;
pub mod label;
pub mod narrate;
pub mod report;
pub mod types;

pub use digest::{
    CACHE_SCHEMA_VERSION, DegradedTier, DigestInput, IDLE_GAP_SECS, ProjectionCaps, ReaderFacts,
    SessionDigest, SessionFacts, TranscriptContent, digest_one, is_user_turn,
};
pub use label::{
    CallGuard, CallGuardRefusal, FACET_CACHE_SCHEMA_VERSION, LabelInput, LabelOutcome, LabelStatus,
    SessionFacets, chunk_summary_request, facet_request, facet_schema, friction_schema,
    fun_ending_schema, horizon_schema, interaction_style_schema, label_sessions,
    project_areas_schema, suggestions_schema, validate_facet_reply, validate_section_reply,
    what_works_schema,
};
pub use narrate::{
    AtAGlance, NarrateFailureClass, NarrateOutcome, SectionFailure, at_a_glance_request,
    build_session_data, narrate, parse_at_a_glance, section_request, splice_at_a_glance,
};
pub use types::{InsightsBudget, NarrateStats};

/// The ten `/insights` prompts (SOT §7): the ported files under
/// `session/templates/`, included at compile time. The depth is `../templates/`
/// — these modules sit one level deeper than the goal templates' consumers
/// (SOT §4 :260-262).
const FACET_PROMPT: &str = include_str!("../templates/insights_facet.md");
const CHUNK_SUMMARY_PROMPT: &str = include_str!("../templates/insights_chunk_summary.md");
const PROJECT_AREAS_PROMPT: &str = include_str!("../templates/insights_project_areas.md");
const INTERACTION_STYLE_PROMPT: &str = include_str!("../templates/insights_interaction_style.md");
const WHAT_WORKS_PROMPT: &str = include_str!("../templates/insights_what_works.md");
const FRICTION_PROMPT: &str = include_str!("../templates/insights_friction.md");
const SUGGESTIONS_PROMPT: &str = include_str!("../templates/insights_suggestions.md");
const HORIZON_PROMPT: &str = include_str!("../templates/insights_horizon.md");
const FUN_ENDING_PROMPT: &str = include_str!("../templates/insights_fun_ending.md");
const AT_A_GLANCE_PROMPT: &str = include_str!("../templates/insights_at_a_glance.md");

/// The single indirection every `/insights` prompt goes through (SOT §14
/// OD-7): the item 8 call path and the v2 prompt-override loader both change
/// in this one place. The ten names are the file stems under
/// `session/templates/`; a name outside that closed set panics, naming it.
pub fn prompt(name: &str) -> &'static str {
    match name {
        "insights_facet" => FACET_PROMPT,
        "insights_chunk_summary" => CHUNK_SUMMARY_PROMPT,
        "insights_project_areas" => PROJECT_AREAS_PROMPT,
        "insights_interaction_style" => INTERACTION_STYLE_PROMPT,
        "insights_what_works" => WHAT_WORKS_PROMPT,
        "insights_friction" => FRICTION_PROMPT,
        "insights_suggestions" => SUGGESTIONS_PROMPT,
        "insights_horizon" => HORIZON_PROMPT,
        "insights_fun_ending" => FUN_ENDING_PROMPT,
        "insights_at_a_glance" => AT_A_GLANCE_PROMPT,
        _ => panic!("unknown insights prompt name: {name}"),
    }
}

/// The seven section prompt names in `SECTION_CARDS` order (SOT §6 :806).
/// Item 8 derives `SECTIONS` from `SECTION_PROMPTS.len()`.
pub const SECTION_PROMPTS: [&str; 7] = [
    "insights_project_areas",
    "insights_interaction_style",
    "insights_what_works",
    "insights_friction",
    "insights_suggestions",
    "insights_horizon",
    "insights_fun_ending",
];

/// The build-time byte cap for a ported prompt file (SOT §6 :742): exceeding
/// it is a build failure, not a runtime truncation. Item 5's `[insights]
/// prompt_max_bytes` config row supersedes this constant when it lands.
pub const PROMPT_MAX_BYTES: usize = 12_288;

// ─────────────────────────────────────────────────────────────────────────────
// Item 8c (the SOT §10/§11/§13 third commit of item 8): the control plane +
// the telemetry record + the orchestration. `handle_insights` runs the
// pre-flight trio (coherence → endpoint consent → spend gate — ALL before
// any request) and then the label → aggregate → narrate → render flow; a
// ceiling stop is a COMPLETED partial run (the record still stamps).
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use serde_json::Value;

use crate::extensions::notification::USD_TICKS_PER_USD;
use aggregate::{Aggregate, AggregateInput, AnalyzedSession, Totals, aggregate};
use report::{ReportContext, ReportSection, SECTION_CARDS, render_report};
use types::{CappedBy, Friction, Outcome, Satisfaction, SuccessMode};
use xai_grok_sampler::SamplingClient;

/// §10's three redaction rungs — the one declared type, the one place the
/// rungs are enumerated; the run stamps the rung it actually used (T-10(d)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionRung {
    Off,
    Prompts,
    Redacted,
}

/// The section count the coherence formula adds (SOT §6 :799-832): derived,
/// not declared — `SECTIONS` is `SECTION_PROMPTS.len()`, so a drift between
/// the const and the prompt set is a compile-time impossibility (R-13).
pub const SECTIONS: usize = SECTION_PROMPTS.len();

/// The §6 rule-1 startup coherence check (pure). The derived worst case —
/// `max_sessions + min(per_session × max_sessions, per_run) + sections + 1`
/// — must not exceed `max_calls`, and the strict band `derived > 0.8 ×
/// max_calls` must hold, asserted in integer form `5 × derived > 4 ×
/// max_calls` so `max_calls` = 260 (0.8 × 260 = 208 = derived) refuses
/// while 259 passes (T-12(b)). A ceiling far above the plan is a decorative
/// one. Returns the derived number; the refusal carries it so the line can
/// print it (T-12(a)).
fn coherence_check(
    max_sessions: usize,
    max_chunk_calls_per_session: usize,
    max_chunk_calls_per_run: usize,
    max_calls: usize,
    sections: usize,
) -> Result<usize, PreFlightRefusal> {
    let derived = max_sessions
        .saturating_add(
            max_chunk_calls_per_session
                .saturating_mul(max_sessions)
                .min(max_chunk_calls_per_run),
        )
        .saturating_add(sections)
        .saturating_add(1);
    if derived > max_calls {
        return Err(PreFlightRefusal::Coherence { derived, max_calls });
    }
    // The strict band, in integer form (u128 so a pathological knob set
    // cannot overflow the check itself).
    if !((derived as u128) * 5 > (max_calls as u128) * 4) {
        return Err(PreFlightRefusal::Coherence { derived, max_calls });
    }
    Ok(derived)
}

/// One session's direct input to the run. `self_excluded` is the invoking
/// session (item 4's self-exclusion flag) — it wins over the digest in the
/// disposition derivation; `digest: None` = excluded (no readable
/// transcript).
#[derive(Clone)]
pub struct InsightsSessionInput<'a> {
    pub session_id: &'a str,
    pub self_excluded: bool,
    /// `None` = excluded / no readable transcript.
    pub digest: Option<&'a SessionDigest>,
    pub projection: &'a str,
}

/// The direct input to [`handle_insights`] (the item-5 command entry builds
/// this; `--no-model` is OUT of 8c — `model` is the resolved slug).
pub struct InsightsRequest<'a> {
    pub client: &'a SamplingClient,
    /// The resolved model slug.
    pub model: String,
    /// The endpoint `insights.model` resolved to (the G2 gate compares it).
    pub model_endpoint_host: String,
    /// The session's own endpoint host (the G2 gate compares against).
    pub session_endpoint_host: String,
    /// `insights.allow_alt_endpoint` — the explicit consent (default false).
    pub allow_alt_endpoint: bool,
    /// The scanned sessions, in scan order.
    pub sessions: Vec<InsightsSessionInput<'a>>,
    pub budget: &'a InsightsBudget,
    pub caps: &'a ProjectionCaps,
    /// The `--yes` flag (the spend-gate consent).
    pub yes: bool,
    /// The redaction rung the run actually used (stamped into the record).
    pub redaction: RedactionRung,
    /// The single `GROK_HOME` scanned (the footer's Root line).
    pub root: String,
    /// The scan window (the footer's Window line).
    pub window: String,
    /// The `--project` path, when set.
    pub scope: Option<String>,
}

/// A pre-flight refusal — returned BEFORE any request is issued (the mock's
/// request count stays at zero and NO `insights_generated` record is built
/// or emitted). Order: coherence → endpoint consent → spend gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreFlightRefusal {
    /// The derived worst case exceeds `max_calls` (or the strict band fails).
    Coherence { derived: usize, max_calls: usize },
    /// `N_expected > confirm_above_calls` without `--yes`.
    SpendGate {
        expected: usize,
        confirm_above_calls: usize,
    },
    /// `insights.model` resolved to a different endpoint than the session's,
    /// without `insights.allow_alt_endpoint`.
    AltEndpoint { host: String },
}

impl PreFlightRefusal {
    /// The one-line refusal: T-12(a) names the derived number, T-12(e) names
    /// the expected count and `--yes`, T-14(b) names the host it was refused
    /// for.
    pub fn refusal_line(&self) -> String {
        match self {
            PreFlightRefusal::Coherence { derived, max_calls } => format!(
                "insights: refusing to start — the derived worst case is {derived} calls, max_calls is {max_calls} (the strict band derived > 0.8 * max_calls must hold)"
            ),
            PreFlightRefusal::SpendGate {
                expected,
                confirm_above_calls,
            } => format!(
                "insights: {expected} expected calls is above confirm_above_calls {confirm_above_calls}; re-run with --yes to proceed"
            ),
            PreFlightRefusal::AltEndpoint { host } => format!(
                "insights: refusing to run — insights.model resolves to endpoint {host}, a different endpoint than this session's; set insights.allow_alt_endpoint = true to consent"
            ),
        }
    }
}

// The record's `section_failures[]` elements are 8b's `SectionFailure` (the
// comment there: "8c's telemetry record copies these into
// `section_failures[]` — a copy, not a recompute"). Both types live in
// `narrate` (OUT of 8c's edit set), so their wire spelling is declared here
// — the house `rename_all = "snake_case"` idiom, applied by hand.
impl Serialize for NarrateFailureClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            NarrateFailureClass::Transport => "transport",
            NarrateFailureClass::Validation => "validation",
            NarrateFailureClass::CeilingStopped => "ceiling_stopped",
        })
    }
}

impl Serialize for SectionFailure {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("SectionFailure", 2)?;
        state.serialize_field("section", &self.section)?;
        state.serialize_field("class", &self.class)?;
        state.end()
    }
}

/// The `insights_generated` telemetry record (§11 :1549-1628), field order
/// as in the SOT. v1 populates the 11-field set (R-5); the U-14/U-9 fields
/// exist with zero/empty values — v2 populates them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InsightsGenerated {
    pub sessions_scanned: u32,
    pub sessions_analyzed: u32,
    pub sessions_partial: u32,
    /// U-14: zero in v1.
    pub sessions_legacy: u32,
    pub sessions_excluded: u32,
    pub sessions_self_excluded: u32,
    pub facet_validation_failure_sessions: u32,
    pub label_cap_sessions: u32,
    pub stopped_before_label_sessions: u32,
    pub unlabelable_sessions: u32,
    /// U-14: zero in v1.
    pub facet_calls: u32,
    /// U-14: zero in v1.
    pub section_calls: u32,
    /// U-14: empty in v1.
    pub section_failures: Vec<SectionFailure>,
    /// U-14: zero in v1.
    pub digest_cache_hits: u32,
    /// U-14: zero in v1.
    pub facet_cache_hits: u32,
    /// U-14: zero in v1.
    pub duration_ms: u64,
    /// U-14: zero in v1.
    pub cost_ticks: u64,
    /// U-14: zero in v1.
    pub render_bytes: u64,
    /// U-9: empty in v1 (the G2 gate is v1; recording the host is v2).
    pub endpoint_host: String,
    /// The rung the run actually used (T-10(d)).
    pub redaction: RedactionRung,
    /// Mirrors `Totals.capped_by` — one source, two sinks (T-12(g)/(i)).
    pub capped_by: Vec<CappedBy>,
}

xai_grok_telemetry::telemetry_event!(
    crate::session::insights::InsightsGenerated,
    "insights_generated"
);

/// The result of a completed run (a ceiling stop is a COMPLETED partial run,
/// not an abort): the rendered report, the totals, the stamped record, the
/// at-a-glance, and both wave outcomes.
#[derive(Debug)]
pub struct InsightsOutcome {
    pub html: String,
    pub totals: Totals,
    pub record: InsightsGenerated,
    pub at_a_glance: Option<AtAGlance>,
    pub label_outcome: LabelOutcome,
    pub narrate_outcome: NarrateOutcome,
    /// The first ceiling line, if any (label wave first, then narrate).
    pub reason_line: Option<String>,
}

/// The spend-gate figure (R-14): `sessions_to_label + chunk_plan +
/// SECTIONS + 1` — one facet call per label-eligible session (capped at
/// `budget.max_sessions`), the chunk plan mirroring `label_sessions`'
/// arithmetic EXACTLY (per session: InputTruncated → 0; else if the
/// projection outgrows `caps.threshold_chars` →
/// `min(div_ceil(n_chars / chunk_chars), max_chunk_calls_per_session)`;
/// run level: `min(total, max_chunk_calls_per_run)`), plus the seven section
/// calls and the one at-a-glance (the warm-run floor). K (the cache) stays 0
/// until item 4.
fn expected_calls(
    eligible: &[&InsightsSessionInput<'_>],
    budget: &InsightsBudget,
    caps: &ProjectionCaps,
) -> usize {
    let sessions_to_label = eligible.len().min(budget.max_sessions);
    let mut chunk_plan = 0usize;
    for input in eligible.iter().take(sessions_to_label) {
        let digest = input
            .digest
            .expect("label-eligible sessions carry a digest (analyzed)");
        if digest.degraded.contains(&DegradedTier::InputTruncated) {
            continue; // 0 chunk calls: the facet carries the truncated projection
        }
        let n_chars = input.projection.chars().count();
        if n_chars > caps.threshold_chars {
            let n_chunks = n_chars.div_ceil(caps.chunk_chars.max(1));
            chunk_plan += n_chunks.min(budget.max_chunk_calls_per_session);
        }
    }
    chunk_plan = chunk_plan.min(budget.max_chunk_calls_per_run);
    sessions_to_label + chunk_plan + SECTIONS + 1
}

/// The R-18 fill: `NarrateStats` from the real `Aggregate` + the analyzed
/// digests + the labelled facets.
fn narrate_stats(
    agg: &Aggregate,
    digests: &[&SessionDigest],
    label: &LabelOutcome,
) -> NarrateStats {
    let mut tools: BTreeMap<String, u32> = BTreeMap::new();
    let mut languages: BTreeMap<String, u32> = BTreeMap::new();
    let mut messages = 0u32;
    let mut hours = 0.0f64;
    let mut cost_ticks = 0i64;
    let mut lo: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut hi: Option<chrono::DateTime<chrono::Utc>> = None;
    for d in digests {
        messages = messages.saturating_add(d.user_turns as u32);
        hours += d.activity_minutes as f64 / 60.0;
        cost_ticks = cost_ticks.saturating_add(d.cost_usd_ticks.unwrap_or(0).max(0));
        lo = Some(lo.map_or(d.started_at, |x| x.min(d.started_at)));
        hi = Some(hi.map_or(d.updated_at, |x| x.max(d.updated_at)));
        for (t, c) in &d.tool_counts {
            *tools.entry(t.clone()).or_insert(0) += c;
        }
        for (l, c) in &d.languages {
            *languages.entry(l.clone()).or_insert(0) += c;
        }
    }
    let date_range = match (lo, hi) {
        (Some(start), Some(end)) => {
            format!("{} .. {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
        }
        _ => String::new(),
    };
    let mut satisfaction: BTreeMap<Satisfaction, u32> = BTreeMap::new();
    let mut success: BTreeMap<SuccessMode, u32> = BTreeMap::new();
    for f in label.facets.iter().flatten() {
        for (s, c) in &f.user_satisfaction_counts {
            *satisfaction.entry(*s).or_insert(0) += c;
        }
        *success.entry(f.primary_success).or_insert(0) += 1;
    }
    NarrateStats {
        sessions: agg.totals.sessions_scanned,
        analyzed: agg.totals.sessions_analyzed,
        date_range,
        messages,
        hours,
        // Verified against the wire unit (R-18's "verify the tick unit"
        // clause): the cost ticks are USD ticks at 1e10 ticks per $1 — the
        // shell's own `USD_TICKS_PER_USD`, the constant report.rs renders
        // cost with — NOT the 1e6 the launch doc guessed.
        cost_usd: cost_ticks as f64 / USD_TICKS_PER_USD,
        top_tools: top_n(&tools, 8),
        top_goals: top_n(&agg.goal_categories, 8),
        outcomes: agg.outcomes.clone(),
        satisfaction,
        friction: agg.friction.clone(),
        success,
        languages,
    }
}

/// Top `n` by count desc, name asc (the deterministic tie-break).
fn top_n(map: &BTreeMap<String, u32>, n: usize) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = map.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

/// The suggestions section's clipboard payload (SOT :1331-1334 — the
/// feature's actionable output, not a candidate for deferral): the one
/// copyable field per item of the validated `insights_suggestions` reply,
/// in the schema's array order — `claude_md_additions[].addition`,
/// `features_to_try[].example_code`, `usage_patterns[].copyable_prompt`.
/// Pure and deterministic; the reply is schema-validated before it gets
/// here, so a missing or non-string field is a non-event — the guard keeps
/// the helper total. A non-object reply yields an empty payload.
fn suggestions_payload(reply: &Value) -> Vec<String> {
    let mut payload = Vec::new();
    for (array, field) in [
        ("claude_md_additions", "addition"),
        ("features_to_try", "example_code"),
        ("usage_patterns", "copyable_prompt"),
    ] {
        if let Some(items) = reply.get(array).and_then(Value::as_array) {
            for item in items {
                if let Some(copyable) = item.get(field).and_then(Value::as_str) {
                    payload.push(copyable.to_string());
                }
            }
        }
    }
    payload
}

/// The item-8 run: derive dispositions → pre-flight (coherence → endpoint
/// consent → spend gate — ALL before any request; a refusal returns with the
/// request count at zero and no record) → the facet labelling wave
/// (label-eligible only) → the pure aggregate → the narrate wave → the pure
/// render → the stamped `insights_generated` record (one emission).
pub async fn handle_insights(
    req: &InsightsRequest<'_>,
) -> Result<InsightsOutcome, PreFlightRefusal> {
    // The disposition derivation (R-4): self_excluded wins; digest `None`
    // is excluded; the rest are analyzed. Label-eligible = analyzed with
    // user_turns > 0; the analyzed with user_turns == 0 are UNLABELABLE — no
    // facet call, badged partial, counted in `unlabelable_sessions`, and
    // they do NOT consume label-cap slots.
    let mut analyzed: Vec<(&InsightsSessionInput<'_>, bool)> = Vec::new();
    let mut excluded = 0u32;
    let mut self_excluded = 0u32;
    for input in &req.sessions {
        if input.self_excluded {
            self_excluded += 1;
        } else if input.digest.is_none() {
            excluded += 1;
        } else {
            let eligible = input.digest.map(|d| d.user_turns > 0).unwrap_or(false);
            analyzed.push((input, eligible));
        }
    }
    let eligible: Vec<&InsightsSessionInput<'_>> = analyzed
        .iter()
        .filter(|(_, e)| *e)
        .map(|(i, _)| *i)
        .collect();
    let unlabelable = (analyzed.len() - eligible.len()) as u32;

    // Pre-flight, in order: coherence → endpoint consent → spend gate.
    coherence_check(
        req.budget.max_sessions,
        req.budget.max_chunk_calls_per_session,
        req.budget.max_chunk_calls_per_run,
        req.budget.max_calls,
        SECTIONS,
    )?;
    if req.model_endpoint_host != req.session_endpoint_host && !req.allow_alt_endpoint {
        return Err(PreFlightRefusal::AltEndpoint {
            host: req.model_endpoint_host.clone(),
        });
    }
    let n_expected = expected_calls(&eligible, req.budget, req.caps);
    if n_expected > req.budget.confirm_above_calls && !req.yes {
        return Err(PreFlightRefusal::SpendGate {
            expected: n_expected,
            confirm_above_calls: req.budget.confirm_above_calls,
        });
    }

    // The facet labelling wave — label-eligible only, in scan order (the
    // `max_sessions` cap applies to these; the surplus is badged LabelCap).
    let inputs: Vec<LabelInput<'_>> = eligible
        .iter()
        .map(|i| LabelInput {
            digest: i.digest.expect("label-eligible sessions carry a digest"),
            projection: i.projection,
        })
        .collect();
    let label = label_sessions(req.client, &req.model, &inputs, req.caps, req.budget).await;

    // The analyzed set (scan order) + the per-session facts only the
    // labeller knows. The R-4 badge: `digest.degraded` non-empty OR label
    // status non-Labelled OR unlabelable.
    let mut owned_goals: Vec<Vec<String>> = Vec::with_capacity(analyzed.len());
    let mut owned_friction: Vec<BTreeMap<Friction, u32>> = Vec::with_capacity(analyzed.len());
    let mut owned_outcomes: Vec<Option<Outcome>> = Vec::with_capacity(analyzed.len());
    let mut partials: Vec<bool> = Vec::with_capacity(analyzed.len());
    let mut li = 0usize; // the index into the label arrays (eligible only)
    for (input, is_elig) in &analyzed {
        let digest = input.digest.expect("analyzed sessions carry a digest");
        let facet: Option<&SessionFacets>;
        let non_labelled: bool;
        if *is_elig {
            facet = label.facets[li].as_ref();
            non_labelled = label.status[li] != LabelStatus::Labelled;
            li += 1;
        } else {
            facet = None;
            non_labelled = true;
        }
        partials.push(!digest.degraded.is_empty() || non_labelled);
        owned_goals.push(
            facet
                .map(|f| f.goal_categories.keys().cloned().collect())
                .unwrap_or_default(),
        );
        owned_friction.push(facet.map(|f| f.friction_counts.clone()).unwrap_or_default());
        owned_outcomes.push(facet.map(|f| f.outcome));
    }
    let sessions: Vec<AnalyzedSession<'_>> = (0..analyzed.len())
        .map(|i| AnalyzedSession {
            digest: analyzed[i]
                .0
                .digest
                .expect("analyzed sessions carry a digest"),
            partial: partials[i],
            goal_categories: &owned_goals[i],
            friction_counts: &owned_friction[i],
            outcome: owned_outcomes[i],
        })
        .collect();

    // The pre-narrate aggregate: `NarrateStats` (R-18) reads the aggregate's
    // maps, which exist before the narrate wave. The FINAL capped_by union
    // (label + narrate, R-6) is re-aggregated after the wave — one source,
    // two sinks (the footer + the record).
    let pre = aggregate(&AggregateInput {
        analyzed: &sessions,
        sessions_excluded: excluded,
        sessions_self_excluded: self_excluded,
        capped_by: &label.capped_by,
    });
    let digests: Vec<&SessionDigest> = analyzed
        .iter()
        .map(|(i, _)| i.digest.expect("analyzed sessions carry a digest"))
        .collect();
    let stats = narrate_stats(&pre, &digests, &label);

    let narr = narrate(
        req.client,
        &req.model,
        &stats,
        &label.facets,
        label.total_cost_ticks,
        req.budget,
    )
    .await;

    // The run-level ceiling record: the BTreeSet union of both waves'
    // `capped_by` (R-6).
    let mut union: BTreeSet<CappedBy> = label.capped_by.iter().copied().collect();
    union.extend(narr.capped_by.iter().copied());
    let union_vec: Vec<CappedBy> = union.into_iter().collect();
    let agg = aggregate(&AggregateInput {
        analyzed: &sessions,
        sessions_excluded: excluded,
        sessions_self_excluded: self_excluded,
        capped_by: &union_vec,
    });

    // The seven sections (SECTION_CARDS order; a failed section keeps its
    // anchor with an explicit placeholder body — the same marker format the
    // narrate splice uses).
    let sections: Vec<ReportSection> = SECTION_CARDS
        .iter()
        .enumerate()
        .map(|(i, (anchor, display))| ReportSection {
            display_name: (*display).to_string(),
            anchor_id: (*anchor).to_string(),
            body: narr.section_replies[i]
                .clone()
                .map(|v| v.to_string())
                .unwrap_or_else(|| format!("[{} unavailable]", SECTION_PROMPTS[i])),
            suggestions: match &narr.section_replies[i] {
                // The suggestions section (SOT :1331-1334) carries its
                // clipboard payload; the six prose sections carry none.
                Some(v) if SECTION_PROMPTS[i] == "insights_suggestions" => suggestions_payload(v),
                _ => Vec::new(),
            },
        })
        .collect();

    let html = render_report(
        &agg,
        &ReportContext {
            window: req.window.clone(),
            root: req.root.clone(),
            scope: req.scope.clone(),
            sections: Some(&sections),
            sessions: &sessions,
        },
    );

    // The record (§11 field order; R-5's v1 population) + the one emission.
    let totals = &agg.totals;
    let record = InsightsGenerated {
        sessions_scanned: totals.sessions_scanned,
        sessions_analyzed: totals.sessions_analyzed,
        sessions_partial: totals.sessions_partial,
        sessions_legacy: 0, // U-14
        sessions_excluded: totals.sessions_excluded,
        sessions_self_excluded: totals.sessions_self_excluded,
        facet_validation_failure_sessions: label.facet_validation_failure_sessions as u32,
        label_cap_sessions: label.label_cap_sessions as u32,
        stopped_before_label_sessions: label.stopped_before_label_sessions as u32,
        unlabelable_sessions: unlabelable,
        facet_calls: 0,               // U-14
        section_calls: 0,             // U-14
        section_failures: Vec::new(), // U-14
        digest_cache_hits: 0,         // U-14
        facet_cache_hits: 0,          // U-14
        duration_ms: 0,               // U-14
        cost_ticks: 0,                // U-14
        render_bytes: 0,              // U-14
        endpoint_host: String::new(), // U-9
        redaction: req.redaction,
        capped_by: totals.capped_by.clone(), // mirrors Totals (R-6)
    };
    xai_grok_telemetry::session_ctx::log_event(record.clone());

    // The first ceiling line (label wave first, then narrate) — computed
    // before the moves into the outcome below.
    let reason_line = label
        .reason_line
        .clone()
        .or_else(|| narr.reason_line.clone());

    Ok(InsightsOutcome {
        html,
        totals: agg.totals.clone(),
        record,
        at_a_glance: narr.at_a_glance.clone(),
        label_outcome: label,
        narrate_outcome: narr,
        reason_line,
    })
}

#[cfg(test)]
mod tests {
    //! T-9 (a)(b)(c)(e-file) and the seam arms — SOT §12 T-9, item 7a (R7).
    //! Everything here runs under the established `session::insights` filter.

    use super::report::SECTION_CARDS;
    use super::{PROMPT_MAX_BYTES, SECTION_PROMPTS, prompt};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    /// The ten prompt names — the closed file-stem set under `session/templates/`.
    const NAMES: [&str; 10] = [
        "insights_facet",
        "insights_chunk_summary",
        "insights_project_areas",
        "insights_interaction_style",
        "insights_what_works",
        "insights_friction",
        "insights_suggestions",
        "insights_horizon",
        "insights_fun_ending",
        "insights_at_a_glance",
    ];

    // -- T-9(a): the four lines §7 names, each in the file that must carry it.

    #[test]
    fn t9a_the_negative_example_list_survives_in_at_a_glance() {
        let p = prompt("insights_at_a_glance");
        assert!(
            p.contains("Avoid stuff like \"Ask Claude to confirm"),
            "at_a_glance lost the head of the negative-example list"
        );
        assert!(
            p.contains("or \"Type out more context up front\" which are less compelling."),
            "at_a_glance lost the tail of the negative-example list"
        );
    }

    #[test]
    fn t9a_second_person_is_named_in_the_three_prompts_that_carry_it() {
        assert!(
            prompt("insights_what_works").contains("Use second person (\"you\")"),
            "what_works lost its second-person line"
        );
        assert!(
            prompt("insights_friction").contains("Use second person (\"you\")"),
            "friction lost its second-person line"
        );
        assert!(
            prompt("insights_interaction_style").contains("Use second person 'you'"),
            "interaction_style lost its second-person line (the 'you' form in the JSON example)"
        );
    }

    #[test]
    fn t9a_the_count_only_the_users_ask_rule_survives_in_facet() {
        let p = prompt("insights_facet");
        assert!(
            p.contains("Count ONLY what the USER explicitly asked for."),
            "facet lost the count-only rule"
        );
        assert!(
            p.contains("DO NOT count Claude's autonomous codebase exploration"),
            "facet lost the autonomous-exclusion line"
        );
    }

    #[test]
    fn t9a_the_named_uncertainty_escape_hatch_survives_in_facet() {
        assert!(
            prompt("insights_facet").contains("warmup_minimal"),
            "facet lost the warmup_minimal escape-hatch category"
        );
    }

    // -- T-9(b): every `See _Section_.` anchor in at_a_glance resolves against
    // the seven `SECTION_CARDS` display names — the single source of truth (R8).

    #[test]
    fn t9b_at_a_glance_anchors_resolve_against_the_section_cards() {
        let p = prompt("insights_at_a_glance");
        let re = regex::Regex::new(r"See _([^_]+)_\.").unwrap();
        let anchors: Vec<&str> = re
            .captures_iter(p)
            .map(|c| c.get(1).expect("group 1 matched").as_str())
            .collect();
        assert!(
            !anchors.is_empty(),
            "at_a_glance must carry See _Section_. anchors"
        );
        let display_names: Vec<&str> = SECTION_CARDS.iter().map(|(_, d)| *d).collect();
        for anchor in &anchors {
            assert!(
                display_names.iter().any(|d| d == anchor),
                "anchor {anchor:?} does not name one of the seven sections: {display_names:?}"
            );
        }
    }

    // -- T-9(c): every placeholder in every ported prompt is UPPERCASE, and each
    // file carries its pinned splice placeholder (the no-vacuity floor).

    #[test]
    fn t9c_every_placeholder_token_is_uppercase_in_all_ten_prompts() {
        let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
        for name in NAMES {
            let p = prompt(name);
            for caps in re.captures_iter(p) {
                let token = &caps[1];
                assert!(
                    token
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                    "{name}: placeholder {{{token}}} is not all uppercase"
                );
            }
        }
    }

    #[test]
    fn t9c_every_file_carries_its_pinned_splice_placeholders() {
        assert!(
            prompt("insights_facet").contains("{TRANSCRIPT}"),
            "facet lost its {{TRANSCRIPT}} splice"
        );
        assert!(
            prompt("insights_chunk_summary").contains("{TRANSCRIPT_CHUNK}"),
            "chunk_summary lost its {{TRANSCRIPT_CHUNK}} splice"
        );
        for name in SECTION_PROMPTS {
            assert!(
                prompt(name).contains("{SESSION_DATA}"),
                "{name} lost its {{SESSION_DATA}} splice"
            );
        }
        let glance = prompt("insights_at_a_glance");
        for ph in [
            "{SESSION_DATA}",
            "{PROJECT_AREAS}",
            "{BIG_WINS}",
            "{FRICTION_CATEGORIES}",
            "{FEATURES_TO_TRY}",
            "{USAGE_PATTERNS}",
            "{ON_THE_HORIZON}",
        ] {
            assert!(glance.contains(ph), "at_a_glance lost its {ph} splice");
        }
    }

    // -- T-9(e-file): every ported prompt file is at or under the byte cap.

    #[test]
    fn t9e_file_every_prompt_file_is_within_prompt_max_bytes() {
        for name in NAMES {
            let p = prompt(name);
            assert!(
                p.len() <= PROMPT_MAX_BYTES,
                "{name} is {} bytes, over the {PROMPT_MAX_BYTES}-byte cap",
                p.len()
            );
        }
    }

    // -- T-9 (item-7 follow-up): the `insights_project_areas` port verdict is
    // file-level "verbatim minus product name" (SOT §7 :1032) — the ported
    // file may name neither the source product nor its abbreviation.

    #[test]
    fn t9_project_areas_is_ported_verbatim_minus_product_name() {
        let p = prompt("insights_project_areas");
        assert!(
            !p.contains("Claude"),
            "insights_project_areas must carry no \"Claude\" mention (verbatim minus product name)"
        );
        assert!(
            !p.contains("CC"),
            "insights_project_areas must carry no \"CC\" mention (verbatim minus product name)"
        );
    }

    // -- The seam: the ten names resolve to pairwise-distinct texts,
    // `SECTION_PROMPTS` is the seven section stems, and unknown names panic.

    #[test]
    fn seam_the_ten_prompts_resolve_and_are_pairwise_distinct() {
        for (i, a) in NAMES.iter().enumerate() {
            let pa = prompt(a);
            assert!(!pa.is_empty(), "{a} resolved to an empty prompt");
            for b in &NAMES[i + 1..] {
                assert_ne!(
                    pa,
                    prompt(b),
                    "{a} and {b} return the same text (a file swap?)"
                );
            }
        }
    }

    #[test]
    fn seam_section_prompts_are_the_seven_section_stems_and_resolve() {
        assert_eq!(
            SECTION_PROMPTS.len(),
            7,
            "SECTION_PROMPTS must hold the seven section stems"
        );
        for name in SECTION_PROMPTS {
            assert!(
                NAMES.contains(&name),
                "SECTION_PROMPTS entry {name:?} is not one of the ten prompt names"
            );
            assert!(!prompt(name).is_empty(), "{name} failed to resolve");
        }
    }

    #[test]
    fn seam_an_unknown_name_panics_naming_it() {
        let outcome = catch_unwind(AssertUnwindSafe(|| prompt("nope")));
        let Err(panic) = outcome else {
            panic!("prompt(\"nope\") resolved instead of panicking");
        };
        let msg = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().copied().map(String::from));
        assert!(
            msg.as_deref().is_some_and(|m| m.contains("nope")),
            "the panic did not name the unknown prompt; got {msg:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Item 8c's arms: T-0(m) (the SOT-enumerated 5-session store), the FULL
    // T-9(d) sentinel run-arm, the T-11 run-level call count, T-12's
    // (a)(b)(e)(f)(g) arms, T-10(d) (the redaction rungs), T-14(b) (the
    // endpoint-consent gate) and R-4's unlabelable disposition. The mock +
    // client plumbing copies the 8a/8b pattern (the label.rs / narrate.rs
    // test modules); every full-run arm runs at budget.concurrency = 1
    // (R-17) so the scripted replies stay deterministic.
    // ─────────────────────────────────────────────────────────────────────────

    use super::{
        FACET_CACHE_SCHEMA_VERSION, InsightsRequest, InsightsSessionInput, PreFlightRefusal,
        RedactionRung, SECTIONS, coherence_check, handle_insights, suggestions_payload,
    };
    use crate::session::insights::digest::{DegradedTier, ProjectionCaps, SessionDigest};
    use crate::session::insights::types::{CappedBy, InsightsBudget};
    use chrono::DateTime;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use xai_grok_sampler::{ApiBackend, SamplerConfig, SamplingClient};
    use xai_grok_test_support::mock_server::MockInferenceServer;
    use xai_grok_test_support::scripted::{ScriptedResponse, SseEvent};
    use xai_grok_test_support::sse::responses_api_script_exact;

    const MODEL: &str = "test-model";
    const PATH: &str = "/v1/responses";
    const SENTINEL: &str = "SENTINEL-T9D-FULL";

    // -- Mock plumbing (the 8a/8b pattern, copied).

    async fn client_for(server: &MockInferenceServer) -> SamplingClient {
        SamplingClient::new(SamplerConfig {
            base_url: server.url(),
            api_backend: ApiBackend::Responses,
            api_key: Some("test-key".to_string()),
            model: MODEL.to_string(),
            context_window: 200_000,
            ..SamplerConfig::default()
        })
        .expect("sampler client should build")
    }

    fn sse_reply(text: &str) -> ScriptedResponse {
        ScriptedResponse::sse(responses_api_script_exact(text, MODEL))
    }

    fn cost_reply(text: &str, ticks: i64) -> ScriptedResponse {
        ScriptedResponse::sse(cost_carrying_responses_sse(text, MODEL, ticks))
    }

    /// The mock's `responses_api_script_exact` carries no cost; this mirrors
    /// its frame order but stamps `cost_in_usd_ticks` on the
    /// `response.completed` usage — the wire field the client parses into
    /// `ConversationResponse::cost_usd_ticks`.
    fn cost_carrying_responses_sse(text: &str, model: &str, ticks: i64) -> Vec<SseEvent> {
        vec![
            SseEvent::data(
                json!({
                    "type": "response.created",
                    "sequence_number": 0,
                    "response": {
                        "id": "resp_test", "object": "response", "created_at": 1234567890,
                        "model": model, "status": "in_progress", "output": []
                    }
                })
                .to_string(),
            ),
            SseEvent::data(
                json!({
                    "type": "response.output_text.delta",
                    "sequence_number": 1,
                    "item_id": "item_test", "output_index": 0, "content_index": 0, "delta": text
                })
                .to_string(),
            ),
            SseEvent::data(
                json!({
                    "type": "response.completed",
                    "sequence_number": 2,
                    "response": {
                        "id": "resp_test", "object": "response", "created_at": 1234567890,
                        "model": model, "status": "completed",
                        "output": [{
                            "type": "message", "id": "msg_test", "role": "assistant", "status": "completed",
                            "content": [{ "type": "output_text", "text": text, "annotations": [] }]
                        }],
                        "usage": {
                            "input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                            "cost_in_usd_ticks": ticks
                        }
                    }
                })
                .to_string(),
            ),
            SseEvent::data("[DONE]".to_string()),
        ]
    }

    // -- Fixtures.

    fn valid_facet_json() -> String {
        json!({
            "cache_version": FACET_CACHE_SCHEMA_VERSION,
            "transcript_sha256": "9f2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c",
            "session_id": "sess-8c",
            "underlying_goal": "Ship the session insights",
            "goal_categories": { "debugging": 2 },
            "outcome": "fully_achieved",
            "user_satisfaction_counts": { "satisfied": 1 },
            "claude_helpfulness": "very_helpful",
            "session_type": "iterative_refinement",
            "friction_counts": { "tool_failed": 1 },
            "friction_detail": "The build failed twice.",
            "primary_success": "good_debugging",
            "brief_summary": "A short summary."
        })
        .to_string()
    }

    /// A schema-valid reply for section `i` (the `SECTION_PROMPTS` order),
    /// carrying the distinctive marker `ZebraForge-<i>` in a required string
    /// slot.
    fn valid_section_reply(i: usize) -> String {
        let marker = format!("ZebraForge-{i}");
        let v = match i {
            0 => json!({
                "areas": [{ "name": marker, "session_count": 3, "description": "d" }]
            }),
            1 => json!({ "narrative": marker, "key_pattern": "kp" }),
            2 => json!({
                "intro": marker,
                "impressive_workflows": [{ "title": "t", "description": "d" }]
            }),
            3 => json!({
                "intro": marker,
                "categories": [{ "category": "c", "description": "d", "examples": ["e"] }]
            }),
            4 => json!({
                "claude_md_additions": [{ "addition": marker, "why": "w", "prompt_scaffold": "p" }],
                "features_to_try": [{ "feature": "f", "one_liner": "o", "why_for_you": "y", "example_code": "c" }],
                "usage_patterns": [{ "title": "t", "suggestion": "s", "detail": "d", "copyable_prompt": "p" }]
            }),
            5 => json!({
                "intro": marker,
                "opportunities": [{ "title": "t", "whats_possible": "w", "how_to_try": "h", "copyable_prompt": "p" }]
            }),
            6 => json!({ "headline": marker, "detail": "d" }),
            other => panic!("section index out of range: {other}"),
        };
        v.to_string()
    }

    fn valid_glance_reply() -> String {
        json!({
            "whats_working": "wg",
            "whats_hindering": "wh",
            "quick_wins": "qw",
            "ambitious_workflows": "aw"
        })
        .to_string()
    }

    /// A full 36-field digest with `user_turns` + `degraded` knobs (the 8a
    /// `test_digest` shape).
    fn test_digest(user_turns: usize, degraded: Vec<DegradedTier>) -> SessionDigest {
        SessionDigest {
            cache_version: 1,
            session_id: "sess".to_string(),
            cwd: "/tmp".to_string(),
            git_root: None,
            head_branch: None,
            started_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
            wall_minutes: 0,
            activity_minutes: 0,
            user_turns,
            assistant_turns: 1,
            tool_counts: BTreeMap::new(),
            languages: BTreeMap::new(),
            files_modified: 0,
            lines_added: 0,
            lines_removed: 0,
            projection_chars: 0,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_read_tokens: 0,
            cost_usd_ticks: None,
            cost_is_partial: false,
            tool_errors: 0,
            interruptions: 0,
            response_gaps_secs: Vec::new(),
            message_hours: Vec::new(),
            user_message_timestamps: Vec::new(),
            time_degraded: false,
            model_ids: Vec::new(),
            uses_mcp: false,
            uses_subagent: false,
            first_prompt: None,
            degraded,
            unreadable_lines: 0,
            transcript_sha256: "0".repeat(64),
        }
    }

    fn clean_input<'a>(
        id: &'a str,
        digest: &'a SessionDigest,
        projection: &'a str,
    ) -> InsightsSessionInput<'a> {
        InsightsSessionInput {
            session_id: id,
            self_excluded: false,
            digest: Some(digest),
            projection,
        }
    }

    /// Script a clean full run's replies: `n_facets` facet replies (the label
    /// wave is sequential), then the seven section replies + the at-a-glance
    /// reply (the narrate wave runs at concurrency = 1 — R-17).
    fn script_full_run(server: &MockInferenceServer, n_facets: usize) {
        for _ in 0..n_facets {
            server.enqueue_response(PATH, sse_reply(&valid_facet_json()));
        }
        for i in 0..SECTION_PROMPTS.len() {
            server.enqueue_response(PATH, sse_reply(&valid_section_reply(i)));
        }
        server.enqueue_response(PATH, sse_reply(&valid_glance_reply()));
    }

    /// The sequential budget: concurrency = 1 so the mock's reply-to-section
    /// assignment stays deterministic (R-17).
    fn sequential_budget() -> InsightsBudget {
        InsightsBudget {
            concurrency: 1,
            ..InsightsBudget::default()
        }
    }

    fn make_req<'a>(
        client: &'a SamplingClient,
        sessions: Vec<InsightsSessionInput<'a>>,
        budget: &'a InsightsBudget,
        caps: &'a ProjectionCaps,
        yes: bool,
        redaction: RedactionRung,
    ) -> InsightsRequest<'a> {
        InsightsRequest {
            client,
            model: MODEL.to_string(),
            model_endpoint_host: "session.example".to_string(),
            session_endpoint_host: "session.example".to_string(),
            allow_alt_endpoint: false,
            sessions,
            budget,
            caps,
            yes,
            redaction,
            root: "/tmp/grok-home".to_string(),
            window: "2026-08-01 .. 2026-09-30".to_string(),
            scope: None,
        }
    }

    /// The plain-string content of the `input` item carrying role `user` (the
    /// 8b idiom), or `None` when the body has no such item (the at-a-glance
    /// request is system-only).
    fn user_content(body: &Value) -> Option<String> {
        body["input"]
            .as_array()?
            .iter()
            .find(|i| i["role"] == json!("user"))
            .and_then(|i| i.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    // -- T-0(m) (R-7): the SOT-enumerated 5-session store — all four footer
    // outcomes at once. The SOT's printed example ("5 = 4 + 1 + 0 and
    // 4 = 2 + 2") contradicts its own enumeration; the enumerated store is
    // adjudicated here (scanned 5 = analyzed 3 + excluded 1 + self_excluded
    // 1; analyzed 3 = clean 2 + partial 1). FLAG to operator; the SOT is not
    // patched.

    #[tokio::test]
    async fn t0m_the_sot_enumerated_store_footer_identity_and_eleven_calls() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();

        // The enumerated store, in scan order: 1 badged (a corrupt line),
        // 1 excluded (no readable transcript), 1 self-excluded (the invoking
        // session), 2 clean.
        let badged = test_digest(1, vec![DegradedTier::UnreadableLines]);
        let self_ex = test_digest(1, Vec::new());
        let clean1 = test_digest(1, Vec::new());
        let clean2 = test_digest(1, Vec::new());
        let sessions = vec![
            InsightsSessionInput {
                session_id: "badged",
                self_excluded: false,
                digest: Some(&badged),
                projection: "b",
            },
            InsightsSessionInput {
                session_id: "excluded",
                self_excluded: false,
                digest: None,
                projection: "x",
            },
            InsightsSessionInput {
                session_id: "self",
                self_excluded: true,
                digest: Some(&self_ex),
                projection: "s",
            },
            clean_input("c1", &clean1, "c1"),
            clean_input("c2", &clean2, "c2"),
        ];
        // 3 facet calls (the badged session is still label-eligible) +
        // 7 sections + 1 glance = 11 mock calls.
        script_full_run(&server, 3);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("the run completes");

        // The identity equations on the stored totals...
        let t = &outcome.totals;
        assert_eq!(t.sessions_scanned, 5, "scanned");
        assert_eq!(t.sessions_analyzed, 3, "analyzed");
        assert_eq!(t.sessions_partial, 1, "partial");
        assert_eq!(t.sessions_excluded, 1, "excluded");
        assert_eq!(t.sessions_self_excluded, 1, "self-excluded");
        // ...and, as equations on the PRINTED footer (all four counters
        // non-zero): scanned 5 = analyzed 3 + excluded 1 + self_excluded 1;
        // analyzed 3 = clean 2 + partial 1.
        assert!(
            outcome.html.contains(
                "Scanned 5 sessions: analyzed 3 (clean 2, partial 1), excluded 1, self-excluded 1."
            ),
            "the printed footer lost the adjudicated identity equations"
        );
        assert_eq!(
            server.request_count_for(PATH),
            11,
            "3 facet + 7 section + 1 glance = 11 calls"
        );
        // The record mirrors the footer counters.
        let r = &outcome.record;
        assert_eq!(r.sessions_scanned, 5);
        assert_eq!(r.sessions_analyzed, 3);
        assert_eq!(r.sessions_partial, 1);
        assert_eq!(r.sessions_excluded, 1);
        assert_eq!(r.sessions_self_excluded, 1);
        assert_eq!(r.unlabelable_sessions, 0);
        assert!(r.capped_by.is_empty(), "no ceiling bound this run");
    }

    // -- T-9(d) FULL (R-8): the sentinel reaches the facet request and never
    // the seven section bodies or the at-a-glance body; every section body
    // carries the facet-derived brief line.

    #[tokio::test]
    async fn t9d_full_the_sentinel_reaches_the_facet_and_never_the_sections_or_glance() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.set_keep_requests(true);
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();

        let clean = test_digest(1, Vec::new());
        let sessions = vec![clean_input("only", &clean, SENTINEL)];
        script_full_run(&server, 1);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("the run completes");
        assert!(outcome.at_a_glance.is_some(), "the glance parsed");

        let bodies = server.request_bodies();
        assert_eq!(bodies.len(), 9, "1 facet + 7 sections + 1 glance");

        // The facet: the only request whose user item carries the sentinel
        // (the projection, below the chunk threshold, goes raw). The seven
        // sections: the user item is the `SESSION SUMMARIES:` payload. The
        // glance: system-only (no user item).
        let mut facet_bodies: Vec<&Value> = Vec::new();
        let mut section_bodies: Vec<&Value> = Vec::new();
        let mut glance_bodies: Vec<&Value> = Vec::new();
        for body in &bodies {
            match user_content(body) {
                Some(text) if text.contains(SENTINEL) => facet_bodies.push(body),
                Some(text) if text.contains("SESSION SUMMARIES:") => section_bodies.push(body),
                Some(text) => panic!("an unexpected user-carrying request body: {text}"),
                None => glance_bodies.push(body),
            }
        }
        assert_eq!(facet_bodies.len(), 1, "exactly one facet request");
        assert_eq!(section_bodies.len(), 7, "the seven section requests");
        assert_eq!(glance_bodies.len(), 1, "the one at-a-glance request");

        // PRESENT in the facet body...
        assert!(
            facet_bodies[0].to_string().contains(SENTINEL),
            "the facet request must carry the raw projection"
        );
        // ...ABSENT from every section body and the at-a-glance body.
        for body in &section_bodies {
            assert!(
                !body.to_string().contains(SENTINEL),
                "a section body leaked the sentinel"
            );
        }
        for body in &glance_bodies {
            assert!(
                !body.to_string().contains(SENTINEL),
                "the at-a-glance body leaked the sentinel"
            );
        }
        // Every section body carries the facet-derived brief line.
        for body in &section_bodies {
            let user = user_content(body).expect("the section body carries a user item");
            assert!(
                user.contains("- A short summary. (fully_achieved, very_helpful)"),
                "a section body lost the facet-derived brief line: {user}"
            );
        }
    }

    // -- T-12(a): max_sessions = 41 → derived 209 > 208 → the startup
    // refusal names the derived number, issues no request and emits no
    // record (the Err variant IS the proof — no InsightsOutcome is built).

    #[tokio::test]
    async fn t12a_max_sessions_41_refuses_at_startup_printing_the_derived_number() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = InsightsBudget {
            max_sessions: 41,
            ..InsightsBudget::default()
        };

        let req = make_req(
            &client,
            Vec::new(),
            &budget,
            &caps,
            true,
            RedactionRung::Off,
        );
        match handle_insights(&req).await {
            Err(PreFlightRefusal::Coherence { derived, max_calls }) => {
                assert_eq!(derived, 209, "41 + min(4 × 41, 160) + 7 + 1 = 209");
                assert_eq!(max_calls, 208);
                let line = PreFlightRefusal::Coherence { derived, max_calls }.refusal_line();
                assert!(
                    line.contains("209"),
                    "the refusal line must name the derived number: {line}"
                );
            }
            other => panic!(
                "a derived worst case of 209 over max_calls 208 must refuse at coherence, got {other:?}"
            ),
        }
        assert_eq!(
            server.request_count_for(PATH),
            0,
            "no request before a run exists"
        );
    }

    // -- T-12(b): the strict band, asserted in integer form (5 × derived >
    // 4 × max_calls) at its boundary — 260 refuses (0.8 × 260 = 208 =
    // derived; 208 > 208 is false), 259 passes, 300 refuses; the defaults
    // pass with derived = 208.

    #[test]
    fn t12b_the_strict_band_is_asserted_at_its_boundary_in_integer_form() {
        // Defaults: derived = 40 + min(4 × 40, 160) + 7 + 1 = 208 = max_calls.
        assert_eq!(coherence_check(40, 4, 160, 208, SECTIONS), Ok(208));
        // 260: 0.8 × 260 = 208 = derived → the strict band fails.
        match coherence_check(40, 4, 160, 260, SECTIONS) {
            Err(PreFlightRefusal::Coherence { derived, max_calls }) => {
                assert_eq!((derived, max_calls), (208, 260));
            }
            other => panic!("max_calls 260 must fail the strict band, got {other:?}"),
        }
        // 259: 5 × 208 = 1040 > 4 × 259 = 1036 → passes.
        assert_eq!(coherence_check(40, 4, 160, 259, SECTIONS), Ok(208));
        // 300: 5 × 208 = 1040 > 4 × 300 = 1200 is false → refuses.
        assert!(
            matches!(
                coherence_check(40, 4, 160, 300, SECTIONS),
                Err(PreFlightRefusal::Coherence {
                    derived: 208,
                    max_calls: 300
                })
            ),
            "max_calls 300 must fail the strict band"
        );
    }

    // -- T-12(e): a cold run (K = 0) with N_expected > 8 refuses without
    // --yes (naming the expected count + --yes, no request) and proceeds
    // with it; the 0-analyzed boundary (N_expected = 8) proceeds WITHOUT
    // --yes — the warm-run floor is not a refusal.

    #[tokio::test]
    async fn t12e_cold_run_refuses_without_yes_and_proceeds_with_it() {
        // Refuse: 2 clean sessions → N_expected = 2 + 0 + 7 + 1 = 10 > 8.
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        let d1 = test_digest(1, Vec::new());
        let d2 = test_digest(1, Vec::new());
        let sessions = vec![clean_input("a", &d1, "a"), clean_input("b", &d2, "b")];

        let req = make_req(
            &client,
            sessions.clone(),
            &budget,
            &caps,
            false,
            RedactionRung::Off,
        );
        match handle_insights(&req).await {
            Err(PreFlightRefusal::SpendGate {
                expected,
                confirm_above_calls,
            }) => {
                assert_eq!((expected, confirm_above_calls), (10, 8));
                let line = PreFlightRefusal::SpendGate {
                    expected,
                    confirm_above_calls,
                }
                .refusal_line();
                assert!(
                    line.contains("10"),
                    "the line must name the expected count: {line}"
                );
                assert!(line.contains("--yes"), "the line must name --yes: {line}");
            }
            other => {
                panic!("a cold run of 10 expected calls must refuse without --yes, got {other:?}")
            }
        }
        assert_eq!(
            server.request_count_for(PATH),
            0,
            "no request before a run exists"
        );

        // Proceed: the same store with --yes — a full 10 calls.
        script_full_run(&server, 2);
        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("--yes proceeds");
        assert_eq!(
            server.request_count_for(PATH),
            10,
            "2 facet + 7 section + 1 glance = 10 calls"
        );
        assert_eq!(outcome.totals.sessions_analyzed, 2);
    }

    #[tokio::test]
    async fn t12e_boundary_zero_analyzed_proceeds_without_yes() {
        // 0 analyzed → N_expected = 0 + 0 + 7 + 1 = 8; 8 > 8 is false → the
        // run proceeds without --yes.
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        script_full_run(&server, 0);

        let req = make_req(
            &client,
            Vec::new(),
            &budget,
            &caps,
            false,
            RedactionRung::Off,
        );
        let outcome = handle_insights(&req)
            .await
            .expect("8 == confirm_above_calls is not above it");
        assert_eq!(
            server.request_count_for(PATH),
            8,
            "the 8 prose calls (7 sections + the glance) still run"
        );
        assert_eq!(outcome.totals.sessions_scanned, 0);
        assert_eq!(outcome.record.sessions_scanned, 0);
    }

    // -- T-12(f): the coherence check passed sections = 8 (a section count
    // beyond the seven) refuses at startup — 40 + 160 + 8 + 1 = 209 > 208.
    // The run path passes `SECTIONS` (= 7), so a section added elsewhere
    // cannot pass the check silently.

    #[test]
    fn t12f_a_section_count_of_eight_refuses_at_startup() {
        assert_eq!(
            coherence_check(40, 4, 160, 208, 8),
            Err(PreFlightRefusal::Coherence {
                derived: 209,
                max_calls: 208
            })
        );
        // And the derived SECTIONS is the seven (R-13).
        assert_eq!(SECTIONS, SECTION_PROMPTS.len());
        assert_eq!(SECTIONS, 7);
    }

    // -- T-12(g) runtime arms: the completed run's record.capped_by carries
    // the ceilings that bound it and MIRRORS totals.capped_by (one source,
    // two sinks — R-6).

    #[tokio::test]
    async fn t12g_chunk_per_session_caps_the_record_and_mirrors_the_totals() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();

        // 100 001 chars → div_ceil(100001 / 25000) = 5 chunks > the 4-chunk
        // per-session ceiling, and the shipped defaults tie (4 × 40 = 160 =
        // max_chunk_calls_per_run) → BOTH chunk caps are recorded (T-12(i)'s
        // "both").
        let big = test_digest(1, Vec::new());
        let projection = "x".repeat(100_001);
        let sessions = vec![clean_input("big", &big, &projection)];
        // 4 chunk replies + 1 facet + 7 sections + 1 glance = 13 calls.
        for _ in 0..4 {
            server.enqueue_response(PATH, sse_reply("chunk summary"));
        }
        script_full_run(&server, 1);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req)
            .await
            .expect("the run completes partial");

        assert_eq!(
            outcome.record.capped_by,
            vec![CappedBy::ChunkPerSession, CappedBy::ChunkPerRun],
            "the per-session cap (and the shipped-default tie) bound the run"
        );
        assert!(
            outcome
                .record
                .capped_by
                .contains(&CappedBy::ChunkPerSession),
            "record.capped_by must contain ChunkPerSession"
        );
        assert_eq!(
            outcome.record.capped_by, outcome.totals.capped_by,
            "the record mirrors Totals.capped_by (one source, two sinks)"
        );
        assert_eq!(
            server.request_count_for(PATH),
            13,
            "4 chunk + 1 facet + 7 section + 1 glance = 13 calls"
        );
    }

    #[tokio::test]
    async fn t12g_chunk_per_run_caps_the_record_when_the_per_run_backstop_binds() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        // Small config: 5 + min(4 × 5, 10) + 7 + 1 = 23 = max_calls (coherent).
        let budget = InsightsBudget {
            max_sessions: 5,
            max_chunk_calls_per_session: 4,
            max_chunk_calls_per_run: 10,
            max_calls: 23,
            concurrency: 1,
            ..InsightsBudget::default()
        };

        // 80 000 chars → exactly 4 chunks each (at, not over, the per-session
        // cap); three sessions × 4 = 12 > the 10-chunk per-run backstop → it
        // binds on the third session (room = 2).
        let big1 = test_digest(1, Vec::new());
        let big2 = test_digest(1, Vec::new());
        let big3 = test_digest(1, Vec::new());
        let p1 = "x".repeat(80_000);
        let p2 = "x".repeat(80_000);
        let p3 = "x".repeat(80_000);
        let sessions = vec![
            clean_input("a", &big1, &p1),
            clean_input("b", &big2, &p2),
            clean_input("c", &big3, &p3),
        ];
        // s1: 4 chunks + facet; s2: 4 chunks + facet; s3: 2 chunks (room) +
        // facet; then 7 sections + 1 glance = 21 calls.
        for _ in 0..4 {
            server.enqueue_response(PATH, sse_reply("chunk summary"));
        }
        server.enqueue_response(PATH, sse_reply(&valid_facet_json()));
        for _ in 0..4 {
            server.enqueue_response(PATH, sse_reply("chunk summary"));
        }
        server.enqueue_response(PATH, sse_reply(&valid_facet_json()));
        for _ in 0..2 {
            server.enqueue_response(PATH, sse_reply("chunk summary"));
        }
        server.enqueue_response(PATH, sse_reply(&valid_facet_json()));
        script_full_run(&server, 0);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req)
            .await
            .expect("the run completes partial");

        assert_eq!(
            outcome.record.capped_by,
            vec![CappedBy::ChunkPerRun],
            "only the per-run backstop bound this run (no per-session tie: 10 != 4 × 5)"
        );
        assert!(
            outcome.record.capped_by.contains(&CappedBy::ChunkPerRun),
            "record.capped_by must contain ChunkPerRun"
        );
        assert_eq!(
            outcome.record.capped_by, outcome.totals.capped_by,
            "the record mirrors Totals.capped_by (one source, two sinks)"
        );
        assert_eq!(
            server.request_count_for(PATH),
            21,
            "10 chunk + 3 facet + 7 section + 1 glance = 21 calls"
        );
    }

    #[tokio::test]
    async fn t12g_cost_ceiling_caps_the_record_and_completes_partial() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = InsightsBudget {
            max_cost_ticks: 5,
            concurrency: 1,
            ..InsightsBudget::default()
        };

        // 2 clean sessions; the cost feed (10 ticks per call) crosses the
        // 5-tick ceiling on the very first call.
        let d1 = test_digest(1, Vec::new());
        let d2 = test_digest(1, Vec::new());
        let sessions = vec![clean_input("a", &d1, "a"), clean_input("b", &d2, "b")];
        // s1's facet (10 ticks → total 10 > 5 → the label run stops; s2 is
        // StoppedBeforeLabel), then narrate's first section (10 ticks → total
        // 20 > 5 → the wave stops: 6 sections + the glance unissued).
        server.enqueue_response(PATH, cost_reply(&valid_facet_json(), 10));
        server.enqueue_response(PATH, cost_reply(&valid_section_reply(0), 10));

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req)
            .await
            .expect("a cost stop is a COMPLETED partial run");

        assert_eq!(
            outcome.record.capped_by,
            vec![CappedBy::Cost],
            "the cost ceiling bound the run"
        );
        assert!(
            outcome.record.capped_by.contains(&CappedBy::Cost),
            "record.capped_by must contain Cost"
        );
        assert_eq!(
            outcome.record.capped_by, outcome.totals.capped_by,
            "the record mirrors Totals.capped_by (one source, two sinks)"
        );
        // The run completes PARTIAL: s2 never got its facet.
        assert_eq!(outcome.totals.sessions_partial, 1);
        assert_eq!(outcome.record.stopped_before_label_sessions, 1);
        assert_eq!(
            server.request_count_for(PATH),
            2,
            "the stop is real: only 2 calls issued"
        );
    }

    // -- T-10(d) (R-10): the record's redaction field carries the rung the
    // run actually used, for all three rungs.

    #[tokio::test]
    async fn t10d_the_record_stamps_the_rung_the_run_actually_used() {
        for rung in [
            RedactionRung::Off,
            RedactionRung::Prompts,
            RedactionRung::Redacted,
        ] {
            let server = MockInferenceServer::start()
                .await
                .expect("mock should start");
            let client = client_for(&server).await;
            let caps = ProjectionCaps::default();
            let budget = sequential_budget();
            let clean = test_digest(1, Vec::new());
            let sessions = vec![clean_input("only", &clean, "p")];
            script_full_run(&server, 1);

            let req = make_req(&client, sessions, &budget, &caps, true, rung);
            let outcome = handle_insights(&req).await.expect("the run completes");
            assert_eq!(
                outcome.record.redaction, rung,
                "the record must stamp the rung the run actually used"
            );
        }
    }

    // -- T-14(b) (R-11): insights.model on another endpoint refuses BEFORE
    // any request (the counter at zero, the host named) while the knob is
    // false, and proceeds once the knob is true.

    #[tokio::test]
    async fn t14b_the_endpoint_consent_gate_refuses_before_any_request() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        let session_host = server.origin().trim_start_matches("http://").to_string();
        let clean = test_digest(1, Vec::new());
        let sessions = vec![clean_input("only", &clean, "p")];

        // Knob false: refuses, names the host, issues nothing.
        let req = InsightsRequest {
            model_endpoint_host: "other.example".to_string(),
            session_endpoint_host: session_host.clone(),
            allow_alt_endpoint: false,
            ..make_req(
                &client,
                sessions.clone(),
                &budget,
                &caps,
                true,
                RedactionRung::Off,
            )
        };
        match handle_insights(&req).await {
            Err(PreFlightRefusal::AltEndpoint { host }) => {
                assert_eq!(host, "other.example");
                let line = PreFlightRefusal::AltEndpoint { host: host.clone() }.refusal_line();
                assert!(
                    line.contains("other.example"),
                    "the refusal line must name the host it was refused for: {line}"
                );
            }
            other => {
                panic!("a different-endpoint model must refuse without consent, got {other:?}")
            }
        }
        assert_eq!(
            server.request_count_for(PATH),
            0,
            "the refusal is pre-request — the counter at zero is the non-vacuity"
        );

        // Knob true: the full run proceeds (the mock receives all 9 requests).
        script_full_run(&server, 1);
        let req = InsightsRequest {
            model_endpoint_host: "other.example".to_string(),
            allow_alt_endpoint: true,
            ..make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off)
        };
        let outcome = handle_insights(&req).await.expect("consent proceeds");
        assert_eq!(
            server.request_count_for(PATH),
            9,
            "1 facet + 7 section + 1 glance = 9 calls"
        );
        let _ = outcome;
    }

    // -- T-11 (R-12, the run-level share of 8c): a clean full run issues
    // exactly N facet + 7 section + 1 glance.

    #[tokio::test]
    async fn t11_a_clean_full_run_issues_exactly_n_facet_plus_seven_sections_plus_glance() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        let d1 = test_digest(1, Vec::new());
        let d2 = test_digest(1, Vec::new());
        let d3 = test_digest(1, Vec::new());
        let sessions = vec![
            clean_input("a", &d1, "a"),
            clean_input("b", &d2, "b"),
            clean_input("c", &d3, "c"),
        ];
        script_full_run(&server, 3);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("the run completes");
        assert_eq!(
            server.request_count_for(PATH),
            11,
            "3 facet + 7 section + 1 glance = 11 calls"
        );
        assert_eq!(outcome.totals.sessions_analyzed, 3);
        assert!(outcome.at_a_glance.is_some(), "the glance parsed");
    }

    // -- R-4's disposition: an analyzed session with user_turns == 0 is
    // UNLABELABLE — no facet call, badged partial, counted in
    // unlabelable_sessions, and it does not consume a label-cap slot.

    #[tokio::test]
    async fn r4_disposition_analyzed_with_zero_user_turns_is_unlabelable() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        let zero = test_digest(0, Vec::new());
        let one = test_digest(1, Vec::new());
        let sessions = vec![
            clean_input("zero", &zero, "z"),
            clean_input("one", &one, "o"),
        ];
        // Only the eligible session gets a facet: 1 facet + 8 narrate = 9.
        script_full_run(&server, 1);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("the run completes");
        assert_eq!(
            server.request_count_for(PATH),
            9,
            "no facet call for the unlabelable session"
        );
        assert_eq!(outcome.totals.sessions_analyzed, 2);
        assert_eq!(
            outcome.totals.sessions_partial, 1,
            "the unlabelable session is badged partial"
        );
        assert_eq!(outcome.record.unlabelable_sessions, 1);
        assert_eq!(
            outcome.record.label_cap_sessions, 0,
            "unlabelable does not consume label-cap slots"
        );
    }

    // -- The suggestions clipboard population (the 4-round Major): the
    // suggestions section carries the payload extracted from its validated
    // reply; the other six sections carry none. The rendered HTML is the
    // observation point — an empty `suggestions` renders no block at all
    // (report.rs's `push_suggestions_block` early-returns).

    /// The one card with the given anchor id (its `</section>` close is the
    /// end), so a per-card assertion never sees another card's content.
    fn card_of<'a>(html: &'a str, anchor: &str) -> &'a str {
        let start = html
            .find(&format!(r#"id="{anchor}""#))
            .unwrap_or_else(|| panic!("card {anchor} missing from the report"));
        let end = html[start..]
            .find("</section>")
            .expect("the card is closed")
            + start;
        &html[start..end]
    }

    #[test]
    fn suggestions_payload_walks_the_three_arrays_in_schema_order() {
        // One field per array entry, in the schema's array order —
        // claude_md_additions, then features_to_try, then usage_patterns —
        // with items walked in order within each array.
        let reply = json!({
            "claude_md_additions": [
                { "addition": "Add rule one", "why": "w1", "prompt_scaffold": "s1" },
                { "addition": "Add rule two", "why": "w2", "prompt_scaffold": "s2" }
            ],
            "features_to_try": [
                { "feature": "f", "one_liner": "o", "why_for_you": "y", "example_code": "code one" }
            ],
            "usage_patterns": [
                { "title": "t", "suggestion": "s", "detail": "d", "copyable_prompt": "prompt one" },
                { "title": "t", "suggestion": "s", "detail": "d", "copyable_prompt": "prompt two" }
            ]
        });
        assert_eq!(
            suggestions_payload(&reply),
            vec![
                "Add rule one",
                "Add rule two",
                "code one",
                "prompt one",
                "prompt two"
            ]
        );
        // And the run fixture (the schema-valid reply the mock serves for
        // the suggestions section) extracts its three copyable fields.
        let fixture: Value = serde_json::from_str(&valid_section_reply(4)).expect("fixture parses");
        assert_eq!(
            suggestions_payload(&fixture),
            vec!["ZebraForge-4", "c", "p"]
        );
    }

    #[test]
    fn suggestions_payload_empty_arrays_yield_an_empty_vec() {
        let reply = json!({
            "claude_md_additions": [],
            "features_to_try": [],
            "usage_patterns": []
        });
        assert!(suggestions_payload(&reply).is_empty());
    }

    #[test]
    fn suggestions_payload_a_non_object_reply_yields_an_empty_vec() {
        assert!(suggestions_payload(&json!(null)).is_empty());
        assert!(suggestions_payload(&json!([1, 2, 3])).is_empty());
    }

    #[tokio::test]
    async fn suggestions_section_carries_the_payload_and_the_other_six_do_not() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        let client = client_for(&server).await;
        let caps = ProjectionCaps::default();
        let budget = sequential_budget();
        let clean = test_digest(1, Vec::new());
        let sessions = vec![clean_input("only", &clean, "p")];
        script_full_run(&server, 1);

        let req = make_req(&client, sessions, &budget, &caps, true, RedactionRung::Off);
        let outcome = handle_insights(&req).await.expect("the run completes");

        // Exactly the three copyable fields render as clipboard items, and
        // no other section renders any (an empty `suggestions` renders no
        // block, so the document-wide count is the per-section evidence).
        assert_eq!(
            outcome
                .html
                .matches(r#"<div class="suggestion-item""#)
                .count(),
            3,
            "the populated payload is exactly three items; no other section carries any"
        );
        // All three live in the suggestions card, under the copyable field
        // values the fixture carries.
        let card = card_of(&outcome.html, "section-suggestions");
        for text in ["ZebraForge-4", "c", "p"] {
            assert!(
                card.contains(&format!(r#"data-text="{text}""#)),
                "the suggestions card lost its copyable field {text:?}"
            );
        }
        // And none of the other six cards carries a suggestions block.
        for (anchor, _) in SECTION_CARDS
            .iter()
            .filter(|(a, _)| *a != "section-suggestions")
        {
            assert!(
                !card_of(&outcome.html, anchor).contains(r#"class="suggestions""#),
                "a prose section card carried a suggestions block: {anchor}"
            );
        }
    }
}
