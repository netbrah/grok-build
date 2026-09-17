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
