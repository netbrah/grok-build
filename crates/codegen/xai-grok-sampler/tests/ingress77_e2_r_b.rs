//! apex-ayl.77 INGRESS-NORMALIZE-1 — E2 L0 invariant, arm R-B (named-flag
//! opt-in) — ruling of record: the coordinator accepted R-B with a binding
//! flag spec (SDD §3.5). The flagless empty-family skip is the status quo
//! (SOL-path safety, byte-pinned by `provider::tests::
//! openai_families_are_byte_identical` incl. its `""` member); content-type
//! normalization fires for a family-less row only when the row sets the
//! named opt-in flag `normalize_content_types` (plumbing per SDD §4 D4).
//!
//! Separate compile unit on purpose (mask-free 3-wave RED plan, SDD §6):
//! both calls use the post-cut 5-arg `patch_responses_request` shape, so at
//! RED state this target is compile-RED (Wave C) while `--lib` (Wave A) and
//! `xai-grok-sampling-types` (Wave B) record their reds independently.

use xai_grok_sampler::provider::patch_responses_request;

/// R-B invariant, no-flag side: with `normalize_content_types` unset, a
/// family-less row (`None` or `Some("")`) keeps skipping content-type
/// normalization — the flagless path is byte-identical to the pre-cut
/// status quo (SOL-path safety).
#[test]
fn ingress77_e2_r_b_no_flag_empty_family_skips_normalization() {
    for family in [None, Some("".to_owned())] {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}
            ]
        });
        patch_responses_request(&mut body, family.as_deref(), None, false, false);
        assert_eq!(
            body["input"][0]["content"][0]["type"], "input_text",
            "family {family:?}: no-flag empty family must keep skipping normalization (status-quo pin)"
        );
    }
}

/// R-B invariant, flag side: with `normalize_content_types` set, a
/// family-less row on a vLLM shim MUST fire the rewrite — both content
/// parts land as `"text"` so the shim accepts the request instead of
/// pydantic-failing on input_text/output_text.
#[test]
fn ingress77_e2_r_b_flag_set_empty_family_normalization_fires() {
    for family in [None, Some("".to_owned())] {
        let mut body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}
            ]
        });
        patch_responses_request(&mut body, family.as_deref(), None, false, true);
        assert_eq!(
            body["input"][0]["content"][0]["type"], "text",
            "family {family:?}: flag-set empty family must normalize input_text -> text"
        );
        assert_eq!(
            body["input"][1]["content"][0]["type"], "text",
            "family {family:?}: flag-set empty family must normalize output_text -> text"
        );
    }
}
