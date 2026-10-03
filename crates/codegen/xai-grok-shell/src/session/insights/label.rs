//! Facet labelling + JSON Schema validation (SDD §4 module map; item 7
//! cycle 2).
//!
//! Pure typed contract, no execution: `SessionFacets` (the §5:503 fence +
//! the R1 `languages` ruling), the eight const-built JSON Schemas (the
//! facet's shape + the seven section payload shapes) and the two validators
//! that wrap the mandated `workflow/schema_contract.rs` helpers. NO model
//! call, NO I/O, NO `with_json_schema` EMISSION (item 8's call path), NO
//! retry, NO badging (§6:855-862: a failed reply is an `Err` here; item 8
//! maps it to the `facet_validation_failure_sessions` badge).
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
