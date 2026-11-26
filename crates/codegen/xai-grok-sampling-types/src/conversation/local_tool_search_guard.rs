//! Pre-send refusal for a tool-search history that must not leave the client.
//!
//! A trip is a defect. This function does not drop, rewrite, or repair items.

use super::{ConversationItem, ConversationRequest, ConversationToolChoice, SyntheticReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBlock {
    ServerMintedAnswer,
    OrphanOutput,
    DuplicateOutput,
    OutputBeforeCall,
    UnansweredCall,
    DanglingTool,
    ForcedMissingTool,
    ClientExecutionOnXai,
}

pub fn local_tool_search_guard(
    request: &ConversationRequest,
    xai_endpoint: bool,
) -> Result<(), LocalBlock> {
    if xai_endpoint {
        for tool in &request.tools {
            let declared = serde_json::to_string(tool).unwrap_or_default();
            if declared.contains("\"execution\":\"client\"")
                || declared.contains("\"execution\": \"client\"")
            {
                return Err(LocalBlock::ClientExecutionOnXai);
            }
        }
    }
    if let Some(ConversationToolChoice::Function(name)) = &request.tool_choice {
        if !request.tools.iter().any(|tool| tool.name == *name) {
            return Err(LocalBlock::ForcedMissingTool);
        }
    }

    let mut calls: Vec<String> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    for item in &request.items {
        if let ConversationItem::User(user) = item {
            if user.synthetic_reason == Some(SyntheticReason::CompactionMeta) {
                continue;
            }
        }
        match item {
            ConversationItem::Assistant(message) => {
                for call in &message.tool_calls {
                    if !request.tools.iter().any(|tool| tool.name == call.name) {
                        return Err(LocalBlock::DanglingTool);
                    }
                    calls.push(call.id.to_string());
                }
            }
            ConversationItem::ToolResult(result) => {
                let id = result.tool_call_id.as_str();
                if id.is_empty() || id.starts_with("srvtoolu_") {
                    return Err(LocalBlock::ServerMintedAnswer);
                }
                if !calls.iter().any(|call| call == id) {
                    return Err(LocalBlock::OrphanOutput);
                }
                if outputs.iter().any(|output| output == id) {
                    return Err(LocalBlock::DuplicateOutput);
                }
                outputs.push(id.to_string());
            }
            ConversationItem::Discovery { item } => {
                use super::tool_search::ToolSearchKind;
                let id = item.call_id().unwrap_or("").to_string();
                match item.kind() {
                    ToolSearchKind::ServerToolUse => {
                        if id.is_empty() {
                            return Err(LocalBlock::OrphanOutput);
                        }
                        calls.push(id);
                    }
                    ToolSearchKind::ToolSearchToolResult => {
                        if id.is_empty() || !calls.iter().any(|call| call == &id) {
                            return Err(LocalBlock::OrphanOutput);
                        }
                        if outputs.iter().any(|output| output == &id) {
                            return Err(LocalBlock::DuplicateOutput);
                        }
                        outputs.push(id);
                    }
                    ToolSearchKind::Call => {
                        if !id.is_empty() {
                            calls.push(id);
                        }
                    }
                    ToolSearchKind::Output => {
                        if id.is_empty() || !calls.iter().any(|call| call == &id) {
                            return Err(LocalBlock::OutputBeforeCall);
                        }
                        if outputs.iter().any(|output| output == &id) {
                            return Err(LocalBlock::DuplicateOutput);
                        }
                        outputs.push(id);
                    }
                }
            }
            _ => {}
        }
    }
    if calls.iter().any(|call| !outputs.iter().any(|output| output == call)) {
        return Err(LocalBlock::UnansweredCall);
    }
    Ok(())
}
