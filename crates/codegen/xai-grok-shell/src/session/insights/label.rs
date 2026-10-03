//! Facet labelling + JSON Schema validation (SDD §4 module map).
//!
//! **Item 7 cycle 2 (the pure typed contract):** `SessionFacets` (the §5:503
//! fence + the R1 `languages` ruling), the eight const-built JSON Schemas
//! (the facet's shape + the seven section payload shapes) and the two
//! validators that wrap the mandated `workflow/schema_contract.rs` helpers.
//! The contract half is pure — NO model call, NO I/O, NO `with_json_schema`
//! EMISSION, NO retry, NO badging (§6:855-862: a failed reply is an `Err`
//! here; the execution half maps it to the badge).
//!
//! **Item 8a (the execution, the second half of the file):** the facet
//! labelling wave — the request-builder helpers (the first caller of
//! `contract_prompt`, Departure 1's emission pair), the per-session facet +
//! chunk calls, the `max_calls` guard, the `max_cost_ticks` stop, and
//! validation → `SessionFacets` with no retry + the badge causes.
//!
//! The facet `enum` arrays are built from the `X_VARIANTS` consts in
//! `types.rs` (the type spine's single home) — each member serialized in
//! const order; the spelling list is never hand-typed (R-const-built).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{
    FRICTION_VARIANTS, Friction, HELPFULNESS_VARIANTS, Helpfulness, OUTCOME_VARIANTS, Outcome,
    SATISFACTION_VARIANTS, SESSION_TYPE_VARIANTS, SUCCESS_MODE_VARIANTS, Satisfaction, SessionType,
    SuccessMode,
};
use crate::session::workflow::schema_contract::{
    compile_contract_schema, validate_contract_output,
};

/// The facet schema's own version axis (SOT §5:503, R-cache): its own
/// constant, so a facet bump does not invalidate 100 digests (§8). DISTINCT
/// from the digest's `CACHE_SCHEMA_VERSION` (`digest.rs`) — an independent
/// versioning axis beside the digest's.
pub const FACET_CACHE_SCHEMA_VERSION: u32 = 1;

/// Model output for one session (SOT §5:503 fence, verbatim): the 13 core
/// fields, plus the R1 `languages` ruling — the fence has no `languages`,
/// but §7:1113, §6:952 and T-0(l) require the facet-boundary cap, so the
/// field is `#[serde(default)]` (a reply omitting it deserializes to
/// `Vec::new()` and stays out of the schema's `required`).
/// `deny_unknown_fields` is the strict unknown-field gate (R11): the facet
/// schema deliberately carries NO `additionalProperties: false`.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SessionFacets {
    /// `FACET_CACHE_SCHEMA_VERSION` — its own constant, so a facet bump
    /// does not invalidate 100 digests (§8).
    pub cache_version: u32,
    /// Same key as the digest: the facet is stale when the transcript is.
    pub transcript_sha256: String,
    pub session_id: String,
    pub underlying_goal: String,
    pub goal_categories: BTreeMap<String, u32>,
    /// enum, not a free string; the six sets are the type spine's consts.
    pub outcome: Outcome,
    /// CC's wire key verbatim: the §5 `deny_unknown_fields` rejects any key
    /// that is not a `Satisfaction` spelling.
    pub user_satisfaction_counts: BTreeMap<Satisfaction, u32>,
    pub claude_helpfulness: Helpfulness,
    pub session_type: SessionType,
    /// `deny_unknown_fields` rejects any key that is not a `Friction` spelling.
    pub friction_counts: BTreeMap<Friction, u32>,
    pub friction_detail: String,
    pub primary_success: SuccessMode,
    pub brief_summary: String,
    /// R1: not in the §5:503 fence — defaulted so a reply omitting it
    /// still validates and deserializes.
    #[serde(default)]
    pub languages: Vec<String>,
}

/// Serialize a const's members (in const order) to a schema `enum` array —
/// R-const-built: the spelling list is never hand-typed.
fn enum_array<T: Serialize + Copy>(variants: &[T]) -> Value {
    Value::Array(
        variants
            .iter()
            .map(|v| {
                serde_json::to_value(*v)
                    .unwrap_or_else(|e| panic!("facet enum member failed to serialize: {e}"))
            })
            .collect(),
    )
}

/// The facet schema (SOT §7 :1024-1026): the shape of `SessionFacets`. The
/// six `enum` arrays are built from the six `X_VARIANTS` consts; the four
/// §7:1113 caps live here (a cap not in the schema is a comment); NO
/// `additionalProperties: false` (R11 — serde's `deny_unknown_fields` is
/// the unknown-field gate); `languages` is NOT in `required` (R1).
pub fn facet_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": [
            "cache_version",
            "transcript_sha256",
            "session_id",
            "underlying_goal",
            "goal_categories",
            "outcome",
            "user_satisfaction_counts",
            "claude_helpfulness",
            "session_type",
            "friction_counts",
            "friction_detail",
            "primary_success",
            "brief_summary"
        ],
        "properties": {
            "cache_version": { "type": "integer" },
            "transcript_sha256": { "type": "string" },
            "session_id": { "type": "string" },
            "underlying_goal": { "type": "string" },
            "goal_categories": {
                "type": "object",
                "additionalProperties": { "type": "integer" },
                "maxProperties": 8,
                "propertyNames": { "maxLength": 40 }
            },
            "outcome": { "enum": enum_array(OUTCOME_VARIANTS) },
            "user_satisfaction_counts": {
                "type": "object",
                "additionalProperties": { "type": "integer" }
            },
            "claude_helpfulness": { "enum": enum_array(HELPFULNESS_VARIANTS) },
            "session_type": { "enum": enum_array(SESSION_TYPE_VARIANTS) },
            "friction_counts": {
                "type": "object",
                "additionalProperties": { "type": "integer" }
            },
            "friction_detail": { "type": "string", "maxLength": 240 },
            "primary_success": { "enum": enum_array(SUCCESS_MODE_VARIANTS) },
            "brief_summary": { "type": "string", "maxLength": 160 },
            "languages": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": 8
            }
        }
    })
}

/// The `insights_project_areas` payload schema — ported from the 7a file's
/// `RESPOND WITH ONLY A VALID JSON OBJECT:` block (R-section: the file is
/// the source of truth).
pub fn project_areas_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["areas"],
        "properties": {
            "areas": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name", "session_count", "description"],
                    "properties": {
                        "name": { "type": "string" },
                        "session_count": { "type": "number" },
                        "description": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// The `insights_interaction_style` payload schema (R-section).
pub fn interaction_style_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["narrative", "key_pattern"],
        "properties": {
            "narrative": { "type": "string" },
            "key_pattern": { "type": "string" }
        }
    })
}

/// The `insights_what_works` payload schema (R-section).
pub fn what_works_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["intro", "impressive_workflows"],
        "properties": {
            "intro": { "type": "string" },
            "impressive_workflows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["title", "description"],
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// The `insights_friction` payload schema (R-section).
pub fn friction_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["intro", "categories"],
        "properties": {
            "intro": { "type": "string" },
            "categories": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["category", "description", "examples"],
                    "properties": {
                        "category": { "type": "string" },
                        "description": { "type": "string" },
                        "examples": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                }
            }
        }
    })
}

/// The `insights_suggestions` payload schema (R-section).
pub fn suggestions_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["claude_md_additions", "features_to_try", "usage_patterns"],
        "properties": {
            "claude_md_additions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["addition", "why", "prompt_scaffold"],
                    "properties": {
                        "addition": { "type": "string" },
                        "why": { "type": "string" },
                        "prompt_scaffold": { "type": "string" }
                    }
                }
            },
            "features_to_try": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["feature", "one_liner", "why_for_you", "example_code"],
                    "properties": {
                        "feature": { "type": "string" },
                        "one_liner": { "type": "string" },
                        "why_for_you": { "type": "string" },
                        "example_code": { "type": "string" }
                    }
                }
            },
            "usage_patterns": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["title", "suggestion", "detail", "copyable_prompt"],
                    "properties": {
                        "title": { "type": "string" },
                        "suggestion": { "type": "string" },
                        "detail": { "type": "string" },
                        "copyable_prompt": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// The `insights_horizon` payload schema (R-section).
pub fn horizon_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["intro", "opportunities"],
        "properties": {
            "intro": { "type": "string" },
            "opportunities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["title", "whats_possible", "how_to_try", "copyable_prompt"],
                    "properties": {
                        "title": { "type": "string" },
                        "whats_possible": { "type": "string" },
                        "how_to_try": { "type": "string" },
                        "copyable_prompt": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// The `insights_fun_ending` payload schema (R-section).
pub fn fun_ending_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["headline", "detail"],
        "properties": {
            "headline": { "type": "string" },
            "detail": { "type": "string" }
        }
    })
}

/// The seven section schemas in `SECTION_PROMPTS` order (SOT §7
/// :1024-1026: each structured prompt carries its schema, declared next to
/// it), so T-6(a) and T-9(e)-full can range over all eight (the facet's
/// beside them).
pub const SECTION_SCHEMAS: [fn() -> Value; 7] = [
    project_areas_schema,
    interaction_style_schema,
    what_works_schema,
    friction_schema,
    suggestions_schema,
    horizon_schema,
    fun_ending_schema,
];

/// Validate the facet reply: compile the facet schema, extract + validate
/// the reply with the mandated helpers, then deserialize into
/// `SessionFacets` so `deny_unknown_fields` and the counts-map key enums
/// (serde-side, R11) are enforced. A failure is an `Err` — NO retry, NO
/// badging here (§6:855-862; item 8 maps the `Err` to the
/// `facet_validation_failure_sessions` badge).
pub fn validate_facet_reply(final_text: &str) -> Result<SessionFacets, String> {
    let validator = compile_contract_schema(&facet_schema())?;
    let value = validate_contract_output(&validator, final_text)?;
    serde_json::from_value::<SessionFacets>(value)
        .map_err(|e| format!("facet reply matched the schema but is not a SessionFacets: {e}"))
}

/// Validate one section's reply against that section's schema (selected by
/// the section prompt name — the `SECTION_PROMPTS` stem), returning the
/// parsed `Value`. The same compile + validate path as the facet.
pub fn validate_section_reply(name: &str, final_text: &str) -> Result<Value, String> {
    let schema = match name {
        "insights_project_areas" => project_areas_schema(),
        "insights_interaction_style" => interaction_style_schema(),
        "insights_what_works" => what_works_schema(),
        "insights_friction" => friction_schema(),
        "insights_suggestions" => suggestions_schema(),
        "insights_horizon" => horizon_schema(),
        "insights_fun_ending" => fun_ending_schema(),
        other => return Err(format!("unknown section prompt name: {other}")),
    };
    let validator = compile_contract_schema(&schema)?;
    validate_contract_output(&validator, final_text)
}

// ============================================================
// 8a execution: the facet labelling wave — the call helper, the max_calls
// guard, the max_cost_ticks stop, and validation → SessionFacets with the
// badge causes. 7b's schemas + validators above are untouched; this is the
// first caller of `contract_prompt` (Departure 1's emission pair, §6 :678-697).
// ============================================================

use std::collections::BTreeSet;
use xai_grok_sampler::SamplingClient;
use xai_grok_sampling_types::{ConversationItem, ConversationRequest};

use crate::session::insights::digest::{DegradedTier, ProjectionCaps, SessionDigest};
use crate::session::insights::types::{CappedBy, InsightsBudget};
use crate::session::workflow::schema_contract::contract_prompt;

/// The STRUCTURED facet request — carries the JSON Schema alongside the
/// `contract_prompt` wrap (the Departure 1 emission pair). The resolved model
/// + the shared output cap are inputs (8c resolves the model; 8a takes it).
pub fn facet_request(model: &str, max_tokens: u32, payload: &str) -> ConversationRequest {
    ConversationRequest::from_items(vec![
        ConversationItem::system(contract_prompt(
            super::prompt("insights_facet"),
            &facet_schema(),
        )),
        ConversationItem::user(payload),
    ])
    .with_model(model)
    .with_json_schema(facet_schema())
    .with_max_output_tokens(max_tokens)
}

/// The PROSE chunk-summary request — NO schema (the prose summariser).
pub fn chunk_summary_request(
    model: &str,
    max_tokens: u32,
    chunk_text: &str,
) -> ConversationRequest {
    ConversationRequest::from_items(vec![
        ConversationItem::system(super::prompt("insights_chunk_summary")),
        ConversationItem::user(chunk_text),
    ])
    .with_model(model)
    .with_max_output_tokens(max_tokens)
}

/// The `max_calls` guard (R-guard): reserve-before-compare — the `>=` is
/// checked BEFORE the counter increments, so a counter at the ceiling is
/// refused. The counter is shared across facet + chunk calls and counts the
/// calls actually issued.
#[derive(Debug)]
pub struct CallGuard {
    calls: usize,
    max_calls: usize,
}

/// A refused acquire — carries the counter + ceiling so the reason line names
/// the call ceiling.
#[derive(Debug)]
pub struct CallGuardRefusal {
    pub calls: usize,
    pub max_calls: usize,
}

impl CallGuardRefusal {
    /// The one-line reason: names the call ceiling + the figure.
    pub fn reason_line(&self) -> String {
        format!(
            "call ceiling reached: {} calls (ceiling {}); run stopped",
            self.calls, self.max_calls
        )
    }
}

impl CallGuard {
    pub fn new(max_calls: usize) -> Self {
        Self {
            calls: 0,
            max_calls,
        }
    }

    /// The current call count (the calls actually issued).
    pub fn count(&self) -> usize {
        self.calls
    }

    /// Reserve a call slot. The `>=` is checked BEFORE issuing: a counter at
    /// `max_calls` is refused (the 89th acquire with a ceiling of 88). The
    /// counter increments only on a successful reserve.
    pub fn acquire(&mut self) -> Result<(), CallGuardRefusal> {
        if self.calls >= self.max_calls {
            Err(CallGuardRefusal {
                calls: self.calls,
                max_calls: self.max_calls,
            })
        } else {
            self.calls += 1;
            Ok(())
        }
    }
}

/// The direct input to the labelling wave: the digest (for the degraded
/// markers + the projection length) + the projection text (for the calls).
/// The labeller re-projects — the digest stores the length only.
pub struct LabelInput<'a> {
    pub digest: &'a SessionDigest,
    pub projection: &'a str,
}

/// What happened to a session in the labelling wave (the badge causes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStatus {
    /// The facet was issued + validated (a `SessionFacets` was produced).
    Labelled,
    /// The facet (or a chunk) failed — badged; the run continued (no retry).
    FacetValidationFailure,
    /// A ceiling bound before the session was labelled — badged.
    StoppedBeforeLabel,
    /// The label cap bound before the session was labelled — badged.
    LabelCap,
}

/// The result of the labelling wave: the badge causes (as numbers, not drops),
/// the `capped_by` (sorted + deduped, mirrored into `Totals` by item 3's
/// footer), the cost figure, and the reason line.
#[derive(Debug)]
pub struct LabelOutcome {
    /// One entry per input session: the facets (if labelled) — `None` for the
    /// badged (non-Labelled) sessions.
    pub facets: Vec<Option<SessionFacets>>,
    /// One entry per input session.
    pub status: Vec<LabelStatus>,
    /// The sorted, deduped ceilings that bound this run.
    pub capped_by: Vec<CappedBy>,
    /// The count of sessions badged `FacetValidationFailure` (a number).
    pub facet_validation_failure_sessions: usize,
    /// The count of sessions badged `LabelCap` (a number).
    pub label_cap_sessions: usize,
    /// The count of sessions badged `StoppedBeforeLabel` (a number).
    pub stopped_before_label_sessions: usize,
    /// True iff some calls were priced and some unpriced (the §9 partial-cost
    /// badge — some_priced && some_unpriced).
    pub cost_is_partial: bool,
    /// The accumulated cost across the run's calls, in USD ticks.
    pub total_cost_ticks: i64,
    /// The first ceiling that bound the run, named (one line) — for the footer.
    pub reason_line: Option<String>,
    /// The total sessions considered (`inputs.len()`).
    pub sessions_analyzed: usize,
    /// The count of non-Labelled sessions (the partial count).
    pub sessions_partial: usize,
}

/// Truncate `s` to at most `max_chars` characters (a char boundary, not a
/// byte). Unlike `session::goal_classifier::cap_chars` it appends NO `…`
/// suffix: the result is a model-facing projection payload, not display
/// text, and the byte budget is the point — a suffix would spend it.
fn truncate_to_chars(s: &str, max_chars: usize) -> String {
    s.char_indices()
        .nth(max_chars)
        .map(|(i, _)| &s[..i])
        .unwrap_or(s)
        .to_string()
}

/// Fold one call's reported cost into the accumulator (R-cost): `Some` is
/// priced (adds to the total), `None` is unpriced (the accumulator stays).
fn accumulate_cost(
    total: &mut i64,
    some_priced: &mut bool,
    some_unpriced: &mut bool,
    cost: Option<i64>,
) {
    match cost {
        Some(ticks) => {
            *total += ticks;
            *some_priced = true;
        }
        None => {
            *some_unpriced = true;
        }
    }
}

/// Run the facet labelling wave over `inputs` (direct inputs: digests +
/// projections + the caps + the budget), returning the badge causes + the
/// `capped_by` + the cost figure. Sequential (8a); the `concurrency` bound is
/// 8c's. No retry (R-noretry): a failed facet/chunk reply badges the session
/// and the run continues. The cost stop (R-cost) yields a COMPLETED partial
/// run (not an abort): the crossing session is still labelled, only the
/// sessions after it render badged.
pub async fn label_sessions(
    client: &SamplingClient,
    model: &str,
    inputs: &[LabelInput<'_>],
    caps: &ProjectionCaps,
    budget: &InsightsBudget,
) -> LabelOutcome {
    let mut guard = CallGuard::new(budget.max_calls);
    let mut capped_by: BTreeSet<CappedBy> = BTreeSet::new();
    let mut reason_line: Option<String> = None;
    let mut run_stopped = false;
    let mut total_cost_ticks: i64 = 0;
    let mut some_priced = false;
    let mut some_unpriced = false;
    let mut chunk_calls_run = 0usize;
    let mut label_count = 0usize;

    let mut facets: Vec<Option<SessionFacets>> = Vec::with_capacity(inputs.len());
    let mut status: Vec<LabelStatus> = Vec::with_capacity(inputs.len());

    for input in inputs {
        // A ceiling bound before this session was labelled → badge + skip.
        if run_stopped {
            facets.push(None);
            status.push(LabelStatus::StoppedBeforeLabel);
            continue;
        }
        // The label cap: a session never labelled once the cap binds.
        if label_count >= budget.max_sessions {
            facets.push(None);
            status.push(LabelStatus::LabelCap);
            continue;
        }

        // Consume the digest's markers (R-chunk): InputTruncated → truncate +
        // no chunk fan-out (the facet carries the truncated projection).
        let truncated = input
            .digest
            .degraded
            .contains(&DegradedTier::InputTruncated);
        let projection: String = if truncated {
            truncate_to_chars(input.projection, caps.max_input_chars)
        } else {
            input.projection.to_string()
        };

        // The chunk plan (R-chunk): map-reduce kicks in when the projection
        // exceeds `chunk_summarize.threshold_chars` (SOT §6:724, read by the
        // labelling stage); a projection at or below it stays un-chunked and
        // its facet call carries the raw projection (§6:781). The per-session
        // chunk ceiling (the digest's ChunkCapHit) + the run-level backstop
        // (max_chunk_calls_per_run).
        let mut chunk_calls = 0usize;
        let mut chunked = false;
        let n_chars = projection.chars().count();
        if !truncated && n_chars > caps.threshold_chars {
            chunked = true;
            let n_chunks = n_chars.div_ceil(caps.chunk_chars.max(1));
            let per_session = n_chunks.min(budget.max_chunk_calls_per_session);
            if per_session < n_chunks {
                capped_by.insert(CappedBy::ChunkPerSession);
                reason_line.get_or_insert(format!(
                    "per-session chunk ceiling: {per_session} of {n_chunks} chunks"
                ));
                // The SOT tie (§6:728, T-12(i)): when
                // max_chunk_calls_per_session × max_sessions ties
                // max_chunk_calls_per_run (the shipped default: 4 × 40 = 160),
                // a run that truncates records BOTH chunk caps — the footer
                // has no single answer and must not invent one.
                if budget.max_chunk_calls_per_run
                    == budget.max_chunk_calls_per_session * budget.max_sessions
                {
                    capped_by.insert(CappedBy::ChunkPerRun);
                }
            }
            chunk_calls = per_session;
            let room = budget
                .max_chunk_calls_per_run
                .saturating_sub(chunk_calls_run);
            if chunk_calls > room {
                capped_by.insert(CappedBy::ChunkPerRun);
                reason_line.get_or_insert(format!(
                    "per-run chunk ceiling: {room} chunks (ceiling {})",
                    budget.max_chunk_calls_per_run
                ));
                chunk_calls = room;
            }
        }

        // A chunked session whose plan a ceiling cut to zero cannot be
        // labelled: its facet would have to carry the raw projection (up to
        // max_input_chars — outside §6's facet-input bound, which a chunked
        // session's facet call does not read), so the ceiling stops it before
        // the label (StoppedBeforeLabel) and issues no call.
        if chunked && chunk_calls == 0 {
            facets.push(None);
            status.push(LabelStatus::StoppedBeforeLabel);
            continue;
        }

        // The chunk calls (each guarded + cost-checked).
        let chars: Vec<char> = projection.chars().collect();
        let mut summaries: Vec<String> = Vec::new();
        let mut session_failed = false;
        for chunk in chars.chunks(caps.chunk_chars.max(1)).take(chunk_calls) {
            let chunk_text: String = chunk.iter().collect();
            let req = chunk_summary_request(model, budget.max_tokens, &chunk_text);
            if let Err(refusal) = guard.acquire() {
                capped_by.insert(CappedBy::Calls);
                reason_line.get_or_insert(refusal.reason_line());
                run_stopped = true;
                break;
            }
            match client.conversation_collect(req).await {
                Ok(response) => {
                    accumulate_cost(
                        &mut total_cost_ticks,
                        &mut some_priced,
                        &mut some_unpriced,
                        response.cost_usd_ticks,
                    );
                    if total_cost_ticks > budget.max_cost_ticks {
                        capped_by.insert(CappedBy::Cost);
                        reason_line.get_or_insert(format!(
                            "cost ceiling reached: {total_cost_ticks} ticks accumulated (ceiling {})",
                            budget.max_cost_ticks
                        ));
                        run_stopped = true;
                        break;
                    }
                    summaries.push(response.assistant_text());
                    chunk_calls_run += 1;
                }
                Err(_) => {
                    // A chunk error badges the session the same way as a facet
                    // failure (R-noretry): no retry, the run continues.
                    session_failed = true;
                    break;
                }
            }
        }

        // A ceiling bounded before the facet → the session is badged
        // StoppedBeforeLabel (the run may still have labelled the ones before).
        if run_stopped {
            facets.push(None);
            status.push(LabelStatus::StoppedBeforeLabel);
            continue;
        }
        // A chunk error → badge + skip the facet; the run continues.
        if session_failed {
            facets.push(None);
            status.push(LabelStatus::FacetValidationFailure);
            continue;
        }

        // The facet (the validation boundary — one call per labelled session).
        let payload = if summaries.is_empty() {
            projection.clone()
        } else {
            summaries.join("\n\n")
        };
        if let Err(refusal) = guard.acquire() {
            capped_by.insert(CappedBy::Calls);
            reason_line.get_or_insert(refusal.reason_line());
            run_stopped = true;
            facets.push(None);
            status.push(LabelStatus::StoppedBeforeLabel);
            continue;
        }
        label_count += 1;
        let req = facet_request(model, budget.max_tokens, &payload);
        match client.conversation_collect(req).await {
            Ok(response) => {
                accumulate_cost(
                    &mut total_cost_ticks,
                    &mut some_priced,
                    &mut some_unpriced,
                    response.cost_usd_ticks,
                );
                if total_cost_ticks > budget.max_cost_ticks {
                    capped_by.insert(CappedBy::Cost);
                    reason_line.get_or_insert(format!(
                        "cost ceiling reached: {total_cost_ticks} ticks accumulated (ceiling {})",
                        budget.max_cost_ticks
                    ));
                    run_stopped = true;
                }
                match validate_facet_reply(&response.assistant_text()) {
                    Ok(parsed) => {
                        facets.push(Some(parsed));
                        status.push(LabelStatus::Labelled);
                    }
                    Err(_) => {
                        facets.push(None);
                        status.push(LabelStatus::FacetValidationFailure);
                    }
                }
            }
            Err(_) => {
                // A transport failure badges the session the same way (no retry).
                facets.push(None);
                status.push(LabelStatus::FacetValidationFailure);
            }
        }
    }

    let sessions_partial = status
        .iter()
        .filter(|s| **s != LabelStatus::Labelled)
        .count();
    let facet_validation_failure_sessions = status
        .iter()
        .filter(|s| **s == LabelStatus::FacetValidationFailure)
        .count();
    let label_cap_sessions = status
        .iter()
        .filter(|s| **s == LabelStatus::LabelCap)
        .count();
    let stopped_before_label_sessions = status
        .iter()
        .filter(|s| **s == LabelStatus::StoppedBeforeLabel)
        .count();
    LabelOutcome {
        facets,
        status,
        capped_by: capped_by.into_iter().collect(),
        facet_validation_failure_sessions,
        label_cap_sessions,
        stopped_before_label_sessions,
        cost_is_partial: some_priced && some_unpriced,
        total_cost_ticks,
        reason_line,
        sessions_analyzed: inputs.len(),
        sessions_partial,
    }
}

#[cfg(test)]
mod tests {
    //! T-6(a) (SOT §12 :1729): the four arms × all eight schemas — plus the
    //! const-built-schema assertion, the four §7:1113 cap pins (the
    //! schema-side half of T-0(l)), the `deny_unknown_fields` pin, and
    //! T-9(e)-full (the schema half; 7a's T-9(e-file) already covers the
    //! file bytes). All of it runs under the established
    //! `session::insights` filter.

    use std::collections::BTreeMap;

    use serde::Serialize;
    use serde_json::{Map, Value, json};

    use super::{
        FACET_CACHE_SCHEMA_VERSION, SECTION_SCHEMAS, SessionFacets, facet_schema, friction_schema,
        fun_ending_schema, horizon_schema, interaction_style_schema, project_areas_schema,
        suggestions_schema, validate_facet_reply, validate_section_reply, what_works_schema,
    };
    use crate::session::insights::types::{
        FRICTION_VARIANTS, Friction, HELPFULNESS_VARIANTS, Helpfulness, OUTCOME_VARIANTS, Outcome,
        SATISFACTION_VARIANTS, SESSION_TYPE_VARIANTS, SUCCESS_MODE_VARIANTS, Satisfaction,
        SessionType, SuccessMode,
    };
    use crate::session::insights::{PROMPT_MAX_BYTES, SECTION_PROMPTS, prompt};

    /// The valid facet reply: the 13 §5 fields. The R1 `languages` field is
    /// deliberately OMITTED — a reply without it must validate and
    /// deserialize to `Vec::new()` (a mutant that makes it `required` goes
    /// red in the valid arms).
    fn valid_facet_value() -> Value {
        json!({
            "cache_version": FACET_CACHE_SCHEMA_VERSION,
            "transcript_sha256": "9f2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c",
            "session_id": "sess-t6a",
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
    }

    fn facet_reply(mutate: impl FnOnce(&mut Value)) -> String {
        let mut v = valid_facet_value();
        mutate(&mut v);
        serde_json::to_string(&v).unwrap()
    }

    // -- T-6(a), the facet schema: the four arms.

    #[test]
    fn t6a_facet_a_valid_reply_validates_and_deserializes() {
        let facets: SessionFacets = validate_facet_reply(&valid_facet_value().to_string())
            .unwrap_or_else(|e| panic!("the fully-valid facet reply must validate: {e}"));
        assert_eq!(facets.cache_version, FACET_CACHE_SCHEMA_VERSION);
        assert_eq!(facets.session_id, "sess-t6a");
        assert_eq!(facets.outcome, Outcome::FullyAchieved);
        assert_eq!(facets.claude_helpfulness, Helpfulness::VeryHelpful);
        assert_eq!(facets.session_type, SessionType::IterativeRefinement);
        assert_eq!(facets.primary_success, SuccessMode::GoodDebugging);
        assert_eq!(
            facets.goal_categories,
            BTreeMap::from([("debugging".to_string(), 2)])
        );
        assert_eq!(
            facets.user_satisfaction_counts,
            BTreeMap::from([(Satisfaction::Satisfied, 1)])
        );
        assert_eq!(
            facets.friction_counts,
            BTreeMap::from([(Friction::ToolFailed, 1)])
        );
        // R1: the reply omitted `languages` — it deserialized to the default.
        assert!(
            facets.languages.is_empty(),
            "R1: an omitted `languages` must default to empty"
        );
    }

    #[test]
    fn t6a_facet_an_out_of_range_enum_value_is_rejected_by_the_schema() {
        let reply = facet_reply(|v| {
            v["outcome"] = json!("sideways");
        });
        let Err(err) = validate_facet_reply(&reply) else {
            panic!("an out-of-range `outcome` value was accepted");
        };
        assert!(
            err.contains("does not match the required schema"),
            "the rejection must be the schema's, not a parse error: {err}"
        );
    }

    #[test]
    fn t6a_facet_a_reply_wrapped_in_prose_is_accepted_and_round_trips() {
        let payload = valid_facet_value();
        let reply =
            format!("I read the transcript.\n\n```json\n{payload}\n```\n\nThat is the facet.");
        let facets = validate_facet_reply(&reply)
            .unwrap_or_else(|e| panic!("a ```json-fenced facet reply must validate: {e}"));
        let mut expected = payload;
        expected["languages"] = json!([]);
        assert_eq!(
            serde_json::to_value(&facets).unwrap(),
            expected,
            "the extracted value must round-trip to the reply's payload (the serde default fills `languages`)"
        );
    }

    #[test]
    fn t6a_facet_two_top_level_json_objects_fail_with_the_parse_error() {
        let payload = valid_facet_value().to_string();
        let reply = format!("{payload}{payload}");
        let Err(err) = validate_facet_reply(&reply) else {
            panic!("two top-level JSON objects were accepted");
        };
        assert!(
            err.contains("did not contain valid JSON"),
            "two top-level objects must be a parse error, not a schema error: {err}"
        );
    }

    // -- T-6(a), the seven section schemas: the four arms, parameterized.
    // (name, a valid payload, a type violation at a required property) — in
    // `SECTION_PROMPTS` order; the payloads come from the seven 7a files'
    // `RESPOND WITH ONLY A VALID JSON OBJECT:` blocks.

    const SECTION_CASES: [(&str, &str, &str); 7] = [
        (
            "insights_project_areas",
            r#"{"areas":[{"name":"Area A","session_count":2,"description":"What was worked on."}]}"#,
            r#"{"areas":"not-an-array"}"#,
        ),
        (
            "insights_interaction_style",
            r#"{"narrative":"You iterate quickly.","key_pattern":"Short prompts."}"#,
            r#"{"narrative":5,"key_pattern":"Short prompts."}"#,
        ),
        (
            "insights_what_works",
            r#"{"intro":"Context.","impressive_workflows":[{"title":"T","description":"D."}]}"#,
            r#"{"intro":true,"impressive_workflows":[]}"#,
        ),
        (
            "insights_friction",
            r#"{"intro":"Context.","categories":[{"category":"C","description":"D.","examples":["E1","E2"]}]}"#,
            r#"{"intro":7,"categories":[]}"#,
        ),
        (
            "insights_suggestions",
            r#"{"claude_md_additions":[{"addition":"A","why":"W","prompt_scaffold":"P"}],"features_to_try":[{"feature":"F","one_liner":"O","why_for_you":"Y","example_code":"E"}],"usage_patterns":[{"title":"T","suggestion":"S","detail":"D","copyable_prompt":"C"}]}"#,
            r#"{"claude_md_additions":[],"features_to_try":[],"usage_patterns":"x"}"#,
        ),
        (
            "insights_horizon",
            r#"{"intro":"Context.","opportunities":[{"title":"T","whats_possible":"W","how_to_try":"H","copyable_prompt":"C"}]}"#,
            r#"{"intro":{},"opportunities":[]}"#,
        ),
        (
            "insights_fun_ending",
            r#"{"headline":"The build said hello.","detail":"First run."}"#,
            r#"{"headline":{"h":1},"detail":"First run."}"#,
        ),
    ];

    #[test]
    fn t6a_sections_all_seven_pass_the_four_arms() {
        for (name, valid, invalid) in SECTION_CASES {
            let parsed: Value = serde_json::from_str(valid).unwrap();

            // 1. a valid reply -> Ok, and the extracted value round-trips.
            let got = validate_section_reply(name, valid)
                .unwrap_or_else(|e| panic!("{name}: a valid section payload must validate: {e}"));
            assert_eq!(got, parsed, "{name}: the extracted value must round-trip");

            // 2. a type violation at a required property -> the schema's Err
            // (the seven section schemas carry no enums, so this is their
            // out-of-range arm — SOT §12 T-6(a)).
            let Err(err) = validate_section_reply(name, invalid) else {
                panic!("{name}: a type violation at a required property was accepted");
            };
            assert!(
                err.contains("does not match the required schema"),
                "{name}: the rejection must be the schema's: {err}"
            );

            // 3. the same valid payload wrapped in a ```json fence, with prose
            // around it -> Ok, extracted value round-trips.
            let wrapped = format!("Done.\n\n```json\n{valid}\n```\n\nThat covers it.");
            let got = validate_section_reply(name, &wrapped)
                .unwrap_or_else(|e| panic!("{name}: a prose-wrapped payload must validate: {e}"));
            assert_eq!(
                got, parsed,
                "{name}: the fence-extracted value must round-trip"
            );

            // 4. two top-level JSON objects -> the parse error.
            let doubled = format!("{valid}{valid}");
            let Err(err) = validate_section_reply(name, &doubled) else {
                panic!("{name}: two top-level JSON objects were accepted");
            };
            assert!(
                err.contains("did not contain valid JSON"),
                "{name}: two top-level objects must be a parse error: {err}"
            );
        }
    }

    // -- The const-built-schema assertion (R-const-built): the type spine's
    // two-site enum↔const residual, finally asserted. Each facet `enum` array
    // equals its `X_VARIANTS` const's serde spellings IN CONST ORDER — a
    // hand-typed, dropped or reordered array goes red here. (The two counts
    // maps carry NO schema key-enum: their key sets — the remaining two
    // consts — are serde-side, exercised in the valid arm by the
    // `satisfied`/`tool_failed` keys deserializing.)

    fn const_spellings<T: Serialize + Copy>(variants: &[T]) -> Value {
        Value::Array(
            variants
                .iter()
                .map(|v| serde_json::to_value(*v).unwrap())
                .collect(),
        )
    }

    #[test]
    fn t6a_facet_schema_enums_are_built_from_the_x_variants_consts_in_const_order() {
        let schema = facet_schema();
        let props = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("the facet schema carries a properties object");
        for (field, expected) in [
            ("outcome", const_spellings(OUTCOME_VARIANTS)),
            ("claude_helpfulness", const_spellings(HELPFULNESS_VARIANTS)),
            ("session_type", const_spellings(SESSION_TYPE_VARIANTS)),
            ("primary_success", const_spellings(SUCCESS_MODE_VARIANTS)),
        ] {
            let enum_arr = props
                .get(field)
                .and_then(|p| p.get("enum"))
                .unwrap_or_else(|| panic!("the facet schema {field} property has no enum array"));
            assert_eq!(
                enum_arr, &expected,
                "the {field} enum array must equal its X_VARIANTS const's spellings in const order"
            );
        }
        // The two counts maps: integer values, and NO key-enum in the schema
        // (serde's `BTreeMap<Satisfaction,u32>`/`BTreeMap<Friction,u32>`
        // deserialization is the key gate for the remaining two consts).
        for (field, key_count) in [
            ("user_satisfaction_counts", SATISFACTION_VARIANTS.len()),
            ("friction_counts", FRICTION_VARIANTS.len()),
        ] {
            let prop = props
                .get(field)
                .unwrap_or_else(|| panic!("the facet schema has no {field} property"));
            assert_eq!(
                prop.get("additionalProperties"),
                Some(&json!({ "type": "integer" })),
                "{field}: the schema must stay an integer-valued open object (keys are serde-side)"
            );
            assert!(
                prop.get("enum").is_none(),
                "{field}: the schema must carry no key-enum (serde's {key_count}-member set is the key gate)"
            );
        }
    }

    // -- The four §7:1113 caps (the schema-side half of T-0(l)): "a cap that
    // is not in the schema is a comment, so all four live in the facet
    // output_format and are enforced by rejection, not truncation."

    #[test]
    fn cap_brief_summary_is_capped_at_160_chars() {
        let over = facet_reply(|v| {
            v["brief_summary"] = json!("x".repeat(161));
        });
        let Err(err) = validate_facet_reply(&over) else {
            panic!("a 161-char brief_summary was accepted");
        };
        assert!(
            err.contains("does not match the required schema"),
            "the cap must be a schema rejection: {err}"
        );
        let at = facet_reply(|v| {
            v["brief_summary"] = json!("x".repeat(160));
        });
        assert!(
            validate_facet_reply(&at).is_ok(),
            "a 160-char brief_summary must validate"
        );
    }

    #[test]
    fn cap_friction_detail_is_capped_at_240_chars() {
        let over = facet_reply(|v| {
            v["friction_detail"] = json!("x".repeat(241));
        });
        let Err(err) = validate_facet_reply(&over) else {
            panic!("a 241-char friction_detail was accepted");
        };
        assert!(
            err.contains("does not match the required schema"),
            "the cap must be a schema rejection: {err}"
        );
        let at = facet_reply(|v| {
            v["friction_detail"] = json!("x".repeat(240));
        });
        assert!(
            validate_facet_reply(&at).is_ok(),
            "a 240-char friction_detail must validate"
        );
    }

    #[test]
    fn cap_goal_categories_is_capped_at_8_keys() {
        let nine: Map<String, Value> = (0..9).map(|i| (format!("k{i}"), json!(1))).collect();
        let over = facet_reply(|v| v["goal_categories"] = Value::Object(nine));
        let Err(err) = validate_facet_reply(&over) else {
            panic!("9 goal_categories keys were accepted (maxProperties 8)");
        };
        assert!(
            err.contains("does not match the required schema"),
            "the cap must be a schema rejection: {err}"
        );
        let eight: Map<String, Value> = (0..8).map(|i| (format!("k{i}"), json!(1))).collect();
        let at = facet_reply(|v| v["goal_categories"] = Value::Object(eight));
        assert!(
            validate_facet_reply(&at).is_ok(),
            "8 goal_categories keys must validate"
        );
    }

    /// The R11 GREEN-TIME CHECK: `jsonschema 0.30.0` must actually enforce
    /// `propertyNames: { "maxLength": 40 }` — if it does not, the 41-char key
    /// passes and this arm goes red: STOP and report (R11), do NOT drop the
    /// cap or ship it unenforced.
    #[test]
    fn cap_goal_categories_keys_are_capped_at_40_chars() {
        let over = facet_reply(|v| {
            let mut gc = Map::new();
            gc.insert("a".repeat(41), json!(1));
            v["goal_categories"] = Value::Object(gc);
        });
        let Err(err) = validate_facet_reply(&over) else {
            panic!(
                "R11: a 41-char goal_categories key was accepted (propertyNames.maxLength 40 unenforced?)"
            );
        };
        assert!(
            err.contains("does not match the required schema"),
            "the cap must be a schema rejection: {err}"
        );
        let at = facet_reply(|v| {
            let mut gc = Map::new();
            gc.insert("a".repeat(40), json!(1));
            v["goal_categories"] = Value::Object(gc);
        });
        assert!(
            validate_facet_reply(&at).is_ok(),
            "a 40-char goal_categories key must validate"
        );
    }

    #[test]
    fn cap_languages_is_capped_at_8_entries() {
        let nine = Value::Array((0..9).map(|i| json!(format!("lang{i}"))).collect());
        let over = facet_reply(|v| v["languages"] = nine);
        let Err(err) = validate_facet_reply(&over) else {
            panic!("9 languages entries were accepted (maxItems 8)");
        };
        assert!(
            err.contains("does not match the required schema"),
            "the cap must be a schema rejection: {err}"
        );
        let eight = Value::Array((0..8).map(|i| json!(format!("lang{i}"))).collect());
        let at = facet_reply(|v| v["languages"] = eight);
        assert!(
            validate_facet_reply(&at).is_ok(),
            "8 languages entries must validate"
        );
    }

    // -- The `deny_unknown_fields` pin (R11): the schema carries NO
    // `additionalProperties: false`, so the jsonschema layer accepts an extra
    // field; serde's `deny_unknown_fields` at the `from_value` step is the
    // strict unknown-field gate.

    #[test]
    fn deny_unknown_fields_an_extra_facet_field_is_rejected_by_serde() {
        let reply = facet_reply(|v| {
            v["not_in_the_fence"] = json!("surprise");
        });
        let Err(err) = validate_facet_reply(&reply) else {
            panic!("an extra unknown field was accepted");
        };
        assert!(
            err.contains("unknown field"),
            "serde's deny_unknown_fields must be the gate that rejects it: {err}"
        );
    }

    // -- T-9(e)-full (the schema half, R5): for the EIGHT structured prompts,
    // file bytes + compact schema bytes <= PROMPT_MAX_BYTES. The two prose
    // prompts (chunk_summary, at_a_glance) stay at 7a's file-only bound —
    // not re-asserted here.

    #[test]
    fn t9e_full_the_eight_structured_prompts_fit_file_plus_schema_within_prompt_max_bytes() {
        let facet_file = prompt("insights_facet").len();
        let facet_schema_bytes = serde_json::to_string(&facet_schema()).unwrap().len();
        assert!(
            facet_file + facet_schema_bytes <= PROMPT_MAX_BYTES,
            "insights_facet: {facet_file} file + {facet_schema_bytes} schema = {} bytes, over the {PROMPT_MAX_BYTES}-byte cap",
            facet_file + facet_schema_bytes
        );
        for (name, schema_fn) in SECTION_PROMPTS.iter().zip(SECTION_SCHEMAS.iter()) {
            let file = prompt(name).len();
            let schema_bytes = serde_json::to_string(&schema_fn()).unwrap().len();
            assert!(
                file + schema_bytes <= PROMPT_MAX_BYTES,
                "{name}: {file} file + {schema_bytes} schema = {} bytes, over the {PROMPT_MAX_BYTES}-byte cap",
                file + schema_bytes
            );
        }
    }

    // -- The array↔fn correspondence: `SECTION_SCHEMAS[i]` is the named fn
    // matching `SECTION_PROMPTS[i]` (a swap goes red here AND in the section
    // valid arms, whose selection goes through `validate_section_reply`).

    #[test]
    fn section_schemas_array_matches_the_named_fns_in_section_prompts_order() {
        let expected = [
            project_areas_schema(),
            interaction_style_schema(),
            what_works_schema(),
            friction_schema(),
            suggestions_schema(),
            horizon_schema(),
            fun_ending_schema(),
        ];
        for (i, &name) in SECTION_PROMPTS.iter().enumerate() {
            assert_eq!(
                SECTION_CASES[i].0, name,
                "SECTION_CASES order must follow SECTION_PROMPTS"
            );
            assert_eq!(
                SECTION_SCHEMAS[i](),
                expected[i],
                "{name}: SECTION_SCHEMAS[{i}] is not the matching named schema fn (a swap?)"
            );
        }
    }
}

#[cfg(test)]
mod execution_tests {
    //! 8a execution tests: T-0 c/d/i/j/k/l + T-12 c/d/g/h + T-11 (call-count
    //! arm). The mock is `xai-grok-test-support`'s `MockInferenceServer`; the
    //! calls go through a real `SamplingClient` (Responses backend) so the wire
    //! shape is the production one — Departure 1's `with_json_schema` +
    //! `contract_prompt`, the shared `max_output_tokens` cap.

    use super::{
        CallGuard, FACET_CACHE_SCHEMA_VERSION, LabelInput, LabelOutcome, LabelStatus,
        label_sessions,
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

    fn valid_facet_value() -> Value {
        json!({
            "cache_version": FACET_CACHE_SCHEMA_VERSION,
            "transcript_sha256": "9f2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c",
            "session_id": "sess-8a",
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
    }

    fn valid_facet_json() -> String {
        valid_facet_value().to_string()
    }

    fn invalid_facet_json() -> String {
        let mut v = valid_facet_value();
        v["outcome"] = json!("sideways"); // an out-of-range enum variant (the §7 enum is closed)
        v.to_string()
    }

    async fn client_for(server: &MockInferenceServer) -> SamplingClient {
        SamplingClient::new(SamplerConfig {
            base_url: server.url(),
            api_backend: ApiBackend::Responses,
            api_key: Some("test-key".to_string()),
            model: "test-model".to_string(),
            context_window: 200_000,
            ..SamplerConfig::default()
        })
        .expect("sampler client should build")
    }

    fn sse_reply(text: &str, model: &str) -> ScriptedResponse {
        ScriptedResponse::sse(responses_api_script_exact(text, model))
    }

    fn cost_reply(text: &str, model: &str, ticks: i64) -> ScriptedResponse {
        ScriptedResponse::sse(cost_carrying_responses_sse(text, model, ticks))
    }

    /// The mock's `responses_api_script_exact` carries no cost; this mirrors
    /// its frame order but stamps `cost_in_usd_ticks` on the `response.completed`
    /// usage — the wire field the client parses into
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

    fn test_digest(degraded: Vec<DegradedTier>, projection_chars: usize) -> SessionDigest {
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
            user_turns: 1,
            assistant_turns: 1,
            tool_counts: BTreeMap::new(),
            languages: BTreeMap::new(),
            files_modified: 0,
            lines_added: 0,
            lines_removed: 0,
            projection_chars,
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

    // -- T-12(d): the guard seam — no model call, no client.

    #[test]
    fn t12d_guard_seam_the_89th_acquire_is_refused_and_names_the_ceiling() {
        let mut guard = CallGuard::new(88);
        for i in 0..88 {
            guard
                .acquire()
                .unwrap_or_else(|r| panic!("acquire #{i} (of 88) must succeed: {r:?}"));
        }
        assert_eq!(
            guard.count(),
            88,
            "after 88 successful acquires the counter must read 88"
        );
        let refusal = guard
            .acquire()
            .expect_err("the 89th acquire must be refused");
        let line = refusal.reason_line();
        assert!(
            line.contains("88"),
            "the reason line must name the call ceiling (88): {line}"
        );
        assert!(
            line.to_ascii_lowercase().contains("call"),
            "the reason line must name the CALL ceiling: {line}"
        );
        assert_eq!(
            guard.count(),
            88,
            "a refusal must not increment the counter"
        );
    }

    // -- T-11: one facet call per labelled session.

    #[tokio::test]
    async fn t11_one_facet_call_per_labelled_session() {
        for n in [1usize, 5] {
            let server = MockInferenceServer::start()
                .await
                .expect("mock should start");
            server.set_response(valid_facet_json());
            let client = client_for(&server).await;
            let digests: Vec<SessionDigest> = (0..n).map(|_| test_digest(Vec::new(), 1)).collect();
            let projections: Vec<String> = (0..n).map(|_| "x".to_string()).collect();
            let inputs: Vec<LabelInput> = digests
                .iter()
                .zip(projections.iter())
                .map(|(d, p)| LabelInput {
                    digest: d,
                    projection: p,
                })
                .collect();
            let outcome = label_sessions(
                &client,
                "test-model",
                &inputs,
                &ProjectionCaps::default(),
                &InsightsBudget::default(),
            )
            .await;
            assert_eq!(
                server.request_count_for("/v1/responses"),
                n,
                "one facet call per labelled session ({n} sessions)"
            );
            assert_eq!(
                outcome
                    .status
                    .iter()
                    .filter(|s| **s == LabelStatus::Labelled)
                    .count(),
                n,
                "all {n} sessions labelled"
            );
        }
    }

    // -- T-0(i): a failed facet reply is badged and the run continues.

    #[tokio::test]
    async fn t0i_a_failed_facet_reply_badges_and_the_run_continues() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&invalid_facet_json(), "test-model"),
        );
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = (0..3).map(|_| test_digest(Vec::new(), 1)).collect();
        let projections: Vec<String> = (0..3).map(|_| "x".to_string()).collect();
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            outcome.facet_validation_failure_sessions, 1,
            "exactly one session badged (no retry)"
        );
        assert_eq!(
            outcome.status,
            vec![
                LabelStatus::Labelled,
                LabelStatus::FacetValidationFailure,
                LabelStatus::Labelled
            ]
        );
        assert_eq!(outcome.sessions_analyzed, 3);
        assert_eq!(
            outcome.sessions_partial, 1,
            "only the failed session is partial"
        );
        assert_eq!(
            server.request_count_for("/v1/responses"),
            3,
            "the run continues past the failure (3 facet calls)"
        );
    }

    // -- T-0(j): the label cap badges the unlabelled remainder as a number.

    #[tokio::test]
    async fn t0j_the_label_cap_badges_the_unlabelled_remainder_as_a_number() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.set_response(valid_facet_json());
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = (0..5).map(|_| test_digest(Vec::new(), 1)).collect();
        let projections: Vec<String> = (0..5).map(|_| "x".to_string()).collect();
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let budget = InsightsBudget {
            max_sessions: 2,
            ..InsightsBudget::default()
        };
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &budget,
        )
        .await;
        assert_eq!(
            outcome.label_cap_sessions, 3,
            "label_cap_sessions == 3 (a number, not a drop)"
        );
        assert_eq!(
            outcome.status,
            vec![
                LabelStatus::Labelled,
                LabelStatus::Labelled,
                LabelStatus::LabelCap,
                LabelStatus::LabelCap,
                LabelStatus::LabelCap
            ]
        );
        assert_eq!(
            server.request_count_for("/v1/responses"),
            2,
            "only max_sessions facet calls are issued"
        );
    }

    // -- T-0(k): a cost crossing completes a partial run.

    #[tokio::test]
    async fn t0k_a_cost_crossing_completes_a_partial_run() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for _ in 0..5 {
            server.enqueue_response(
                "/v1/responses",
                cost_reply(&valid_facet_json(), "test-model", 60),
            );
        }
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = (0..5).map(|_| test_digest(Vec::new(), 1)).collect();
        let projections: Vec<String> = (0..5).map(|_| "x".to_string()).collect();
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let budget = InsightsBudget {
            max_cost_ticks: 100,
            ..InsightsBudget::default()
        };
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &budget,
        )
        .await;
        assert!(
            outcome.capped_by.contains(&CappedBy::Cost),
            "capped_by must include Cost: {:?}",
            outcome.capped_by
        );
        assert_eq!(
            outcome.stopped_before_label_sessions, 3,
            "the three never-labelled render badged"
        );
        assert_eq!(
            outcome.sessions_analyzed - outcome.sessions_partial,
            2,
            "clean = 2 (not 5)"
        );
        assert!(
            outcome
                .reason_line
                .as_deref()
                .unwrap()
                .contains("cost ceiling"),
            "the reason line names the cost ceiling: {:?}",
            outcome.reason_line
        );
        assert_eq!(
            server.request_count_for("/v1/responses"),
            2,
            "the run stops after the crossing session (2 facet calls)"
        );
        assert_eq!(outcome.total_cost_ticks, 120, "cost = 2 x 60 ticks");
    }

    // -- T-0(l): a capped-string facet reply is rejected (the same path as (i)).

    #[tokio::test]
    async fn t0l_a_capped_string_facet_reply_is_rejected_and_badged() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        // brief_summary exceeds the 160-char §7 cap (enforced by rejection, not
        // truncation) → the same badge path as (i).
        let mut v = valid_facet_value();
        v["brief_summary"] = json!("x".repeat(161));
        let over = v.to_string();
        server.enqueue_response("/v1/responses", sse_reply(&over, "test-model"));
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = (0..2).map(|_| test_digest(Vec::new(), 1)).collect();
        let projections: Vec<String> = (0..2).map(|_| "x".to_string()).collect();
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            outcome.facet_validation_failure_sessions, 1,
            "the over-cap reply is rejected and badged (same path as (i))"
        );
        assert_eq!(
            outcome.status,
            vec![LabelStatus::FacetValidationFailure, LabelStatus::Labelled]
        );
    }

    /// Run a one-session labelling wave against one scripted facet reply (the
    /// T-0(l) round-13 siblings): returns the outcome + the request count.
    async fn label_one_session(reply: &str) -> (LabelOutcome, usize) {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.enqueue_response("/v1/responses", sse_reply(reply, "test-model"));
        let client = client_for(&server).await;
        let digest = test_digest(Vec::new(), 1);
        let projection = "x".to_string();
        let input = LabelInput {
            digest: &digest,
            projection: &projection,
        };
        let outcome = label_sessions(
            &client,
            "test-model",
            std::slice::from_ref(&input),
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        (outcome, server.request_count_for("/v1/responses"))
    }

    // -- T-0(l) round 13: the other three capped facet strings, driven
    // through the labeller (the schema-side halves are the 7b `cap_*` tests).

    #[tokio::test]
    async fn t0l_friction_detail_over_the_240_cap_is_rejected_and_badged() {
        let reply = {
            let mut v = valid_facet_value();
            v["friction_detail"] = json!("x".repeat(241));
            v.to_string()
        };
        let (outcome, calls) = label_one_session(&reply).await;
        assert_eq!(
            outcome.facet_validation_failure_sessions, 1,
            "the 241-char friction_detail is rejected and badged (same path as (i))"
        );
        assert_eq!(outcome.status, vec![LabelStatus::FacetValidationFailure]);
        assert_eq!(calls, 1, "no retry: exactly one facet call");
    }

    #[tokio::test]
    async fn t0l_goal_categories_key_over_40_chars_is_rejected_and_badged() {
        let reply = {
            let mut v = valid_facet_value();
            let mut gc = serde_json::Map::new();
            gc.insert("a".repeat(41), json!(1));
            v["goal_categories"] = Value::Object(gc);
            v.to_string()
        };
        let (outcome, calls) = label_one_session(&reply).await;
        assert_eq!(
            outcome.facet_validation_failure_sessions, 1,
            "the 41-char goal_categories key is rejected and badged (same path as (i))"
        );
        assert_eq!(outcome.status, vec![LabelStatus::FacetValidationFailure]);
        assert_eq!(calls, 1, "no retry: exactly one facet call");
    }

    #[tokio::test]
    async fn t0l_a_ninth_languages_entry_is_rejected_and_badged() {
        let reply = {
            let mut v = valid_facet_value();
            v["languages"] = Value::Array((0..9).map(|i| json!(format!("lang{i}"))).collect());
            v.to_string()
        };
        let (outcome, calls) = label_one_session(&reply).await;
        assert_eq!(
            outcome.facet_validation_failure_sessions, 1,
            "the 9th languages entry is rejected and badged (same path as (i))"
        );
        assert_eq!(outcome.status, vec![LabelStatus::FacetValidationFailure]);
        assert_eq!(calls, 1, "no retry: exactly one facet call");
    }

    // -- T-0(c): InputTruncated consumes the marker (no chunk fan-out).

    #[tokio::test]
    async fn t0c_input_truncated_consumes_the_marker_no_chunk_fan_out() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.set_response(valid_facet_json());
        let client = client_for(&server).await;
        let caps = ProjectionCaps {
            max_input_chars: 60_000,
            ..ProjectionCaps::default()
        };
        let digests: Vec<SessionDigest> =
            vec![test_digest(vec![DegradedTier::InputTruncated], 90_000)];
        let projections: Vec<String> = vec!["x".repeat(90_000)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &caps,
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            server.request_count_for("/v1/responses"),
            1,
            "one facet call on the truncated projection, zero chunk calls"
        );
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
        // The truncation itself, pinned on the wire: the facet body's user
        // content is the 60 000-char truncated projection, not the 90 000-char
        // original.
        let entry = server
            .requests()
            .into_iter()
            .find(|e| e.path == "/v1/responses")
            .expect("the facet call is captured");
        let body = entry.body.expect("the facet call carries a JSON body");
        let user = body["input"]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["role"] == "user"))
            .expect("the facet body carries the user item");
        assert_eq!(
            user["content"]
                .as_str()
                .expect("the user content is a plain string")
                .chars()
                .count(),
            60_000,
            "the facet call must carry the TRUNCATED projection (max_input_chars 60 000)"
        );
    }

    // -- T-0(d): a ChunkCapHit makes at most the per-session chunk calls.

    #[tokio::test]
    async fn t0d_chunk_cap_hit_makes_at_most_the_per_session_chunk_calls() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        // 100 001 chars / 25 000 = 5 chunks → capped at 4; the facet carries the
        // summaries. 4 chunk calls + 1 facet call = 5 total.
        for _ in 0..4 {
            server.enqueue_response(
                "/v1/responses",
                sse_reply("a summary of this chunk", "test-model"),
            );
        }
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> =
            vec![test_digest(vec![DegradedTier::ChunkCapHit], 100_001)];
        let projections: Vec<String> = vec!["x".repeat(100_001)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            server.request_count_for("/v1/responses"),
            5,
            "4 chunk calls + 1 facet call (the per-session chunk ceiling binds)"
        );
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
    }

    // -- The threshold band (SOT §6:724): map-reduce kicks in above
    // `threshold_chars` (30 000), NOT above `chunk_chars` (25 000) — a
    // 25 001–30 000-char projection stays un-chunked and its facet call
    // carries the raw projection (§6:781).

    #[tokio::test]
    async fn t0_band_projections_below_the_threshold_stay_un_chunked() {
        // 27 000 chars: above chunk_chars (25 000), below threshold_chars
        // (30 000) → ONE facet call on the raw projection, zero chunk calls.
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.set_response(valid_facet_json());
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = vec![test_digest(Vec::new(), 27_000)];
        let projections: Vec<String> = vec!["x".repeat(27_000)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            server.request_count_for("/v1/responses"),
            1,
            "below the threshold the projection is un-chunked: one facet call, zero chunk calls"
        );
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
        assert!(
            outcome.capped_by.is_empty(),
            "no ceiling binds in this band: {:?}",
            outcome.capped_by
        );
        let body = server
            .requests()
            .into_iter()
            .find(|e| e.path == "/v1/responses")
            .expect("the facet call is captured")
            .body
            .expect("the facet call carries a JSON body");
        let user = body["input"]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["role"] == "user"))
            .expect("the facet body carries the user item");
        assert_eq!(
            user["content"]
                .as_str()
                .expect("the user content is a plain string")
                .chars()
                .count(),
            27_000,
            "the un-chunked facet carries the RAW projection"
        );
        // 30 001 chars: one char above the threshold → chunked: 2 chunk calls
        // + 1 facet call carrying the summaries.
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        server.enqueue_response("/v1/responses", sse_reply("a chunk summary", "test-model"));
        server.enqueue_response(
            "/v1/responses",
            sse_reply("another chunk summary", "test-model"),
        );
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = vec![test_digest(Vec::new(), 30_001)];
        let projections: Vec<String> = vec!["x".repeat(30_001)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(
            server.request_count_for("/v1/responses"),
            3,
            "one char above the threshold: 2 chunk calls + 1 facet call"
        );
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
    }

    // -- T-12(c): a cost crossing mid-facet-loop yields a partial run + reason.

    #[tokio::test]
    async fn t12c_a_cost_crossing_mid_facet_loop_yields_a_partial_run_and_reason_line() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for _ in 0..3 {
            server.enqueue_response(
                "/v1/responses",
                cost_reply(&valid_facet_json(), "test-model", 70),
            );
        }
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = (0..3).map(|_| test_digest(Vec::new(), 1)).collect();
        let projections: Vec<String> = (0..3).map(|_| "x".to_string()).collect();
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let budget = InsightsBudget {
            max_cost_ticks: 130,
            ..InsightsBudget::default()
        };
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &budget,
        )
        .await;
        assert!(
            outcome.capped_by.contains(&CappedBy::Cost),
            "capped_by must include Cost"
        );
        let line = outcome.reason_line.as_deref().unwrap();
        assert!(
            line.contains("cost ceiling"),
            "the reason line names the cost ceiling: {line}"
        );
        assert!(
            line.contains("140"),
            "the reason line names the accumulated figure (140): {line}"
        );
        assert_eq!(
            outcome.sessions_analyzed - outcome.sessions_partial,
            2,
            "2 clean (sessions 1, 2), 1 stopped (session 3)"
        );
    }

    // -- T-12(g): the runtime ceilings name themselves and set capped_by.

    #[tokio::test]
    async fn t12g_runtime_ceilings_name_themselves_and_set_capped_by() {
        // ChunkPerSession: one session needing 5 chunks, per-session cap 4.
        // (Shipped defaults tie the two chunk caps, so this arm also records
        // ChunkPerRun — the T-12(i) tie arm pins that explicitly.)
        {
            let server = MockInferenceServer::start()
                .await
                .expect("mock should start");
            // 4 chunk calls + 1 facet: the 5th response is the FACET's reply,
            // so it must be valid facet JSON (prose there would end the
            // session in FacetValidationFailure, which this arm does not pin).
            for _ in 0..4 {
                server
                    .enqueue_response("/v1/responses", sse_reply("a chunk summary", "test-model"));
            }
            server.enqueue_response(
                "/v1/responses",
                sse_reply(&valid_facet_json(), "test-model"),
            );
            let client = client_for(&server).await;
            let digests: Vec<SessionDigest> =
                vec![test_digest(vec![DegradedTier::ChunkCapHit], 100_001)];
            let projections: Vec<String> = vec!["x".repeat(100_001)];
            let inputs: Vec<LabelInput> = digests
                .iter()
                .zip(projections.iter())
                .map(|(d, p)| LabelInput {
                    digest: d,
                    projection: p,
                })
                .collect();
            let outcome = label_sessions(
                &client,
                "test-model",
                &inputs,
                &ProjectionCaps::default(),
                &InsightsBudget::default(),
            )
            .await;
            assert!(
                outcome.capped_by.contains(&CappedBy::ChunkPerSession),
                "ChunkPerSession must be set: {:?}",
                outcome.capped_by
            );
            let line = outcome.reason_line.as_deref().unwrap();
            assert!(
                line.contains("per-session"),
                "the reason line names the PER-SESSION ceiling: {line}"
            );
            assert_eq!(
                outcome.status,
                vec![LabelStatus::Labelled],
                "the capped session is still labelled (4 summaries + facet)"
            );
        }
        // ChunkPerRun: two sessions each needing 4 chunks, per-run cap 3.
        // A SEPARATED configuration (3 != 4 x 40): the one cap that bound is
        // recorded, and the room-0 session is badged StoppedBeforeLabel rather
        // than facet-called on the raw projection.
        {
            let server = MockInferenceServer::start()
                .await
                .expect("mock should start");
            // session 1: 3 chunks + 1 facet; session 2: the per-run ceiling
            // cuts its plan to 0 → StoppedBeforeLabel, NO facet = 4 requests.
            for _ in 0..3 {
                server
                    .enqueue_response("/v1/responses", sse_reply("a chunk summary", "test-model"));
            }
            server.enqueue_response(
                "/v1/responses",
                sse_reply(&valid_facet_json(), "test-model"),
            );
            let client = client_for(&server).await;
            let budget = InsightsBudget {
                max_chunk_calls_per_run: 3,
                ..InsightsBudget::default()
            };
            let digests: Vec<SessionDigest> =
                (0..2).map(|_| test_digest(Vec::new(), 100_000)).collect();
            let projections: Vec<String> = (0..2).map(|_| "x".repeat(100_000)).collect();
            let inputs: Vec<LabelInput> = digests
                .iter()
                .zip(projections.iter())
                .map(|(d, p)| LabelInput {
                    digest: d,
                    projection: p,
                })
                .collect();
            let outcome = label_sessions(
                &client,
                "test-model",
                &inputs,
                &ProjectionCaps::default(),
                &budget,
            )
            .await;
            assert!(
                outcome.capped_by.contains(&CappedBy::ChunkPerRun),
                "ChunkPerRun must be set: {:?}",
                outcome.capped_by
            );
            let line = outcome.reason_line.as_deref().unwrap();
            assert!(
                line.contains("per-run"),
                "the reason line names the PER-RUN ceiling: {line}"
            );
            assert_eq!(
                outcome.status,
                vec![LabelStatus::Labelled, LabelStatus::StoppedBeforeLabel],
                "the room-0 session is badged, not facet-called on the raw projection"
            );
            assert_eq!(
                outcome.stopped_before_label_sessions, 1,
                "the room-0 session counts in the stopped bucket"
            );
            assert_eq!(
                server.request_count_for("/v1/responses"),
                4,
                "3 chunk calls + 1 facet call (the zero-plan session issues no call)"
            );
            assert!(
                !outcome.capped_by.contains(&CappedBy::ChunkPerSession),
                "a separated configuration records the ONE cap that bound: {:?}",
                outcome.capped_by
            );
        }
        // Cost: names itself (covered by T-0(k)/T-12(c); re-asserted here).
        {
            let server = MockInferenceServer::start()
                .await
                .expect("mock should start");
            for _ in 0..2 {
                server.enqueue_response(
                    "/v1/responses",
                    cost_reply(&valid_facet_json(), "test-model", 60),
                );
            }
            let client = client_for(&server).await;
            let budget = InsightsBudget {
                max_cost_ticks: 100,
                ..InsightsBudget::default()
            };
            let digests: Vec<SessionDigest> = (0..2).map(|_| test_digest(Vec::new(), 1)).collect();
            let projections: Vec<String> = (0..2).map(|_| "x".to_string()).collect();
            let inputs: Vec<LabelInput> = digests
                .iter()
                .zip(projections.iter())
                .map(|(d, p)| LabelInput {
                    digest: d,
                    projection: p,
                })
                .collect();
            let outcome = label_sessions(
                &client,
                "test-model",
                &inputs,
                &ProjectionCaps::default(),
                &budget,
            )
            .await;
            assert!(
                outcome.capped_by.contains(&CappedBy::Cost),
                "Cost must be set: {:?}",
                outcome.capped_by
            );
            assert!(
                outcome
                    .reason_line
                    .as_deref()
                    .unwrap()
                    .contains("cost ceiling"),
                "the cost reason line names the cost ceiling: {:?}",
                outcome.reason_line
            );
        }
    }

    // -- T-12(i): at shipped defaults the tie records BOTH chunk caps.

    #[tokio::test]
    async fn t12i_at_shipped_defaults_a_truncating_run_records_both_chunk_caps() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        // 100 001 chars → 5 chunks → the per-session cap (4) truncates the
        // session. Shipped defaults: 4 × 40 == 160 ties the two chunk caps,
        // so a run that truncates records BOTH (SOT §6:728 — "a run that
        // truncates records both and says so").
        for _ in 0..4 {
            server.enqueue_response("/v1/responses", sse_reply("a chunk summary", "test-model"));
        }
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> =
            vec![test_digest(vec![DegradedTier::ChunkCapHit], 100_001)];
        let projections: Vec<String> = vec!["x".repeat(100_001)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert!(
            outcome.capped_by.contains(&CappedBy::ChunkPerSession),
            "ChunkPerSession must be set: {:?}",
            outcome.capped_by
        );
        assert!(
            outcome.capped_by.contains(&CappedBy::ChunkPerRun),
            "the tie records BOTH chunk caps (the default-configured user only
             ever sees this branch): {:?}",
            outcome.capped_by
        );
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
    }

    // -- T-12(h): facet AND chunk calls carry the shared output cap (4096).

    #[tokio::test]
    async fn t12h_facet_and_chunk_calls_carry_the_shared_output_cap() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        // 50 000 chars → 2 chunks (ceil(50 000 / 25 000) = 2); 2 chunk calls +
        // 1 facet call = 3.
        server.enqueue_response("/v1/responses", sse_reply("a chunk summary", "test-model"));
        server.enqueue_response(
            "/v1/responses",
            sse_reply("another chunk summary", "test-model"),
        );
        server.enqueue_response(
            "/v1/responses",
            sse_reply(&valid_facet_json(), "test-model"),
        );
        let client = client_for(&server).await;
        let digests: Vec<SessionDigest> = vec![test_digest(Vec::new(), 50_000)];
        let projections: Vec<String> = vec!["x".repeat(50_000)];
        let inputs: Vec<LabelInput> = digests
            .iter()
            .zip(projections.iter())
            .map(|(d, p)| LabelInput {
                digest: d,
                projection: p,
            })
            .collect();
        let outcome = label_sessions(
            &client,
            "test-model",
            &inputs,
            &ProjectionCaps::default(),
            &InsightsBudget::default(),
        )
        .await;
        assert_eq!(outcome.status, vec![LabelStatus::Labelled]);
        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        assert_eq!(responses.len(), 3, "2 chunk calls + 1 facet call");
        for entry in responses {
            let body = entry.body.expect("each call carries a JSON body");
            assert_eq!(
                body.get("max_output_tokens"),
                Some(&json!(4096)),
                "each facet/chunk call must carry the shared output cap (4096)"
            );
        }
    }
}
