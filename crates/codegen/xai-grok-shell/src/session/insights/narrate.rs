//! The narrate wave (SDD §4 :246; item 8b): the seven section calls
//! (bounded + concurrent, each a structured `with_json_schema` emission
//! against 7b's `SECTION_SCHEMAS`) + the at-a-glance second-wave call
//! (prose-ish, NO schema — the tenth prompt, not one of T-6(a)'s eight
//! structured outputs). The two-tier payload builder (T-9(d): the section
//! calls carry the facet-derived lines, NOT the raw transcript — the wave
//! fn's inputs have NO transcript / projection / digest field, so the
//! "none of the seven carries the transcript" half of T-9(d) holds by
//! construction). Section validation → the section replies +
//! `section_failures[]` with NO retry (attempts equal calls).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use xai_grok_sampler::SamplingClient;
use xai_grok_sampling_types::{
    ConversationItem, ConversationRequest, ConversationResponse, SamplingError,
};

use super::label::{CallGuard, SECTION_SCHEMAS, SessionFacets, validate_section_reply};
use super::types::{CappedBy, InsightsBudget, NarrateStats};
use super::{SECTION_PROMPTS, prompt};
use crate::session::workflow::schema_contract::contract_prompt;

/// A spawned section call: its `SECTION_PROMPTS` index + the join handle
/// over its `conversation_collect`.
type SectionCall = (
    usize,
    tokio::task::JoinHandle<std::result::Result<ConversationResponse, SamplingError>>,
);

/// The STRUCTURED section request (one per section, in `SECTION_PROMPTS`
/// order): the `contract_prompt` wrap of that section's ported prompt
/// alongside `with_json_schema` (the Departure 1 emission pair, §6
/// :678-697), the two-tier payload as the user item, the resolved model +
/// the shared output cap. NO `with_tools` / `with_tool_choice` — an
/// insights call wants the raw model output (§6 :1008-1020).
pub fn section_request(
    model: &str,
    max_tokens: u32,
    i: usize,
    session_data: &str,
) -> ConversationRequest {
    ConversationRequest::from_items(vec![
        ConversationItem::system(contract_prompt(
            prompt(SECTION_PROMPTS[i]),
            &SECTION_SCHEMAS[i](),
        )),
        ConversationItem::user(session_data),
    ])
    .with_model(model)
    .with_json_schema(SECTION_SCHEMAS[i]())
    .with_max_output_tokens(max_tokens)
}

/// The PROSE at-a-glance request (the tenth prompt — NOT one of T-6(a)'s
/// eight structured outputs, so NO `with_json_schema`; R-ataglace). The
/// spliced template (all seven placeholders replaced) is the system item.
pub fn at_a_glance_request(model: &str, max_tokens: u32, spliced: &str) -> ConversationRequest {
    ConversationRequest::from_items(vec![ConversationItem::system(spliced)])
        .with_model(model)
        .with_max_output_tokens(max_tokens)
}

/// Serialize a facet-enum value to its snake_case wire spelling (the brief
/// line renders `outcome` / `claude_helpfulness` in wire spelling, §7
/// :1113-1114).
fn wire<T: Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .expect("facet enum serializes")
        .as_str()
        .expect("facet enum serializes to a string")
        .to_string()
}

/// The two-tier `{SESSION_DATA}` text (SOT §7 :1096-1111) — the SAME text on
/// every section call's user item AND the at-a-glance's `{SESSION_DATA}`
/// splice:
/// 1. the aggregate half — the 13 keys, names verbatim (`commits` DROPPED —
///    nothing produces a commit count; `top_tools` / `top_goals` capped at
///    8 entries);
/// 2. the brief echo — at most `min(budget.max_sessions, 50)` lines, one per
///    labelled facet (`"- <brief_summary> (<outcome>, <helpfulness>)"`, wire
///    spelling);
/// 3. the friction echo — at most `min(budget.max_sessions, 20)` lines, one
///    per labelled facet (`"- <friction_detail>"`).
/// The two windows differ at the shipped default (40 and 20, R13-T17). NO
/// transcript access anywhere — T-9(d) holds by construction.
pub fn build_session_data(
    stats: &NarrateStats,
    facets: &[Option<SessionFacets>],
    budget: &InsightsBudget,
) -> String {
    let top_tools: Vec<(String, u32)> = stats.top_tools.iter().take(8).cloned().collect();
    let top_goals: Vec<(String, u32)> = stats.top_goals.iter().take(8).cloned().collect();
    let aggregate = json!({
        "sessions": stats.sessions,
        "analyzed": stats.analyzed,
        "date_range": stats.date_range,
        "messages": stats.messages,
        "hours": stats.hours,
        "cost_usd": stats.cost_usd,
        "top_tools": top_tools,
        "top_goals": top_goals,
        "outcomes": stats.outcomes,
        "satisfaction": stats.satisfaction,
        "friction": stats.friction,
        "success": stats.success,
        "languages": stats.languages,
    });
    let brief: Vec<String> = facets
        .iter()
        .flatten()
        .take(budget.max_sessions.min(50))
        .map(|f| {
            format!(
                "- {} ({}, {})",
                f.brief_summary,
                wire(f.outcome),
                wire(f.claude_helpfulness)
            )
        })
        .collect();
    let friction: Vec<String> = facets
        .iter()
        .flatten()
        .take(budget.max_sessions.min(20))
        .map(|f| format!("- {}", f.friction_detail))
        .collect();
    format!(
        "{}\n\nSESSION SUMMARIES:\n{}\n\nFRICTION DETAILS:\n{}",
        aggregate,
        brief.join("\n"),
        friction.join("\n")
    )
}

/// Splice all seven placeholders into the `insights_at_a_glance` template
/// (the file is the source of truth — 7a's T-9 pins them):
/// `{SESSION_DATA}` → the two-tier payload, `{PROJECT_AREAS}` → the
/// project_areas reply, `{USAGE_PATTERNS}` → the interaction_style reply,
/// `{BIG_WINS}` → the what_works reply, `{FRICTION_CATEGORIES}` → the
/// friction reply, `{FEATURES_TO_TRY}` → the suggestions reply,
/// `{ON_THE_HORIZON}` → the horizon reply. The `insights_fun_ending` reply
/// is NOT spliced (the template has no placeholder for it). A failed
/// section splices a short explicit marker naming the missing section —
/// never a silent empty, never raw data. No raw `{UPPERCASE}` placeholder
/// may survive into the sent text.
pub fn splice_at_a_glance(session_data: &str, replies: &[Option<Value>]) -> String {
    let rendered: Vec<String> = replies
        .iter()
        .zip(SECTION_PROMPTS.iter())
        .map(|(reply, name)| {
            reply
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| format!("[{name} unavailable]"))
        })
        .collect();
    // `replies` is in `SECTION_PROMPTS` order: [project_areas,
    // interaction_style, what_works, friction, suggestions, horizon,
    // fun_ending] — indices 0-6 (the fun_ending at 6 is not spliced).
    let mut text = prompt("insights_at_a_glance").to_string();
    text = text.replace("{SESSION_DATA}", session_data);
    text = text.replace("{PROJECT_AREAS}", &rendered[0]);
    text = text.replace("{USAGE_PATTERNS}", &rendered[1]);
    text = text.replace("{BIG_WINS}", &rendered[2]);
    text = text.replace("{FRICTION_CATEGORIES}", &rendered[3]);
    text = text.replace("{FEATURES_TO_TRY}", &rendered[4]);
    text = text.replace("{ON_THE_HORIZON}", &rendered[5]);
    text
}

/// The at-a-glance reply (R-ataglace): the four fields of the template's
/// `RESPOND WITH ONLY A VALID JSON OBJECT` block (the file is the source of
/// truth). NO `deny_unknown_fields` — the lenient parse tolerates extra
/// keys.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AtAGlance {
    pub whats_working: String,
    pub whats_hindering: String,
    pub quick_wins: String,
    pub ambitious_workflows: String,
}

/// Parse the at-a-glance reply LENIENTLY (R-ataglace): the first three
/// candidates of the contract validator's `validate_contract_output` — the
/// last ```json fence interior, the whole trimmed text, then the outermost
/// `{` … `}` span (the validator's fourth candidate, an outermost `[` … `]`
/// span, is inapplicable to the 4-field object) — and the first candidate
/// that deserializes wins.
pub fn parse_at_a_glance(text: &str) -> Result<AtAGlance, String> {
    let text = text.trim();
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(start) = text.rfind("```json") {
        let body = &text[start + "```json".len()..];
        if let Some(end) = body.find("```") {
            candidates.push(body[..end].trim());
        }
    }
    candidates.push(text);
    if let (Some(open), Some(close)) = (text.find('{'), text.rfind('}'))
        && open < close
    {
        candidates.push(&text[open..=close]);
    }
    let mut last_err = String::new();
    for candidate in candidates {
        match serde_json::from_str::<AtAGlance>(candidate) {
            Ok(parsed) => return Ok(parsed),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!(
        "at-a-glance reply did not contain a 4-field JSON object: {last_err}"
    ))
}

/// The class of a failed narrate call (one entry per failure — NO retry):
/// a transport error, a schema/parse validation failure, or a ceiling stop
/// (a cost/call ceiling bound the wave before or at that call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrateFailureClass {
    Transport,
    Validation,
    CeilingStopped,
}

/// One `section_failures[]` entry: the section prompt stem + the failure
/// class (8c's telemetry record copies these into `section_failures[]` —
/// a copy, not a recompute).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionFailure {
    pub section: String,
    pub class: NarrateFailureClass,
}

/// The result of the narrate wave (8c's orchestrator assembles this into
/// the `insights_generated` telemetry record): the seven section reply
/// slots in `SECTION_PROMPTS` order, the section failures, the at-a-glance
/// (or its failure class), the cost figure (offset-aware from
/// `start_cost_ticks`), the ceilings + the reason line, and the call count.
#[derive(Debug)]
pub struct NarrateOutcome {
    /// The seven section replies, in `SECTION_PROMPTS` order — `None` for a
    /// failed or unissued section.
    pub section_replies: Vec<Option<Value>>,
    /// One entry per failed section (transport / validation /
    /// ceiling-stopped), in `SECTION_PROMPTS` order.
    pub section_failures: Vec<SectionFailure>,
    /// The parsed at-a-glance (the second wave) — `None` on a parse /
    /// transport failure or a ceiling stop.
    pub at_a_glance: Option<AtAGlance>,
    /// The at-a-glance failure class (set exactly when `at_a_glance` is
    /// `None`).
    pub at_a_glance_failure: Option<NarrateFailureClass>,
    /// The run's accumulated cost in USD ticks (`start_cost_ticks` + the
    /// wave's own accumulation — the offset-aware figure 8c's orchestrator
    /// continues).
    pub total_cost_ticks: i64,
    /// True iff some calls were priced and some unpriced (the §9
    /// partial-cost badge — `some_priced && some_unpriced`, the 8a rule).
    pub cost_is_partial: bool,
    /// The sorted, deduped ceilings that bound this wave.
    pub capped_by: Vec<CappedBy>,
    /// The first ceiling that bound the wave, named (one line) — for the
    /// footer.
    pub reason_line: Option<String>,
    /// The model calls this wave actually issued (attempts equal calls — no
    /// retry).
    pub calls: usize,
}

/// Fold one call's reported cost into the accumulator (the same 4-line
/// shape as 8a's private `accumulate_cost` — no cross-module refactor):
/// `Some` is priced (adds to the total), `None` is unpriced (the
/// accumulator stays).
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

/// The narrate wave (item 8b): the seven section calls — bounded (≤
/// `budget.concurrency` in flight) + concurrent, each guarded by the
/// wave-local `CallGuard::new(budget.max_calls)` — then the at-a-glance
/// second-wave call, issued ONLY after all seven have finished (the last of
/// the eight in the mock's request log). The cost checkpoint is
/// offset-aware from `start_cost_ticks` (the label wave's accumulated cost;
/// the overshoot bound is ≤ `concurrency` in-flight calls, §6 :893-938). A
/// ceiling stop is a COMPLETED PARTIAL WAVE, not an abort: the replies
/// already validated are kept, the unissued sections are ceiling-stopped,
/// the at-a-glance is not issued. NO retry anywhere (attempts equal calls):
/// a failed section / at-a-glance records one failure entry and the wave
/// continues.
pub async fn narrate(
    client: &SamplingClient,
    model: &str,
    stats: &NarrateStats,
    facets: &[Option<SessionFacets>],
    start_cost_ticks: i64,
    budget: &InsightsBudget,
) -> NarrateOutcome {
    let session_data = build_session_data(stats, facets, budget);
    let mut guard = CallGuard::new(budget.max_calls);
    let mut capped_by: BTreeSet<CappedBy> = BTreeSet::new();
    let mut reason_line: Option<String> = None;
    let mut total_cost_ticks = start_cost_ticks;
    let mut some_priced = false;
    let mut some_unpriced = false;
    let mut calls = 0usize;
    let mut wave_stopped = false;

    let section_count = SECTION_PROMPTS.len();
    let mut section_replies: Vec<Option<Value>> = vec![None; section_count];
    let mut section_class: [Option<NarrateFailureClass>; SECTION_PROMPTS.len()] =
        [None; SECTION_PROMPTS.len()];

    // Wave 1: the seven section calls, in batches of at most
    // `budget.concurrency` (the in-flight bound).
    let batch_size = budget.concurrency.max(1);
    for start in (0..section_count).step_by(batch_size) {
        if wave_stopped {
            break;
        }
        let mut batch: Vec<SectionCall> = Vec::new();
        for i in start..section_count.min(start + batch_size) {
            if let Err(refusal) = guard.acquire() {
                capped_by.insert(CappedBy::Calls);
                reason_line.get_or_insert(refusal.reason_line());
                wave_stopped = true;
                break;
            }
            calls += 1;
            let client = client.clone();
            let model = model.to_string();
            let session_data = session_data.clone();
            let max_tokens = budget.max_tokens;
            batch.push((
                i,
                tokio::spawn(async move {
                    let request = section_request(&model, max_tokens, i, &session_data);
                    client.conversation_collect(request).await
                }),
            ));
        }
        for (i, handle) in batch {
            match handle.await.expect("a section task does not panic") {
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
                        wave_stopped = true;
                    }
                    match validate_section_reply(SECTION_PROMPTS[i], &response.assistant_text()) {
                        Ok(value) => section_replies[i] = Some(value),
                        Err(_) => section_class[i] = Some(NarrateFailureClass::Validation),
                    }
                }
                Err(_) => section_class[i] = Some(NarrateFailureClass::Transport),
            }
        }
    }
    // Unissued sections (a stop bound before they were issued) are
    // ceiling-stopped — the completed-partial-wave bookkeeping: the
    // validated replies are kept, the rest get a named failure.
    for i in 0..section_count {
        if section_replies[i].is_none() && section_class[i].is_none() {
            section_class[i] = Some(NarrateFailureClass::CeilingStopped);
        }
    }
    let section_failures: Vec<SectionFailure> = (0..section_count)
        .filter_map(|i| {
            section_class[i].map(|class| SectionFailure {
                section: SECTION_PROMPTS[i].to_string(),
                class,
            })
        })
        .collect();

    // Wave 2: the at-a-glance — issued ONLY after all seven have finished
    // (the two-wave structure, §7 :1116-1118), with all seven placeholders
    // spliced from the first wave's replies.
    let (at_a_glance, at_a_glance_failure) = if wave_stopped {
        (None, Some(NarrateFailureClass::CeilingStopped))
    } else {
        let guard_ok = match guard.acquire() {
            Ok(()) => true,
            Err(refusal) => {
                capped_by.insert(CappedBy::Calls);
                reason_line.get_or_insert(refusal.reason_line());
                false
            }
        };
        if !guard_ok {
            (None, Some(NarrateFailureClass::CeilingStopped))
        } else {
            calls += 1;
            let spliced = splice_at_a_glance(&session_data, &section_replies);
            match client
                .conversation_collect(at_a_glance_request(model, budget.max_tokens, &spliced))
                .await
            {
                Ok(response) => {
                    accumulate_cost(
                        &mut total_cost_ticks,
                        &mut some_priced,
                        &mut some_unpriced,
                        response.cost_usd_ticks,
                    );
                    // The same post-call ceiling check as wave 1 and 8a — the
                    // wave's FINAL call can cross the ceiling too. No stop
                    // flag: the wave is over, so this is bookkeeping for 8c's
                    // run-level capped_by / reason line, not a cutoff.
                    if total_cost_ticks > budget.max_cost_ticks {
                        capped_by.insert(CappedBy::Cost);
                        reason_line.get_or_insert(format!(
                            "cost ceiling reached: {total_cost_ticks} ticks accumulated (ceiling {})",
                            budget.max_cost_ticks
                        ));
                    }
                    match parse_at_a_glance(&response.assistant_text()) {
                        Ok(parsed) => (Some(parsed), None),
                        Err(_) => (None, Some(NarrateFailureClass::Validation)),
                    }
                }
                Err(_) => (None, Some(NarrateFailureClass::Transport)),
            }
        }
    };

    NarrateOutcome {
        section_replies,
        section_failures,
        at_a_glance,
        at_a_glance_failure,
        total_cost_ticks,
        cost_is_partial: some_priced && some_unpriced,
        capped_by: capped_by.into_iter().collect(),
        reason_line,
        calls,
    }
}

#[cfg(test)]
mod tests {
    //! Item 8b's narrate-wave arms (SDD §12): the T-9(d) section + window
    //! sub-arms, the T-12(h) section half, the section-validation arm, the
    //! two-wave coherence arm, the cost-checkpoint arm and the call-count
    //! arm. The mock is `xai-grok-test-support`'s `MockInferenceServer`
    //! (8a's test module in `label.rs` is the established pattern); the
    //! calls go through a real `SamplingClient` (Responses backend) so the
    //! wire shape is the production one. All run under the
    //! `session::insights` filter.

    use super::{NarrateFailureClass, narrate, parse_at_a_glance};
    use crate::session::insights::SECTION_PROMPTS;
    use crate::session::insights::label::{SECTION_SCHEMAS, SessionFacets};
    use crate::session::insights::types::{
        CappedBy, Friction, Helpfulness, InsightsBudget, NarrateStats, Outcome, Satisfaction,
        SessionType, SuccessMode,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use xai_grok_sampler::{ApiBackend, SamplerConfig, SamplingClient};
    use xai_grok_test_support::mock_server::MockInferenceServer;
    use xai_grok_test_support::scripted::{ScriptedResponse, SseEvent};
    use xai_grok_test_support::sse::responses_api_script_exact;

    const MODEL: &str = "test-model";

    // -- Mock plumbing (the 8a pattern, copied).

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

    /// A schema-valid reply for section `i` (the `SECTION_PROMPTS` order),
    /// carrying the distinctive marker `ZebraForge-<i>` in a required string
    /// slot — the two-wave coherence arm asserts which markers reach the
    /// at-a-glance splice.
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

    fn test_facets(n: usize) -> Vec<Option<SessionFacets>> {
        (0..n)
            .map(|i| {
                Some(SessionFacets {
                    cache_version: 1,
                    transcript_sha256: "0".repeat(64),
                    session_id: format!("sess-{i}"),
                    underlying_goal: format!("Goal {i}"),
                    goal_categories: BTreeMap::new(),
                    outcome: Outcome::FullyAchieved,
                    user_satisfaction_counts: BTreeMap::new(),
                    claude_helpfulness: Helpfulness::VeryHelpful,
                    session_type: SessionType::SingleTask,
                    friction_counts: BTreeMap::new(),
                    friction_detail: format!("Friction detail {i}"),
                    primary_success: SuccessMode::None,
                    brief_summary: format!("Brief summary {i}"),
                    languages: Vec::new(),
                })
            })
            .collect()
    }

    fn test_stats() -> NarrateStats {
        NarrateStats {
            sessions: 45,
            analyzed: 40,
            date_range: "2026-08-01 .. 2026-09-30".to_string(),
            messages: 1234,
            hours: 12.5,
            cost_usd: 3.14,
            // 9 entries each: the payload must cap at 8 (tool7 in, tool8 out).
            top_tools: (0..9).map(|i| (format!("tool{i}"), 9 - i as u32)).collect(),
            top_goals: (0..9)
                .map(|i| (format!("goalcat{i}"), 9 - i as u32))
                .collect(),
            outcomes: BTreeMap::from([(Outcome::FullyAchieved, 30u32)]),
            satisfaction: BTreeMap::from([(Satisfaction::Satisfied, 30u32)]),
            friction: BTreeMap::from([(Friction::ToolFailed, 5u32)]),
            success: BTreeMap::from([(SuccessMode::GoodDebugging, 7u32)]),
            languages: BTreeMap::from([("rust".to_string(), 20u32)]),
        }
    }

    /// Script the seven section replies + the at-a-glance reply, all valid.
    fn script_clean_replies(server: &MockInferenceServer) {
        for i in 0..SECTION_PROMPTS.len() {
            server.enqueue_response("/v1/responses", sse_reply(&valid_section_reply(i)));
        }
        server.enqueue_response("/v1/responses", sse_reply(&valid_glance_reply()));
    }

    /// The sequential budget: the scripted-reply content arms run at
    /// concurrency = 1 so the mock's reply-to-section assignment stays
    /// deterministic (R-mock-8b).
    fn sequential_budget() -> InsightsBudget {
        InsightsBudget {
            concurrency: 1,
            ..InsightsBudget::default()
        }
    }

    // -- Request-log readers.

    /// The plain-string content of the `input` item carrying `role`.
    fn item_content(body: &Value, role: &str) -> String {
        body["input"]
            .as_array()
            .expect("body carries an input array")
            .iter()
            .find(|i| i["role"] == json!(role))
            .expect("an item with the role exists")
            .get("content")
            .and_then(Value::as_str)
            .expect("the item content is a plain string")
            .to_string()
    }

    /// Count the `- ` bullet lines between `header` and the next window
    /// header (or the end of the text).
    fn count_window_lines(text: &str, header: &str, next: Option<&str>) -> usize {
        let start = text
            .find(header)
            .unwrap_or_else(|| panic!("payload missing the {header:?} header"));
        let rest = &text[start + header.len()..];
        let rest = match next {
            Some(n) => rest.find(n).map(|i| &rest[..i]).unwrap_or(rest),
            None => rest,
        };
        rest.lines().filter(|l| l.starts_with("- ")).count()
    }

    // -- T-9(d) section sub-arm: every section request AND the at-a-glance
    // request carry the two-tier payload (the facet-derived lines), the 13
    // key names verbatim, `commits` DROPPED, top_tools capped at 8.

    #[tokio::test]
    async fn t9d_every_section_and_the_glance_carry_the_two_tier_payload() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        script_clean_replies(&server);
        let client = client_for(&server).await;
        let facets = test_facets(3);
        let stats = test_stats();
        let outcome = narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        assert_eq!(responses.len(), 8, "7 section calls + 1 at-a-glance");
        for (idx, entry) in responses.iter().enumerate() {
            let body = entry.body.as_ref().expect("each call carries a JSON body");
            // The seven section calls carry the payload on their user item;
            // the at-a-glance (the last) carries it via the {SESSION_DATA}
            // splice in its system item.
            let text = if idx < 7 {
                item_content(body, "user")
            } else {
                item_content(body, "system")
            };
            for i in 0..3 {
                let brief = format!("- Brief summary {i} (fully_achieved, very_helpful)");
                assert!(
                    text.contains(&brief),
                    "request {idx} lost the brief line: {brief}"
                );
                let friction = format!("- Friction detail {i}");
                assert!(
                    text.contains(&friction),
                    "request {idx} lost the friction line: {friction}"
                );
            }
        }
        // The 13 key names verbatim on a section call's user item;
        // `commits` DROPPED; top_tools capped at 8 (tool7 in, tool8 out).
        let user0 = item_content(&responses[0].body.as_ref().unwrap(), "user");
        for key in [
            "sessions",
            "analyzed",
            "date_range",
            "messages",
            "hours",
            "cost_usd",
            "top_tools",
            "top_goals",
            "outcomes",
            "satisfaction",
            "friction",
            "success",
            "languages",
        ] {
            assert!(
                user0.contains(key),
                "the aggregate half must name {key:?} verbatim"
            );
        }
        assert!(
            !user0.contains("\"commits\""),
            "commits is DROPPED from the two-tier payload"
        );
        assert!(
            user0.contains("tool7"),
            "top_tools must carry its 8th entry (tool7)"
        );
        assert!(
            !user0.contains("tool8"),
            "top_tools must cap at 8 entries (tool8 is the 9th, dropped)"
        );
        assert_eq!(
            outcome
                .at_a_glance
                .as_ref()
                .expect("clean glance")
                .whats_working,
            "wg"
        );
    }

    // -- T-9(d) window sub-arm (R13-T17): at the shipped default
    // (max_sessions = 40) the echo holds exactly 40 brief lines and exactly
    // 20 friction lines — NOT both 40.

    #[tokio::test]
    async fn t9d_window_the_brief_and_friction_echoes_are_40_and_20_at_the_default() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        script_clean_replies(&server);
        let client = client_for(&server).await;
        let facets = test_facets(45); // > 40: the windows bind
        let stats = test_stats();
        narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        let user0 = item_content(&responses[0].body.as_ref().unwrap(), "user");
        assert_eq!(
            count_window_lines(&user0, "SESSION SUMMARIES:", Some("FRICTION DETAILS:")),
            40,
            "the brief echo holds min(max_sessions, 50) = 40 lines"
        );
        assert_eq!(
            count_window_lines(&user0, "FRICTION DETAILS:", None),
            20,
            "the friction echo holds min(max_sessions, 20) = 20 lines — NOT both 40"
        );
        assert!(
            user0.contains("Brief summary 39"),
            "the 40th brief line (index 39) is in"
        );
        assert!(
            !user0.contains("Brief summary 40"),
            "the 41st brief line (index 40) is out"
        );
        assert!(
            user0.contains("Friction detail 19"),
            "the 20th friction line (index 19) is in"
        );
        assert!(
            !user0.contains("Friction detail 20"),
            "the 21st friction line (index 20) is out"
        );
    }

    // -- T-12(h) section half: the section calls AND the at-a-glance body
    // carry the shared output cap (4096); the seven sections emit
    // with_json_schema (text.format.type = "json_schema", schema = that
    // section's), the at-a-glance carries NO `text` field; no tools on
    // either.

    #[tokio::test]
    async fn t12h_section_and_glance_bodies_carry_the_cap_and_the_emission_shape() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        script_clean_replies(&server);
        let client = client_for(&server).await;
        let facets = test_facets(1);
        let stats = test_stats();
        narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        assert_eq!(responses.len(), 8);
        let section0 = responses[0].body.as_ref().expect("body");
        assert_eq!(
            section0.get("max_output_tokens"),
            Some(&json!(4096)),
            "the section call must carry the shared output cap (4096)"
        );
        assert_eq!(
            section0
                .get("text")
                .and_then(|t| t.get("format"))
                .and_then(|f| f.get("type")),
            Some(&json!("json_schema")),
            "the section call is a structured emission (with_json_schema)"
        );
        assert_eq!(
            section0
                .get("text")
                .and_then(|t| t.get("format"))
                .and_then(|f| f.get("schema")),
            Some(&SECTION_SCHEMAS[0]()),
            "the section call carries its own section schema"
        );
        assert!(
            section0.get("tools").is_none(),
            "no tools on an insights call"
        );
        let glance = responses[7].body.as_ref().expect("body");
        assert_eq!(
            glance.get("max_output_tokens"),
            Some(&json!(4096)),
            "the at-a-glance call must carry the shared output cap (4096)"
        );
        assert!(
            glance.get("text").is_none(),
            "the at-a-glance is the tenth prompt: NO with_json_schema"
        );
        assert!(
            glance.get("tools").is_none(),
            "no tools on an insights call"
        );
    }

    // -- The section-validation arm (the 8b analogue of 8a's T-0(i)): one
    // schema-invalid reply (insights_friction) → that slot None + exactly
    // one section_failures entry + the other six validate + the at-a-glance
    // still runs with the explicit failure marker + NO retry (8 calls).

    #[tokio::test]
    async fn section_validation_failure_records_one_entry_and_the_glance_still_runs() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for i in 0..SECTION_PROMPTS.len() {
            let reply = if i == 3 {
                // insights_friction: the `categories` array is missing —
                // schema-invalid.
                json!({ "intro": "ZebraForge-3" }).to_string()
            } else {
                valid_section_reply(i)
            };
            server.enqueue_response("/v1/responses", sse_reply(&reply));
        }
        server.enqueue_response("/v1/responses", sse_reply(&valid_glance_reply()));
        let client = client_for(&server).await;
        let facets = test_facets(2);
        let stats = test_stats();
        let outcome = narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        assert!(
            outcome.section_replies[3].is_none(),
            "the failed section's slot is None"
        );
        let validated = outcome
            .section_replies
            .iter()
            .filter(|r| r.is_some())
            .count();
        assert_eq!(validated, 6, "the other six validate");
        assert_eq!(outcome.section_failures.len(), 1);
        assert_eq!(outcome.section_failures[0].section, SECTION_PROMPTS[3]);
        assert_eq!(
            outcome.section_failures[0].class,
            NarrateFailureClass::Validation
        );
        assert!(
            outcome.at_a_glance.is_some(),
            "the at-a-glance still runs after a section failure"
        );
        assert_eq!(
            server.request_count_for("/v1/responses"),
            8,
            "NO retry: exactly 7 sections + 1 at-a-glance"
        );
        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        let glance_text = item_content(&responses[7].body.as_ref().unwrap(), "system");
        assert!(
            glance_text.contains(SECTION_PROMPTS[3]),
            "the failure marker names the missing section"
        );
        assert!(
            glance_text.to_ascii_lowercase().contains("unavailable"),
            "the failure marker is explicit, not a silent empty"
        );
        assert!(
            !glance_text.contains("{FRICTION_CATEGORIES}"),
            "no raw placeholder survives the splice"
        );
    }

    // -- Two-wave coherence (§7 :1116-1118): the at-a-glance sees the first
    // wave's bullets (every spliced section reply's marker reaches it; the
    // unspliced fun_ending marker does not) and is the LAST of the eight in
    // the log — with no raw {UPPERCASE} placeholder surviving.

    #[tokio::test]
    async fn two_wave_the_glance_sees_the_first_wave_bullets_and_is_last() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        script_clean_replies(&server);
        let client = client_for(&server).await;
        let facets = test_facets(1);
        let stats = test_stats();
        narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        assert_eq!(responses.len(), 8);
        // The seven sections are structured; the at-a-glance (last) is not.
        for entry in responses[..7].iter() {
            let body = entry.body.as_ref().unwrap();
            assert_eq!(
                body.get("text")
                    .and_then(|t| t.get("format"))
                    .and_then(|f| f.get("type")),
                Some(&json!("json_schema")),
                "each of the seven sections is a structured emission"
            );
        }
        assert!(
            responses[7].body.as_ref().unwrap().get("text").is_none(),
            "the at-a-glance (the LAST of the eight) carries no schema"
        );
        let glance_text = item_content(&responses[7].body.as_ref().unwrap(), "system");
        for i in 0..6 {
            let marker = format!("ZebraForge-{i}");
            assert!(
                glance_text.contains(&marker),
                "the at-a-glance must see the first wave's {marker} bullets"
            );
        }
        assert!(
            !glance_text.contains("ZebraForge-6"),
            "the fun_ending reply is NOT spliced (no placeholder for it)"
        );
        let placeholder = regex::Regex::new(r"\{[A-Z0-9_]+\}").unwrap();
        assert!(
            placeholder.find(&glance_text).is_none(),
            "no raw {{UPPERCASE}} placeholder may survive into the sent text"
        );
    }

    // -- Cost-checkpoint arm (the 8b half of the run-level stop):
    // start_cost_ticks just below the ceiling, the cost feed crossing it
    // after the first call → the wave COMPLETES AS A PARTIAL (no abort):
    // the first reply is kept, the unissued sections are ceiling-stopped,
    // the at-a-glance is NOT issued, capped_by includes Cost, the reason
    // line names the ceiling + the figure.

    #[tokio::test]
    async fn cost_checkpoint_a_mid_wave_crossing_completes_a_partial_wave() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for i in 0..SECTION_PROMPTS.len() {
            server.enqueue_response("/v1/responses", cost_reply(&valid_section_reply(i), 10));
        }
        server.enqueue_response("/v1/responses", cost_reply(&valid_glance_reply(), 10));
        let client = client_for(&server).await;
        let facets = test_facets(1);
        let stats = test_stats();
        let budget = InsightsBudget {
            concurrency: 1,
            max_cost_ticks: 100,
            ..InsightsBudget::default()
        };
        let outcome = narrate(&client, MODEL, &stats, &facets, 95, &budget).await;

        assert_eq!(
            server.request_count_for("/v1/responses"),
            1,
            "the stop fires after the first call: only it issued"
        );
        assert_eq!(outcome.calls, 1);
        assert!(
            outcome.section_replies[0].is_some(),
            "the reply already validated is kept (a completed partial wave, not an abort)"
        );
        for i in 1..7 {
            assert!(outcome.section_replies[i].is_none());
        }
        assert_eq!(outcome.section_failures.len(), 6);
        for f in &outcome.section_failures {
            assert_eq!(f.class, NarrateFailureClass::CeilingStopped);
        }
        assert_eq!(
            outcome.section_failures[0].section, SECTION_PROMPTS[1],
            "the first unissued section is index 1"
        );
        assert!(outcome.at_a_glance.is_none());
        assert_eq!(
            outcome.at_a_glance_failure,
            Some(NarrateFailureClass::CeilingStopped)
        );
        assert!(
            outcome.capped_by.contains(&CappedBy::Cost),
            "the cost ceiling bound the wave"
        );
        let reason = outcome
            .reason_line
            .as_deref()
            .expect("one reason line names the ceiling + the figure");
        assert!(
            reason.contains("cost ceiling"),
            "the reason line names the ceiling: {reason}"
        );
        assert!(
            reason.contains("105"),
            "the reason line names the figure: {reason}"
        );
        assert!(
            reason.contains("100"),
            "the reason line names the bound: {reason}"
        );
        assert_eq!(outcome.total_cost_ticks, 105);
    }

    // -- Cost-checkpoint arm, wave-2 half: start just below the ceiling,
    // every wave-1 call priced 0 (the total never crosses mid-wave), the
    // at-a-glance reply crossing it → the crossing on the wave's FINAL call
    // still caps the wave: capped_by includes Cost, the reason line names
    // the figure, and the parsed at-a-glance is KEPT (a cap on the last
    // call is bookkeeping, not a stop — the wave is over).

    #[tokio::test]
    async fn cost_checkpoint_a_final_call_crossing_caps_the_wave() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for i in 0..SECTION_PROMPTS.len() {
            server.enqueue_response("/v1/responses", cost_reply(&valid_section_reply(i), 0));
        }
        server.enqueue_response("/v1/responses", cost_reply(&valid_glance_reply(), 10));
        let client = client_for(&server).await;
        let facets = test_facets(1);
        let stats = test_stats();
        let budget = InsightsBudget {
            concurrency: 1,
            max_cost_ticks: 100,
            ..InsightsBudget::default()
        };
        let outcome = narrate(&client, MODEL, &stats, &facets, 95, &budget).await;

        assert_eq!(
            server.request_count_for("/v1/responses"),
            8,
            "all eight calls issue (no stop — the wave is already over)"
        );
        assert_eq!(outcome.total_cost_ticks, 105);
        assert!(
            outcome.capped_by.contains(&CappedBy::Cost),
            "a crossing on the final call still caps the wave"
        );
        let reason = outcome
            .reason_line
            .as_deref()
            .expect("one reason line names the ceiling + the figure");
        assert!(
            reason.contains("cost ceiling"),
            "the reason line names the ceiling: {reason}"
        );
        assert!(
            reason.contains("105"),
            "the reason line names the figure: {reason}"
        );
        assert!(
            reason.contains("100"),
            "the reason line names the bound: {reason}"
        );
        assert!(
            outcome.section_replies.iter().all(|r| r.is_some()),
            "wave 1 completed in budget: all seven replies kept"
        );
        assert!(
            outcome.at_a_glance.is_some(),
            "the crossing is on the last call: the parsed at-a-glance is kept"
        );
        assert!(outcome.at_a_glance_failure.is_none());
    }

    // -- Call-count arm: a clean run issues exactly 7 section calls + 1
    // at-a-glance = 8 (attempts equal calls — no retry anywhere).

    #[tokio::test]
    async fn call_count_a_clean_run_issues_exactly_eight_calls() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        script_clean_replies(&server);
        let client = client_for(&server).await;
        let facets = test_facets(2);
        let stats = test_stats();
        let outcome = narrate(&client, MODEL, &stats, &facets, 0, &sequential_budget()).await;

        assert_eq!(server.request_count_for("/v1/responses"), 8);
        assert_eq!(outcome.calls, 8);
        assert_eq!(
            outcome
                .section_replies
                .iter()
                .filter(|r| r.is_some())
                .count(),
            7
        );
        assert!(outcome.at_a_glance.is_some());
        assert!(outcome.section_failures.is_empty());
        assert!(outcome.at_a_glance_failure.is_none());
        assert!(outcome.capped_by.is_empty());
        assert!(outcome.reason_line.is_none());
        assert_eq!(outcome.total_cost_ticks, 0);
        assert!(!outcome.cost_is_partial);
    }

    // -- R-mock-8b full-concurrency arm: at the shipped bound (concurrency =
    // 8) only the call shape is asserted — 7 section + 1 at-a-glance, the
    // at-a-glance LAST — with the replies scripted to fail (reply order
    // then irrelevant).

    #[tokio::test]
    async fn full_concurrency_the_wave_shape_is_seven_sections_then_one_glance() {
        let server = MockInferenceServer::start()
            .await
            .expect("mock should start");
        for _ in 0..8 {
            server.enqueue_response("/v1/responses", sse_reply("not a reply at all"));
        }
        let client = client_for(&server).await;
        let facets = test_facets(1);
        let stats = test_stats();
        let outcome = narrate(
            &client,
            MODEL,
            &stats,
            &facets,
            0,
            &InsightsBudget::default(),
        )
        .await;

        assert_eq!(server.request_count_for("/v1/responses"), 8);
        assert_eq!(outcome.calls, 8);
        let responses: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|e| e.path == "/v1/responses")
            .collect();
        for entry in responses[..7].iter() {
            let body = entry.body.as_ref().unwrap();
            assert_eq!(
                body.get("text")
                    .and_then(|t| t.get("format"))
                    .and_then(|f| f.get("type")),
                Some(&json!("json_schema")),
                "each of the seven sections is a structured emission"
            );
        }
        assert!(
            responses[7].body.as_ref().unwrap().get("text").is_none(),
            "the at-a-glance is the LAST of the eight and carries no schema"
        );
        assert!(
            outcome.section_replies.iter().all(|r| r.is_none()),
            "all seven scripted replies fail validation"
        );
        assert_eq!(outcome.section_failures.len(), 7);
        for f in &outcome.section_failures {
            assert_eq!(f.class, NarrateFailureClass::Validation);
        }
        assert!(outcome.at_a_glance.is_none());
        assert_eq!(
            outcome.at_a_glance_failure,
            Some(NarrateFailureClass::Validation)
        );
    }

    // -- The lenient parse (R-ataglace): bare JSON, fenced JSON after
    // prose, and JSON embedded in prose all parse; missing fields and
    // non-JSON refuse.

    #[test]
    fn parse_at_a_glance_is_lenient_about_prose_and_refuses_missing_fields() {
        let bare = parse_at_a_glance(&valid_glance_reply()).expect("bare JSON parses");
        assert_eq!(bare.whats_working, "wg");
        assert_eq!(bare.ambitious_workflows, "aw");
        let fenced = format!(
            "Here is the summary:\n```json\n{}\n```\nDone.",
            valid_glance_reply()
        );
        let p = parse_at_a_glance(&fenced).expect("fenced JSON parses");
        assert_eq!(p.quick_wins, "qw");
        let embedded = format!("Result: {} — end.", valid_glance_reply());
        let p = parse_at_a_glance(&embedded).expect("embedded JSON parses");
        assert_eq!(p.whats_hindering, "wh");
        let missing = json!({
            "whats_working": "wg",
            "whats_hindering": "wh",
            "quick_wins": "qw"
        })
        .to_string();
        assert!(
            parse_at_a_glance(&missing).is_err(),
            "a missing field refuses"
        );
        assert!(
            parse_at_a_glance("no json here at all").is_err(),
            "non-JSON refuses"
        );
    }
}
