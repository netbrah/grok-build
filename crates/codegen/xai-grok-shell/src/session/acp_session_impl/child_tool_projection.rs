use xai_grok_sampling_types::ToolSpec;
use xai_grok_tools::implementations::grok_build::V2_COLLABORATION_TOOL_NAMES;
use xai_grok_tools::types::tool::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildToolProjection {
    Rebuilt,
    VerbatimMirror,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildMessagingGrant {
    Granted,
    Ungranted,
}

pub(super) fn child_safe_tool_specs(
    specs: Vec<ToolSpec>,
    projection: ChildToolProjection,
    messaging_grant: ChildMessagingGrant,
    kind_for_name: impl Fn(&str) -> Option<ToolKind>,
) -> Vec<ToolSpec> {
    // merge: grant + fail-closed unknown kinds, and keep v2 collaboration names
    // Unknown names are parent-only capabilities; granted messaging exempts only the active-message kind.
    // v2 collaboration tools share that kind and stay available to children.
    match projection {
        ChildToolProjection::Rebuilt | ChildToolProjection::VerbatimMirror => specs
            .into_iter()
            .filter(|spec| {
                if V2_COLLABORATION_TOOL_NAMES.contains(&spec.name.as_str()) {
                    return true;
                }
                match kind_for_name(&spec.name) {
                    Some(ToolKind::ActiveAgentMessage) => {
                        messaging_grant == ChildMessagingGrant::Granted
                    }
                    Some(_) => true,
                    None => false,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "child_tool_projection_tests.rs"]
mod tests;
