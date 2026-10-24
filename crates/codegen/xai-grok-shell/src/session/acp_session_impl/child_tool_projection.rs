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

pub(crate) type ToolIdentityInventory = std::sync::Arc<
    [(
        xai_grok_tools::types::definition::ToolDefinition,
        xai_grok_tools::types::tool::ToolNamespace,
        ToolKind,
    )],
>;

#[derive(Clone, Debug)]
pub(crate) struct ToolIdentityCompanion {
    pub(crate) inventory: ToolIdentityInventory,
    pub(crate) coordinates: Vec<usize>,
    expected_specs: Vec<ToolSpec>,
}

/// Request-local specs carried by the public snapshot command; identities stay shell-owned.
#[derive(Clone, Debug, Default)]
pub struct ToolSpecBundle {
    pub(crate) specs: Vec<ToolSpec>,
    inventory: Option<ToolIdentityInventory>,
    pub(crate) companion: Option<ToolIdentityCompanion>,
}

fn same_spec(left: &ToolSpec, right: &ToolSpec) -> bool {
    left.name == right.name
        && left.description == right.description
        && left.parameters == right.parameters
        && left.exposure == right.exposure
}

impl ToolSpecBundle {
    pub(super) fn from_selected(
        inventory: ToolIdentityInventory,
        selected: impl IntoIterator<Item = (usize, ToolSpec)>,
    ) -> Self {
        let (coordinates, specs): (Vec<_>, Vec<_>) = selected.into_iter().unzip();
        let companion = ToolIdentityCompanion {
            inventory: inventory.clone(),
            coordinates,
            expected_specs: specs.clone(),
        };
        Self {
            specs,
            inventory: Some(inventory),
            companion: Some(companion),
        }
    }

    #[cfg(test)]
    pub(crate) fn without_identity(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            ..Self::default()
        }
    }

    pub(crate) fn has_valid_identity(&self) -> bool {
        let (Some(inventory), Some(companion)) = (&self.inventory, &self.companion) else {
            return false;
        };
        std::sync::Arc::ptr_eq(inventory, &companion.inventory)
            && companion.coordinates.len() == self.specs.len()
            && companion.expected_specs.len() == self.specs.len()
            && self
                .specs
                .iter()
                .zip(&companion.expected_specs)
                .all(|(actual, expected)| same_spec(actual, expected))
            && companion
                .coordinates
                .iter()
                .zip(&self.specs)
                .all(|(coordinate, spec)| {
                    inventory
                        .get(*coordinate)
                        .is_some_and(|(definition, _, _)| {
                            definition.function.name == spec.name
                                && definition.function.description == spec.description
                                && definition.function.parameters == spec.parameters
                        })
                })
    }

    pub(crate) fn filter_specs(mut self, mut filter: impl FnMut(&mut Vec<ToolSpec>)) -> Self {
        let mut valid = self.has_valid_identity();
        let mut positions = Vec::new();
        // Apply retention to each original coordinate, never rediscover identity by name.
        for (position, spec) in std::mem::take(&mut self.specs).into_iter().enumerate() {
            let mut survivor = vec![spec.clone()];
            filter(&mut survivor);
            if !survivor.is_empty() {
                valid &= survivor.len() == 1 && same_spec(&survivor[0], &spec);
                positions.push(position);
                self.specs.extend(survivor);
            }
        }
        if valid {
            if let Some(companion) = &mut self.companion {
                let coordinates = positions
                    .iter()
                    .map(|position| companion.coordinates[*position])
                    .collect();
                let expected_specs = positions
                    .into_iter()
                    .map(|position| companion.expected_specs[position].clone())
                    .collect();
                companion.coordinates = coordinates;
                companion.expected_specs = expected_specs;
            }
        } else {
            self.companion = None;
        }
        self
    }

    pub(crate) fn filter_for_subagent(self) -> Self {
        self.filter_specs(|specs| {
            crate::agent::subagent::strip_ask_user_question_tool(specs);
            crate::agent::subagent::strip_workflow_tool(specs);
        })
    }

    pub(super) fn child_safe(self, projection: ChildToolProjection) -> Self {
        let kinds: Vec<_> = match self
            .companion
            .as_ref()
            .filter(|_| self.has_valid_identity())
        {
            Some(companion) => companion
                .coordinates
                .iter()
                .map(|coordinate| Some(companion.inventory[*coordinate].2))
                .collect(),
            None => vec![None; self.specs.len()],
        };
        let mut position = 0;
        self.filter_specs(|specs| {
            let kind = kinds[position];
            position += 1;
            *specs = child_safe_tool_specs(std::mem::take(specs), projection, |_| kind);
        })
    }
}

#[cfg(test)]
#[path = "child_tool_projection_tests.rs"]
mod tests;
