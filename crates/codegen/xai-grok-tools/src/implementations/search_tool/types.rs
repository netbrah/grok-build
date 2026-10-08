//! Types for the `search_tool`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for the `search_tool` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchToolInput {
    /// Keywords to match against tool names, server names, and descriptions.
    /// Include the server name and action for best results
    /// (e.g. "linear create issue", "slack read thread history").
    pub query: String,
    /// Maximum number of results to return (default 5).
    ///
    /// `max_results` is the key the Messages encoder declares for this tool
    /// (`xai-grok-sampling-types/src/conversation/messages.rs`,
    /// `tool_search_d2_input_schema`: R-3.1 requires it, R-3.3 forbids `limit`
    /// in that schema). The dispatcher parses this type with plain serde
    /// (`xai-grok-tools/src/registry/types.rs:563`
    /// `serde_json::from_value::<T::Args>(json)`) and this struct carries no
    /// `deny_unknown_fields`, so without the alias a model that sends what the
    /// declaration promises has the value dropped silently and the count always
    /// defaults. Deserialize-only: the serialized form and the derived
    /// `JsonSchema` keep the `limit` spelling the Responses wire declares.
    ///
    /// Known input-strictness consequence of the alias (R-3.1/R-3.3), not a
    /// registry regression: serde folds every alias onto one field slot
    /// (serde_derive 1.0.228 `de/identifier.rs:125-136`) and errors when that
    /// slot is set twice (`de/struct_.rs:266-272`), so a payload carrying BOTH
    /// `limit` and `max_results` is rejected where the pre-alias shape silently
    /// ignored `max_results`. The dispatcher turns that into an
    /// invalid-arguments `ToolError` the model can retry — the `?` on the parse
    /// above, inside `try_parse`'s `Result<ToolInput, ToolError>`
    /// (`xai-grok-tools/src/registry/types.rs:562-565`, `:1389-1399`).
    #[serde(default = "default_limit", alias = "max_results")]
    pub limit: Option<u8>,
}

fn default_limit() -> Option<u8> {
    Some(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// apex-hw0c (TOOLSEARCH-NAME-1): the Messages encoder declares this tool
    /// with `max_results` REQUIRED and no `limit` key (R-3.1/R-3.3), so a model
    /// that sends what the declaration promises must get the behavior it
    /// implies. This drives the deserialiser the dispatcher itself uses —
    /// `crates/codegen/xai-grok-tools/src/registry/types.rs:563`
    /// `serde_json::from_value::<T::Args>(json)` with `T::Args = SearchToolInput`
    /// — not a hand-matched key.
    #[test]
    fn declared_max_results_spelling_reaches_the_limit_field() {
        let parsed: SearchToolInput = serde_json::from_value(serde_json::json!({
            "query": "shipping ETA by order ID",
            "max_results": 3,
        }))
        .expect("the declared spelling deserialises");
        assert_eq!(parsed.limit, Some(3), "max_results must not be discarded");

        // The Responses-family spelling still resolves, and an absent count
        // still defaults.
        let by_limit: SearchToolInput = serde_json::from_value(serde_json::json!({
            "query": "shipping ETA by order ID",
            "limit": 7,
        }))
        .expect("`limit` is still accepted");
        assert_eq!(by_limit.limit, Some(7));
        let defaulted: SearchToolInput = serde_json::from_value(serde_json::json!({
            "query": "shipping ETA by order ID",
        }))
        .expect("the count is optional");
        assert_eq!(defaulted.limit, Some(5), "absent count keeps default 5");

        // The alias is deserialize-only: emitting or advertising this type keeps
        // the `limit` spelling (R-3.3 — the Responses spelling must not gain a
        // `max_results` key from this alias).
        let schema = serde_json::to_value(schemars::schema_for!(SearchToolInput))
            .expect("schema serialises");
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("properties");
        assert!(
            properties.contains_key("limit"),
            "`limit` is advertised: {properties:?}"
        );
        assert!(
            !properties.contains_key("max_results"),
            "the alias must not leak `max_results` into the schema: {properties:?}"
        );
    }
}
