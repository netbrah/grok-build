use super::*;

fn tool(name: &str, description: Option<&str>, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.map(str::to_owned),
        parameters,
    }
}

fn specs() -> Vec<ToolSpec> {
    vec![
        tool(
            "read_file",
            Some("read"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "relay_to_subagent",
            Some("renamed message"),
            serde_json::json!({"required": ["text"]}),
        ),
        tool(
            "grep",
            None,
            serde_json::json!({"type": "object", "required": ["pattern"]}),
        ),
    ]
}

fn kind_for_name(name: &str) -> Option<ToolKind> {
    (name == "relay_to_subagent").then_some(ToolKind::ActiveAgentMessage)
}

#[test]
fn rebuilt_projection_removes_renamed_active_message_tool() {
    let projected = child_safe_tool_specs(specs(), ChildToolProjection::Rebuilt, kind_for_name);

    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].name, "read_file");
    assert_eq!(projected[0].description.as_deref(), Some("read"));
    assert_eq!(projected[1].name, "grep");
    assert_eq!(
        projected[1].parameters,
        serde_json::json!({"type": "object", "required": ["pattern"]})
    );
}

#[test]
fn verbatim_mirror_projection_strips_root_only_keeps_ordinary_byte_identical() {
    // Ordinary tools pass through unchanged, so the child's specs serialize to the parent's exact bytes and the radix cache stays aligned
    // The ActiveAgentMessage tool exists only at the root, so even the mirror drops it: by kind when renamed, by canonical name with no kind known
    let parent = vec![
        tool(
            "read_file",
            Some("read"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "relay_to_subagent",
            Some("renamed message"),
            serde_json::json!({"required": ["text"]}),
        ),
        tool(
            SEND_SUBAGENT_MESSAGE_TOOL_NAME,
            Some("canonical message"),
            serde_json::json!({"required": ["subagent_id", "message"]}),
        ),
        tool(
            "grep",
            None,
            serde_json::json!({"type": "object", "required": ["pattern"]}),
        ),
    ];

    let projected = child_safe_tool_specs(
        parent.clone(),
        ChildToolProjection::VerbatimMirror,
        kind_for_name,
    );
    let expected = vec![parent[0].clone(), parent[3].clone()];

    assert_eq!(
        projected
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read_file", "grep"]
    );
    assert_eq!(
        serde_json::to_vec(&projected).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );

    // With the kind lookup returning None, only the canonical-name check is left to drop the tool
    let name_only = child_safe_tool_specs(
        vec![
            tool("read_file", Some("read"), serde_json::json!({})),
            tool(
                SEND_SUBAGENT_MESSAGE_TOOL_NAME,
                Some("canonical"),
                serde_json::json!({}),
            ),
        ],
        ChildToolProjection::VerbatimMirror,
        |_| None,
    );
    assert_eq!(
        name_only
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read_file"]
    );
}

#[test]
fn verbatim_mirror_path_strips_ask_user_and_active_message() {
    // Runs the same two steps as production spawn: child_safe_tool_specs, then strip_ask_user_question_tool
    let parent = vec![
        tool(
            "read_file",
            Some("read"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "ask_user_question",
            Some("ask"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            SEND_SUBAGENT_MESSAGE_TOOL_NAME,
            Some("message"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "relay_to_subagent",
            Some("renamed"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "grep",
            None,
            serde_json::json!({"type": "object", "required": ["pattern"]}),
        ),
    ];

    let mut tools =
        child_safe_tool_specs(parent, ChildToolProjection::VerbatimMirror, kind_for_name);
    crate::agent::subagent::strip_ask_user_question_tool(&mut tools);

    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["read_file", "grep"]
    );
    assert!(tools.iter().all(|t| {
        t.name != "ask_user_question"
            && t.name != SEND_SUBAGENT_MESSAGE_TOOL_NAME
            && t.name != "relay_to_subagent"
    }));
}

#[test]
fn v2_collaboration_tools_survive_child_projection() {
    // MA-3.1 (b): the five v2 collaboration tools ride the ActiveAgentMessage
    // kind on purpose; child projection must keep them (v2 siblings message each
    // other through the mailbox) while still stripping the v1 root-only
    // active-message tool, by canonical name and by kind when renamed.
    let parent = vec![
        tool(
            "read_file",
            Some("read"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "send_message",
            Some("v2 message"),
            serde_json::json!({"required": ["target", "message"]}),
        ),
        tool(
            "followup_task",
            Some("v2 work"),
            serde_json::json!({"required": ["target", "message"]}),
        ),
        tool(
            "list_agents",
            Some("v2 list"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "wait_agent",
            Some("v2 wait"),
            serde_json::json!({"type": "object"}),
        ),
        tool(
            "interrupt_agent",
            Some("v2 interrupt"),
            serde_json::json!({"required": ["target"]}),
        ),
        tool(
            "relay_to_subagent",
            Some("renamed v1"),
            serde_json::json!({"required": ["text"]}),
        ),
        tool(
            SEND_SUBAGENT_MESSAGE_TOOL_NAME,
            Some("canonical v1"),
            serde_json::json!({"required": ["subagent_id", "message"]}),
        ),
        tool(
            "grep",
            None,
            serde_json::json!({"type": "object", "required": ["pattern"]}),
        ),
    ];
    // Mirrors the real child/parent bridge: v2 names resolve to the
    // ActiveAgentMessage kind, as does the renamed v1 tool.
    let v2_kind_for_name = |name: &str| {
        const V2: &[&str] = &[
            "send_message",
            "followup_task",
            "list_agents",
            "wait_agent",
            "interrupt_agent",
        ];
        (V2.contains(&name) || name == "relay_to_subagent")
            .then_some(ToolKind::ActiveAgentMessage)
    };

    for projection in [
        ChildToolProjection::Rebuilt,
        ChildToolProjection::VerbatimMirror,
    ] {
        let projected =
            child_safe_tool_specs(parent.clone(), projection, &v2_kind_for_name);
        assert_eq!(
            projected.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec![
                "read_file",
                "send_message",
                "followup_task",
                "list_agents",
                "wait_agent",
                "interrupt_agent",
                "grep",
            ],
            "projection {projection:?} must keep v2 collaboration tools and strip only the v1 active-message tool"
        );
    }
}
