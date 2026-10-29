use super::test_support::*;
use super::*;

use crate::ReasoningEffort;
use crate::gemini::{FunctionCallingMode, SKIP_THOUGHT_SIGNATURE_VALIDATOR, ThinkingLevel};

const MODEL: &str = "gemini-3-pro-preview";

fn gemini_request(items: Vec<ConversationItem>) -> ConversationRequest {
    ConversationRequest::from_items(items).with_model(MODEL)
}

fn body(req: &ConversationRequest) -> serde_json::Value {
    serde_json::to_value(build_generate_content_request(req)).unwrap()
}

fn assistant_calls_with_model(calls: &[(&str, &str)], model_id: Option<&str>) -> ConversationItem {
    let ConversationItem::Assistant(mut a) = assistant_with_calls(calls) else {
        unreachable!()
    };
    a.model_id = model_id.map(str::to_owned);
    ConversationItem::Assistant(a)
}

fn parts(body: &serde_json::Value, idx: usize) -> &Vec<serde_json::Value> {
    body["contents"][idx]["parts"].as_array().unwrap()
}

fn role(body: &serde_json::Value, idx: usize) -> &str {
    body["contents"][idx]["role"].as_str().unwrap()
}

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: Some("Read a file".into()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
        exposure: ToolExposure::default(),
    }
}

#[test]
fn system_items_become_system_instruction_not_contents() {
    let req = gemini_request(vec![
        ConversationItem::system("Be terse."),
        ConversationItem::system("Answer in French."),
        ConversationItem::user("Hi"),
    ]);
    let b = body(&req);
    let sys = b["systemInstruction"]["parts"].as_array().unwrap();
    assert_eq!(sys.len(), 2);
    assert_eq!(sys[0]["text"], "Be terse.");
    assert_eq!(sys[1]["text"], "Answer in French.");
    assert!(b["systemInstruction"].get("role").is_none());
    assert_eq!(b["contents"].as_array().unwrap().len(), 1);
    assert_eq!(role(&b, 0), "user");
    assert_eq!(parts(&b, 0)[0]["text"], "Hi");
    assert!(
        b.get("model").is_none(),
        "model rides the URL, never the body"
    );
}

#[test]
fn plain_exchange_alternates_user_and_model() {
    let req = gemini_request(vec![
        ConversationItem::user("Hi"),
        ConversationItem::assistant("Hello!"),
        ConversationItem::user("How are you?"),
    ]);
    let b = body(&req);
    assert_eq!(
        (role(&b, 0), role(&b, 1), role(&b, 2)),
        ("user", "model", "user")
    );
    assert_eq!(parts(&b, 1)[0]["text"], "Hello!");
    assert!(parts(&b, 1)[0].get("thought").is_none());
    assert!(parts(&b, 1)[0].get("thoughtSignature").is_none());
}

#[test]
fn tool_round_trip_has_function_call_then_function_response_in_user_content() {
    let req = gemini_request(vec![
        ConversationItem::user("Fix the bug"),
        assistant_with_calls(&[("call_1", "read_file")]),
        ConversationItem::tool_result("call_1", "fn main() {}"),
        ConversationItem::assistant("Done."),
    ])
    .with_tools(vec![read_tool()]);
    let b = body(&req);
    assert_eq!(role(&b, 1), "model");
    let call = &parts(&b, 1)[0]["functionCall"];
    assert_eq!(call["name"], "read_file");
    assert_eq!(call["args"], serde_json::json!({}));
    assert!(call.get("id").is_none(), "ids are local, never echoed");

    assert_eq!(role(&b, 2), "user");
    let resp = &parts(&b, 2)[0]["functionResponse"];
    assert_eq!(resp["name"], "read_file");
    assert_eq!(
        resp["response"],
        serde_json::json!({ "output": "fn main() {}" })
    );
    assert!(resp.get("id").is_none());

    assert_eq!(role(&b, 3), "model");
    assert_eq!(parts(&b, 3)[0]["text"], "Done.");
}

#[test]
fn error_tool_result_uses_error_key_and_object_results_pass_through() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        assistant_with_calls(&[("call_1", "a"), ("call_2", "b")]),
        ConversationItem::tool_result_error("call_1", "boom"),
        ConversationItem::tool_result("call_2", r#"{"files": ["x.rs"]}"#),
    ]);
    let b = body(&req);
    let p = parts(&b, 2);
    assert_eq!(p.len(), 2, "parallel results share one user content");
    assert_eq!(
        p[0]["functionResponse"]["response"],
        serde_json::json!({ "error": "boom" })
    );
    assert_eq!(
        p[1]["functionResponse"]["response"],
        serde_json::json!({ "files": ["x.rs"] })
    );
}

#[test]
fn signature_rides_first_function_call_and_others_get_bypass() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        reasoning_sibling("", "thinking about it", Some("SIG_A")),
        assistant_calls_with_model(&[("call_1", "a"), ("call_2", "b")], Some(MODEL)),
        ConversationItem::tool_result("call_1", "1"),
        ConversationItem::tool_result("call_2", "2"),
    ]);
    let b = body(&req);
    let p = parts(&b, 1);
    assert_eq!(p.len(), 2, "thought summary is not replayed: {p:#?}");
    assert_eq!(p[0]["functionCall"]["name"], "a");
    assert_eq!(p[0]["thoughtSignature"], "SIG_A");
    assert_eq!(p[1]["functionCall"]["name"], "b");
    assert_eq!(p[1]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE_VALIDATOR);
}

#[test]
fn signature_rides_last_text_part_when_no_function_calls() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        reasoning_sibling("", "hmm", Some("SIG_T")),
        ConversationItem::assistant_with_model("Answer.", MODEL),
        ConversationItem::user("thanks"),
    ]);
    let b = body(&req);
    let p = parts(&b, 1);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0]["text"], "Answer.");
    assert_eq!(p[0]["thoughtSignature"], "SIG_T");
}

#[test]
fn thought_summaries_are_never_replayed_regardless_of_thinking_replay() {
    for replay in [None, Some("off")] {
        let mut req = gemini_request(vec![
            ConversationItem::user("go"),
            reasoning_sibling("", "secret", Some("SIG")),
            ConversationItem::assistant_with_model("Answer.", MODEL),
            ConversationItem::user("ok"),
        ]);
        req.thinking_replay = replay.map(str::to_owned);
        let b = body(&req);
        assert!(
            b["contents"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|c| c["parts"].as_array().unwrap())
                .all(|p| p.get("thought").is_none()),
            "no thought part may be sent ({replay:?}): {b:#}"
        );
        let p = parts(&b, 1);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0]["thoughtSignature"], "SIG");
    }
}

#[test]
fn unsigned_function_calls_always_get_bypass_signature() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        assistant_with_calls(&[("call_1", "a")]),
        ConversationItem::tool_result("call_1", "1"),
    ]);
    let b = body(&req);
    assert_eq!(
        parts(&b, 1)[0]["thoughtSignature"],
        SKIP_THOUGHT_SIGNATURE_VALIDATOR
    );
    // Plain text never gets the bypass value.
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        ConversationItem::assistant("text"),
        ConversationItem::user("more"),
    ]);
    let b = body(&req);
    assert!(parts(&b, 1)[0].get("thoughtSignature").is_none());
}

#[test]
fn r8_suppresses_thoughts_and_signature_from_another_model() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        reasoning_sibling("", "foreign thought", Some("FOREIGN_SIG")),
        assistant_calls_with_model(&[("call_1", "a")], Some("claude-opus-4")),
        ConversationItem::tool_result("call_1", "1"),
    ]);
    let b = body(&req);
    let p = parts(&b, 1);
    assert_eq!(p.len(), 1, "foreign thought dropped: {p:#?}");
    assert_eq!(p[0]["functionCall"]["name"], "a");
    assert_eq!(
        p[0]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE_VALIDATOR,
        "a foreign signature is replaced by the bypass value"
    );
}

#[test]
fn r8_does_not_suppress_when_owner_has_no_model_id() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        reasoning_sibling("", "kept", Some("SIG")),
        assistant_with_calls(&[("call_1", "a")]),
        ConversationItem::tool_result("call_1", "1"),
    ]);
    let b = body(&req);
    let p = parts(&b, 1);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0]["thoughtSignature"], "SIG");
}

#[test]
fn r8_tolerates_canonical_model_version_and_vertex_resource_paths() {
    // Google reports the dated/versioned `modelVersion`; the row configures
    // the alias. The signature must survive the round trip.
    for owner in [
        "gemini-3-pro-preview",
        "gemini-3-pro-preview-05-20",
        "gemini-3-pro",
        "gemini-3-pro-preview-001",
        "models/gemini-3-pro-preview",
        "GEMINI-3-PRO-PREVIEW",
    ] {
        let req = gemini_request(vec![
            ConversationItem::user("go"),
            reasoning_sibling("", "kept", Some("SIG")),
            assistant_calls_with_model(&[("call_1", "a")], Some(owner)),
            ConversationItem::tool_result("call_1", "1"),
        ]);
        let b = body(&req);
        assert_eq!(parts(&b, 1)[0]["thoughtSignature"], "SIG", "owner={owner}");
    }
    // Vertex: the configured slug is a resource path, the stream reports
    // the bare canonical name.
    let mut req = gemini_request(vec![
        ConversationItem::user("go"),
        reasoning_sibling("", "kept", Some("SIG")),
        assistant_calls_with_model(&[("call_1", "a")], Some("gemini-3.5-flash-001")),
        ConversationItem::tool_result("call_1", "1"),
    ]);
    req.model =
        Some("projects/p/locations/us-central1/publishers/google/models/gemini-3.5-flash".into());
    assert_eq!(
        body(&req)["contents"][1]["parts"][0]["thoughtSignature"],
        "SIG"
    );
}

#[test]
fn same_gemini_model_rejects_variants_and_other_vendors() {
    assert!(same_gemini_model("gemini-3.5-flash", "gemini-3.5-flash"));
    assert!(same_gemini_model(
        "gemini-3.5-flash-preview-09-2025",
        "gemini-3.5-flash"
    ));
    assert!(same_gemini_model(
        "gemini-3.5-flash",
        "gemini-3.5-flash-latest"
    ));
    assert!(same_gemini_model("gemini-4-argon@001", "gemini-4-argon"));
    assert!(same_gemini_model("gemini-3-pro-exp", "models/gemini-3-pro"));
    assert!(!same_gemini_model(
        "gemini-3.5-flash-lite",
        "gemini-3.5-flash"
    ));
    assert!(!same_gemini_model(
        "gemini-3.5-flash",
        "gemini-3.5-flash-8b"
    ));
    assert!(!same_gemini_model("gemini-3-flash", "gemini-3-pro"));
    assert!(!same_gemini_model("claude-opus-4", "gemini-3-pro"));
    assert!(!same_gemini_model("", "gemini-3-pro"));
}

#[test]
fn consecutive_user_items_merge_and_leading_model_gets_continue_sentinel() {
    let req = gemini_request(vec![
        ConversationItem::assistant("I was first."),
        ConversationItem::user("a"),
        ConversationItem::user("b"),
    ]);
    let b = body(&req);
    assert_eq!(role(&b, 0), "user");
    assert_eq!(parts(&b, 0)[0]["text"], "[Continue]");
    assert_eq!(role(&b, 1), "model");
    assert_eq!(role(&b, 2), "user");
    assert_eq!(parts(&b, 2).len(), 2);
}

#[test]
fn trailing_model_content_gets_sentinel_by_shape() {
    let b = body(&gemini_request(vec![
        ConversationItem::user("go"),
        ConversationItem::assistant("done"),
    ]));
    let last = b["contents"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["parts"][0]["text"], "[Continue]");

    // An unanswered call is normally removed by orphan cleanup; a trailing
    // model content with a call survives only via a mid-run truncation, so
    // guard the sentinel choice on a hand-built case through the repair.
    let mut contents = vec![
        crate::gemini::Content::user(vec![crate::gemini::Part::text("go")]),
        crate::gemini::Content::model(vec![crate::gemini::Part::function_call(
            crate::gemini::FunctionCall {
                id: None,
                name: "a".into(),
                args: None,
            },
        )]),
    ];
    super::gemini::repair_head_and_tail(&mut contents);
    assert_eq!(contents.len(), 3);
    assert_eq!(
        contents[2].parts[0].text.as_deref(),
        Some("[Awaiting tool result]")
    );
}

#[test]
fn orphaned_tool_results_are_cleaned_before_projection() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        ConversationItem::tool_result("ghost", "never called"),
        ConversationItem::assistant("fine"),
        ConversationItem::user("ok"),
    ]);
    let b = body(&req);
    let all = serde_json::to_string(&b).unwrap();
    assert!(!all.contains("never called"), "{all}");
    assert!(!all.contains("functionResponse"), "{all}");
}

#[test]
fn images_project_to_inline_data_file_data_or_text_fallback() {
    let req = gemini_request(vec![ConversationItem::user_with_parts(vec![
        ContentPart::Text {
            text: "look".into(),
        },
        ContentPart::Image {
            url: "data:image/png;base64,AAAA".into(),
        },
        ContentPart::Image {
            url: "https://example.com/x.jpg".into(),
        },
        ContentPart::Image {
            url: "file:///tmp/x.png".into(),
        },
    ])]);
    let b = body(&req);
    let p = parts(&b, 0);
    assert_eq!(p[0]["text"], "look");
    assert_eq!(p[1]["inlineData"]["mimeType"], "image/png");
    assert_eq!(p[1]["inlineData"]["data"], "AAAA");
    assert_eq!(p[2]["fileData"]["fileUri"], "https://example.com/x.jpg");
    assert!(p[2]["fileData"].get("mimeType").is_none());
    assert_eq!(p[3]["text"], "[invalid image: file:///tmp/x.png]");
}

#[test]
fn tool_result_images_nest_under_function_response_parts() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        assistant_with_calls(&[("call_1", "screenshot")]),
        ConversationItem::tool_result_with_images(
            "call_1",
            "captured",
            vec![
                ContentPart::Image {
                    url: "data:image/jpeg;base64,BBBB".into(),
                },
                ContentPart::Image {
                    url: "https://example.com/shot.png?v=2".into(),
                },
                ContentPart::Image {
                    url: "https://example.com/opaque".into(),
                },
                ContentPart::Image {
                    url: "file:///tmp/x.png".into(),
                },
            ],
        ),
    ]);
    let b = body(&req);
    let p = parts(&b, 2);
    assert_eq!(p.len(), 3, "response + two text fallbacks: {p:#?}");
    let fr = &p[0]["functionResponse"];
    assert_eq!(fr["name"], "screenshot");
    assert_eq!(fr["response"], serde_json::json!({ "output": "captured" }));
    let nested = fr["parts"].as_array().unwrap();
    assert_eq!(nested.len(), 2);
    assert_eq!(nested[0]["inlineData"]["mimeType"], "image/jpeg");
    assert_eq!(nested[0]["inlineData"]["data"], "BBBB");
    assert_eq!(
        nested[1]["fileData"]["fileUri"],
        "https://example.com/shot.png?v=2"
    );
    assert_eq!(nested[1]["fileData"]["mimeType"], "image/png");
    assert!(p[0].get("inlineData").is_none(), "no sibling media parts");
    assert_eq!(p[1]["text"], "[image: https://example.com/opaque]");
    assert_eq!(p[2]["text"], "[invalid image: file:///tmp/x.png]");

    // No media ⇒ `parts` is omitted entirely.
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        assistant_with_calls(&[("call_1", "screenshot")]),
        ConversationItem::tool_result("call_1", "plain"),
    ]);
    let b = body(&req);
    assert!(parts(&b, 2)[0]["functionResponse"].get("parts").is_none());
}

#[test]
fn function_responses_are_hoisted_and_ordered_to_the_call_order() {
    // Two parallel calls to the same tool whose results arrive reversed,
    // with a user message interleaved in the same user content.
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        assistant_with_calls(&[
            ("call_1", "read_file"),
            ("call_2", "grep"),
            ("call_3", "read_file"),
        ]),
        ConversationItem::user("fyi"),
        ConversationItem::tool_result("call_3", "third"),
        ConversationItem::tool_result("call_2", "second"),
        ConversationItem::tool_result("call_1", "first"),
    ]);
    let b = body(&req);
    let p = parts(&b, 2);
    assert_eq!(p.len(), 4, "{p:#?}");
    let outputs: Vec<&str> = p[..3]
        .iter()
        .map(|part| {
            part["functionResponse"]["response"]["output"]
                .as_str()
                .unwrap()
        })
        .collect();
    // Positional by name: the 1st `read_file` call takes the 1st stored
    // `read_file` response (`third`, stored first), the 2nd takes `first`.
    assert_eq!(outputs, ["third", "second", "first"]);
    assert_eq!(p[0]["functionResponse"]["name"], "read_file");
    assert_eq!(p[1]["functionResponse"]["name"], "grep");
    assert_eq!(p[2]["functionResponse"]["name"], "read_file");
    assert_eq!(p[3]["text"], "fyi", "text is hoisted behind the responses");

    // A user content with no preceding model calls is left untouched.
    let req = gemini_request(vec![
        ConversationItem::user("a"),
        ConversationItem::user("b"),
    ]);
    let b = body(&req);
    assert_eq!(parts(&b, 0)[0]["text"], "a");
    assert_eq!(parts(&b, 0)[1]["text"], "b");
}

#[test]
fn tools_become_function_declarations_with_json_schema_verbatim() {
    let tool = read_tool();
    let req = gemini_request(vec![ConversationItem::user("go")]).with_tools(vec![tool.clone()]);
    let b = body(&req);
    let decls = b["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0]["name"], "read_file");
    assert_eq!(decls[0]["description"], "Read a file");
    assert_eq!(decls[0]["parametersJsonSchema"], tool.parameters);
    assert!(decls[0].get("parameters").is_none());
    assert!(b.get("toolConfig").is_none(), "no choice ⇒ no toolConfig");
}

#[test]
fn tool_choice_maps_to_function_calling_config() {
    let cases = [
        (
            ConversationToolChoice::Auto,
            FunctionCallingMode::Auto,
            None,
        ),
        (
            ConversationToolChoice::None,
            FunctionCallingMode::None,
            None,
        ),
        (
            ConversationToolChoice::Required,
            FunctionCallingMode::Any,
            None,
        ),
        (
            ConversationToolChoice::Function("read_file".into()),
            FunctionCallingMode::Any,
            Some("read_file"),
        ),
    ];
    for (choice, mode, allowed) in cases {
        let req = gemini_request(vec![ConversationItem::user("go")])
            .with_tools(vec![read_tool()])
            .with_tool_choice(choice.clone());
        let built = build_generate_content_request(&req);
        let fcc = built
            .tool_config
            .as_ref()
            .and_then(|c| c.function_calling_config.as_ref())
            .unwrap_or_else(|| panic!("{choice:?} must emit functionCallingConfig"));
        assert_eq!(fcc.mode, Some(mode), "{choice:?}");
        assert_eq!(
            fcc.allowed_function_names,
            allowed.map(|s| vec![s.to_owned()]).unwrap_or_default(),
            "{choice:?}"
        );
    }
    // Serialized enum names are the proto SCREAMING_SNAKE_CASE values.
    let req = gemini_request(vec![ConversationItem::user("go")])
        .with_tools(vec![read_tool()])
        .with_tool_choice(ConversationToolChoice::Required);
    assert_eq!(
        body(&req)["toolConfig"]["functionCallingConfig"]["mode"],
        "ANY"
    );
}

#[test]
fn json_schema_is_native_only_without_tools() {
    let schema =
        serde_json::json!({ "type": "object", "properties": { "x": { "type": "string" } } });
    let req = gemini_request(vec![ConversationItem::user("go")]).with_json_schema(schema.clone());
    let b = body(&req);
    assert_eq!(
        b["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(b["generationConfig"]["responseJsonSchema"], schema);

    let req = req.with_tools(vec![read_tool()]);
    let b = body(&req);
    assert!(b["generationConfig"].get("responseMimeType").is_none());
    assert!(b["generationConfig"].get("responseJsonSchema").is_none());
}

#[test]
fn generation_config_drops_sampling_knobs_on_level_models_and_always_requests_thoughts() {
    let mut req = gemini_request(vec![ConversationItem::user("go")]);
    req.temperature = Some(0.2);
    req.top_p = Some(0.9);
    req.top_k = Some(40);
    req.max_output_tokens = Some(1234);
    req.stop_sequences = Some(vec!["END".into()]);
    let b = body(&req);
    let gc = &b["generationConfig"];
    // Gemini 3+: the sampling parameters are deprecated (model-tuned
    // defaults); sending them degrades reasoning, so they are dropped.
    assert!(gc.get("temperature").is_none(), "{gc:#}");
    assert!(gc.get("topP").is_none());
    assert!(gc.get("topK").is_none());
    assert_eq!(gc["maxOutputTokens"], 1234);
    assert_eq!(gc["stopSequences"], serde_json::json!(["END"]));
    assert_eq!(gc["thinkingConfig"]["includeThoughts"], true);
    assert!(gc["thinkingConfig"].get("thinkingBudget").is_none());
    assert!(gc["thinkingConfig"].get("thinkingLevel").is_none());

    // Budget-controlled generations still honour them.
    req.model = Some("gemini-2.5-flash".into());
    let b = body(&req);
    let gc = &b["generationConfig"];
    assert!((gc["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert!((gc["topP"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(gc["topK"], 40);
}

#[test]
fn reasoning_effort_maps_to_level_on_gemini_3_plus_and_budget_on_older_generations() {
    assert!(is_thinking_level_model("gemini-3-pro-preview"));
    assert!(is_thinking_level_model("gemini-3.1-flash"));
    assert!(is_thinking_level_model("models/gemini-3-flash"));
    assert!(is_thinking_level_model("gemini-3.5-flash"));
    assert!(is_thinking_level_model("gemini-4-argon"));
    assert!(is_thinking_level_model(
        "projects/p/locations/l/publishers/google/models/gemini-3.5-pro"
    ));
    assert!(
        is_thinking_level_model("litellm-gemini-3.5-flash"),
        "proxy alias with a marker"
    );
    assert!(
        is_thinking_level_model("unknown-model"),
        "no marker ⇒ 3.5+ deployment assumed"
    );
    assert!(!is_thinking_level_model("gemini-2.5-pro"));
    assert!(!is_thinking_level_model("gemini-2.0-flash"));
    assert!(!is_thinking_level_model("models/gemini-2.5-flash-lite"));

    let level = thinking_config("gemini-3-pro-preview", Some(ReasoningEffort::High));
    assert_eq!(level.thinking_level, Some(ThinkingLevel::High));
    assert_eq!(level.thinking_budget, None);
    assert_eq!(level.include_thoughts, Some(true));
    // MINIMAL exists on Gemini 3 Flash models only; everywhere else the
    // floor is LOW (thinking cannot be switched off).
    for flash in [
        "gemini-3-flash",
        "gemini-3.5-flash-lite",
        "models/gemini-3.1-flash",
    ] {
        assert_eq!(
            thinking_config(flash, Some(ReasoningEffort::None)).thinking_level,
            Some(ThinkingLevel::Minimal),
            "{flash}"
        );
        assert_eq!(
            thinking_config(flash, Some(ReasoningEffort::Minimal)).thinking_level,
            Some(ThinkingLevel::Minimal),
            "{flash}"
        );
    }
    for low_floor in [
        "gemini-3-pro-preview",
        "gemini-3.1-pro",
        "gemini-4-argon",
        "gemini-4-flash",
        "prod-coder",
    ] {
        assert_eq!(
            thinking_config(low_floor, Some(ReasoningEffort::None)).thinking_level,
            Some(ThinkingLevel::Low),
            "{low_floor}"
        );
        assert_eq!(
            thinking_config(low_floor, Some(ReasoningEffort::Minimal)).thinking_level,
            Some(ThinkingLevel::Low),
            "{low_floor}"
        );
    }
    assert_eq!(
        thinking_config("gemini-4-argon", Some(ReasoningEffort::Medium)).thinking_level,
        Some(ThinkingLevel::Medium)
    );
    // No effort ⇒ no level ⇒ the model's own default depth.
    let default = thinking_config("gemini-4-argon", None);
    assert_eq!(default.thinking_level, None);
    assert_eq!(default.thinking_budget, None);

    let budget = thinking_config("gemini-2.5-pro", Some(ReasoningEffort::Medium));
    assert_eq!(budget.thinking_budget, Some(-1), "medium = dynamic");
    assert_eq!(budget.thinking_level, None);
    let budget = thinking_config("gemini-2.5-flash", Some(ReasoningEffort::None));
    assert_eq!(budget.thinking_budget, Some(0));

    let mut req = gemini_request(vec![ConversationItem::user("go")]);
    req.reasoning_effort = Some(ReasoningEffort::Low);
    assert_eq!(
        body(&req)["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "LOW"
    );
}

#[test]
#[allow(deprecated)]
fn backend_tool_records_degrade_to_text_in_the_model_turn() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        ConversationItem::BackendToolCall(BackendToolCallItem {
            kind: BackendToolKind::WebSearch(rs::WebSearchToolCall {
                id: "ws_1".to_string(),
                status: rs::WebSearchCallStatus::Completed,
                action: Some(rs::WebSearchToolCallAction::Search(
                    rs::WebSearchActionSearch {
                        query: Some("rust".to_string()),
                        queries: None,
                        sources: Some(vec![]),
                    },
                )),
            }),
        }),
        ConversationItem::assistant("Found it."),
        ConversationItem::user("ok"),
    ]);
    let b = body(&req);
    assert_eq!(b["contents"].as_array().unwrap().len(), 3);
    let p = parts(&b, 1);
    assert_eq!(p.len(), 2);
    assert!(p[0]["text"].as_str().unwrap().contains("rust"), "{p:#?}");
    assert_eq!(p[1]["text"], "Found it.");
}

#[test]
fn malformed_tool_call_arguments_degrade_to_empty_object() {
    let req = gemini_request(vec![
        ConversationItem::user("go"),
        ConversationItem::assistant_tool_calls(vec![
            ToolCall {
                id: "c1".into(),
                name: "a".into(),
                arguments: "not json".into(),
            },
            ToolCall {
                id: "c2".into(),
                name: "b".into(),
                arguments: "[1,2]".into(),
            },
            ToolCall {
                id: "c3".into(),
                name: "c".into(),
                arguments: r#"{"k":1}"#.into(),
            },
        ]),
        ConversationItem::tool_result("c1", "1"),
        ConversationItem::tool_result("c2", "2"),
        ConversationItem::tool_result("c3", "3"),
    ]);
    let b = body(&req);
    let p = parts(&b, 1);
    assert_eq!(p[0]["functionCall"]["args"], serde_json::json!({}));
    assert_eq!(p[1]["functionCall"]["args"], serde_json::json!({}));
    assert_eq!(p[2]["functionCall"]["args"], serde_json::json!({ "k": 1 }));
}

#[test]
fn request_body_round_trips_through_serde() {
    let req = gemini_request(vec![
        ConversationItem::system("sys"),
        ConversationItem::user("go"),
        reasoning_sibling("", "t", Some("S")),
        assistant_calls_with_model(&[("call_1", "a")], Some(MODEL)),
        ConversationItem::tool_result("call_1", "1"),
    ])
    .with_tools(vec![read_tool()]);
    let built = build_generate_content_request(&req);
    let json = serde_json::to_string(&built).unwrap();
    let back: crate::gemini::GenerateContentRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.contents.len(), built.contents.len());
    assert_eq!(back.contents[1].parts.len(), 1, "signature-only replay");
    assert_eq!(
        back.contents[1].parts[0].thought_signature.as_deref(),
        Some("S")
    );
    assert!(back.model.is_empty(), "model is skipped on the wire");
}
