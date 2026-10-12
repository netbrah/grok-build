use super::*;
use serde_json::{Value, json};
use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
use xai_grok_sampling_types::{ConversationItem, ToolCall};

const CONVERSATION_ID: &str = "apex-5nnr-stable-prefix";
const ORDINARY_ID: &str = "call_5nnr_ordinary";
const ORDINARY_ARGUMENTS: &str = r#"{"path":"src/ordinary.rs","limit":7}"#;
const ORDINARY_OUTPUT: &str = "ordinary result must remain at its own position";

fn prefix_items() -> Vec<ConversationItem> {
    vec![
        ConversationItem::system("stable nonempty system prefix"),
        ConversationItem::user("stable nonempty user prefix"),
    ]
}

fn prefix_wire() -> Vec<Value> {
    vec![
        json!({"type": "message", "role": "system", "content": "stable nonempty system prefix"}),
        json!({"type": "message", "role": "user", "content": "stable nonempty user prefix"}),
    ]
}

fn policy_wire() -> Value {
    json!({
        "type": "message",
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": "<multi_agent_mode>explicit_request_only</multi_agent_mode>"
        }]
    })
}

fn native_call() -> Value {
    json!({
        "type": "tool_search_call",
        "id": "tsc_5nnr_opaque_call",
        "call_id": "call_5nnr_native",
        "execution": "client",
        "status": "completed",
        "arguments": {"query": "ordinary and native must coexist", "limit": 3},
        "opaque_extension": {"retain": [1, "verbatim", null]}
    })
}

fn native_output() -> Value {
    json!({
        "type": "tool_search_output",
        "id": "tso_5nnr_opaque_output",
        "call_id": "call_5nnr_native",
        "execution": "client",
        "status": "completed",
        "tools": [{
            "type": "function",
            "name": "shipping__lookup_eta",
            "parameters": {"type": "object", "properties": {"order_id": {"type": "string"}}}
        }],
        "opaque_extension": {"retain": [false, {"payload": "unchanged"}]}
    })
}

fn discovery(raw: Value) -> ConversationItem {
    ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("valid discovery fixture"),
    }
}

fn mixed_items() -> Vec<ConversationItem> {
    vec![
        discovery(native_call()),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: ORDINARY_ID.into(),
            name: "read_file".to_owned(),
            arguments: ORDINARY_ARGUMENTS.into(),
        }]),
        discovery(native_output()),
        ConversationItem::tool_result(ORDINARY_ID, ORDINARY_OUTPUT),
    ]
}

fn mixed_wire() -> Vec<Value> {
    vec![
        native_call(),
        json!({
            "type": "function_call",
            "call_id": ORDINARY_ID,
            "name": "read_file",
            "arguments": ORDINARY_ARGUMENTS
        }),
        native_output(),
        json!({
            "type": "function_call_output",
            "call_id": ORDINARY_ID,
            "output": ORDINARY_OUTPUT
        }),
    ]
}

async fn capture_conversation(
    streaming: bool,
    request: ConversationRequest,
    anchor: &mut Option<MultiAgentModeAnchor>,
) -> Value {
    let (mut client, body_rx, server) = spawn_capture_client(streaming).await;
    client.defaults.model_family = Some("codex".to_owned());
    if streaming {
        let (mut stream, ..) = client
            .conversation_stream_responses(request, anchor)
            .await
            .expect("actual streaming request builder sends");
        while let Some(item) = stream.next().await {
            item.expect("hermetic capture stream succeeds");
        }
    } else {
        client
            .conversation_responses(request, anchor)
            .await
            .expect("actual unary request builder sends");
    }
    let body = captured_body(&body_rx.await.expect("actual POST body captured"));
    server.abort();
    println!("apex-5nnr captured streaming={streaming} body={body}");
    body
}

async fn assert_anchored_mixed_builder(streaming: bool) {
    let mut anchor = None;
    let first = capture_conversation(
        streaming,
        ConversationRequest::from_items(prefix_items()).with_conv_id(CONVERSATION_ID),
        &mut anchor,
    )
    .await;
    let mut first_expected = prefix_wire();
    first_expected.push(policy_wire());
    assert_eq!(
        first["input"],
        json!(first_expected),
        "seed request keeps the full prefix and tail policy"
    );
    let first_anchor = anchor
        .clone()
        .expect("the real seed builder persists its anchor");
    assert_eq!(first_anchor.position, 2);

    let mut items = prefix_items();
    items.extend(mixed_items());
    let second = capture_conversation(
        streaming,
        ConversationRequest::from_items(items).with_conv_id(CONVERSATION_ID),
        &mut anchor,
    )
    .await;
    assert_eq!(
        anchor,
        Some(first_anchor),
        "the actual unchanged prefix pins the same policy anchor"
    );
    let mut expected = first_expected;
    expected.extend(mixed_wire());
    assert_eq!(
        second["input"],
        json!(expected),
        "complete mixed input must preserve prefix, pinned policy, both opaque native values, ordinary call and ordinary result; streaming={streaming}"
    );
}

#[tokio::test]
async fn unary_builder_preserves_complete_anchored_mixed_input() {
    assert_anchored_mixed_builder(false).await;
}

#[tokio::test]
async fn stream_builder_preserves_complete_anchored_mixed_input() {
    assert_anchored_mixed_builder(true).await;
}

#[tokio::test]
async fn both_builders_preserve_legacy_and_first_tail_placement() {
    for streaming in [false, true] {
        for legacy in [false, true] {
            let mut items = prefix_items();
            items.extend(mixed_items());
            items.push(ConversationItem::user("final user"));
            let request = ConversationRequest::from_items(items).with_conv_id(if legacy {
                ""
            } else {
                CONVERSATION_ID
            });
            let mut anchor = None;
            let body = capture_conversation(streaming, request, &mut anchor).await;
            let mut expected = prefix_wire();
            expected.extend(mixed_wire());
            if legacy {
                expected.push(policy_wire());
                expected.push(json!({"type": "message", "role": "user", "content": "final user"}));
                assert_eq!(
                    anchor, None,
                    "empty conversation ID must not create an anchor"
                );
            } else {
                expected.push(json!({"type": "message", "role": "user", "content": "final user"}));
                expected.push(policy_wire());
                assert_eq!(anchor.as_ref().unwrap().position, 7);
            }
            assert_eq!(
                body["input"],
                json!(expected),
                "whole input preserves legacy={legacy}, streaming={streaming}"
            );
        }
    }
}

#[tokio::test]
async fn both_builders_reanchor_reset_prefix_at_tail_without_moving_raw_values() {
    for streaming in [false, true] {
        let mut anchor = None;
        capture_conversation(
            streaming,
            ConversationRequest::from_items(prefix_items()).with_conv_id(CONVERSATION_ID),
            &mut anchor,
        )
        .await;
        let previous = anchor.clone();
        let mut items = vec![
            ConversationItem::system("reset system prefix"),
            ConversationItem::user("stable nonempty user prefix"),
        ];
        items.extend(mixed_items());
        let body = capture_conversation(
            streaming,
            ConversationRequest::from_items(items).with_conv_id(CONVERSATION_ID),
            &mut anchor,
        )
        .await;
        let mut expected = prefix_wire();
        expected[0]["content"] = json!("reset system prefix");
        expected.extend(mixed_wire());
        expected.push(policy_wire());
        assert_eq!(body["input"], json!(expected));
        assert_ne!(anchor, previous);
        assert_eq!(
            anchor.unwrap().position,
            6,
            "reset uses existing tail reanchor rule"
        );
    }
}

#[tokio::test]
async fn both_builders_restore_xai_native_carrier_without_changing_ordinary_items() {
    use xai_grok_sampling_types::{BackendToolCallItem, BackendToolKind};
    for streaming in [false, true] {
        let mut anchor = None;
        let first = capture_conversation(
            streaming,
            ConversationRequest::from_items(prefix_items()).with_conv_id(CONVERSATION_ID),
            &mut anchor,
        )
        .await;
        let arguments = r#"{"type":"search","query":"opaque xai query"}"#;
        let native = ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::XSearch(
                serde_json::from_value(json!({
                    "id": "xsc_5nnr", "call_id": "call_xai_5nnr",
                    "name": "x_search", "input": arguments
                }))
                .unwrap(),
            ),
        });
        let mut items = prefix_items();
        items.push(native);
        items.extend([mixed_items()[1].clone(), mixed_items()[3].clone()]);
        let (mut client, body_rx, server) = spawn_capture_client(streaming).await;
        client.defaults.model_family = Some("xai".to_owned());
        let request = ConversationRequest::from_items(items).with_conv_id(CONVERSATION_ID);
        if streaming {
            let (mut stream, ..) = client
                .conversation_stream_responses(request, &mut anchor)
                .await
                .unwrap();
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
        } else {
            client
                .conversation_responses(request, &mut anchor)
                .await
                .unwrap();
        }
        let body = captured_body(&body_rx.await.unwrap());
        server.abort();
        println!("apex-5nnr xai captured streaming={streaming} body={body}");
        let mut expected = prefix_wire();
        expected.push(json!({
            "type": "message", "role": "developer", "content": [{
                "type": "input_text",
                "text": "<multi_agent_mode>Multi-agent mode: explicit_request_only — subagents may be spawned only when the user explicitly requests.</multi_agent_mode>"
            }]
        }));
        expected.push(json!({
            "type": "x_search_call", "id": "xsc_5nnr", "call_id": "call_xai_5nnr",
            "name": "x_search", "status": "completed", "arguments": arguments,
            "action": {"type": "search", "query": "opaque xai query"}
        }));
        expected.extend([mixed_wire()[1].clone(), mixed_wire()[3].clone()]);
        assert_eq!(body["input"], json!(expected));
        assert_eq!(
            &body["input"].as_array().unwrap()[..2],
            &first["input"].as_array().unwrap()[..2]
        );
        assert_eq!(anchor.unwrap().position, 2);
    }
}

#[tokio::test]
async fn both_builders_keep_native_content_raw_and_project_strict_reasoning_last() {
    use xai_grok_sampling_types::{BackendToolCallItem, BackendToolKind, CodexRawInputItem};
    for streaming in [false, true] {
        let mut anchor = None;
        capture_conversation(
            streaming,
            ConversationRequest::from_items(prefix_items()).with_conv_id(CONVERSATION_ID),
            &mut anchor,
        )
        .await;
        let raw_message = json!({
            "type": "message", "id": "opaque_message_5nnr", "role": "assistant",
            "content": [{"type": "output_text", "text": "native text must not normalize", "opaque": true}],
            "opaque_extension": {"keep": "exact"}
        });
        let raw_reasoning = json!({
            "type": "reasoning", "id": "rs_native_5nnr",
            "content": [{"type": "reasoning_text", "text": "strict content must be removed"}],
            "summary": [{"type": "summary_text", "text": "retained summary"}],
            "encrypted_content": "opaque-encrypted-control"
        });
        let raw_item = |raw: Value| {
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                    id: raw["id"].as_str().unwrap().to_owned(),
                    raw,
                    cross_provider_fallback: None,
                    mint_tag: None,
                }),
            })
        };
        let mut items = prefix_items();
        items.extend([
            raw_item(raw_message.clone()),
            raw_item(raw_reasoning.clone()),
        ]);
        let (mut client, body_rx, server) = spawn_capture_client(streaming).await;
        client.defaults.model_family = Some("codex".to_owned());
        client.defaults.normalize_content_types = true;
        client.defaults.strict_responses_input = true;
        let request = ConversationRequest::from_items(items).with_conv_id(CONVERSATION_ID);
        if streaming {
            let (mut stream, ..) = client
                .conversation_stream_responses(request, &mut anchor)
                .await
                .unwrap();
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
        } else {
            client
                .conversation_responses(request, &mut anchor)
                .await
                .unwrap();
        }
        let body = captured_body(&body_rx.await.unwrap());
        server.abort();
        println!("apex-5nnr order captured streaming={streaming} body={body}");
        let mut expected = prefix_wire();
        let mut normalized_policy = policy_wire();
        normalized_policy["content"][0]["type"] = json!("text");
        expected.push(normalized_policy);
        expected.push(raw_message);
        let mut strict_reasoning = raw_reasoning;
        strict_reasoning.as_object_mut().unwrap().remove("id");
        strict_reasoning.as_object_mut().unwrap().remove("content");
        expected.push(strict_reasoning);
        assert_eq!(
            body["input"],
            json!(expected),
            "normalization precedes raw restore; strict replay follows it"
        );
        assert_eq!(anchor.unwrap().position, 2);
    }
}

#[tokio::test]
async fn both_builders_without_native_replacements_keep_exact_request_bytes() {
    for streaming in [false, true] {
        let mut anchor = None;
        let items = prefix_items();
        let request = ConversationRequest::from_items(items).with_conv_id(CONVERSATION_ID);
        let (mut expected_client, body_rx, server) = spawn_capture_client(streaming).await;
        expected_client.defaults.model_family = Some("codex".to_owned());
        let mut defaulted = request.clone();
        expected_client
            .apply_conversation_defaults(&mut defaulted)
            .unwrap();
        let mut wrapper = CreateResponseWrapper::new(rs::CreateResponse::from(&defaulted));
        expected_client
            .apply_response_defaults(&mut wrapper)
            .unwrap();
        if streaming {
            wrapper.inner.stream = Some(true);
        }
        let mut expected = serde_json::to_value(&wrapper.inner).unwrap();
        xai_grok_sampling_types::patch_reasoning_text_types(&mut expected);
        xai_grok_sampling_types::patch_reasoning_empty_ids(&mut expected, "test-model");
        crate::provider::patch_responses_request(
            &mut expected,
            Some("codex"),
            None,
            false,
            None,
            &mut DAnchorState::Anchor(None),
        );
        xai_grok_sampling_types::strip_sentinel_web_search_actions(&mut expected);
        patch_raw_input_replacements(&mut expected, &[]).unwrap();
        crate::provider::project_strict_responses_input(&mut expected, false);
        if streaming {
            let (mut stream, ..) = expected_client
                .conversation_stream_responses(request, &mut anchor)
                .await
                .unwrap();
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
        } else {
            expected_client
                .conversation_responses(request, &mut anchor)
                .await
                .unwrap();
        }
        let raw = body_rx.await.unwrap();
        let actual = captured_body(&raw);
        server.abort();
        assert_eq!(
            raw.as_ref(),
            serde_json::to_vec(&expected).unwrap().as_slice(),
            "the actual POST bytes are unchanged"
        );
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap(),
            "empty-replacement request retains existing whole body bytes"
        );
    }
}

fn replacement(index: usize, value: Value) -> xai_grok_sampling_types::RawInputItemReplacement {
    xai_grok_sampling_types::RawInputItemReplacement {
        input_item_index: index,
        value,
    }
}

fn stale_policy(text: &str) -> Value {
    json!({
        "type": "message", "role": "developer",
        "content": [{"type": "input_text", "text": text}]
    })
}

#[test]
fn rebasing_uses_actual_removals_and_insertion_on_both_sides() {
    let retained = [
        json!({"type": "message", "role": "system", "content": "prefix"}),
        json!({"type": "message", "role": "assistant", "content": "placeholder-before"}),
        json!({"type": "message", "role": "assistant", "content": "placeholder-middle"}),
        json!({"type": "message", "role": "assistant", "content": "placeholder-after"}),
        json!({"type": "message", "role": "user", "content": "last user"}),
    ];
    let mut seeded = json!({"input": retained[..2]});
    let mut anchored = DAnchorState::Anchor(None);
    crate::provider::patch_responses_request(
        &mut seeded,
        Some("codex"),
        None,
        false,
        None,
        &mut anchored,
    );
    let original = json!({"input": [
        retained[0],
        stale_policy("prefix <multi_agent_mode>old-one</multi_agent_mode>"),
        retained[1],
        stale_policy("<multi_agent_mode>old-two</multi_agent_mode>"),
        retained[2],
        retained[3],
        stale_policy("<multi_agent_mode>old-three</multi_agent_mode>"),
        retained[4]
    ]});
    let raw_values = [
        json!({"type": "compaction", "encrypted_content": "before-first-removal"}),
        native_call(),
        native_output(),
        json!({"type": "compaction", "encrypted_content": "between-removals"}),
        json!({"type": "message", "role": "user", "content": "after-last-removal", "opaque": [3, null]}),
    ];
    let replacements = [
        replacement(0, raw_values[0].clone()),
        replacement(2, raw_values[1].clone()),
        replacement(4, raw_values[2].clone()),
        replacement(5, raw_values[3].clone()),
        replacement(7, raw_values[4].clone()),
    ];
    for (name, mut anchor, insert_at) in [
        ("pinned", anchored, 2),
        ("legacy", DAnchorState::Legacy, 4),
        ("first-tail", DAnchorState::Anchor(None), 5),
    ] {
        let mut body = original.clone();
        let rebase = RawInputReplacementRebase::capture(&body, &replacements).unwrap();
        crate::provider::patch_responses_request(
            &mut body,
            Some("codex"),
            None,
            false,
            None,
            &mut anchor,
        );
        rebase.restore(&mut body).unwrap();
        let mut expected = retained.to_vec();
        expected.clone_from_slice(&raw_values);
        expected.insert(insert_at, policy_wire());
        assert_eq!(
            body["input"],
            json!(expected),
            "actual removals/insertion preserve every slot: {name}"
        );
    }
}

#[test]
fn duplicate_replacements_preserve_original_order_and_last_write_semantics() {
    let mut body = json!({"input": [
        {"type": "message", "role": "assistant", "content": "placeholder"},
        {"type": "message", "role": "user", "content": "unrelated"}
    ]});
    let replacements = [
        replacement(0, native_call()),
        replacement(0, native_output()),
    ];
    let rebase = RawInputReplacementRebase::capture(&body, &replacements).unwrap();
    crate::provider::patch_responses_request(
        &mut body,
        Some("codex"),
        None,
        false,
        None,
        &mut DAnchorState::Legacy,
    );
    rebase.restore(&mut body).unwrap();
    assert_eq!(
        body["input"],
        json!([
            native_output(), policy_wire(), {"type": "message", "role": "user", "content": "unrelated"}
        ])
    );
}

#[test]
fn invalid_original_targets_are_rejected_before_any_write() {
    let body = json!({"input": [
        {"type": "message", "role": "assistant", "content": "unrelated"},
        stale_policy("<multi_agent_mode>remove-me</multi_agent_mode>")
    ]});
    for index in [1, 2, usize::MAX] {
        let before = body.clone();
        let replacements = [
            replacement(0, native_call()),
            replacement(index, native_output()),
        ];
        assert!(matches!(
            RawInputReplacementRebase::capture(&body, &replacements),
            Err(SamplingError::InvalidConfiguration(_))
        ));
        assert_eq!(
            body, before,
            "a later invalid target cannot overwrite an earlier unrelated slot"
        );
    }
    for malformed in [json!({}), json!({"input": "not an array"})] {
        let replacements = [replacement(0, native_call())];
        assert!(matches!(
            RawInputReplacementRebase::capture(&malformed, &replacements),
            Err(SamplingError::InvalidConfiguration(_))
        ));
    }
}

#[test]
fn unexpected_provider_length_or_policy_shape_cannot_partially_overwrite() {
    let original = json!({"input": [
        {"type": "message", "role": "assistant", "content": "first unrelated slot"},
        {"type": "message", "role": "assistant", "content": "second unrelated slot"}
    ]});
    let replacements = [
        replacement(0, native_call()),
        replacement(1, native_output()),
    ];
    let mut valid = original.clone();
    crate::provider::patch_responses_request(
        &mut valid,
        Some("codex"),
        None,
        false,
        None,
        &mut DAnchorState::Legacy,
    );
    let mut wrong_length = valid.clone();
    wrong_length["input"].as_array_mut().unwrap().pop();
    let mut zero_policies = valid.clone();
    zero_policies["input"][2] =
        json!({"type": "message", "role": "assistant", "content": "not a policy"});
    let mut multiple_policies = valid;
    multiple_policies["input"][1] = stale_policy("<multi_agent_mode>extra</multi_agent_mode>");
    for mut body in [
        wrong_length,
        zero_policies,
        multiple_policies,
        json!({"input": null}),
    ] {
        let before = body.clone();
        let rebase = RawInputReplacementRebase::capture(&original, &replacements).unwrap();
        assert!(matches!(
            rebase.restore(&mut body),
            Err(SamplingError::InvalidConfiguration(_))
        ));
        assert_eq!(
            body, before,
            "all policy shape checks precede every assignment"
        );
    }
}

#[test]
fn no_replacements_keep_body_bytes_and_legacy_error_behavior() {
    for mut body in [
        json!({}),
        json!({"input": "unchanged"}),
        json!({"input": []}),
        json!({"input": [stale_policy("<multi_agent_mode>unchanged</multi_agent_mode>")]}),
    ] {
        let before = serde_json::to_vec(&body).unwrap();
        let rebase = RawInputReplacementRebase::capture(&body, &[]).unwrap();
        rebase.restore(&mut body).unwrap();
        assert_eq!(serde_json::to_vec(&body).unwrap(), before);
        patch_raw_input_replacements(&mut body, &[]).unwrap();
        assert_eq!(serde_json::to_vec(&body).unwrap(), before);
    }
    let mut expected = json!({"input": [
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "normalization control"}]}
    ]});
    let mut actual = expected.clone();
    let rebase = RawInputReplacementRebase::capture(&actual, &[]).unwrap();
    crate::provider::patch_responses_request(
        &mut expected,
        Some("glm"),
        None,
        false,
        None,
        &mut DAnchorState::Legacy,
    );
    crate::provider::patch_responses_request(
        &mut actual,
        Some("glm"),
        None,
        false,
        None,
        &mut DAnchorState::Legacy,
    );
    patch_raw_input_replacements(&mut expected, &[]).unwrap();
    rebase.restore(&mut actual).unwrap();
    assert_eq!(
        serde_json::to_vec(&actual).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}
