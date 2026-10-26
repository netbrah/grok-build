// crates/codegen/xai-grok-mods/src/lib.rs
pub mod manifest;
pub mod protocol;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::SubagentProgress;

pub use manifest::{ModCapabilities, ModConfig, ModError, ModMcpBridge, ModRuntime, ValidatedMod};
pub use protocol::{
    BridgeInfo, BridgeRpcRequest, BridgeRpcResponse, BridgedTool, HostFrame, MessageMode,
    SidecarFrame, VendorToolActivity,
};

pub type BorrowedSendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct ModStarted {
    pub vendor_session_id: String,
    pub effective_model: String,
}

#[derive(Debug, Clone)]
pub struct ModRunInput {
    pub validated: ValidatedMod,
    pub subagent_id: String,
    pub parent_session_id: String,
    pub subagent_type: String,
    pub agent_name: String,
    pub vendor_model: String,
    pub cwd: PathBuf,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub resume_key: Option<String>,
    pub read_only: bool,
    pub reasoning_effort: Option<String>,
    pub bridged_tools: Vec<BridgedTool>,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct ModRunOutcome {
    pub promoted: bool,
    pub final_text: String,
    pub error: Option<String>,
    pub cancelled: bool,
    pub turns: u32,
    pub tool_calls: u32,
    pub tokens_used: u64,
    pub vendor_session_id: Option<String>,
    pub admission_uncertain: bool,
    pub warnings: Vec<String>,
}

pub mod bridge;
pub mod control;
pub mod store;

pub use control::ModChildControl;
pub use store::{ModSessionStore, SessionRecord, select_resume_key};

pub trait ModHost: Send + Sync + 'static {
    fn on_started<'a>(
        &'a self,
        started: ModStarted,
        control: ModChildControl,
    ) -> BorrowedSendFuture<'a, bool>;

    fn on_progress(&self, progress: &SubagentProgress, tool: Option<&VendorToolActivity>);

    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool>;

    fn call_mcp_tool<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: serde_json::Value,
    ) -> BorrowedSendFuture<'a, Result<serde_json::Value, String>>;
}

pub mod process;
pub mod session;

pub use session::run_mod_child_with_store;

pub async fn run_mod_child(input: ModRunInput, host: std::sync::Arc<dyn ModHost>) -> ModRunOutcome {
    let store = ModSessionStore::default_grok_home();
    run_mod_child_with_store(input, host, &store).await
}
