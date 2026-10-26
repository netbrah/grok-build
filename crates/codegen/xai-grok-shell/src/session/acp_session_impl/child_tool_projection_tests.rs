use super::*;
use xai_grok_sampling_types::ToolExposure;
use xai_grok_tools::implementations::grok_build::SEND_SUBAGENT_MESSAGE_TOOL_NAME;

fn tool(name: &str, description: Option<&str>, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.map(str::to_owned),
        parameters,
        exposure: ToolExposure::default(),
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

/// The child bridge as the projection sees it: anything it does not register (parent-only tools) is unknown.
fn kind_for_name(name: &str) -> Option<ToolKind> {
    match name {
        "read_file" => Some(ToolKind::Read),
        "grep" => Some(ToolKind::Search),
        "relay_to_subagent" => Some(ToolKind::ActiveAgentMessage),
        _ => None,
    }
}

#[test]
fn rebuilt_projection_removes_renamed_active_message_tool() {
    let projected = child_safe_tool_specs(
        specs(),
        ChildToolProjection::Rebuilt,
        ChildMessagingGrant::Ungranted,
        kind_for_name,
    );

    let [read, grep] = projected.as_slice() else {
        panic!("expected two projected tools: {projected:?}");
    };
    assert_eq!(read.name, "read_file");
    assert_eq!(read.description.as_deref(), Some("read"));
    assert_eq!(grep.name, "grep");
    assert_eq!(
        grep.parameters,
        serde_json::json!({"type": "object", "required": ["pattern"]})
    );
}

#[test]
fn granted_projection_keeps_only_child_resolvable_tools() {
    let parent = specs();
    let projected = child_safe_tool_specs(
        parent,
        ChildToolProjection::VerbatimMirror,
        ChildMessagingGrant::Granted,
        kind_for_name,
    );
    assert_eq!(
        projected
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read_file", "relay_to_subagent", "grep"]
    );
}

#[test]
fn verbatim_mirror_projection_strips_root_only_keeps_ordinary_byte_identical() {
    // Ordinary tools pass through unchanged, so the child's specs serialize to the parent's exact bytes and the radix cache stays aligned
    // The active-message tool exists only at the root, so even the mirror drops it: by kind when renamed, as unresolvable when the child never registered it
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
        ChildMessagingGrant::Ungranted,
        kind_for_name,
    );
    let [first, _, _, last] = parent.as_slice() else {
        panic!("expected four parent tools: {parent:?}");
    };
    let expected = vec![first.clone(), last.clone()];

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

#[test]
fn verbatim_mirror_drops_parent_only_tools_the_child_cannot_resolve() {
    // Plan mode and ask-user exist only in the parent's bridge; the mirror must not advertise tools the child cannot run
    let parent = vec![
        tool("read_file", Some("read"), serde_json::json!({})),
        tool("enter_plan_mode", Some("plan"), serde_json::json!({})),
        tool("exit_plan_mode", Some("plan"), serde_json::json!({})),
        tool("ask_user_question", Some("ask"), serde_json::json!({})),
        tool("grep", None, serde_json::json!({})),
    ];

    let projected = child_safe_tool_specs(
        parent.clone(),
        ChildToolProjection::VerbatimMirror,
        ChildMessagingGrant::Ungranted,
        kind_for_name,
    );

    assert_eq!(
        projected
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["read_file", "grep"]
    );
    assert_eq!(
        serde_json::to_vec(&projected).unwrap(),
        serde_json::to_vec(&[
            parent.first().expect("parent tool 0").clone(),
            parent.get(4).expect("parent tool 4").clone(),
        ])
        .unwrap()
    );
}
