use xai_grok_sampling_types::ToolSpec;
use xai_grok_tools::implementations::grok_build::{
    SEND_SUBAGENT_MESSAGE_TOOL_NAME, V2_COLLABORATION_TOOL_NAMES,
};
use xai_grok_tools::types::tool::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildToolProjection {
    Rebuilt,
    VerbatimMirror,
}

pub(super) fn child_safe_tool_specs(
    specs: Vec<ToolSpec>,
    projection: ChildToolProjection,
    kind_for_name: impl Fn(&str) -> Option<ToolKind>,
) -> Vec<ToolSpec> {
    // Rebuilt and VerbatimMirror children both drop the v1 root-only active-message tool,
    // which only the root session may use.
    // The filter matches by kind so a renamed tool is still caught, and by canonical name when the child bridge no longer registers the tool.
    // The five v2 collaboration tools ride the same ActiveAgentMessage kind on purpose
    // (the WT has no dedicated agent-collaboration kind) and are exempt: v2 siblings
    // message each other through the mailbox, so children keep them (their wire names
    // are pinned by the v2 protocol, so the name exemption is stable).
    // VerbatimMirror leaves every other field of the parent's ToolSpecs unchanged so the child's request still hits the parent's radix cache.
    match projection {
        ChildToolProjection::Rebuilt | ChildToolProjection::VerbatimMirror => specs
            .into_iter()
            .filter(|spec| {
                let v2_collaboration = V2_COLLABORATION_TOOL_NAMES.contains(&spec.name.as_str());
                (kind_for_name(&spec.name) != Some(ToolKind::ActiveAgentMessage)
                    || v2_collaboration)
                    && spec.name != SEND_SUBAGENT_MESSAGE_TOOL_NAME
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "child_tool_projection_tests.rs"]
mod tests;
