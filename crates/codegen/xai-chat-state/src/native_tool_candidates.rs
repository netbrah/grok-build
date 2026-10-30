//! Owned, nonserialized tool inputs for a bound request build.

use std::collections::BTreeSet;

use xai_grok_sampling_types::{ApiBackend, SamplingConfig, ToolExposure, ToolSpec};

#[derive(Debug, Clone)]
pub struct NativeToolCandidates {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) mcp_membership: BTreeSet<String>,
    native_inputs: Option<
        std::sync::Arc<
            xai_grok_sampling_types::conversation::native_search_history::NativeEligibleInputs,
        >,
    >,
    /// Explicitly captured requirement/intent for one bound build. `None` means
    /// UNCAPTURED transport input, which is not `Inactive`; the guarded build
    /// entrypoint refuses it. It lives beside, never inside, `native_inputs`.
    history_entry: Option<crate::types::NativeHistoryEntry>,
}

impl NativeToolCandidates {
    /// Preserve prepared builtin specs without adding MCP membership.
    pub fn builtins_only(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            mcp_membership: BTreeSet::new(),
            native_inputs: None,
            history_entry: None,
        }
    }

    /// The shell supplies membership from its qualified, typed MCP snapshot.
    pub fn with_mcp_membership(specs: Vec<ToolSpec>, mcp_membership: BTreeSet<String>) -> Self {
        Self {
            specs,
            mcp_membership,
            native_inputs: None,
            history_entry: None,
        }
    }

    /// Select MCP exposure using the captured request row.
    pub fn select_for_row(self, config: &SamplingConfig) -> Vec<ToolSpec> {
        self.select_with_identity(config).0
    }

    /// Transform prepared specs, preserving membership only for surviving tools.
    pub fn map_specs(mut self, transform: impl FnOnce(Vec<ToolSpec>) -> Vec<ToolSpec>) -> Self {
        self.specs = transform(self.specs);
        self.native_inputs = None;
        // `history_entry` is deliberately untouched: an arbitrary spec map may
        // drop the identity backing, but it never erases a captured decision.
        let surviving_names: BTreeSet<_> =
            self.specs.iter().map(|spec| spec.name.as_str()).collect();
        self.mcp_membership
            .retain(|name| surviving_names.contains(name.as_str()));
        self
    }
    /// Preserve the shell-owned envelope without interpreting tool namespaces.
    pub fn with_native_inputs(
        mut self,
        inputs: Option<
            std::sync::Arc<
                xai_grok_sampling_types::conversation::native_search_history::NativeEligibleInputs,
            >,
        >,
    ) -> Self {
        self.native_inputs = inputs
            .and_then(|inputs| inputs.with_appended(&self.specs).ok())
            .map(std::sync::Arc::new);
        self
    }

    /// Attach the explicitly captured entry exactly once. A second attachment,
    /// even the same value, refuses: the capture is write-once per build.
    pub fn with_history_entry(
        mut self,
        entry: crate::types::NativeHistoryEntry,
    ) -> Result<Self, xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable>
    {
        if self.history_entry.is_some() {
            return Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::InvalidReference);
        }
        self.history_entry = Some(entry);
        Ok(self)
    }

    /// The captured entry, or a refusal when nothing was captured. The refusal is
    /// not `Inactive` — callers must not collapse the two.
    pub fn history_entry(
        &self,
    ) -> Result<
        &crate::types::NativeHistoryEntry,
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    > {
        self.history_entry.as_ref().ok_or(
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Incomplete,
        )
    }

    /// Fork transfer: consume the candidates, keep the captured entry
    /// byte-for-byte, and drop membership and identity for the new specs.
    pub fn replace_specs_without_identity(self, specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            mcp_membership: BTreeSet::new(),
            native_inputs: None,
            history_entry: self.history_entry,
        }
    }

    /// Guarded bound-build entrypoint: the row selection plus the retained
    /// captured entry. Refuses when nothing was captured, before any effect.
    pub fn select_for_bound_build(
        self,
        config: &SamplingConfig,
    ) -> Result<
        (
            Vec<ToolSpec>,
            Option<
                std::sync::Arc<
                    xai_grok_sampling_types::conversation::native_search_history::NativeEligibleInputs,
                >,
            >,
            crate::types::NativeHistoryEntry,
        ),
        xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
    >{
        let entry = self.history_entry().cloned()?;
        let (specs, identity) = self.select_with_identity(config);
        Ok((specs, identity, entry))
    }

    pub fn select_with_identity(
        self,
        config: &SamplingConfig,
    ) -> (
        Vec<ToolSpec>,
        Option<
            std::sync::Arc<
                xai_grok_sampling_types::conversation::native_search_history::NativeEligibleInputs,
            >,
        >,
    ) {
        let include_mcp = config.api_backend == ApiBackend::Messages && config.supports_search_tool;
        let mut positions = Vec::new();
        let mut specs = Vec::new();
        for (position, mut spec) in self.specs.into_iter().enumerate() {
            if self.mcp_membership.contains(&spec.name) {
                if !include_mcp {
                    continue;
                }
                spec.exposure = ToolExposure::Deferred;
            }
            positions.push(position);
            specs.push(spec);
        }
        let inputs = self
            .native_inputs
            .and_then(|inputs| inputs.select_exposure(&positions, &specs).ok())
            .map(std::sync::Arc::new);
        (specs, inputs)
    }

    pub fn filter_positions(mut self, positions: &[usize]) -> Self {
        let specs: Option<Vec<_>> = positions
            .iter()
            .map(|position| self.specs.get(*position).cloned())
            .collect();
        if let Some(specs) = specs {
            self.native_inputs = self
                .native_inputs
                .and_then(|inputs| inputs.select(positions, &specs).ok())
                .map(std::sync::Arc::new);
            self.specs = specs;
            let names: BTreeSet<_> = self.specs.iter().map(|spec| spec.name.as_str()).collect();
            self.mcp_membership
                .retain(|name| names.contains(name.as_str()));
        } else {
            self.native_inputs = None;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use xai_grok_sampling_types::{ApiBackend, ToolExposure};

    use super::*;

    #[test]
    fn native_identity_companion_tracks_surviving_specs() {
        use crate::persistence::native::digest;
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let (builtins, mcp) = prepared_specs();
        let specs: Vec<_> = builtins.iter().chain(&mcp).cloned().collect();
        let entries = specs
            .iter()
            .enumerate()
            .map(|(position, spec)| {
                let (kind, namespace, child) = if position < builtins.len() {
                    (
                        NativeNamespaceKind::GrokBuild,
                        "grok_build",
                        spec.name.as_str(),
                    )
                } else {
                    let (namespace, child) = spec.name.split_once("__").unwrap();
                    (NativeNamespaceKind::Mcp, namespace, child)
                };
                let schema = encode_native(&spec.parameters, NATIVE_SOURCE_BYTES).unwrap();
                NativeEligibleEntry::checked(
                    position,
                    kind,
                    namespace.into(),
                    child.into(),
                    spec.clone(),
                    schema.clone().into(),
                    digest(&schema),
                )
                .unwrap()
            })
            .collect();
        let inputs =
            std::sync::Arc::new(NativeEligibleInputs::checked(entries, specs.len()).unwrap());
        let candidates = candidates(&builtins, &mcp).with_native_inputs(Some(inputs.clone()));
        let mut config: SamplingConfig = serde_json::from_value(json!({
            "base_url":"https://u3.invalid", "model":"u3", "context_window":128000,
            "api_backend":"messages", "supports_search_tool":true
        }))
        .unwrap();
        let (selected, identity) = candidates.clone().select_with_identity(&config);
        let identity = identity.unwrap();
        assert!(identity.matches_specs(&selected));
        assert_eq!(identity.entries().len(), specs.len());
        assert_eq!(
            identity.entries()[2].namespace_kind(),
            NativeNamespaceKind::Mcp
        );
        assert_eq!(identity.entries()[2].qualified_namespace(), "alpha");
        assert_eq!(identity.entries()[2].child_name(), "lookup");
        assert_eq!(
            identity.entries()[2].schema_bytes(),
            serde_json::to_vec(&mcp[0].parameters).unwrap()
        );
        assert_eq!(
            identity.entries()[2].declaration().exposure,
            ToolExposure::Deferred
        );
        let (reordered, identity) = candidates
            .clone()
            .filter_positions(&[3, 0, 2])
            .select_with_identity(&config);
        let identity = identity.unwrap();
        assert!(identity.matches_specs(&reordered));
        assert_eq!(
            identity
                .entries()
                .iter()
                .map(|entry| (entry.position(), entry.child_name()))
                .collect::<Vec<_>>(),
            vec![(0, "fetch"), (1, "z_builtin"), (2, "lookup")]
        );
        assert_eq!(
            identity.entries()[0].schema_digest(),
            inputs.entries()[3].schema_digest()
        );
        let (_, ambiguous) = candidates
            .clone()
            .filter_positions(&[2, 2])
            .select_with_identity(&config);
        assert!(
            ambiguous.is_none(),
            "duplicate original positions cannot inherit identity"
        );
        let mut appended = specs.clone();
        appended.push(mcp[0].clone());
        let appended_inputs = inputs.with_appended(&appended).unwrap();
        assert!(appended_inputs.matches_specs(&appended));
        assert_eq!(appended_inputs.entries().len(), specs.len());
        assert!(
            !appended_inputs
                .entries()
                .iter()
                .any(|entry| entry.position() == specs.len())
        );
        let (_, mapped) = candidates
            .clone()
            .map_specs(|mut values| {
                values.push(values[2].clone());
                values
            })
            .select_with_identity(&config);
        assert!(
            mapped.is_none(),
            "arbitrary maps cannot back appended equal-valued stubs"
        );
        for change in 0..3 {
            let mut changed = specs.clone();
            match change {
                0 => changed[2].parameters["properties"]["query"]["default"] = json!("changed"),
                1 => changed[2].description = Some("changed declaration".into()),
                _ => changed[2].parameters["description"] = json!("changed schema description"),
            }
            let filtered = inputs
                .select(&(0..specs.len()).collect::<Vec<_>>(), &changed)
                .unwrap();
            assert_eq!(filtered.entries().len(), specs.len() - 1);
            assert!(!filtered.entries().iter().any(|entry| entry.position() == 2));
        }
        config.supports_search_tool = false;
        let (selected, identity) = candidates.select_with_identity(&config);
        assert_eq!(
            serde_json::to_value(&selected).unwrap(),
            serde_json::to_value(&builtins).unwrap()
        );
        assert!(identity.unwrap().matches_specs(&selected));
        let empty = inputs.select(&[], &[]).unwrap();
        assert!(empty.entries().is_empty());
        assert!(empty.matches_specs(&[]));
        let (_, legacy) = NativeToolCandidates::builtins_only(specs).select_with_identity(&config);
        assert!(legacy.is_none());
        assert!(!format!("{inputs:?}").contains("alpha__lookup"));
    }

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
