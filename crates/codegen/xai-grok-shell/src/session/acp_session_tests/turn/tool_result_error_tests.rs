//! GAP-B4 (TOOLRES-ERROR-1, spec L3898-3902): a failed tool execution
//! (`Ok(result)` with `output.is_error()`) must produce the pairing
//! conversation item with `is_error == true`, so the projection emits
//! `"is_error": true` on the wire `tool_result` block.
use super::support::*;
use super::*;
use xai_grok_sampling_types::ConversationItem;
use xai_grok_tools::types::output::{MCPOutput, ToolOutput, ToolRunResult};

/// A successful MCP shape carrying the failure flag — `is_error()` true.
fn errored_mcp_result() -> ToolRunResult {
    let mut mcp =
        MCPOutput::okay_output("browser_screenshot".into(), "browser-use".into(), "boom".into());
    mcp.is_error = true;
    ToolRunResult {
        output: ToolOutput::MCP(mcp),
        prompt_text: "boom".into(),
        effective_tool_name: None,
    }
}

fn last_tool_result_is_error(conv: &[ConversationItem]) -> bool {
    conv.iter()
        .rev()
        .find_map(|item| match item {
            ConversationItem::ToolResult(tr) => Some(tr.is_error),
            _ => None,
        })
        .expect("a tool result must have been pushed")
}

/// A failed (is_error) tool execution pushes a ToolResult item with
/// `is_error == true`.
#[tokio::test(flavor = "current_thread")]
async fn failed_tool_execution_marks_item_is_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                xai_acp_lib::AcpClientMessage,
            >();
            let (persistence_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let parsed_args = serde_json::json!({});
            let followups = actor
                .handle_bridge_tool_success(BridgeToolSuccess {
                    tool_call_id: &acp::ToolCallId::new("tc-err-1"),
                    call_id: "tc-err-1",
                    requested_tool_name: "browser_screenshot",
                    effective_tool_name: "browser_screenshot",
                    drained: DrainedToolSuccess::new(errored_mcp_result()),
                    concatenated_json_count: 0,
                    model_id: "test-model",
                    tool_parsed_args: &parsed_args,
                    model_output_override: None,
                    wait_aborted: false,
                })
                .await
                .expect("bridge success must not error");
            let _ = followups;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                last_tool_result_is_error(&conv),
                "a failed (output.is_error) execution must push a ToolResult item \
                 with is_error == true"
            );
        })
        .await;
}

/// An interrupted wait (abort for a pending interjection) is a cancelled
/// outcome: even though the synthesized output is not an `is_error()`
/// failure, the pairing item must carry `is_error == true`.
#[tokio::test(flavor = "current_thread")]
async fn interrupted_wait_result_marks_item_is_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                xai_acp_lib::AcpClientMessage,
            >();
            let (persistence_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let parsed_args = serde_json::json!({});
            // A successful (non-error) output, aborted-wait flagged.
            let ok_result = ToolRunResult {
                output: ToolOutput::MCP(MCPOutput::okay_output(
                    "wait_tasks".into(),
                    "builtin".into(),
                    "ok".into(),
                )),
                prompt_text: "ok".into(),
                effective_tool_name: None,
            };
            let _ = actor
                .handle_bridge_tool_success(BridgeToolSuccess {
                    tool_call_id: &acp::ToolCallId::new("tc-wait-abort"),
                    call_id: "tc-wait-abort",
                    requested_tool_name: "wait_tasks",
                    effective_tool_name: "wait_tasks",
                    drained: DrainedToolSuccess::new(ok_result),
                    concatenated_json_count: 0,
                    model_id: "test-model",
                    tool_parsed_args: &parsed_args,
                    model_output_override: None,
                    wait_aborted: true,
                })
                .await
                .expect("bridge success must not error");
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                last_tool_result_is_error(&conv),
                "an interrupted (aborted) wait must push a ToolResult item with \
                 is_error == true even when the output shape is a success"
            );
        })
        .await;
}

/// An execution that fails with `Err(ToolError)` (class-1 failure: the tool
/// runtime returns an error instead of an `Ok` output) must also produce a
/// pairing item with `is_error == true`. Pins existing, already-correct
/// behavior — REG-style, expected to pass without a RED.
#[tokio::test(flavor = "current_thread")]
async fn tool_error_result_marks_item_is_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                xai_acp_lib::AcpClientMessage,
            >();
            let (persistence_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let err = anyhow::anyhow!("simulated tool execution failure");
            let _ = actor
                .handle_tool_error(
                    &acp::ToolCallId::new("tc-err-class1"),
                    "tc-err-class1",
                    "run_terminal_cmd",
                    None,
                    &err,
                    "test-model",
                )
                .await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                last_tool_result_is_error(&conv),
                "an Err(ToolError) execution must push a ToolResult item with \
                 is_error == true"
            );
        })
        .await;
}
