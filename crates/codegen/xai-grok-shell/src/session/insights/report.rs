//! Item 3's pure HTML render: `(Aggregate, ReportContext)` → one self-contained
//! HTML string (SDD §4 module layout, §13 item 3).
//!
//! The document frame (CSP `<meta http-equiv>`, system font stack, inline CSS
//! and JS, no network fetches, no CDN font, no external reference of any
//! kind), the TOC, the seven section cards (rule-5 placeholder when
//! `ctx.sections` is `None` — v1's no-model checkpoint), the four
//! bar-family chart cards (rule-1 global-max bars, rule-2 display names,
//! rule-4 stated N, rule-6 per-row session ids on the model-derived three;
//! the rule-5 placeholder when a chart's map is empty), rule 8's
//! concurrency card and the two scalar cards still rule-5 placeholders
//! (item 3c), the suggestions/clipboard block, and the footer: §9 rule 3's
//! four counters
//! with the identity equations holding on the printed numbers, the
//! `capped_by` attribution read from `Totals::capped_by` (T-12(i)), the
//! `report.json` link, the window, the `GROK_HOME` root, the `--project`
//! scope with its surviving count, and §10's foreign-write disclosure.
//!
//! Escaping is a hard requirement, not a style note (SDD §9): nothing
//! crosses the template boundary unescaped, numbers included —
//! `encode_text` for text nodes, `encode_double_quoted_attribute` for
//! attribute contexts (the crate's name for the SOT's `encode_attribute` —
//! that name does not exist in html-escape 0.2.13), and the markdown
//! renderer escapes every raw-HTML event rather than passing it through.
//!
//! Pure: no I/O, no model call, no global state, no nondeterminism (SDD §13
//! item 3). `report.json` is written by item 8's orchestrator; this renderer
//! only emits the footer link to it.

use std::collections::BTreeMap;

use html_escape::{encode_double_quoted_attribute, encode_text};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::aggregate::{Aggregate, AnalyzedSession, Totals};
use super::types::{CappedBy, Friction, Outcome};

/// The inline clipboard script (`session/templates/insights_report.js`):
/// plain ES2019, no build step, run under the document's CSP
/// (`script-src 'unsafe-inline'`). SDD §9.
const REPORT_JS: &str = include_str!("../templates/insights_report.js");

/// §10's foreign-write disclosure, the footer's "what this command touched"
/// line: merely *listing* sessions performs the worktree-summary repair.
const FOREIGN_WRITE_DISCLOSURE: &str = "listing sessions performs the \
worktree-summary repair, which may rewrite a session's summary.json (only the \
missing session_kind / worktree_label fields, with the pre-write mtime restore \
warn-only) — the run's one foreign write.";

/// The seven section cards, in report order — the §7 ported-prompt names.
/// Source of the TOC entries and the placeholder cards when `ctx.sections`
/// is `None` (v1's no-model checkpoint).
const SECTION_CARDS: [(&str, &str); 7] = [
    ("section-project-areas", "Project areas"),
    ("section-interaction-style", "Interaction style"),
    ("section-what-works", "What works"),
    ("section-friction-analysis", "Friction analysis"),
    ("section-suggestions", "Suggestions"),
    ("section-on-the-horizon", "On the horizon"),
    ("section-fun-ending", "Fun ending"),
];

/// The seven chart cards, in report order. Item 3b fills the four
/// bar-family cards; rule 8's concurrency card and the two scalar cards
/// stay rule-5 placeholders for item 3c.
const CHART_CARDS: [(&str, &str); 7] = [
    ("chart-friction", "Friction (model-estimated)"),
    ("chart-outcome", "Outcome (model-estimated)"),
    ("chart-goal-categories", "Goal categories (model-estimated)"),
    ("chart-hours-utc", "Hour histogram (UTC)"),
    // §9 rule 8's bar label, byte-for-byte from the SOT (item 3b's
    // T-6(c) asserts it on the bar).
    (
        "chart-concurrency",
        "peak concurrent sessions (within 30 min)",
    ),
    ("chart-cost", "Cost"),
    ("chart-lines-changed", "Lines changed (transcript-derived)"),
];

/// The document head: the CSP `<meta>` (shipped as `http-equiv` because the
/// file opens over `file://`, SDD §9), the system font stack (no CDN font),
/// the inline CSS. No external reference of any kind.
const HEAD: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<title>Grok session insights</title>
<style>
body {
  font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  margin: 0 auto;
  max-width: 64rem;
  padding: 2rem 1.5rem;
  color: #1f2328;
  background: #ffffff;
  line-height: 1.55;
}
h1 { font-size: 1.6rem; margin: 0 0 0.25rem; }
.card { border: 1px solid #d0d7de; border-radius: 8px; padding: 1rem 1.25rem; margin: 1.25rem 0; }
.card h2 { font-size: 1.15rem; margin: 0 0 0.5rem; }
.toc { border: 1px solid #d0d7de; border-radius: 8px; padding: 0.75rem 1.25rem; }
.toc-title { font-weight: 600; margin: 0 0 0.25rem; }
.toc a { color: #0969da; text-decoration: none; margin-right: 0.75rem; }
.empty { color: #57606a; font-style: italic; }
.suggestion-item { display: flex; gap: 0.5rem; align-items: flex-start; border: 1px solid #d0d7de; border-radius: 6px; padding: 0.5rem 0.75rem; margin: 0.5rem 0; }
.suggestion-item input { margin-top: 0.3rem; }
.suggestion-item label { flex: 1; white-space: pre-wrap; }
.copy-item, .copy-all-checked { border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; padding: 0.15rem 0.6rem; cursor: pointer; font-size: 0.85rem; }
.copy-all-row { margin: 0.75rem 0 0; }
.bars { margin: 0.5rem 0 0; }
.bar-row { margin: 0.3rem 0; font-size: 0.95rem; }
.bar-row .bar-label { display: inline-block; min-width: 12rem; }
.bar-row .bar-value { font-variant-numeric: tabular-nums; }
.bar-fill { display: inline-block; height: 0.65rem; background: #0969da; border-radius: 3px; margin-left: 0.5rem; }
.footer { margin-top: 2.5rem; border-top: 1px solid #d0d7de; padding-top: 1rem; color: #57606a; font-size: 0.9rem; }
.footer p { margin: 0.25rem 0; }
.footer a { color: #0969da; }
</style>
</head>
<body>
<h1>Grok session insights</h1>
"#;

/// The run-level render facts the SOT mandates the footer print but the
/// 8-field [`Aggregate`] does not carry (SDD §13 item 3).
pub struct ReportContext<'a> {
    /// The scan window — the period the scan covered (SDD §8).
    pub window: String,
    /// The single `GROK_HOME` scanned — one home per run, and say which
    /// (SDD §8); the footer removes the multi-home caveat.
    pub root: String,
    /// The `--project` path, when set. The surviving count the line pairs
    /// with comes from the aggregate (SDD §8: the line names the path and
    /// the count that survived it).
    pub scope: Option<String>,
    /// The seven narrative sections; `None` at v1's no-model checkpoint,
    /// where every section card renders the rule-5 placeholder and the
    /// canonical card names stand in.
    pub sections: Option<&'a [ReportSection]>,
    /// The analysed sessions, in scan order — the provenance behind the
    /// per-row session ids (SDD §9 rule 6, T-6(d)). Each model-derived
    /// chart row lists the sessions whose facets feed that bar; a badged
    /// or unlabelled session (empty `friction_counts`, `None` outcome, no
    /// goals) contributes to no row.
    pub sessions: &'a [AnalyzedSession<'a>],
}

/// One of the seven sections' rendered content. Item 8's narrate is the
/// future supplier; item 3's tests construct these directly.
pub struct ReportSection {
    /// The card heading and the TOC label.
    pub display_name: String,
    /// The TOC anchor id; the card carries `id` = this value, so a
    /// section's TOC anchor never outlives the section (SDD §9 rule 5).
    pub anchor_id: String,
    /// Markdown prose. Rendered to HTML with every raw-HTML event escaped
    /// (SDD §9's blanket escape discipline).
    pub body: String,
    /// The copy-able suggestion items — the suggestions section carries
    /// its clipboard payload here; empty for the six prose sections.
    pub suggestions: Vec<String>,
}

/// The pure renderer: one self-contained HTML string from its inputs. No
/// I/O, no model call, no global state, no nondeterminism (SDD §13 item 3).
pub fn render_report(aggregate: &Aggregate, ctx: &ReportContext) -> String {
    let totals = &aggregate.totals;
    // §9 rule 3's second identity equation, computed for the footer:
    // analyzed = clean + partial.
    let clean = totals.sessions_analyzed - totals.sessions_partial;

    // (anchor, display, supplied section) per section card: the supplied
    // sections stand in for the canonical cards, so the TOC is built from
    // exactly the cards that render and no anchor dangles (SDD §9 rule 5).
    let section_cards: Vec<(&str, &str, Option<&ReportSection>)> = match ctx.sections {
        Some(sections) => sections
            .iter()
            .map(|s| (s.anchor_id.as_str(), s.display_name.as_str(), Some(s)))
            .collect(),
        None => SECTION_CARDS
            .iter()
            .map(|(anchor, display)| (*anchor, *display, None))
            .collect(),
    };

    let mut html = String::with_capacity(64 * 1024);
    html.push_str(HEAD);

    // The TOC: one anchor per section card and per chart card.
    html.push_str("<nav class=\"toc\"><p class=\"toc-title\">Contents</p><p>");
    let mut first = true;
    for (anchor, display, _) in &section_cards {
        if !first {
            html.push_str(" ");
        }
        first = false;
        push_toc_entry(&mut html, anchor, display);
    }
    for (anchor, display) in CHART_CARDS {
        if !first {
            html.push_str(" ");
        }
        first = false;
        push_toc_entry(&mut html, anchor, display);
    }
    html.push_str("</p></nav>");

    // The section cards: prose when supplied (escaped), the rule-5
    // placeholder otherwise — the card and its anchor survive either way.
    for (anchor, display, section) in &section_cards {
        html.push_str("<section class=\"card\" id=\"");
        html.push_str(&encode_double_quoted_attribute(anchor));
        html.push_str("\"><h2>");
        html.push_str(&encode_text(display));
        html.push_str("</h2>");
        match section {
            Some(section) if !section.body.trim().is_empty() => {
                html.push_str("<div class=\"prose\">");
                html.push_str(&markdown_to_escaped_html(&section.body));
                html.push_str("</div>");
            }
            // A card whose body is empty but which carries suggestions is
            // not no-data: its items, rendered just below, are its content
            // (SDD §9 rule 5 ties the placeholder to no-data cards).
            Some(section) if !section.suggestions.is_empty() => {}
            _ => html.push_str("<p class=\"empty\">No data</p>"),
        }
        if let Some(section) = section {
            push_suggestions_block(&mut html, &section.suggestions);
        }
        html.push_str("</section>");
    }

    // Rule 1: every bar normalises to the report's GLOBAL max across the
    // four bar families (rule 8's concurrency bar joins the same max when
    // item 3c lands), so a 3-next-to-4 pair on one card never invites a
    // ratio comparison another card's scale cannot support.
    let bar_max = aggregate
        .friction
        .values()
        .chain(aggregate.outcomes.values())
        .chain(aggregate.goal_categories.values())
        .chain(aggregate.hour_histogram.values())
        .copied()
        .max()
        .unwrap_or(0);
    let norm_max = bar_max.max(1);

    // Rule 6 (T-6(d)): the session ids behind each model-derived bar,
    // built from the provenance sessions in scan order. A badged or
    // unlabelled session (empty `friction_counts`, `None` outcome, no
    // goals) contributes to no row, mirroring item 2's aggregation.
    let mut friction_ids: BTreeMap<Friction, Vec<String>> = BTreeMap::new();
    let mut outcome_ids: BTreeMap<Outcome, Vec<String>> = BTreeMap::new();
    let mut goal_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for s in ctx.sessions {
        let id = s.digest.session_id.as_str();
        for f in s.friction_counts.keys() {
            push_id_once(&mut friction_ids, *f, id);
        }
        if let Some(o) = s.outcome {
            push_id_once(&mut outcome_ids, o, id);
        }
        for g in s.goal_categories {
            push_id_once(&mut goal_ids, g.clone(), id);
        }
    }

    // One row per bar per family, in the family's chart order (the
    // `Ord` order of the map keys — for `Friction` and `Outcome` that is
    // rule 2's severity order); the model-derived three carry their
    // session ids, the transcript-derived hours chart does not.
    let friction_rows: Vec<Bar> = aggregate
        .friction
        .iter()
        .map(|(f, v)| Bar {
            label: friction_display(*f),
            value: *v,
            sessions: friction_ids.get(f).map(Vec::as_slice),
        })
        .collect();
    let outcome_rows: Vec<Bar> = aggregate
        .outcomes
        .iter()
        .map(|(o, v)| Bar {
            label: outcome_display(*o),
            value: *v,
            sessions: outcome_ids.get(o).map(Vec::as_slice),
        })
        .collect();
    let goal_rows: Vec<Bar> = aggregate
        .goal_categories
        .iter()
        .map(|(g, v)| Bar {
            label: g.as_str(),
            value: *v,
            sessions: goal_ids.get(g).map(Vec::as_slice),
        })
        .collect();
    let hour_labels: Vec<String> = aggregate
        .hour_histogram
        .keys()
        .map(|h| h.to_string())
        .collect();
    let hour_rows: Vec<Bar> = hour_labels
        .iter()
        .zip(aggregate.hour_histogram.values())
        .map(|(label, v)| Bar {
            label,
            value: *v,
            sessions: None,
        })
        .collect();

    // The seven chart cards. Item 3b fills the four bar-family cards;
    // rule 8's concurrency card and the two scalar cards stay rule-5
    // placeholders for item 3c.
    for (anchor, display) in CHART_CARDS {
        html.push_str("<section class=\"card chart\" id=\"");
        html.push_str(&encode_double_quoted_attribute(anchor));
        html.push_str("\"><h2>");
        html.push_str(&encode_text(display));
        html.push_str("</h2>");
        let rows: &[Bar] = match anchor {
            "chart-friction" => &friction_rows,
            "chart-outcome" => &outcome_rows,
            "chart-goal-categories" => &goal_rows,
            "chart-hours-utc" => &hour_rows,
            _ => &[],
        };
        push_bar_chart(&mut html, norm_max, rows);
        html.push_str("</section>");
    }

    push_footer(&mut html, totals, clean, ctx);

    // The inline clipboard script, verbatim (SDD §9).
    html.push_str("<script>");
    html.push_str(REPORT_JS);
    html.push_str("</script></body></html>");
    html
}

fn push_toc_entry(html: &mut String, anchor: &str, display: &str) {
    html.push_str("<a href=\"#");
    html.push_str(&encode_double_quoted_attribute(anchor));
    html.push_str("\">");
    html.push_str(&encode_text(display));
    html.push_str("</a>");
}

/// The suggestions/clipboard block: per-item copy + copy-all-checked —
/// v1's exactly two behaviours (SDD §9). Each item carries its payload in
/// `data-text`, escaped with `encode_double_quoted_attribute` because
/// `encode_text` does not escape quotes.
fn push_suggestions_block(html: &mut String, suggestions: &[String]) {
    if suggestions.is_empty() {
        return;
    }
    html.push_str("<div class=\"suggestions\">");
    for item in suggestions {
        html.push_str("<div class=\"suggestion-item\" data-text=\"");
        html.push_str(&encode_double_quoted_attribute(item));
        // The checkbox sits inside its label so a label click toggles it
        // (the label is the row's largest click target); the copy button
        // stays outside so its click does not toggle.
        html.push_str(
            "\"><label><input type=\"checkbox\" checked aria-label=\"check suggestion\">",
        );
        html.push_str(&encode_text(item));
        html.push_str("</label> <button type=\"button\" class=\"copy-item\">Copy</button></div>");
    }
    html.push_str(
        "<p class=\"copy-all-row\"><button type=\"button\" class=\"copy-all-checked\">Copy all checked</button></p></div>",
    );
}

/// §9 rule 3's footer: the four counters with the identity equations
/// holding on the printed numbers, the `capped_by` attribution (T-12(i)),
/// the `report.json` link, the window, the root, the scope and the §10
/// foreign-write disclosure.
fn push_footer(html: &mut String, totals: &Totals, clean: u32, ctx: &ReportContext) {
    html.push_str("<footer class=\"footer\">");
    html.push_str("<p>Scanned ");
    push_num(html, totals.sessions_scanned);
    html.push_str(" sessions: analyzed ");
    push_num(html, totals.sessions_analyzed);
    html.push_str(" (clean ");
    push_num(html, clean);
    html.push_str(", partial ");
    push_num(html, totals.sessions_partial);
    html.push_str("), excluded ");
    push_num(html, totals.sessions_excluded);
    html.push_str(", self-excluded ");
    push_num(html, totals.sessions_self_excluded);
    html.push_str(".</p>");

    if !totals.capped_by.is_empty() {
        html.push_str("<p>Capped by: ");
        for (i, cap) in totals.capped_by.iter().enumerate() {
            if i > 0 {
                html.push_str(", ");
            }
            html.push_str(&encode_text(capped_by_display(*cap)));
        }
        html.push_str("</p>");
    }

    html.push_str("<p>Window: ");
    html.push_str(&encode_text(&ctx.window));
    html.push_str("</p><p>Root: ");
    html.push_str(&encode_text(&ctx.root));
    html.push_str("</p>");
    if let Some(scope) = &ctx.scope {
        html.push_str("<p>Scope: ");
        html.push_str(&encode_text(scope));
        html.push_str(" (");
        push_num(html, totals.sessions_scanned);
        html.push_str(" sessions).</p>");
    }

    html.push_str("<p>Machine-readable aggregate: <a href=\"report.json\">report.json</a></p>");
    html.push_str("<p>What this command touched: ");
    html.push_str(&encode_text(FOREIGN_WRITE_DISCLOSURE));
    html.push_str("</p></footer>");
}

/// The blanket escape discipline reaches numbers too (SDD §9): every
/// interpolated value crosses the template boundary through `encode_text`.
fn push_num(html: &mut String, value: u32) {
    html.push_str(&encode_text(&value.to_string()));
}

/// The footer's display name for a `CappedBy` ceiling. Exhaustive: a new
/// ceiling variant must get a display name here, not a wildcard default.
fn capped_by_display(cap: CappedBy) -> &'static str {
    match cap {
        CappedBy::Calls => "calls",
        CappedBy::ChunkPerSession => "chunk-per-session",
        CappedBy::ChunkPerRun => "chunk-per-run",
        CappedBy::Cost => "cost",
    }
}

/// The rule-2 display name for a `Friction` bar. Exhaustive: a new variant
/// gets a label here, not a wildcard default, and the raw variant name
/// (`BuggyCode`) never appears as a label (SDD §9 rule 2).
fn friction_display(f: Friction) -> &'static str {
    match f {
        Friction::MisunderstoodRequest => "misunderstood the request",
        Friction::WrongApproach => "wrong approach",
        Friction::BuggyCode => "buggy code",
        Friction::UserRejectedAction => "user rejected an action",
        Friction::ClaudeGotBlocked => "got blocked",
        Friction::UserStoppedEarly => "stopped early",
        Friction::WrongFileOrLocation => "wrong file or location",
        Friction::ExcessiveChanges => "excessive changes",
        Friction::SlowOrVerbose => "slow or verbose",
        Friction::ToolFailed => "a tool failed",
        Friction::UserUnclear => "user was unclear",
        Friction::ExternalIssue => "external issue",
    }
}

/// The rule-2 display name for an `Outcome` bar (same discipline as
/// [`friction_display`]).
fn outcome_display(o: Outcome) -> &'static str {
    match o {
        Outcome::FullyAchieved => "fully achieved",
        Outcome::MostlyAchieved => "mostly achieved",
        Outcome::PartiallyAchieved => "partially achieved",
        Outcome::NotAchieved => "not achieved",
        Outcome::UnclearFromTranscript => "unclear from transcript",
    }
}

/// One rendered bar: its label, its value, and — on the model-derived
/// charts — the session ids behind it (SDD §9 rule 6); `None` on the
/// transcript-derived hours chart.
struct Bar<'a> {
    label: &'a str,
    value: u32,
    sessions: Option<&'a [String]>,
}

/// Appends `id` to the row for `key`, at most once per session (a
/// session's goal list may name the same category twice; its id belongs
/// on the row once).
fn push_id_once<K: Ord>(map: &mut BTreeMap<K, Vec<String>>, key: K, id: &str) {
    let ids = map.entry(key).or_default();
    if !ids.iter().any(|existing| existing == id) {
        ids.push(id.to_string());
    }
}

/// The §9 chart rules for one bar-family card (rules 1, 4, 5 and 6; the
/// labels arrive pre-resolved by the rule-2 display tables or escaped
/// as-is for the free-string goal categories). An empty row list is a
/// no-data chart: the rule-5 placeholder, never an empty bar list.
fn push_bar_chart(html: &mut String, norm_max: u32, rows: &[Bar]) {
    if rows.is_empty() {
        html.push_str("<p class=\"empty\">No data</p>");
        return;
    }
    // Rule 4: the card states its N, and the sum of the bar values equals
    // it — both are this same row list, so they cannot drift apart.
    let stated: u32 = rows.iter().map(|bar| bar.value).sum();
    html.push_str("<p class=\"chart-n\">N = ");
    push_num(html, stated);
    html.push_str("</p>");
    html.push_str("<div class=\"bars\">");
    for bar in rows {
        html.push_str("<div class=\"bar-row\"");
        if let Some(ids) = bar.sessions {
            html.push_str(" data-sessions=\"");
            // A quote-bearing id must not break out of the attribute.
            html.push_str(&encode_double_quoted_attribute(&ids.join(" ")));
            html.push_str("\"");
        }
        html.push_str("><span class=\"bar-label\">");
        html.push_str(&encode_text(bar.label));
        html.push_str("</span><span class=\"bar-value\">");
        push_num(html, bar.value);
        html.push_str("</span><div class=\"bar-fill\" style=\"width:");
        // Rule 1: normalised to the report's global max (one decimal),
        // the same scale on every card.
        let pct = (bar.value as f64 / norm_max as f64 * 1000.0).round() / 10.0;
        html.push_str(&encode_double_quoted_attribute(&pct.to_string()));
        html.push_str("%\"></div></div>");
    }
    html.push_str("</div>");
}

/// Renders a section body (Markdown, model-supplied) to HTML, escaping
/// every raw-HTML event instead of passing it through: pulldown-cmark
/// emits raw HTML verbatim, and SDD §9's blanket rule is that nothing
/// crosses the template boundary unescaped.
///
/// Link and image destinations are external references of the kind §9's
/// no-external-refs ban rules out outright (an autolink included — it
/// arrives as a `Tag::Link`): the tags are dropped and the inner text
/// (link text / alt text) survives, unlinked.
fn markdown_to_escaped_html(body: &str) -> String {
    let mut out = String::new();
    for event in Parser::new(body) {
        match &event {
            Event::Html(raw) | Event::InlineHtml(raw) => {
                out.push_str(&encode_text(raw));
            }
            // 0.13's `Event::End` carries the `TagEnd` mirror, not the
            // `Tag`: drop the close of a dropped open.
            Event::Start(tag) if matches!(tag, Tag::Link { .. } | Tag::Image { .. }) => {}
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {}
            _ => pulldown_cmark::html::push_html(&mut out, std::iter::once(event)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use chrono::{DateTime, Utc};
    use html_escape::{encode_double_quoted_attribute, encode_text};

    use super::{ReportContext, ReportSection, friction_display, outcome_display, render_report};
    use crate::session::insights::aggregate::{Aggregate, AnalyzedSession, Medians, Totals};
    use crate::session::insights::digest::{CACHE_SCHEMA_VERSION, SessionDigest};
    use crate::session::insights::types::{
        CappedBy, FRICTION_VARIANTS, Friction, OUTCOME_VARIANTS, Outcome,
    };

    fn test_aggregate(capped_by: &[CappedBy]) -> Aggregate {
        Aggregate {
            totals: Totals {
                // §9 rule 3's identity equations, by construction:
                // 40 = 38 + 1 + 1; 38 = 34 (clean) + 4 (partial).
                sessions_scanned: 40,
                sessions_analyzed: 38,
                sessions_partial: 4,
                sessions_excluded: 1,
                sessions_self_excluded: 1,
                sessions_legacy: 0,
                cost_ticks: 0,
                cost_is_partial: false,
                capped_by: capped_by.to_vec(),
                duration_secs_total: 0,
                lines_changed: 0,
            },
            medians: Medians {
                response_time_secs: 0,
                wall_time_mins: 0,
            },
            max_concurrent_sessions: 2,
            interval_readable_sessions: 30,
            hour_histogram: BTreeMap::new(),
            goal_categories: BTreeMap::new(),
            friction: BTreeMap::new(),
            outcomes: BTreeMap::new(),
        }
    }

    fn test_ctx<'a>(
        sections: Option<&'a [ReportSection]>,
        sessions: &'a [AnalyzedSession<'a>],
    ) -> ReportContext<'a> {
        ReportContext {
            window: "2026-09-01 to 2026-09-30".to_string(),
            root: "/Users/tester/.grok".to_string(),
            scope: None,
            sections,
            sessions,
        }
    }

    fn suggestions_section(suggestions: Vec<String>) -> ReportSection {
        ReportSection {
            display_name: "Suggestions".to_string(),
            anchor_id: "section-suggestions".to_string(),
            body: String::new(),
            suggestions,
        }
    }

    fn footer_of(html: &str) -> &str {
        let start = html.find("<footer").expect("footer present");
        let end = html.find("</footer>").expect("footer close present");
        &html[start..end]
    }

    fn section_card_of<'a>(html: &'a str, anchor: &str) -> &'a str {
        let marker = format!("<section class=\"card\" id=\"{anchor}\"");
        let start = html
            .find(&marker)
            .unwrap_or_else(|| panic!("section card {anchor} missing"));
        let close = html[start..]
            .find("</section>")
            .expect("section card close present");
        &html[start..start + close]
    }

    // --- T-7: report HTML — escaping in both contexts, the CSP meta, the
    //     no-external-refs ban, the two clipboard behaviours. ---

    #[test]
    fn t7_text_node_payload_is_escaped() {
        let payload = "<img src=x onerror=alert(1)>";
        let section = ReportSection {
            display_name: "What works".to_string(),
            anchor_id: "section-what-works".to_string(),
            body: format!("Prose before {payload} prose after."),
            suggestions: vec![],
        };
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        // The raw tag must not survive the template boundary...
        assert!(
            !html.contains(payload),
            "raw text-node payload reached the output"
        );
        // ...the escaped form is what crosses.
        assert!(html.contains(&encode_text(payload).to_string()));
    }

    #[test]
    fn t7_attribute_context_escapes_quotes() {
        // A quote-bearing brief_summary: encode_text would not escape the
        // quotes, so only encode_double_quoted_attribute keeps it inside
        // the attribute (SDD §9's precision clause).
        let brief = "He said \"quoted\" <b>markup</b> in the summary.";
        let section = suggestions_section(vec![brief.to_string()]);
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        assert!(
            html.contains(&format!(
                "data-text=\"{}\"",
                encode_double_quoted_attribute(brief).to_string()
            )),
            "data-text must carry the attribute-escaped form"
        );
        // The unescaped fragment must not break out of the attribute.
        assert!(!html.contains("markup</b>"));
    }

    #[test]
    fn t7_markdown_link_and_image_hrefs_do_not_cross_the_boundary() {
        // The model's markdown can carry link and image destinations; §9's
        // no-external-refs ban is outright, so the tags drop and the inner
        // text (link text / alt text) survives unlinked (T-7).
        let section = ReportSection {
            display_name: "What works".to_string(),
            anchor_id: "section-what-works".to_string(),
            body: "See [x](https://example.com) and \
                   ![diagram](https://example.com/img.png)."
                .to_string(),
            suggestions: vec![],
        };
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        assert!(
            !html.contains("http://"),
            "external reference crossed the boundary"
        );
        assert!(
            !html.contains("https://"),
            "external reference crossed the boundary"
        );
        // The footer's own report.json link is a well-formed relative
        // reference (§11), so the tag checks scope to the section card.
        let card = section_card_of(&html, "section-what-works");
        assert!(!card.contains("<a "), "link tag must not render");
        assert!(!card.contains("<img"), "image tag must not render");
        assert!(
            card.contains("See x and diagram."),
            "link inner text / image alt text lost"
        );
    }

    #[test]
    fn t7_csp_meta_present_and_default_src_none_survives() {
        let html = render_report(&test_aggregate(&[]), &test_ctx(None, &[]));
        assert!(
            html.contains(
                r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">"#
            ),
            "CSP meta missing"
        );
        assert!(html.contains("default-src 'none'"));
    }

    #[test]
    fn t7_no_external_reference_of_any_kind() {
        let html = render_report(&test_aggregate(&[]), &test_ctx(None, &[]));
        // Banned outright...
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("@import"));
        assert!(!html.contains("fetch("));
        // ...while `//` is banned only in the protocol-relative position.
        for prefix in ["src=\"//", "href=\"//", "url(//", "@import //"] {
            assert!(
                !html.contains(prefix),
                "protocol-relative reference: {prefix}"
            );
        }
    }

    #[test]
    fn t7_clipboard_block_emits_both_behaviours() {
        let section = suggestions_section(vec![
            "Add a rule to AGENTS.md".to_string(),
            "Second suggestion".to_string(),
        ]);
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        assert!(html.contains("copy-item"), "per-item copy control missing");
        assert!(
            html.contains("copy-all-checked"),
            "copy-all-checked control missing"
        );
        assert!(html.contains("data-text="));
        // The checkbox sits inside its label: a label click toggles it.
        assert!(
            html.contains("<label><input type=\"checkbox\""),
            "checkbox must be inside its label"
        );
    }

    #[test]
    fn t7_suggestions_card_with_empty_body_is_not_no_data() {
        // The suggestions card's content is its items, not prose; §9 rule 5
        // ties the placeholder to no-data cards, so an empty body with real
        // suggestions must not print "No data" beside them.
        let section = suggestions_section(vec!["Add a rule to AGENTS.md".to_string()]);
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        let card = section_card_of(&html, "section-suggestions");
        assert!(
            !card.contains("No data"),
            "placeholder beside real data: {card}"
        );
        assert!(
            card.contains("suggestion-item"),
            "items missing from the card"
        );
    }

    // --- T-8: the clipboard payload carries real 0x0a 0x0a bytes, not
    //     the two-character backslash-n sequence (CC's bug). ---

    #[test]
    fn t8_clipboard_payload_carries_real_newlines() {
        let suggestion = "first line\n\nsecond line";
        let section = suggestions_section(vec![suggestion.to_string()]);
        let sections = [section];
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections), &[]));
        let marker = "data-text=\"";
        let start = html.find(marker).expect("data-text attribute present");
        let value = &html[start + marker.len()..];
        let end = value.find('"').expect("closing quote of data-text");
        // The attribute value is the payload verbatim: real newline bytes,
        // no backslash-n pair anywhere in it.
        assert_eq!(&value[..end], suggestion);
    }

    // --- T-12(i): the footer's capped_by attribution, read from
    //     Totals::capped_by and rendered by this item. ---

    #[test]
    fn t12i_tie_footer_names_both_chunk_caps() {
        // The shipped-default tie: 4 x 40 = 160 ties the two chunk caps
        // exactly; the sorted+deduped Vec names both (T-12(i)).
        let tie = [CappedBy::ChunkPerSession, CappedBy::ChunkPerRun];
        let html = render_report(&test_aggregate(&tie), &test_ctx(None, &[]));
        let footer = footer_of(&html);
        let line = footer
            .split("Capped by:")
            .nth(1)
            .expect("attribution line present");
        let line = line.split("</p>").next().expect("line end");
        assert!(line.contains("chunk-per-session"), "line: {line}");
        assert!(line.contains("chunk-per-run"), "line: {line}");
    }

    #[test]
    fn t12i_single_cap_footer_names_only_that_cap() {
        let html = render_report(&test_aggregate(&[CappedBy::Cost]), &test_ctx(None, &[]));
        let footer = footer_of(&html);
        let line = footer
            .split("Capped by:")
            .nth(1)
            .expect("attribution line present");
        let line = line.split("</p>").next().expect("line end");
        assert!(line.contains("cost"), "line: {line}");
        assert!(!line.contains("chunk-per-session"));
        assert!(!line.contains("chunk-per-run"));
        assert!(!line.contains("calls"));
    }

    #[test]
    fn t12i_clean_run_footer_names_no_ceiling() {
        let html = render_report(&test_aggregate(&[]), &test_ctx(None, &[]));
        let footer = footer_of(&html);
        assert!(
            !footer.contains("Capped by"),
            "a clean run must not claim a ceiling fired"
        );
    }

    // --- Item-3b test helpers: build the AnalyzedSession / SessionDigest
    //     fixtures and parse the rendered bar charts. No I/O. ---

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("fixture instant")
            .with_timezone(&Utc)
    }

    /// A [`SessionDigest`] with only `session_id` set (the one field the
    /// chart rows read); every other field defaulted.
    fn digest_with_id(id: &str) -> SessionDigest {
        let t = at("2026-01-01T00:00:00Z");
        SessionDigest {
            cache_version: CACHE_SCHEMA_VERSION,
            session_id: id.to_string(),
            cwd: String::new(),
            git_root: None,
            head_branch: None,
            started_at: t,
            updated_at: t,
            wall_minutes: 0,
            activity_minutes: 0,
            user_turns: 1,
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
            degraded: Vec::new(),
            unreadable_lines: 0,
            transcript_sha256: String::new(),
        }
    }

    fn friction_map(pairs: &[(Friction, u32)]) -> BTreeMap<Friction, u32> {
        pairs.iter().copied().collect()
    }

    fn analyzed<'a>(
        digest: &'a SessionDigest,
        partial: bool,
        friction_counts: &'a BTreeMap<Friction, u32>,
        outcome: Option<Outcome>,
        goal_categories: &'a [String],
    ) -> AnalyzedSession<'a> {
        AnalyzedSession {
            digest,
            partial,
            friction_counts,
            outcome,
            goal_categories,
        }
    }

    /// An [`Aggregate`] with the four bar-family maps set and a consistent
    /// `totals` (scanned = analyzed; `analyzed = clean + partial` with the
    /// supplied `sessions_partial`; no ceilings).
    fn chart_aggregate(
        sessions_analyzed: u32,
        sessions_partial: u32,
        friction: BTreeMap<Friction, u32>,
        outcomes: BTreeMap<Outcome, u32>,
        goal_categories: BTreeMap<String, u32>,
        hour_histogram: BTreeMap<u8, u32>,
    ) -> Aggregate {
        Aggregate {
            totals: Totals {
                sessions_scanned: sessions_analyzed,
                sessions_analyzed,
                sessions_partial,
                sessions_excluded: 0,
                sessions_self_excluded: 0,
                sessions_legacy: 0,
                cost_ticks: 0,
                cost_is_partial: false,
                capped_by: Vec::new(),
                duration_secs_total: 0,
                lines_changed: 0,
            },
            medians: Medians {
                response_time_secs: 0,
                wall_time_mins: 0,
            },
            max_concurrent_sessions: 0,
            interval_readable_sessions: 0,
            hour_histogram,
            goal_categories,
            friction,
            outcomes,
        }
    }

    /// The chart card for `anchor` (distinct from [`section_card_of`], which
    /// matches the non-chart `card` class).
    fn chart_card_of<'a>(html: &'a str, anchor: &str) -> &'a str {
        let marker = format!("<section class=\"card chart\" id=\"{anchor}\"");
        let start = html
            .find(&marker)
            .unwrap_or_else(|| panic!("chart card {anchor} missing"));
        let close = html[start..]
            .find("</section>")
            .expect("chart card close present");
        &html[start..start + close]
    }

    /// The stated N in a chart card's `<p class="chart-n">N = {k}</p>`.
    fn chart_stated_n(html: &str, anchor: &str) -> u32 {
        let card = chart_card_of(html, anchor);
        let marker = "<p class=\"chart-n\">N = ";
        let start = card
            .find(marker)
            .unwrap_or_else(|| panic!("chart {anchor}: stated-N subtitle missing"));
        let rest = &card[start + marker.len()..];
        let end = rest.find('<').expect("stated-N text node end");
        rest[..end].parse::<u32>().expect("stated N is a number")
    }

    /// The bar values in a chart card, in render order.
    fn chart_bar_values(html: &str, anchor: &str) -> Vec<u32> {
        let card = chart_card_of(html, anchor);
        let mut values = Vec::new();
        let mut rest = card;
        let marker = "bar-value\">";
        while let Some(start) = rest.find(marker) {
            rest = &rest[start + marker.len()..];
            let end = rest.find('<').expect("bar-value text node end");
            values.push(rest[..end].parse::<u32>().expect("bar value is a number"));
        }
        values
    }

    /// The bar labels in a chart card, in render order (as rendered —
    /// `encode_text`-escaped; the rule-2 display names are plain, so the
    /// escape is the identity on them).
    fn chart_bar_labels(html: &str, anchor: &str) -> Vec<String> {
        let card = chart_card_of(html, anchor);
        let mut labels = Vec::new();
        let mut rest = card;
        let marker = "<span class=\"bar-label\">";
        while let Some(start) = rest.find(marker) {
            rest = &rest[start + marker.len()..];
            let end = rest.find("</span>").expect("bar-label text node end");
            labels.push(rest[..end].to_string());
        }
        labels
    }

    /// The `data-sessions` value (as rendered, still attribute-escaped) of
    /// the bar whose label is `label` in chart `anchor`; `None` if that bar
    /// has no `data-sessions` (the hours chart) or the bar is absent.
    fn bar_data_sessions(html: &str, anchor: &str, label: &str) -> Option<String> {
        let card = chart_card_of(html, anchor);
        let label_html = format!("<span class=\"bar-label\">{}</span>", encode_text(label));
        for row in card.split("<div class=\"bar-row\"").skip(1) {
            if !row.contains(&label_html) {
                continue;
            }
            let trimmed = row.trim_start();
            let rest = trimmed.strip_prefix("data-sessions=\"")?;
            let end = rest.find('"').expect("closing quote of data-sessions");
            return Some(rest[..end].to_string());
        }
        None
    }

    fn id_set(ids: &str) -> HashSet<&str> {
        ids.split_whitespace().collect()
    }

    fn expected_ids<'a>(ids: &'a [&'a str]) -> HashSet<&'a str> {
        ids.iter().copied().collect()
    }

    // --- T-6(b): chart-N reconciliation (rule 4). Driver ruling
    //     (2026-10-03, coordinator): T-6(b) covers the OUTCOME family
    //     only — the single bar family whose summed bar values reconcile
    //     with `sessions_analyzed` under §9 rule 4. `aggregate()` builds
    //     it via `if let Some(o) = s.outcome` (aggregate.rs:217-222): at
    //     most one bar per session, so the stated N is a subset of
    //     analysed and the gap is exactly the badged / unlabelled
    //     sessions (`None` outcome). No second family satisfies the
    //     SOT's own property in the SOT's own data model: friction and
    //     goal-categories let one session feed several bars
    //     (aggregate.rs:210-215, :203-208) and the hour histogram is a
    //     message count, not a session count (digest.rs:424), so their
    //     sums over-count `analyzed` — the SOT names a count and a
    //     property, not a second chart. The rule-8 concurrency bar is
    //     EXEMPT from T-6(b) by rule 8's own clause (it is T-6(c),
    //     item 3c). The test below is the final T-6(b). ---

    #[test]
    fn t6b_outcome_stated_n_reconciles_with_analyzed() {
        // 5 analysed sessions; 3 carry a labelled outcome (2 FullyAchieved,
        // 1 NotAchieved) and 2 are badged/unlabelled (`None` outcome), so the
        // outcome chart's stated N is 3, a strict subset of the 5 analysed.
        let d: Vec<SessionDigest> = ["oa", "ob", "oc", "obadged1", "obadged2"]
            .iter()
            .copied()
            .map(digest_with_id)
            .collect();
        let empty_fc = BTreeMap::new();
        let g: Vec<String> = vec![];
        let sessions: Vec<AnalyzedSession<'_>> = vec![
            analyzed(&d[0], false, &empty_fc, Some(Outcome::FullyAchieved), &g),
            analyzed(&d[1], false, &empty_fc, Some(Outcome::FullyAchieved), &g),
            analyzed(&d[2], false, &empty_fc, Some(Outcome::NotAchieved), &g),
            analyzed(&d[3], true, &empty_fc, None, &g),
            analyzed(&d[4], true, &empty_fc, None, &g),
        ];
        let mut outcomes = BTreeMap::new();
        outcomes.insert(Outcome::FullyAchieved, 2);
        outcomes.insert(Outcome::NotAchieved, 1);
        // 2 of the 5 are badged (`partial: true`), so the footer's
        // `analyzed = clean + partial` identity holds on the fixture.
        let aggregate = chart_aggregate(
            5,
            2,
            BTreeMap::new(),
            outcomes,
            BTreeMap::new(),
            BTreeMap::new(),
        );

        let html = render_report(&aggregate, &test_ctx(None, &sessions));
        let card = chart_card_of(&html, "chart-outcome");
        assert!(
            !card.contains("No data"),
            "the outcome chart has bars, must not be the no-data placeholder"
        );
        // (i) the stated N equals the sum of the rendered bar values...
        let stated = chart_stated_n(&html, "chart-outcome");
        let sum: u32 = chart_bar_values(&html, "chart-outcome").iter().sum();
        assert_eq!(stated, sum, "stated N must equal the sum of the bar values");
        assert_eq!(sum, 3, "the outcome bars must sum to 3 (2 + 1)");
        // (ii) ...and it reconciles with `analyzed`: stated N <= analysed, the
        //      gap being exactly the badged/unlabelled sessions (5 - 3 = 2).
        assert!(
            stated <= aggregate.totals.sessions_analyzed,
            "stated N ({stated}) must not exceed analysed ({})",
            aggregate.totals.sessions_analyzed
        );
        assert_eq!(
            aggregate.totals.sessions_analyzed - stated,
            2,
            "the gap must be exactly the 2 badged/unlabelled sessions"
        );
    }

    // --- §9 rule 2 (the no-false-green guard): every bar on the
    //     friction and outcome cards carries its display-table name, and
    //     a raw variant name never appears as a label. This is the
    //     direct rule-2 test the item's no-false-green list names:
    //     dropping or corrupting a display table (the T-6(d) fixtures
    //     name only two friction variants) fails it. The display tables
    //     are exhaustive matches, so it is green on first run — its
    //     value is that a raw-variant-name or wrong-label mutant fails
    //     it (a wildcard default would leak the CamelCase variant name
    //     into the card). ---

    #[test]
    fn rule2_bars_carry_display_names_not_raw_variant_names() {
        // Every `Friction` variant (count 1 each) and every `Outcome`
        // variant (count 1 each); 5 analysed sessions keeps the footer's
        // identity equations consistent with the 5 outcome bars (at most
        // one outcome bar per session).
        let friction: BTreeMap<Friction, u32> =
            FRICTION_VARIANTS.iter().copied().map(|f| (f, 1)).collect();
        let outcomes: BTreeMap<Outcome, u32> =
            OUTCOME_VARIANTS.iter().copied().map(|o| (o, 1)).collect();
        let aggregate = chart_aggregate(5, 0, friction, outcomes, BTreeMap::new(), BTreeMap::new());
        let html = render_report(&aggregate, &test_ctx(None, &[]));

        // Every friction bar renders its display-table name, in the
        // family's chart order (the BTreeMap's `Ord` key order = the
        // declaration order the variant const carries)...
        let expected_friction: Vec<String> = FRICTION_VARIANTS
            .iter()
            .map(|f| friction_display(*f).to_string())
            .collect();
        assert_eq!(
            chart_bar_labels(&html, "chart-friction"),
            expected_friction,
            "each chart-friction bar must carry its friction_display label"
        );
        // ...and every outcome bar its display-table name.
        let expected_outcomes: Vec<String> = OUTCOME_VARIANTS
            .iter()
            .map(|o| outcome_display(*o).to_string())
            .collect();
        assert_eq!(
            chart_bar_labels(&html, "chart-outcome"),
            expected_outcomes,
            "each chart-outcome bar must carry its outcome_display label"
        );

        // ...and a raw variant name never appears in either card.
        let friction_card = chart_card_of(&html, "chart-friction");
        for name in [
            "MisunderstoodRequest",
            "WrongApproach",
            "BuggyCode",
            "UserRejectedAction",
            "ClaudeGotBlocked",
            "UserStoppedEarly",
            "WrongFileOrLocation",
            "ExcessiveChanges",
            "SlowOrVerbose",
            "ToolFailed",
            "UserUnclear",
            "ExternalIssue",
        ] {
            assert!(
                !friction_card.contains(name),
                "raw Friction variant name {name} reached the card"
            );
        }
        let outcome_card = chart_card_of(&html, "chart-outcome");
        for name in [
            "FullyAchieved",
            "MostlyAchieved",
            "PartiallyAchieved",
            "NotAchieved",
            "UnclearFromTranscript",
        ] {
            assert!(
                !outcome_card.contains(name),
                "raw Outcome variant name {name} reached the card"
            );
        }
    }

    // --- §9 rule 5 (the no-false-green guard): a model-derived chart
    //     whose map is empty renders the rule-5 placeholder, never an
    //     empty bar list. (U-12 defers the other chart-rule rows to v2;
    //     the placeholder is item 3a's live behaviour, and this item's
    //     no-false-green list names this mutant.) ---

    #[test]
    fn rule5_empty_bar_map_renders_the_placeholder_not_empty_bars() {
        let aggregate = chart_aggregate(
            0,
            0,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let html = render_report(&aggregate, &test_ctx(None, &[]));
        for anchor in [
            "chart-friction",
            "chart-outcome",
            "chart-goal-categories",
            "chart-hours-utc",
        ] {
            let card = chart_card_of(&html, anchor);
            assert!(
                card.contains("<p class=\"empty\">No data</p>"),
                "{anchor}: an empty map must render the rule-5 placeholder"
            );
            assert!(
                !card.contains("bar-row"),
                "{anchor}: no bar markup beside the placeholder"
            );
            assert!(
                !card.contains("chart-n"),
                "{anchor}: a placeholder card states no N"
            );
        }
    }

    #[test]
    fn rule4_hours_card_bars_sum_to_its_stated_n() {
        // Hours is the transcript-derived bar family: its bars come from
        // the aggregate's hour map, UTC-labelled, with no per-row session
        // ids (T-6(d) does not reach it) — but rule 4's stated N does.
        let mut hours = BTreeMap::new();
        hours.insert(9u8, 2);
        hours.insert(13u8, 1);
        let aggregate = chart_aggregate(
            3,
            0,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            hours,
        );
        let html = render_report(&aggregate, &test_ctx(None, &[]));
        let card = chart_card_of(&html, "chart-hours-utc");
        assert!(
            !card.contains("No data"),
            "the hours chart has bars, must not be the placeholder"
        );
        // Both hours render, in UTC hour order (BTreeMap<u8>)...
        let pos9 = card.find("bar-label\">9<").expect("hour 9 bar missing");
        let pos13 = card.find("bar-label\">13<").expect("hour 13 bar missing");
        assert!(pos9 < pos13, "bars must render in UTC hour order");
        // ...with the right values...
        assert_eq!(
            chart_bar_values(&html, "chart-hours-utc"),
            vec![2, 1],
            "hour bar values wrong"
        );
        // ...and the hours card's stated N equals the sum of its bars
        // (rule 4 applies to all four bar-family cards).
        let stated = chart_stated_n(&html, "chart-hours-utc");
        assert_eq!(stated, 3, "stated N must equal the hours bars' sum");
    }

    // --- T-6(d): every model-derived chart row carries the session ids
    //     behind it, and the footer links report.json (SDD §9 rule 6). ---

    #[test]
    fn t6d_model_derived_rows_carry_their_session_ids() {
        // s1: BuggyCode x1, FullyAchieved, [debugging]
        // s2: BuggyCode x2 + ToolFailed x1, NotAchieved, [debugging, refactoring]
        // s3: badged (empty friction, None outcome, no goals) -> contributes nothing
        let d1 = digest_with_id("s1");
        let d2 = digest_with_id("s2");
        let d3 = digest_with_id("s3");
        let fc1 = friction_map(&[(Friction::BuggyCode, 1)]);
        let fc2 = friction_map(&[(Friction::BuggyCode, 2), (Friction::ToolFailed, 1)]);
        let fc3 = BTreeMap::new();
        let g1 = vec!["debugging".to_string()];
        let g2 = vec!["debugging".to_string(), "refactoring".to_string()];
        let g3: Vec<String> = vec![];
        let sessions = [
            analyzed(&d1, false, &fc1, Some(Outcome::FullyAchieved), &g1),
            analyzed(&d2, false, &fc2, Some(Outcome::NotAchieved), &g2),
            analyzed(&d3, true, &fc3, None, &g3),
        ];
        let friction = friction_map(&[(Friction::BuggyCode, 3), (Friction::ToolFailed, 1)]);
        let mut outcomes = BTreeMap::new();
        outcomes.insert(Outcome::FullyAchieved, 1);
        outcomes.insert(Outcome::NotAchieved, 1);
        let mut goals = BTreeMap::new();
        goals.insert("debugging".to_string(), 2);
        goals.insert("refactoring".to_string(), 1);
        // 1 of the 3 is badged (`partial: true`), so the footer's
        // `analyzed = clean + partial` identity holds on the fixture.
        let aggregate = chart_aggregate(3, 1, friction, outcomes, goals, BTreeMap::new());

        let html = render_report(&aggregate, &test_ctx(None, &sessions));

        // friction rows list exactly the sessions carrying that friction.
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-friction", "buggy code")
                    .as_deref()
                    .expect("buggy code bar must carry data-sessions")
            ),
            expected_ids(&["s1", "s2"])
        );
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-friction", "a tool failed")
                    .as_deref()
                    .expect("tool failed bar must carry data-sessions")
            ),
            expected_ids(&["s2"])
        );
        // outcome rows list exactly the session with that outcome.
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-outcome", "fully achieved")
                    .as_deref()
                    .expect("fully achieved bar must carry data-sessions")
            ),
            expected_ids(&["s1"])
        );
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-outcome", "not achieved")
                    .as_deref()
                    .expect("not achieved bar must carry data-sessions")
            ),
            expected_ids(&["s2"])
        );
        // goal-category rows list exactly the sessions carrying that goal.
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-goal-categories", "debugging")
                    .as_deref()
                    .expect("debugging bar must carry data-sessions")
            ),
            expected_ids(&["s1", "s2"])
        );
        assert_eq!(
            id_set(
                bar_data_sessions(&html, "chart-goal-categories", "refactoring")
                    .as_deref()
                    .expect("refactoring bar must carry data-sessions")
            ),
            expected_ids(&["s2"])
        );
        // the badged session's id is absent from every model-derived row.
        for (anchor, labels) in [
            ("chart-friction", ["buggy code", "a tool failed"]),
            ("chart-outcome", ["fully achieved", "not achieved"]),
            ("chart-goal-categories", ["debugging", "refactoring"]),
        ] {
            for label in labels {
                let ids = bar_data_sessions(&html, anchor, label).unwrap_or_default();
                assert!(
                    !ids.contains("s3"),
                    "badged s3 must not appear in {anchor}/{label}: {ids}"
                );
            }
        }
    }

    #[test]
    fn t6d_quote_bearing_session_id_is_escaped() {
        // A session id carrying a double quote must be attribute-escaped so
        // it cannot break out of the data-sessions attribute.
        let d1 = digest_with_id("ok");
        let d2 = digest_with_id("bad\"quote");
        let fc = friction_map(&[(Friction::ToolFailed, 1)]);
        let g: Vec<String> = vec![];
        // Both sessions are labelled (each carries friction), so each
        // carries an outcome and the aggregate's outcomes map is exactly
        // what `aggregate()` would produce for this session set — the
        // fixture matches a state the real pipeline can produce.
        let sessions = [
            analyzed(&d1, false, &fc, Some(Outcome::FullyAchieved), &g),
            analyzed(&d2, false, &fc, Some(Outcome::NotAchieved), &g),
        ];
        let friction = friction_map(&[(Friction::ToolFailed, 2)]);
        let mut outcomes = BTreeMap::new();
        outcomes.insert(Outcome::FullyAchieved, 1);
        outcomes.insert(Outcome::NotAchieved, 1);
        let aggregate = chart_aggregate(2, 0, friction, outcomes, BTreeMap::new(), BTreeMap::new());

        let html = render_report(&aggregate, &test_ctx(None, &sessions));
        let got = bar_data_sessions(&html, "chart-friction", "a tool failed")
            .expect("the tool failed bar must be present");
        // The joined, attribute-escaped form of both ids (one carries a
        // quote): an unescaped quote would truncate the attribute at the
        // breakout, which this equality catches — it is the breakout
        // check.
        assert_eq!(
            got,
            encode_double_quoted_attribute("ok bad\"quote").to_string(),
            "data-sessions must carry the attribute-escaped form"
        );
    }

    #[test]
    fn t6d_report_json_link_is_a_relative_path() {
        let aggregate = chart_aggregate(
            0,
            0,
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        let html = render_report(&aggregate, &test_ctx(None, &[]));
        let footer = footer_of(&html);
        // The link is the well-formed relative reference...
        assert!(
            footer.contains(r#"<a href="report.json">report.json</a>"#),
            "footer must link report.json as a relative path"
        );
        // ...not an absolute or external reference.
        assert!(!footer.contains("http://"), "absolute/external http link");
        assert!(!footer.contains("https://"), "absolute/external https link");
        assert!(
            !footer.contains("/report.json"),
            "must not be an absolute path"
        );
    }
}
