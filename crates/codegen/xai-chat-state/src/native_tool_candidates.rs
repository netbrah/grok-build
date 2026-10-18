//! Owned, nonserialized tool inputs for a bound request build.

use std::collections::BTreeSet;

use xai_grok_sampling_types::{ApiBackend, SamplingConfig, ToolExposure, ToolSpec};

#[derive(Debug, Clone)]
pub struct NativeToolCandidates {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) mcp_membership: BTreeSet<String>,
}

impl NativeToolCandidates {
    /// Preserve prepared builtin specs without adding MCP membership.
    pub fn builtins_only(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            mcp_membership: BTreeSet::new(),
        }
    }

    /// The shell supplies membership from its qualified, typed MCP snapshot.
    pub fn with_mcp_membership(specs: Vec<ToolSpec>, mcp_membership: BTreeSet<String>) -> Self {
        Self {
            specs,
            mcp_membership,
        }
    }

    /// Select MCP exposure using the captured request row.
    pub fn select_for_row(self, config: &SamplingConfig) -> Vec<ToolSpec> {
        let include_mcp = config.api_backend == ApiBackend::Messages && config.supports_search_tool;
        self.specs
            .into_iter()
            .filter_map(|mut spec| {
                if self.mcp_membership.contains(&spec.name) {
                    if !include_mcp {
                        return None;
                    }
                    spec.exposure = ToolExposure::Deferred;
                }
                Some(spec)
            })
            .collect()
    }

    /// Transform prepared specs, preserving membership only for surviving tools.
    pub fn map_specs(mut self, transform: impl FnOnce(Vec<ToolSpec>) -> Vec<ToolSpec>) -> Self {
        self.specs = transform(self.specs);
        let surviving_names: BTreeSet<_> =
            self.specs.iter().map(|spec| spec.name.as_str()).collect();
        self.mcp_membership
            .retain(|name| surviving_names.contains(name.as_str()));
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use xai_grok_sampling_types::{ApiBackend, ToolExposure};

    use super::*;

    fn prepared_specs() -> (Vec<ToolSpec>, Vec<ToolSpec>) {
        let builtins = vec![
            ToolSpec {
                name: "z_builtin".into(),
                description: None,
                parameters: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false,
                }),
                exposure: ToolExposure::Immediate,
            },
            ToolSpec {
                name: "operator__deferred".into(),
                description: Some("Operator-prepared deferred builtin".into()),
                parameters: json!({
                    "type": "object",
                    "properties": {"enabled": {"type": "boolean", "default": true}},
                    "required": [],
                    "additionalProperties": false,
                }),
                exposure: ToolExposure::Deferred,
            },
        ];
        let mcp = vec![
            ToolSpec {
                name: "alpha__lookup".into(),
                description: Some("Qualified lookup candidate".into()),
                parameters: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                    "additionalProperties": false,
                }),
                exposure: ToolExposure::Immediate,
            },
            ToolSpec {
                name: "zeta__fetch".into(),
                description: Some("Qualified fetch candidate".into()),
                parameters: json!({
                    "type": "object",
                    "properties": {"url": {"type": "string"}, "limit": {"type": "integer"}},
                    "required": ["url"],
                    "additionalProperties": false,
                }),
                exposure: ToolExposure::Immediate,
            },
        ];
        (builtins, mcp)
    }

    fn candidates(builtins: &[ToolSpec], mcp: &[ToolSpec]) -> NativeToolCandidates {
        let mut specs = builtins.to_vec();
        specs.extend_from_slice(mcp);
        NativeToolCandidates::with_mcp_membership(
            specs,
            mcp.iter().map(|spec| spec.name.clone()).collect(),
        )
    }

    fn assert_selection_matrix(
        candidates: &NativeToolCandidates,
        admitted: &[ToolSpec],
        excluded: &[ToolSpec],
        context: &str,
    ) {
        for (api_backend, supports_search_tool, expected) in [
            (ApiBackend::Messages, true, admitted),
            (ApiBackend::Messages, false, excluded),
            (ApiBackend::Responses, true, excluded),
            (ApiBackend::Responses, false, excluded),
            (ApiBackend::ChatCompletions, true, excluded),
            (ApiBackend::ChatCompletions, false, excluded),
        ] {
            let config = SamplingConfig {
                api_backend,
                supports_search_tool,
                base_url: "https://row.invalid/v1".into(),
                mtls_cert_dir: None,
                model: "candidate-test".into(),
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                max_retries: None,
                rate_limit_retry_threshold: None,
                extra_headers: Default::default(),
                conversation_group_id: None,
                query_params: Default::default(),
                env_http_headers: Default::default(),
                context_window: std::num::NonZeroU64::new(128_000).unwrap(),
                reasoning_effort: None,
                ultra_wire_effort: None,
                stream_tool_calls: None,
                cache_ttl: None,
                thinking_replay: None,
                top_k: None,
                stop_sequences: None,
                disable_parallel_tool_use: None,
                tool_cache_breakpoint: None,
                server_tools: None,
                mcp_servers: None,
                mcp_toolset_server: None,
            };
            let selected = candidates.clone().select_for_row(&config);
            assert_eq!(
                serde_json::to_value(&selected).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{context}: {:?}, supports_search_tool={supports_search_tool}",
                config.api_backend,
            );
        }
    }

    #[test]
    fn typed_candidates_select_only_mcp_for_flagged_messages() {
        let (builtins, mcp) = prepared_specs();
        let candidates = candidates(&builtins, &mcp);
        let mut admitted = builtins.clone();
        admitted.extend(mcp.into_iter().map(|mut spec| {
            spec.exposure = ToolExposure::Deferred;
            spec
        }));
        assert_selection_matrix(
            &candidates,
            &admitted,
            &builtins,
            "opaque membership selection preserves the ordered complete specs",
        );
    }

    #[test]
    fn builtin_candidates_have_empty_membership() {
        let (mut builtins, qualified_names) = prepared_specs();
        builtins.extend(qualified_names);
        assert_selection_matrix(
            &NativeToolCandidates::builtins_only(builtins.clone()),
            &builtins,
            &builtins,
            "bare specs never acquire membership from names or exposure",
        );
        assert_selection_matrix(
            &NativeToolCandidates::builtins_only(Vec::new()),
            &[],
            &[],
            "empty builtin input stays empty",
        );
    }

    #[test]
    fn selection_preserves_non_mcp_preparation_snapshot() {
        let (builtins, mcp) = prepared_specs();
        let structured_output = ToolSpec {
            name: "structured_output".into(),
            description: Some("Prepared structured answer".into()),
            parameters: json!({
                "type": "object",
                "properties": {"answer": {"type": "array", "items": {"type": "integer"}}},
                "required": ["answer"],
                "additionalProperties": false,
            }),
            exposure: ToolExposure::Immediate,
        };
        let filtered = candidates(&builtins, &mcp).map_specs(|mut specs| {
            specs.retain(|spec| spec.name != mcp[1].name);
            specs.push(structured_output.clone());
            specs
        });
        let mut survivor = mcp[0].clone();
        survivor.exposure = ToolExposure::Deferred;
        let mut admitted = builtins.clone();
        admitted.extend([survivor, structured_output.clone()]);
        let mut excluded = builtins;
        excluded.push(structured_output);
        assert_selection_matrix(
            &filtered,
            &admitted,
            &excluded,
            "filtered survivor and appended structured output preserve the snapshot",
        );

        let replacement = ToolSpec {
            name: mcp[1].name.clone(),
            description: Some("Non-MCP reuse after membership was removed".into()),
            parameters: json!({
                "type": "object",
                "properties": {"confirmed": {"type": "boolean"}},
                "required": ["confirmed"],
                "additionalProperties": false,
            }),
            exposure: ToolExposure::Immediate,
        };
        let reused_name = filtered.map_specs(|mut specs| {
            specs.push(replacement.clone());
            specs
        });
        admitted.push(replacement.clone());
        excluded.push(replacement);
        assert_selection_matrix(
            &reused_name,
            &admitted,
            &excluded,
            "pruned membership stays pruned when a later non-MCP spec reuses its name",
        );
    }
}
