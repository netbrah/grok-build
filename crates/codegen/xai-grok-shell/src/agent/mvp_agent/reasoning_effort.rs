//! Resolves a reasoning-effort hint against the model's advertised menu (EFFORT-SEAM-1 / apex-ayl.59 projection: identity when advertised, the model default when not) and applies it only when the model supports it; shared by session creation, model switch, and the summary client.

use agent_client_protocol as acp;
use xai_grok_sampler::SamplerConfig;
use xai_grok_sampling_types::ReasoningEffort;

use crate::agent::remote_config::ModelsManager;
use crate::sampling::EffortTarget;

impl ModelsManager {
    pub(crate) fn apply_supported_effort(
        &self,
        sampling: &mut SamplerConfig,
        effort: Option<ReasoningEffort>,
        session_id: &acp::SessionId,
        target: EffortTarget,
    ) {
        // PROACTIVE-ULTRA-1 (apex-ayl.86, SDD §3.6 seed assignment 1): the
        // menu-derived wire value is resolved against the PRE-routing model
        // before the no-hint early return, so hintless sessions and the
        // unsupported-model early return still carry a fresh value (S6-i).
        sampling.ultra_wire_effort = self.ultra_wire_effort_for(&sampling.model);
        let Some(effort) = effort else {
            return;
        };
        if !self.model_supports_reasoning_effort(&sampling.model) {
            // SummaryClient stays quiet; the spawn or switch that carried this effort already warned that the model does not support it
            if matches!(target, EffortTarget::NewSession | EffortTarget::ModelSwitch) {
                tracing::warn!(
                    session_id = %session_id.0,
                    model = %sampling.model,
                    effort = %effort,
                    "reasoning_effort: model does not support effort; ignoring it"
                );
            }
            return;
        }
        // Some models are a different model id at each effort, so swap in the id this effort asks for.
        // Do this before the log, or the log records an id we are not sending.
        if let Some(routed) = self.model_for_effort(&sampling.model, effort) {
            sampling.model = routed;
        }
        // PROACTIVE-ULTRA-1 (apex-ayl.86, SDD §3.6 seed assignment 2): re-resolve
        // against the POST-routing model (the same post-routing anchor
        // `project_effort` uses below; S6-ii).
        sampling.ultra_wire_effort = self.ultra_wire_effort_for(&sampling.model);
        // Project the carried hint onto the POST-routing model's advertised menu (codex anchor rule,
        // `project_effort`): identity when the menu advertises it, the model default when it does not.
        // A level the model offers nothing for is left unset — the config must not carry a value the
        // wire would remap or the runtime would 400 on.
        let Some(projected) = self.project_effort(&sampling.model, effort) else {
            // SummaryClient stays quiet; the spawn or switch that carried this effort already warned about it
            if matches!(target, EffortTarget::NewSession | EffortTarget::ModelSwitch) {
                tracing::warn!(
                    session_id = %session_id.0,
                    model = %sampling.model,
                    effort = %effort,
                    "reasoning_effort: effort projects to no level the model offers; leaving it unset"
                );
            }
            return;
        };
        // Same fields at every target; only the level differs
        // tracing bakes the level into a static callsite, so match a const level per arm
        macro_rules! log_applied {
            ($level:expr) => {
                tracing::event!(
                    $level,
                    session_id = %session_id.0,
                    model = %sampling.model,
                    effort = %projected,
                    target = %target.as_ref(),
                    "reasoning_effort: applied effort"
                )
            };
        }
        match target {
            EffortTarget::NewSession | EffortTarget::ModelSwitch => {
                log_applied!(tracing::Level::INFO)
            }
            EffortTarget::SummaryClient => log_applied!(tracing::Level::DEBUG),
        }
        // The projection event {model, from, to} fires only when the value moved — this is what
        // makes "stayed on ultra" visible instead of silent
        if projected != effort {
            match target {
                EffortTarget::NewSession | EffortTarget::ModelSwitch => {
                    tracing::info!(
                        session_id = %session_id.0,
                        model = %sampling.model,
                        from = %effort,
                        to = %projected,
                        target = %target.as_ref(),
                        "reasoning_effort: projected carried effort to the model's menu"
                    )
                }
                EffortTarget::SummaryClient => {
                    tracing::debug!(
                        session_id = %session_id.0,
                        model = %sampling.model,
                        from = %effort,
                        to = %projected,
                        target = %target.as_ref(),
                        "reasoning_effort: projected carried effort to the model's menu"
                    )
                }
            }
        }
        sampling.reasoning_effort = Some(projected);
    }

    /// PROACTIVE-ULTRA-1 (apex-ayl.86, ruling R-MENU-DERIVED): the wire
    /// value a locally-carried `ultra` translates to on this model's
    /// backend = the HIGHEST tier in the model's advertised menu that is
    /// not `ultra`, ranked by canonical effort order (none < minimal < low
    /// < medium < high < xhigh < max < ultra). Display order is irrelevant
    /// (rank-based; S3 pins it). `None` when the model advertises no menu,
    /// only `ultra`, or does not support effort — the egress
    /// (`provider::patch_responses_request`) then falls back to wire
    /// "max".
    pub(crate) fn ultra_wire_effort_for(&self, model_id: &str) -> Option<ReasoningEffort> {
        // A model that does not support effort has no usable menu (S4);
        // the egress then falls back to wire "max".
        if !self.model_supports_reasoning_effort(model_id) {
            return None;
        }
        self.model_reasoning_efforts(model_id)
            .iter()
            .map(|option| option.value)
            .filter(|effort| *effort != ReasoningEffort::Ultra)
            .max_by_key(|effort| Self::effort_rank(*effort))
    }

    /// Canonical effort order (R-MENU-DERIVED). Explicit because
    /// `ReasoningEffort` does not derive `Ord` (no Ord in the derive
    /// list); do NOT add a public Ord derive without adjudication.
    const fn effort_rank(effort: ReasoningEffort) -> u8 {
        match effort {
            ReasoningEffort::None => 0,
            ReasoningEffort::Minimal => 1,
            ReasoningEffort::Low => 2,
            ReasoningEffort::Medium => 3,
            ReasoningEffort::High => 4,
            ReasoningEffort::Xhigh => 5,
            ReasoningEffort::Max => 6,
            ReasoningEffort::Ultra => 7,
        }
    }
}

/// At most one variant carries the hint, so the spawn and switch consumers can never both fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewSessionEffort {
    /// Seed the spawned session's sampling config (default-model path).
    Spawn(ReasoningEffort),
    /// Apply after spawn through the model switch (explicit `modelId` path).
    Switch(ReasoningEffort),
    None,
}

/// Precedence: an explicit `_meta.reasoningEffort` wins over the process-wide last-used or `[models].default_reasoning_effort` value.
/// The catalog default is the last resort and is left on the sampling config when this returns `None`.
pub(crate) fn resolve_new_session_effort_hint(
    meta_hint: Option<ReasoningEffort>,
    current: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    meta_hint.or(current)
}

pub(crate) fn split_new_session_effort(
    resolved_custom_model: Option<&str>,
    hint: Option<ReasoningEffort>,
) -> NewSessionEffort {
    match hint {
        None => NewSessionEffort::None,
        Some(effort) if resolved_custom_model.is_some() => NewSessionEffort::Switch(effort),
        Some(effort) => NewSessionEffort::Spawn(effort),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{Config, ModelEntry, ModelInfo, ModelVariant};
    use indexmap::IndexMap;
    use serde_json::Value;
    use xai_grok_sampling_types::ReasoningEffortOption;

    fn test_manager() -> (ModelsManager, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = std::sync::Arc::new(
            xai_grok_login::AuthManager::new(tmp.path(), xai_grok_login::GrokComConfig::default()),
        );
        let manager = ModelsManager::new(
            None,
            IndexMap::new(),
            acp::ModelId::new("default"),
            auth,
            Config::default(),
        );
        (manager, tmp)
    }

    fn entry_with_menu(id: &str, menu: &[(ReasoningEffort, bool)]) -> ModelEntry {
        let mut info = ModelInfo::fallback(id);
        info.supports_reasoning_effort = true;
        info.reasoning_efforts = menu
            .iter()
            .map(|(effort, default)| ReasoningEffortOption {
                id: effort.as_str().to_owned(),
                value: *effort,
                label: effort.as_str().to_owned(),
                description: None,
                default: *default,
            })
            .collect();
        ModelEntry {
            info,
            mtls_cert_dir: None,
            api_key: None,
            env_key: None,
            auth_provider: None,
            api_base_url: None,
        }
    }

    /// S1 (SDD §4): the sol menu derives the highest non-ultra tier = max
    /// (the codex arm is frozen anyway — identical to today).
    #[test]
    fn sol_menu_derives_max() {
        let (manager, _tmp) = test_manager();
        let menu = [
            (ReasoningEffort::Low, true),
            (ReasoningEffort::Medium, false),
            (ReasoningEffort::High, false),
            (ReasoningEffort::Xhigh, false),
            (ReasoningEffort::Max, false),
            (ReasoningEffort::Ultra, false),
        ];
        manager.insert_test_entry("gpt-5.6-sol", entry_with_menu("gpt-5.6-sol", &menu));
        assert_eq!(
            manager.ultra_wire_effort_for("gpt-5.6-sol"),
            Some(ReasoningEffort::Max)
        );
    }

    /// S2 (SDD §4): the qwen ship-batch menu derives xhigh (R-ULTRA-QWEN;
    /// P1-proven 200 on the wire).
    #[test]
    fn qwen_menu_derives_xhigh() {
        let (manager, _tmp) = test_manager();
        let menu = [
            (ReasoningEffort::Ultra, false),
            (ReasoningEffort::Xhigh, true),
            (ReasoningEffort::Medium, false),
            (ReasoningEffort::Low, false),
        ];
        manager.insert_test_entry("qwen3.8-27b", entry_with_menu("qwen3.8-27b", &menu));
        assert_eq!(
            manager.ultra_wire_effort_for("qwen3.8-27b"),
            Some(ReasoningEffort::Xhigh)
        );
    }

    /// S3 (SDD §4): the derivation is rank-based, NOT array-order — ultra
    /// in the middle of the menu still derives xhigh (the divergence from
    /// the donor's array-order `rev().find`).
    #[test]
    fn derivation_is_rank_based_not_array_order() {
        let (manager, _tmp) = test_manager();
        let menu = [
            (ReasoningEffort::Medium, true),
            (ReasoningEffort::Ultra, false),
            (ReasoningEffort::Xhigh, false),
            (ReasoningEffort::Low, false),
        ];
        manager.insert_test_entry("reordered", entry_with_menu("reordered", &menu));
        assert_eq!(
            manager.ultra_wire_effort_for("reordered"),
            Some(ReasoningEffort::Xhigh)
        );
    }

    /// S4 (SDD §4): no menu (empty menu on a supported model, unsupported
    /// model, unknown model) derives None (=> egress "max", T4).
    #[test]
    fn no_menu_returns_none() {
        let (manager, _tmp) = test_manager();
        manager.insert_test_entry("menuless", entry_with_menu("menuless", &[]));
        assert_eq!(manager.ultra_wire_effort_for("menuless"), None);
        let mut unsupported = entry_with_menu("plain-no-effort", &[(ReasoningEffort::Low, true)]);
        unsupported.info.supports_reasoning_effort = false;
        manager.insert_test_entry("plain-no-effort", unsupported);
        assert_eq!(manager.ultra_wire_effort_for("plain-no-effort"), None);
        assert_eq!(manager.ultra_wire_effort_for("ghost-model"), None);
    }

    /// S5 (SDD §4): a menu advertising ONLY ultra derives None (=> egress
    /// "max" edge).
    #[test]
    fn ultra_only_menu_returns_none() {
        let (manager, _tmp) = test_manager();
        let menu = [(ReasoningEffort::Ultra, true)];
        manager.insert_test_entry("ultra-only", entry_with_menu("ultra-only", &menu));
        assert_eq!(manager.ultra_wire_effort_for("ultra-only"), None);
    }

    /// S6 (SDD §4): the seed assignments cover every `apply_supported_effort`
    /// path — (i) the hintless early return, (ii) the routed entry against
    /// the POST-routing model, (iii) the recap-shaped entry sets the field
    /// harmlessly.
    #[test]
    fn seed_assignments_cover_all_paths() {
        // (i) hintless entry: the early-return path must still maintain the field
        {
            let (manager, _tmp) = test_manager();
            let menu = [
                (ReasoningEffort::Ultra, false),
                (ReasoningEffort::Xhigh, true),
                (ReasoningEffort::Medium, false),
                (ReasoningEffort::Low, false),
            ];
            manager.insert_test_entry("qwen-ultra", entry_with_menu("qwen-ultra", &menu));
            let mut sampling = SamplerConfig::default();
            sampling.model = "qwen-ultra".into();
            manager.apply_supported_effort(
                &mut sampling,
                None,
                &acp::SessionId::new("s"),
                EffortTarget::NewSession,
            );
            assert_eq!(sampling.reasoning_effort, None, "hintless session stays effort-less");
            assert_eq!(
                sampling.ultra_wire_effort,
                Some(ReasoningEffort::Xhigh),
                "the hintless early-return path must still set ultra_wire_effort (S6-i)"
            );
        }
        // (ii) routed entry: the field anchors on the POST-routing model
        // (assignment 2, immediately after the per-effort routing swap)
        {
            let (manager, _tmp) = test_manager();
            let pre_menu = [
                (ReasoningEffort::Ultra, false),
                (ReasoningEffort::High, true),
                (ReasoningEffort::Medium, false),
                (ReasoningEffort::Low, false),
            ];
            let post_menu = [
                (ReasoningEffort::Ultra, false),
                (ReasoningEffort::Xhigh, true),
                (ReasoningEffort::High, false),
                (ReasoningEffort::Medium, false),
                (ReasoningEffort::Low, false),
            ];
            let mut pre = entry_with_menu("pre-model", &pre_menu);
            pre.info.variants = vec![ModelVariant {
                effort: ReasoningEffort::Ultra,
                model_id: "post-model".to_owned(),
            }];
            manager.insert_test_entry("pre-model", pre);
            manager.insert_test_entry("post-model", entry_with_menu("post-model", &post_menu));
            let mut sampling = SamplerConfig::default();
            sampling.model = "pre-model".into();
            manager.apply_supported_effort(
                &mut sampling,
                Some(ReasoningEffort::Ultra),
                &acp::SessionId::new("s"),
                EffortTarget::ModelSwitch,
            );
            assert_eq!(sampling.model, "post-model", "the per-effort routing swap must land");
            assert_eq!(
                sampling.reasoning_effort,
                Some(ReasoningEffort::Ultra),
                "carried ultra survives the post-routing projection (advertised)"
            );
            assert_eq!(
                sampling.ultra_wire_effort,
                Some(ReasoningEffort::Xhigh),
                "the field must anchor on the POST-routing model (S6-ii)"
            );
        }
        // (iii) recap-shaped entry: effort None, SummaryClient target — the
        // field is set harmlessly
        {
            let (manager, _tmp) = test_manager();
            let menu = [
                (ReasoningEffort::Ultra, false),
                (ReasoningEffort::Xhigh, true),
                (ReasoningEffort::Medium, false),
                (ReasoningEffort::Low, false),
            ];
            manager.insert_test_entry("qwen-ultra", entry_with_menu("qwen-ultra", &menu));
            let mut sampling = SamplerConfig::default();
            sampling.model = "qwen-ultra".into();
            sampling.reasoning_effort = None;
            manager.apply_supported_effort(
                &mut sampling,
                None,
                &acp::SessionId::new("s"),
                EffortTarget::SummaryClient,
            );
            assert_eq!(sampling.reasoning_effort, None, "the recap path stays effort-less");
            assert_eq!(
                sampling.ultra_wire_effort,
                Some(ReasoningEffort::Xhigh),
                "the recap-shaped entry sets the field harmlessly (S6-iii)"
            );
        }
    }

    /// T14a (SDD §4): the .59 gate is UNTOUCHED — an advertised ultra
    /// projects to identity, and the carried ultra reaches egress with the
    /// menu-derived wire value + the proactive item (end-to-end).
    #[test]
    fn carried_ultra_survives_projection_when_advertised() {
        let (manager, _tmp) = test_manager();
        let menu = [
            (ReasoningEffort::Ultra, false),
            (ReasoningEffort::Xhigh, true),
            (ReasoningEffort::Medium, false),
            (ReasoningEffort::Low, false),
        ];
        manager.insert_test_entry("qwen-ultra", entry_with_menu("qwen-ultra", &menu));
        assert_eq!(
            manager.project_effort("qwen-ultra", ReasoningEffort::Ultra),
            Some(ReasoningEffort::Ultra),
            "advertised ultra projects to identity (the .59 gate is untouched)"
        );
        let ultra_wire = manager.ultra_wire_effort_for("qwen-ultra");
        assert_eq!(ultra_wire, Some(ReasoningEffort::Xhigh));
        let mut body: Value = serde_json::json!({
            "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }]
        });
        xai_grok_sampler::provider::patch_responses_request(
            &mut body,
            Some("qwen"),
            Some(ReasoningEffort::Ultra),
            false,
            ultra_wire,
        );
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        let developer_items: Vec<&Value> = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item.get("role").and_then(Value::as_str) == Some("developer"))
            .collect();
        assert_eq!(developer_items.len(), 1, "the carried ultra reaches egress with the proactive item");
        assert!(
            developer_items[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("<multi_agent_mode>")
        );
    }

    /// T14b (SDD §4): a qwen menu WITHOUT max anchors a carried max to the
    /// model default (xhigh) — max never reaches the qwen wire via
    /// menu-carry.
    #[test]
    fn carried_max_anchors_to_default_on_qwen() {
        let (manager, _tmp) = test_manager();
        let menu = [
            (ReasoningEffort::Xhigh, true),
            (ReasoningEffort::High, false),
            (ReasoningEffort::Medium, false),
            (ReasoningEffort::Low, false),
        ];
        manager.insert_test_entry("qwen-nomax", entry_with_menu("qwen-nomax", &menu));
        assert_eq!(
            manager.project_effort("qwen-nomax", ReasoningEffort::Max),
            Some(ReasoningEffort::Xhigh),
            "unadvertised max anchors to the model default"
        );
    }

    /// T14c (SDD §4): a menu-less supported model projects ultra to
    /// identity (ultra ∈ LEGACY-6) — the T4 fallback edge is reachable
    /// through the real gate.
    #[test]
    fn menuless_legacy_ultra_identity() {
        let (manager, _tmp) = test_manager();
        manager.insert_test_entry("legacy-model", entry_with_menu("legacy-model", &[]));
        assert_eq!(
            manager.project_effort("legacy-model", ReasoningEffort::Ultra),
            Some(ReasoningEffort::Ultra),
            "menu-less supported rows project ultra to identity (legacy-6)"
        );
    }
}
