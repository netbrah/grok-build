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
//! Item 1 ships only the pure half: the §5 types, the §5 consts, `digest_one`
//! and `is_user_turn`. No orchestration here — `handle_insights` lands with the
//! command entry (item 5), and `project()` / the tag neutraliser land in item 4
//! (SDD §13). No file I/O in this tree yet.

pub mod aggregate;
pub mod digest;
pub mod label;
pub mod report;
pub mod types;

pub use digest::{
    CACHE_SCHEMA_VERSION, DegradedTier, DigestInput, IDLE_GAP_SECS, ProjectionCaps, ReaderFacts,
    SessionDigest, SessionFacts, TranscriptContent, digest_one, is_user_turn,
};
pub use label::{
    FACET_CACHE_SCHEMA_VERSION, SessionFacets, facet_schema, friction_schema, fun_ending_schema,
    horizon_schema, interaction_style_schema, project_areas_schema, suggestions_schema,
    validate_facet_reply, validate_section_reply, what_works_schema,
};

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
}
