//! UNPIN-TOGGLE-WIRE-1 (apex-ayl.126.5): the documented operator unpin toggle
//! — a per-model `extra_headers` row entry `x-litellm-tags = ""` — must
//! produce an UNTAGGED request. The sampler client already normalizes the
//! empty pin to `None` for its `enc_affinity_pin` gate field (construction,
//! `xai-grok-sampler/src/client.rs`), but its verbatim wire loop still sends
//! the empty-valued header, and the deployed proxy rejects it (401:
//! `tags=['']`, TAGPROBE-2).
//!
//! These tests drive the production population site end-to-end: the merged
//! `[model.<id>]` row → `sampling_config_for_model` → a real
//! `SamplingClient` → a live streaming request against the mock inference
//! server, asserting on the wire headers the mock captured.

use super::*;
use xai_grok_sampling_types::rs::{CreateResponse, InputParam};
use xai_grok_sampling_types::{CreateResponseWrapper, ENC_AFFINITY_PIN_HEADER};
use xai_grok_test_support::mock_server::{LogEntry, MockInferenceServer};

/// Resolve one `[model.unpin-row]` through the production config path
/// (the merged model row → `sampling_config_for_model`), build a real
/// `SamplingClient` from the resolved `SamplerConfig`, send one streaming
/// responses request against a local mock, and return the resolved config
/// alongside the wire log entry the mock captured.
async fn wire_headers_for_row(extra_headers_toml: &str) -> (SamplerConfig, LogEntry) {
    let mock = MockInferenceServer::start()
        .await
        .expect("mock inference server should start");
    let raw_config: toml::Value = toml::from_str(&format!(
        r#"
        [model.unpin-row]
        model = "unpin-row"
        base_url = "{base_url}"
        api_key = "test-key"
        context_window = 200000
        api_backend = "responses"
        {extra_headers_toml}
        "#,
        base_url = mock.url(),
        extra_headers_toml = extra_headers_toml,
    ))
    .expect("config should parse");
    let agent_cfg = Config::new_from_toml_cfg(&raw_config).expect("config should build");
    let models = resolve_model_list(&agent_cfg, None);
    let entry = models
        .get("unpin-row")
        .expect("the [model.unpin-row] row should resolve");
    let resolved = sampling_config_for_model(
        entry,
        resolve_credentials(entry, None),
        None,
        None,
        None,
        None,
    );
    let client = xai_grok_sampler::SamplingClient::new(resolved.clone())
        .expect("sampler client should build");
    let wrapper = CreateResponseWrapper::new(CreateResponse {
        input: InputParam::Text("hi".to_owned()),
        ..Default::default()
    });
    let (_stream, _metadata, _collector) = client
        .create_response_stream(
            wrapper,
            &mut xai_grok_sampler::provider::DAnchorState::default(),
        )
        .await
        .expect("streaming request should succeed");
    let wire = mock
        .requests()
        .into_iter()
        .find(|entry| entry.path == "/v1/responses")
        .expect("mock should have logged the /v1/responses request");
    (resolved, wire)
}

/// The documented unpin toggle (`x-litellm-tags = ""` on the row) must
/// produce a truly tagless request: the resolved `SamplerConfig` carries no
/// pin entry, and the built request's wire headers do NOT contain
/// `x-litellm-tags`. Conservative: only the empty PIN entry is dropped —
/// the sibling `x-team` header still rides verbatim.
#[tokio::test]
async fn unpin_toggle_empty_pin_header_is_dropped_from_the_wire() {
    let (resolved, wire) = wire_headers_for_row(
        r#"
        [model.unpin-row.extra_headers]
        x-litellm-tags = ""
        x-team = "codegen"
        "#,
    )
    .await;
    assert!(
        resolved.extra_headers.get(ENC_AFFINITY_PIN_HEADER).is_none(),
        "the empty pin entry must be dropped from the resolved config, got {:?}",
        resolved.extra_headers
    );
    assert_eq!(
        wire.header(ENC_AFFINITY_PIN_HEADER),
        None,
        "the built request must NOT carry x-litellm-tags, wire headers: {:?}",
        wire.headers
    );
    assert_eq!(
        wire.header("x-team"),
        Some("codegen"),
        "non-pin extra headers must still ride verbatim, wire headers: {:?}",
        wire.headers
    );
}

/// A NON-empty pin still rides the wire unchanged (guard against
/// over-stripping in the unpin fix).
#[tokio::test]
async fn non_empty_pin_header_still_rides_the_wire() {
    let (resolved, wire) = wire_headers_for_row(
        r#"
        [model.unpin-row.extra_headers]
        x-litellm-tags = "East US 2"
        x-team = "codegen"
        "#,
    )
    .await;
    assert_eq!(
        resolved
            .extra_headers
            .get(ENC_AFFINITY_PIN_HEADER)
            .map(String::as_str),
        Some("East US 2")
    );
    assert_eq!(
        wire.header(ENC_AFFINITY_PIN_HEADER),
        Some("East US 2"),
        "a non-empty pin must ride the wire, wire headers: {:?}",
        wire.headers
    );
    assert_eq!(
        wire.header("x-team"),
        Some("codegen"),
        "non-pin extra headers must still ride verbatim, wire headers: {:?}",
        wire.headers
    );
}
