//! T-9 base — the feature-catalogue drift arms (SOT §12 T-9, item 7a).
//!
//! The catalogue text arrives through the public seam
//! `xai_grok_shell::session::insights::prompt("insights_suggestions")` — the
//! file is never read from disk. Only the trigger column is assertable: the
//! How-to-use / Good-for lines are free prose (SOT :1087-1090).

use clap::CommandFactory;
use xai_grok_pager::app::cli::PagerArgs;
use xai_grok_pager::slash::command::SlashCommand;
use xai_grok_pager::slash::commands::builtin_commands;
use xai_grok_shell::session::insights::prompt;

/// The closed ten-prompt seam set — `CLAUDE.md` is asserted out of all ten.
const ALL_TEN: [&str; 10] = [
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

/// The trigger column of every catalogue entry line: `N. **Name** — <trigger>`.
/// The name column carries no `*` (the `[^*]+` of the pinned format).
fn trigger_lines() -> Vec<String> {
    let text = prompt("insights_suggestions");
    let mut triggers = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(dot) = trimmed.find(". ") else {
            continue;
        };
        if dot == 0 || !trimmed[..dot].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = &trimmed[dot + 2..];
        let Some(close) = rest.strip_prefix("**").and_then(|r| r.find("**")) else {
            continue;
        };
        let name = &rest[2..2 + close];
        if name.is_empty() || name.contains('*') {
            continue;
        }
        let after = &rest[2 + close + 2..];
        let Some(trigger) = after.strip_prefix(" — ") else {
            continue;
        };
        triggers.push(trigger.to_string());
    }
    triggers
}

/// base-1: every dispatchable `/trigger` in the trigger column is a `name()`
/// or an `aliases()` entry of some command in the live builtin registry.
#[test]
fn base1_every_slash_trigger_dispatches_through_the_live_registry() {
    let commands = builtin_commands();
    let mut dispatched = 0usize;
    for trigger in trigger_lines() {
        if !trigger.starts_with('/') {
            continue;
        }
        for token in trigger.split_whitespace() {
            if !token.starts_with('/') || (token.contains('<') && token.contains('>')) {
                continue;
            }
            let wanted = &token[1..];
            let known = commands
                .iter()
                .any(|c| c.name() == wanted || c.aliases().contains(&wanted));
            assert!(
                known,
                "catalogue trigger {token} does not dispatch: no builtin command is named \
                 {wanted:?} or aliases it"
            );
            dispatched += 1;
        }
    }
    assert!(
        dispatched > 0,
        "no dispatchable slash trigger found — is the catalogue's trigger column empty?"
    );
}

/// base-2: every trigger token carrying `<>` (the `/<skill-name>` row) keeps
/// the slash form; it is format-checked only, never dispatched.
#[test]
fn base2_placeholder_triggers_keep_the_slash_form_and_are_not_dispatched() {
    let mut saw_placeholder = false;
    for trigger in trigger_lines() {
        if !trigger.starts_with('/') {
            continue;
        }
        for token in trigger.split_whitespace() {
            if token.contains('<') && token.contains('>') {
                assert!(
                    token.starts_with('/'),
                    "placeholder trigger {token} lost its leading slash"
                );
                saw_placeholder = true;
            }
        }
    }
    assert!(
        saw_placeholder,
        "the /<skill-name> placeholder row is missing from the catalogue"
    );
}

/// base-3: the `grok …` trigger (the Headless entry) parses under the real
/// clap model — quoted words are single values.
#[test]
fn base3_the_headless_trigger_parses_under_pager_args() {
    let headless = trigger_lines()
        .into_iter()
        .find(|t| !t.starts_with('/'))
        .expect("the catalogue must carry the grok … (Headless) trigger");
    let tokens = tokenize_quoted(&headless);
    assert!(
        tokens.first().is_some_and(|t| t == "grok"),
        "the headless trigger must lead with the program name: {tokens:?}"
    );
    PagerArgs::command()
        .try_get_matches_from(&tokens)
        .unwrap_or_else(|err| {
            panic!("headless trigger {headless:?} does not parse under PagerArgs: {err}")
        });
}

/// `grok -p "PROMPT" --allow RULE` → `[grok, -p, PROMPT, --allow, RULE]`:
/// whitespace separates tokens, quoted words are single values.
fn tokenize_quoted(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in s.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// base-4: the suggestions prompt names exactly one instruction file —
/// `AGENTS.md` occurs, and no prompt of the ten carries a `CLAUDE.md` string.
#[test]
fn base4_the_catalogue_names_exactly_one_instruction_file() {
    let suggestions = prompt("insights_suggestions");
    assert!(
        suggestions.contains("AGENTS.md"),
        "insights_suggestions must name the instruction file AGENTS.md"
    );
    for name in ALL_TEN {
        assert!(
            !prompt(name).contains("CLAUDE.md"),
            "{name} carries a CLAUDE.md string"
        );
    }
}
