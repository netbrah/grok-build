//! Owned, nonserialized tool inputs for a bound request build.

use std::collections::BTreeSet;

use xai_grok_sampling_types::ToolSpec;

#[derive(Debug, Clone)]
pub struct NativeToolCandidates {
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) mcp_membership: BTreeSet<String>,
}

impl NativeToolCandidates {
    /// Preserve prepared builtin specs without adding MCP membership.
    pub fn builtins_only(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            mcp_membership: BTreeSet::new(),
        }
    }
}
