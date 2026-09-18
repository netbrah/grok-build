//! `build_summary_client` model-selection coverage (TDD-81 RED-2): with no `models.session_summary`
//! override the session-summary side-call must ride the SESSION model (and therefore the session's
//! wire), not a compiled catalog default; an explicit override must still win.

use super::build_minimal_agent_for_tests;
use crate::sampling::SamplerConfig;

/// Minimal session sampling config: the session's model on a throwaway endpoint.
fn session_sampling(model: &str) -> SamplerConfig {
    SamplerConfig {
        model: model.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn summary_client_without_override_uses_session_model() {
    let agent = build_minimal_agent_for_tests();
    // Live-config shape: no `models.session_summary` row and no CLI override.
    assert!(agent.cfg.borrow().session_summary_model.is_none());
    let session_cfg = session_sampling("claude-opus-5");
    let (_summary_client, summary_model) = agent
        .build_summary_client(&session_cfg)
        .expect("summary client must build");
    assert_eq!(
        summary_model,
        session_cfg.model,
        "with no session_summary override the side-call must ride the session model"
    );
}

#[tokio::test]
async fn summary_client_with_override_still_honors_override() {
    let agent = build_minimal_agent_for_tests();
    agent
        .cfg
        .borrow_mut()
        .session_summary_model
        .replace("override-summary-model".to_owned());
    let session_cfg = session_sampling("claude-opus-5");
    let (_summary_client, summary_model) = agent
        .build_summary_client(&session_cfg)
        .expect("summary client must build");
    assert_eq!(summary_model, "override-summary-model");
}
