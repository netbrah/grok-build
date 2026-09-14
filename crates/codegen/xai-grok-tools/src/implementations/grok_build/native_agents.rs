//! Native (v2) multi-agent tools — the six model-facing entry points for
//! spawning and operating named background agents within the caller's team
//! scope.
//!
//! Provenance: re-expressed from open-grok@240c99c9
//! `crates/codegen/xai-grok-tools/src/implementations/codex/multi_agent_v2.rs`.
//! ADAPTATION vs source (named in the MA-2 report):
//! - the `codex` namespace becomes `grok_build` (the WT host has no codex
//!   namespace);
//! - `SpawnAgentInput` omits `reasoning_effort`/`service_tier` (spec D-9):
//!   the schema is `deny_unknown_fields`, so a model sending either gets a
//!   model-visible schema error;
//! - the v0 transport is pinned plaintext (`encrypted: false`); the
//!   source's `NativeMessageEncryption` dispatch-context lookup has no WT
//!   counterpart (spec Q3/D-3 — the F3 encrypted carrier is a later stage);
//! - `spawn` hands the parsed `fork_turns`/context to the host through the
//!   `NativeAgentSpawn` tool-call context because the WT `TaskToolInput`
//!   schema is pinned and carries no `context` field (spec D-8);
//! - the `NativeAgentsEnabled` gate resource lives in `task::types`
//!   (registered there), not redefined here.

use crate::implementations::grok_build::task::backend::SubagentBackendResource;
use crate::implementations::grok_build::task::types::{
    AgentMailboxIdentity, AgentMailboxMessage, AgentMailboxMessageKind, NativeAgentMessage,
    NativeAgentOperation, NativeAgentSpawn, NativeAgentsEnabled,
};
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::types::tool_metadata::{ToolMetadata, shared_resources};
use serde::{Deserialize, Serialize};
use xai_tool_runtime::{Tool, ToolCallContext, ToolError};
use xai_tool_types::SubagentContextRequest;

/// `spawn_agent` input. `reasoning_effort` and `service_tier` are
/// deliberately absent (spec D-9): `deny_unknown_fields` rejects them
/// model-visibly.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnAgentInput {
    /// Stable lowercase name for the named agent: letters, digits, `_`.
    pub task_name: String,
    /// The work item delivered to the child as its first agent message.
    pub message: String,
    /// Subagent type to run (defaults to `general-purpose`).
    pub agent_type: Option<String>,
    /// Optional explicit model slug for the child.
    pub model: Option<String>,
    /// Initial context: `none` (fresh), `all` (inherit the parent
    /// conversation; default), or a positive integer string for the most
    /// recent N turns.
    pub fork_turns: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageInput {
    /// Canonical task path, relative task name, or agent ID.
    pub target: String,
    /// The message body (nonempty, at most 32768 bytes).
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    /// Optional canonical task-path prefix filter (no trailing slash).
    pub path_prefix: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitInput {
    /// Deadline in milliseconds. Defaults to 30000; maximum 600000; 0 polls.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterruptInput {
    /// The agent whose current turn should be interrupted.
    pub target: String,
}

macro_rules! dynamic_input {
    ($($input:ty),+ $(,)?) => {$(
        impl From<$input> for crate::types::tool_io::ToolInput {
            fn from(input: $input) -> Self {
                Self::Dynamic(serde_json::to_value(input).expect("native tool input is serializable"))
            }
        }
    )+};
}
dynamic_input!(SpawnAgentInput, MessageInput, ListInput, WaitInput, InterruptInput);

/// The TOOLS-DARK gate (spec G18): every tool in this module fails with a
/// model-visible `tool_unavailable` error unless the host injected
/// `NativeAgentsEnabled(true)`. In MA-2 the only injection path is tests.
async fn resources(
    ctx: &ToolCallContext,
) -> Result<(SubagentBackendResource, AgentMailboxIdentity), ToolError> {
    let resources = shared_resources(ctx)?;
    let resources = resources.lock().await;
    if !resources
        .get::<NativeAgentsEnabled>()
        .is_some_and(|enabled| enabled.0)
    {
        return Err(ToolError::custom(
            "tool_unavailable",
            "Native multi-agent v2 is not enabled for this session",
        ));
    }
    let backend = resources
        .get::<SubagentBackendResource>()
        .cloned()
        .ok_or_else(|| {
            ToolError::custom("missing_resource", "Subagent coordinator is unavailable")
        })?;
    let identity = resources
        .get::<AgentMailboxIdentity>()
        .cloned()
        .ok_or_else(|| {
            ToolError::custom("missing_resource", "Agent identity is unavailable")
        })?;
    Ok((backend, identity))
}

/// Builds the mailbox message envelope: nonempty body, 32768-byte cap,
/// `amsg_{uuidv7}` id. The v0 transport is pinned plaintext (`encrypted`
/// false; spec Q3/D-3).
fn message(
    identity: &AgentMailboxIdentity,
    body: String,
    kind: AgentMailboxMessageKind,
) -> Result<AgentMailboxMessage, ToolError> {
    if body.trim().is_empty() || body.len() > 32 * 1024 {
        return Err(ToolError::invalid_arguments(
            "message must be nonempty and at most 32768 bytes",
        ));
    }
    Ok(AgentMailboxMessage {
        message_id: format!("amsg_{}", uuid::Uuid::now_v7()),
        team_scope_id: identity.team_scope_id.clone(),
        from_agent_id: identity.agent_id.clone(),
        to_agent_id: String::new(),
        kind,
        body,
        created_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        native: Some(NativeAgentMessage {
            author: String::new(),
            recipient: String::new(),
            encrypted: false,
            trigger_prompt_id: None,
        }),
    })
}

async fn operate(
    ctx: &ToolCallContext,
    operation: NativeAgentOperation,
) -> Result<ToolOutput, ToolError> {
    let (backend, identity) = resources(ctx).await?;
    backend
        .backend()
        .native_agent(identity, operation)
        .await
        .map(|output| ToolOutput::Dynamic(output.into()))
        .map_err(|error| ToolError::custom("agent_collaboration", error))
}

/// Parses the `fork_turns` spawn argument (spec §6.4): `none` starts fresh,
/// `all` (also the default) inherits the parent conversation, and a
/// positive integer string truncates to the most recent N turns.
pub fn parse_fork_turns(
    value: Option<&str>,
) -> Result<(SubagentContextRequest, Option<usize>), String> {
    match value.unwrap_or("all") {
        "none" => Ok((SubagentContextRequest::FRESH, None)),
        "all" => Ok((SubagentContextRequest::FORK, None)),
        count => count
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .map(|count| (SubagentContextRequest::FORK, Some(count)))
            .ok_or_else(|| "fork_turns must be none, all, or a positive integer string".to_owned()),
    }
}

macro_rules! native_tool {
    ($tool:ident, $name:literal, $input:ty, $kind:expr, $read_only:literal, $description:literal, $handler:ident) => {
        #[derive(Debug, Default)]
        pub struct $tool;
        impl ToolMetadata for $tool {
            fn kind(&self) -> ToolKind {
                $kind
            }
            fn tool_namespace(&self) -> ToolNamespace {
                ToolNamespace::GrokBuild
            }
            fn is_read_only(&self) -> bool {
                $read_only
            }
            fn description_template(&self) -> &str {
                $description
            }
        }
        impl Tool for $tool {
            type Args = $input;
            type Output = ToolOutput;
            fn id(&self) -> xai_tool_protocol::ToolId {
                xai_tool_protocol::ToolId::new($name).expect("valid native tool id")
            }
            fn description(
                &self,
                _ctx: &xai_tool_runtime::ListToolsContext,
            ) -> xai_tool_types::ToolDescription {
                xai_tool_types::ToolDescription::new(
                    $name,
                    crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
                )
            }
            fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
                xai_tool_protocol::ToolCapabilities {
                    is_read_only: $read_only,
                    tool_scope: Some(if $read_only {
                        xai_tool_protocol::ToolScope::Read
                    } else {
                        xai_tool_protocol::ToolScope::Write
                    }),
                    ..Default::default()
                }
            }
            async fn run(
                &self,
                ctx: ToolCallContext,
                input: Self::Args,
            ) -> Result<ToolOutput, ToolError> {
                $handler(ctx, input).await
            }
        }
    };
}

// WT kind mapping (the source's dedicated `AgentCollaboration` kind has no
// WT counterpart; no new `ToolKind` variant is added in this stage):
// `spawn_agent` rides the `Task` kind so the existing child tool policy
// strips it at max depth; the five collaboration tools ride the
// `ActiveAgentMessage` kind and stay visible to children (siblings
// message each other through the mailbox). Kind consumers that treat
// `ActiveAgentMessage` as the v1 root-only `send_subagent_message` tool
// must exempt these fixed wire names via `V2_COLLABORATION_TOOL_NAMES`
// (see `child_tool_projection::child_safe_tool_specs`).

/// Client names of the five v2 collaboration tools (not `spawn_agent`,
/// which rides the `Task` kind). The v2 protocol pins these wire names,
/// so name-based exemption is stable for kind consumers.
pub const V2_COLLABORATION_TOOL_NAMES: &[&str] = &[
    "send_message",
    "followup_task",
    "list_agents",
    "wait_agent",
    "interrupt_agent",
];
native_tool!(
    SpawnAgentTool,
    "spawn_agent",
    SpawnAgentInput,
    ToolKind::Task,
    false,
    "Start a named background agent. task_name uses lowercase letters, digits, and underscores. fork_turns defaults to all; use none for fresh context or a positive integer string for recent turns. Same-model forks retain compatible history; cross-model forks use a plaintext digest. The host's depth and permission limits still apply. Reuse an existing task with followup_task.",
    spawn
);
native_tool!(
    SendMessageTool,
    "send_message",
    MessageInput,
    ToolKind::ActiveAgentMessage,
    false,
    "Deliver a message promptly to an agent by canonical task path, relative task name, or ID. Does not start a turn when the recipient is idle. Use followup_task to assign work.",
    send
);
native_tool!(
    FollowupTaskTool,
    "followup_task",
    MessageInput,
    ToolKind::ActiveAgentMessage,
    false,
    "Send work to a non-root agent. Starts a turn when idle and delivers promptly when running. Reuses a completed named agent with its own model, role, working directory, and history.",
    followup
);
native_tool!(
    ListAgentsTool,
    "list_agents",
    ListInput,
    ToolKind::ActiveAgentMessage,
    true,
    "List this team's named agents and lifecycle status without exposing transcripts. Optionally filter by a canonical task-path prefix without a trailing slash.",
    list
);
native_tool!(
    WaitAgentTool,
    "wait_agent",
    WaitInput,
    ToolKind::ActiveAgentMessage,
    true,
    "Wait for agent messages or final-status activity. Returns an activity summary, not message contents. Ends early for steered user input. timeout_ms defaults to 30000, maximum 600000; 0 polls.",
    wait
);
native_tool!(
    InterruptAgentTool,
    "interrupt_agent",
    InterruptInput,
    ToolKind::ActiveAgentMessage,
    false,
    "Interrupt an agent's current turn without deleting its history or task identity. The agent remains reusable with followup_task. Cannot target the root or yourself.",
    interrupt
);

async fn spawn(mut ctx: ToolCallContext, input: SpawnAgentInput) -> Result<ToolOutput, ToolError> {
    let (_, identity) = resources(&ctx).await?;
    let (context, fork_turns) =
        parse_fork_turns(input.fork_turns.as_deref()).map_err(ToolError::invalid_arguments)?;
    let message = message(
        &identity,
        input.message,
        AgentMailboxMessageKind::NativeFollowup,
    )?;
    let task_name = input.task_name;
    if task_name.is_empty()
        || task_name.len() > 64
        || task_name == "root"
        || !task_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ToolError::invalid_arguments(
            "task_name must use lowercase letters, digits, and underscores; root is reserved",
        ));
    }
    // WT adaptation: the pinned TaskToolInput schema carries no `context`
    // field, so the parsed fork intent travels through the tool-call
    // context; TaskTool folds it into the SubagentRequest (spec D-8).
    ctx.insert(NativeAgentSpawn {
        task_name: task_name.clone(),
        fork_turns,
        context,
        message: Some(message),
    });
    crate::implementations::grok_build::task::TaskTool
        .run(
            ctx,
            xai_tool_types::TaskToolInput {
                task_id: None,
                description: task_name,
                prompt: "Complete the task in the agent message supplied with this turn."
                    .to_owned(),
                subagent_type: input
                    .agent_type
                    .unwrap_or_else(|| "general-purpose".to_owned()),
                model: input.model,
                run_in_background: true,
                resume_from: None,
                cwd: None,
                capability_mode: None,
                isolation: None,
            },
        )
        .await
}

async fn send(ctx: ToolCallContext, input: MessageInput) -> Result<ToolOutput, ToolError> {
    let (_, identity) = resources(&ctx).await?;
    let message = message(
        &identity,
        input.message,
        AgentMailboxMessageKind::NativeMessage,
    )?;
    operate(
        &ctx,
        NativeAgentOperation::Message {
            target: input.target,
            message,
        },
    )
    .await
}

async fn followup(ctx: ToolCallContext, input: MessageInput) -> Result<ToolOutput, ToolError> {
    let (_, identity) = resources(&ctx).await?;
    let mut message = message(
        &identity,
        input.message,
        AgentMailboxMessageKind::NativeFollowup,
    )?;
    // Root-scope senders anchor the followup to the current prompt so the
    // child's new turn is attributed to this turn's activity.
    if identity.agent_id == identity.team_scope_id {
        let resources = shared_resources(&ctx)?;
        message
            .native
            .as_mut()
            .expect("native message")
            .trigger_prompt_id = resources
                .lock()
                .await
                .get::<crate::implementations::grok_build::task::types::CurrentPromptIdResource>()
                .map(|prompt| prompt.0.clone());
    }
    operate(
        &ctx,
        NativeAgentOperation::Message {
            target: input.target,
            message,
        },
    )
    .await
}

async fn list(ctx: ToolCallContext, input: ListInput) -> Result<ToolOutput, ToolError> {
    operate(
        &ctx,
        NativeAgentOperation::List {
            path_prefix: input.path_prefix,
        },
    )
    .await
}

async fn wait(ctx: ToolCallContext, input: WaitInput) -> Result<ToolOutput, ToolError> {
    operate(
        &ctx,
        NativeAgentOperation::Wait {
            timeout_ms: input.timeout_ms.unwrap_or(30_000),
        },
    )
    .await
}

async fn interrupt(ctx: ToolCallContext, input: InterruptInput) -> Result<ToolOutput, ToolError> {
    operate(
        &ctx,
        NativeAgentOperation::Interrupt {
            target: input.target,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provenance: open-grok@240c99c9
    /// crates/codegen/xai-grok-tools/src/implementations/codex/multi_agent_v2.rs:404
    /// :: native_agent_message_preserves_bytes_and_dispatch_encryption_context
    /// ADAPTATION: the WT pins the v0 transport to plaintext
    /// (`encrypted: false`, spec Q3/D-3), so the source's
    /// `NativeMessageEncryption` dispatch-context loop has no counterpart;
    /// a pinned-plaintext assertion replaces it.
    #[test]
    fn native_agent_message_preserves_bytes_and_dispatch_encryption_context() {
        let identity = AgentMailboxIdentity {
            team_scope_id: "team".to_owned(),
            agent_id: "root".to_owned(),
        };
        let body = "  opaque-test-payload\n";
        let result =
            message(&identity, body.to_owned(), AgentMailboxMessageKind::NativeMessage)
                .unwrap();
        assert_eq!(result.body, body);
        let suffix = result
            .message_id
            .strip_prefix("amsg_")
            .expect("native agent message IDs must use the Responses prefix");
        assert!(uuid::Uuid::parse_str(suffix).is_ok());
        assert_eq!(result.native_wire_item().unwrap()["id"], result.message_id);
        assert!(!result.native.unwrap().encrypted);
    }

    /// Provenance: open-grok@240c99c9
    /// crates/codegen/xai-grok-tools/src/implementations/codex/multi_agent_v2.rs:437
    /// :: native_agents_validate_fork_turns_and_reject_obsolete_parameters
    /// ADAPTATION: assertions compare the WT spawn-context type
    /// `SubagentContextRequest` instead of the source's
    /// `SubagentContextMode`; `fork_context` is one of the obsolete
    /// parameters D-9 rejects model-visibly via `deny_unknown_fields`.
    #[test]
    fn native_agents_validate_fork_turns_and_reject_obsolete_parameters() {
        assert_eq!(
            parse_fork_turns(None).unwrap(),
            (SubagentContextRequest::FORK, None)
        );
        assert_eq!(
            parse_fork_turns(Some("none")).unwrap(),
            (SubagentContextRequest::FRESH, None)
        );
        assert_eq!(
            parse_fork_turns(Some("3")).unwrap(),
            (SubagentContextRequest::FORK, Some(3))
        );
        for invalid in ["0", "-1", "1.5", "fresh", ""] {
            assert!(parse_fork_turns(Some(invalid)).is_err());
        }
        assert!(
            serde_json::from_value::<SpawnAgentInput>(serde_json::json!({
                "task_name":"worker", "message":"work", "fork_context":true,
            }))
            .is_err()
        );
    }

    /// Provenance: open-grok@240c99c9
    /// crates/codegen/xai-grok-tools/src/implementations/codex/multi_agent_v2.rs:462
    /// :: native_agents_require_host_opt_in_before_dispatch
    /// ADAPTATION: none material — the G18 hermetic arm is direct
    /// `NativeAgentsEnabled` resource injection (absent or `false` ⇒
    /// `tool_unavailable`), the same direct-injection precedent as the
    /// source; the gate resource is registered in `task::types` on the WT.
    #[tokio::test]
    async fn native_agents_require_host_opt_in_before_dispatch() {
        for flag in [None, Some(false)] {
            let mut resources = crate::types::resources::Resources::new();
            if let Some(flag) = flag {
                resources.insert(NativeAgentsEnabled(flag));
            }
            let result = SendMessageTool
                .run(
                    crate::types::tool_metadata::test_ctx(resources.into_shared()),
                    MessageInput {
                        target: "/root/worker".to_owned(),
                        message: "test".to_owned(),
                    },
                )
                .await;
            assert!(result.unwrap_err().to_string().contains("not enabled"));
        }
    }
    /// D-9/G8 (item 9, MA-2.4, spec §8 MA-2(8)): the v0 `spawn_agent`
    /// schema is PINNED — the v1-only knobs (`reasoning_effort`,
    /// `service_tier`) and the v1 fork knob (`fork_context`) are rejected
    /// model-visibly by `deny_unknown_fields` (a stray-arg error the model
    /// can see), while a clean v0 input still parses. WT-native (the
    /// source schema never carried these names; D-9 is a WT ruling).
    #[test]
    fn spawn_agent_rejects_v1_parameter_names_model_visibly() {
        for (key, value) in [
            ("reasoning_effort", serde_json::json!("high")),
            ("service_tier", serde_json::json!("flex")),
            ("fork_context", serde_json::json!(true)),
        ] {
            let mut input =
                serde_json::json!({ "task_name": "worker", "message": "work" });
            input
                .as_object_mut()
                .expect("object input")
                .insert(key.to_owned(), value);
            let err = serde_json::from_value::<SpawnAgentInput>(input).unwrap_err();
            assert!(
                err.to_string().contains("unknown field"),
                "stray {key} must be a model-visible unknown-field rejection: {err}"
            );
        }
        // The clean v0 input still parses with every optional field absent.
        let ok: SpawnAgentInput = serde_json::from_value(
            serde_json::json!({ "task_name": "worker", "message": "work" }),
        )
        .unwrap();
        assert!(ok.agent_type.is_none());
        assert!(ok.model.is_none());
        assert!(ok.fork_turns.is_none());
    }

    /// MA-4 pre-roll (micro-fix A): const/registry drift guard.
    /// `child_tool_projection::child_safe_tool_specs` exempts v2 child-wire
    /// tools BY NAME (`V2_COLLABORATION_TOOL_NAMES`) while filtering by KIND
    /// (`ActiveAgentMessage`); a rename or a newly added ActiveAgentMessage
    /// tool without a const update would silently change what v2 children
    /// keep. Fail closed: enumerate every ActiveAgentMessage tool the real
    /// registry declares and pin the split against the const.
    #[test]
    fn v2_collaboration_const_covers_registry_active_agent_message_tools() {
        use crate::registry::types::ToolRegistryBuilder;
        use crate::types::tool::ToolKind;
        let registry = ToolRegistryBuilder::new();
        let registry_aam: std::collections::BTreeSet<String> = registry
            .known_tool_kinds()
            .into_iter()
            .filter(|(_, kind)| matches!(kind, ToolKind::ActiveAgentMessage))
            .map(|(id, _)| {
                id.rsplit_once(':')
                    .map(|(_, name)| name.to_owned())
                    .unwrap_or(id)
            })
            .collect();
        let expected: std::collections::BTreeSet<String> = V2_COLLABORATION_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .chain(
                [crate::implementations::grok_build::SEND_SUBAGENT_MESSAGE_TOOL_NAME]
                    .into_iter()
                    .map(str::to_owned),
            )
            .collect();
        assert_eq!(
            registry_aam, expected,
            "registry ActiveAgentMessage tools drifted from V2_COLLABORATION_TOOL_NAMES (+v1); \
             update the const when renaming or adding these tools"
        );
        // The M-2b child-wire exemption is name-based: a v1 name in the const
        // would let the root-only send_subagent_message ride onto the child wire.
        assert!(
            !V2_COLLABORATION_TOOL_NAMES
                .contains(&crate::implementations::grok_build::SEND_SUBAGENT_MESSAGE_TOOL_NAME),
            "v1 send_subagent_message must never be in the v2 exemption const"
        );
    }
}
