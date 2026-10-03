//! Item 3's pure HTML render: `(Aggregate, ReportContext)` → one self-contained
//! HTML string (SDD §4 module layout, §13 item 3, first commit — the scaffold).
//!
//! The document frame (CSP `<meta http-equiv>`, system font stack, inline CSS
//! and JS, no network fetches, no CDN font, no external reference of any
//! kind), the TOC, the seven section cards (rule-5 placeholder when
//! `ctx.sections` is `None` — v1's no-model checkpoint), the seven chart
//! cards as rule-5 placeholders (the bars land in item 3b), the
//! suggestions/clipboard block, and the footer: §9 rule 3's four counters
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

use html_escape::{encode_double_quoted_attribute, encode_text};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::aggregate::{Aggregate, Totals};
use super::types::CappedBy;

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

/// The seven chart cards, in report order. Item 3a ships every card as the
/// rule-5 placeholder; item 3b fills the bars.
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

    // The seven chart cards — rule-5 placeholders in item 3a.
    for (anchor, display) in CHART_CARDS {
        html.push_str("<section class=\"card chart\" id=\"");
        html.push_str(&encode_double_quoted_attribute(anchor));
        html.push_str("\"><h2>");
        html.push_str(&encode_text(display));
        html.push_str("</h2><p class=\"empty\">No data</p></section>");
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
    use std::collections::BTreeMap;

    use html_escape::{encode_double_quoted_attribute, encode_text};

    use super::{ReportContext, ReportSection, render_report};
    use crate::session::insights::aggregate::{Aggregate, Medians, Totals};
    use crate::session::insights::types::CappedBy;

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

    fn test_ctx<'a>(sections: Option<&'a [ReportSection]>) -> ReportContext<'a> {
        ReportContext {
            window: "2026-09-01 to 2026-09-30".to_string(),
            root: "/Users/tester/.grok".to_string(),
            scope: None,
            sections,
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(None));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(None));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(Some(&sections)));
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
        let html = render_report(&test_aggregate(&tie), &test_ctx(None));
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
        let html = render_report(&test_aggregate(&[CappedBy::Cost]), &test_ctx(None));
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
        let html = render_report(&test_aggregate(&[]), &test_ctx(None));
        let footer = footer_of(&html);
        assert!(
            !footer.contains("Capped by"),
            "a clean run must not claim a ceiling fired"
        );
    }
}
