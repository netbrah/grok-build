// crates/codegen/xai-grok-mods/src/protocol.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgedTool {
    pub server: String,
    pub tool: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeInfo {
    pub socket_path: String,
    pub token: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageMode {
    Steer,
    Queue,
    Followup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum HostFrame {
    #[serde(rename = "run.start")]
    Start {
        protocol: u32,
        subagent_id: String,
        parent_session_id: String,
        subagent_type: String,
        agent_name: String,
        model: String,
        cwd: String,
        prompt: String,
        system_prompt: Option<String>,
        resume_vendor_session_id: Option<String>,
        read_only: bool,
        reasoning_effort: Option<String>,
        bridge: Option<BridgeInfo>,
        bridged_tools: Vec<BridgedTool>,
    },
    #[serde(rename = "run.message")]
    Message {
        message_id: String,
        mode: MessageMode,
        from: String,
        text: String,
    },
    #[serde(rename = "run.drain")]
    Drain,
    #[serde(rename = "run.finish")]
    Finish,
    #[serde(rename = "run.interrupt")]
    Interrupt,
    #[serde(rename = "run.cancel")]
    Cancel { reason: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VendorToolActivity {
    pub name: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum SidecarFrame {
    #[serde(rename = "run.started")]
    Started {
        vendor_session_id: String,
        #[serde(default)]
        effective_model: Option<String>,
    },
    #[serde(rename = "run.progress")]
    Progress {
        turns: u32,
        tool_calls: u32,
        tokens_used: u64,
        #[serde(default)]
        tool: Option<VendorToolActivity>,
    },
    #[serde(rename = "run.consumed")]
    Consumed { message_id: String },
    #[serde(rename = "run.idle")]
    Idle { final_text: String },
    #[serde(rename = "run.completed")]
    Completed {
        final_text: String,
        turns: u32,
        tool_calls: u32,
        tokens_used: u64,
    },
    #[serde(rename = "run.failed")]
    Failed {
        error: String,
        #[serde(default)]
        cancelled: bool,
        #[serde(default)]
        turns: u32,
        #[serde(default)]
        tool_calls: u32,
        #[serde(default)]
        tokens_used: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRpcRequest {
    pub id: String,
    pub token: String,
    pub method: String,
    pub server: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRpcResponse {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
