//! Passive host-only native-search evidence. This record grants no replay authority.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NATIVE_SCHEMA_VERSION: u32 = 2;
pub const NATIVE_POLICY_REVISION: u32 = 1;
pub const NATIVE_READER_FLOOR: u32 = 2;
pub const NATIVE_LIMITS_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeUnavailable {
    Capacity,
    Fidelity,
    Incomplete,
    Failed,
    Cancelled,
    UnknownIdentity,
    UnsupportedVersion,
    InvalidReference,
    StaleHistory,
    ConflictingObservation,
    Persistence,
    Indeterminate,
    UnsafeContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBlobRef {
    pub kind: String,
    pub byte_len: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryBoundary {
    pub items: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLineage {
    pub id: String,
    pub source_revision: u64,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSearchHistoryV1 {
    pub schema_version: u32,
    pub policy_revision: u32,
    pub reader_floor: u32,
    pub limits_revision: u32,
    pub lineage: NativeLineage,
    pub generation: u64,
    pub high_water: u64,
    pub source: NativeBlobRef,
    pub active: NativeBlobRef,
    pub ledger: NativeBlobRef,
    pub history_boundary: NativeHistoryBoundary,
    pub exchange_snapshot: Option<NativeBlobRef>,
    pub previous_checkpoint: Option<NativeBlobRef>,
    pub capacity: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSourceCoordinate {
    pub lineage: String,
    pub producing_binding: String,
    pub turn: u64,
    pub item: u64,
    pub response: u64,
    pub block: u64,
    pub submember: Option<u64>,
    pub occurrence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeResponseStatus {
    Collecting,
    Accepted,
    PartialTerminal(NativeUnavailable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeBlockState {
    Open,
    ClosedFaithful,
    Unavailable(NativeUnavailable),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProducingBinding {
    pub request: String,
    pub generation: Option<u64>,
    pub origin: Value,
    pub origin_ref: NativeBlobRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResponseBoundary {
    pub occurrence: u64,
    pub request: String,
    pub lease: u64,
    pub attempt: u64,
    pub continuation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_response_id: Option<Value>,
    pub source_stop_reason: Option<String>,
    pub block_start: u64,
    pub block_end: u64,
    pub stream_complete: bool,
    pub status: NativeResponseStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRawBlock {
    pub coordinates: NativeSourceCoordinate,
    pub wire_index: u32,
    pub arrival_ordinal: u64,
    pub block: NativeBlobRef,
    pub partial: Option<NativeBlobRef>,
    pub state: NativeBlockState,
    pub eligible_ref: Option<NativeBlobRef>,
    pub eligible_unavailable: Option<NativeUnavailable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSource {
    pub schema_version: u32,
    pub lineage: String,
    pub source_revision: u64,
    pub producing_bindings: Vec<NativeProducingBinding>,
    pub responses: Vec<NativeResponseBoundary>,
    pub ordered_blocks: Vec<NativeRawBlock>,
    pub eligible_snapshots: Vec<NativeBlobRef>,
    pub older_sources: Vec<NativeBlobRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSourceV2 {
    pub schema_version: u32,
    pub lineage: String,
    pub source_revision: u64,
    pub producing_bindings: Vec<NativeProducingBinding>,
    pub responses: Vec<NativeResponseBoundary>,
    pub ordered_blocks: Vec<NativeRawBlock>,
    pub eligible_snapshots: Vec<NativeBlobRef>,
    pub older_sources: Vec<NativeBlobRef>,
    pub eligible_member_links: Vec<NativeEligibleMemberLink>,
}

impl NativeSourceV2 {
    pub fn from_parts(
        source: NativeSource,
        eligible_member_links: Vec<NativeEligibleMemberLink>,
    ) -> Self {
        Self {
            schema_version: source.schema_version,
            lineage: source.lineage,
            source_revision: source.source_revision,
            producing_bindings: source.producing_bindings,
            responses: source.responses,
            ordered_blocks: source.ordered_blocks,
            eligible_snapshots: source.eligible_snapshots,
            older_sources: source.older_sources,
            eligible_member_links,
        }
    }

    pub fn into_parts(self) -> (NativeSource, Vec<NativeEligibleMemberLink>) {
        (
            NativeSource {
                schema_version: self.schema_version,
                lineage: self.lineage,
                source_revision: self.source_revision,
                producing_bindings: self.producing_bindings,
                responses: self.responses,
                ordered_blocks: self.ordered_blocks,
                eligible_snapshots: self.eligible_snapshots,
                older_sources: self.older_sources,
            },
            self.eligible_member_links,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeViewLink {
    pub source: NativeSourceCoordinate,
    pub source_digest: String,
    pub view_item: Option<u64>,
    pub view_digest: Option<String>,
    pub role: String,
    pub source_subrange: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeActive {
    pub projection_revision: u64,
    pub view_links: Vec<NativeViewLink>,
    pub dispositions: BTreeMap<String, String>,
    pub loaded_invalidations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGroupRecord {
    pub key: String,
    pub family: String,
    pub execution: String,
    pub origin_ref: NativeBlobRef,
    pub validated_join: Option<String>,
    pub keyless_server_fifo: bool,
    pub members: Vec<NativeSourceCoordinate>,
    pub duplicate_observations: Vec<(NativeSourceCoordinate, NativeSourceCoordinate)>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeWaitingClient {
    pub client_id: String,
    pub source: NativeSourceCoordinate,
    pub result: Option<NativeSourceCoordinate>,
    pub delivery: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLifecycle {
    pub exchanges: BTreeMap<String, String>,
    pub groups: BTreeMap<String, String>,
    pub waiting_clients: Vec<NativeWaitingClient>,
    pub terminal: BTreeMap<String, NativeTerminalMarker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTerminalMarker {
    pub outcome: NativeUnavailable,
    pub last_accepted_response: Option<u64>,
    pub observed_bytes: u64,
    pub failed_block: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeResponseAccounting {
    pub request: String,
    pub lease: u64,
    pub attempt: u64,
    pub response_occurrence: Option<u64>,
    pub continuation: u64,
    pub observed: BTreeMap<String, Value>,
    pub source_stop_reason: Option<String>,
    pub outcome: NativeResponseStatus,
    pub latency_ms: Option<u64>,
    pub http_dispatches: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeObservation {
    pub request: String,
    pub lease: u64,
    pub attempt: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLedger {
    pub source_digest: String,
    pub active_digest: String,
    pub groups: Vec<NativeGroupRecord>,
    pub lifecycle: NativeLifecycle,
    pub accounting: Vec<NativeViewLink>,
    pub usage: Vec<NativeResponseAccounting>,
    pub observations: Vec<NativeObservation>,
    pub transitions: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLedgerV2 {
    pub source_digest: String,
    pub active_digest: String,
    pub groups: Vec<NativeGroupRecord>,
    pub lifecycle: NativeLifecycle,
    pub accounting: Vec<NativeViewLink>,
    pub usage: Vec<NativeResponseAccounting>,
    pub observations: Vec<NativeObservation>,
    pub transitions: Vec<Value>,
    pub eligible_member_accounting: Vec<NativeEligibleMemberAccounting>,
}

impl NativeLedgerV2 {
    pub fn from_parts(
        ledger: NativeLedger,
        eligible_member_accounting: Vec<NativeEligibleMemberAccounting>,
    ) -> Self {
        Self {
            source_digest: ledger.source_digest,
            active_digest: ledger.active_digest,
            groups: ledger.groups,
            lifecycle: ledger.lifecycle,
            accounting: ledger.accounting,
            usage: ledger.usage,
            observations: ledger.observations,
            transitions: ledger.transitions,
            eligible_member_accounting,
        }
    }

    pub fn into_parts(self) -> (NativeLedger, Vec<NativeEligibleMemberAccounting>) {
        (
            NativeLedger {
                source_digest: self.source_digest,
                active_digest: self.active_digest,
                groups: self.groups,
                lifecycle: self.lifecycle,
                accounting: self.accounting,
                usage: self.usage,
                observations: self.observations,
                transitions: self.transitions,
            },
            self.eligible_member_accounting,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeGroupState {
    Completed,
    LivePending,
    AbandonedPending,
}

pub(super) struct NativeValidatedGroup {
    pub record: NativeGroupRecord,
    pub state: NativeGroupState,
    pub primary: Vec<(
        NativeSourceCoordinate,
        super::tool_search::ToolSearchItem,
        NativeItemShape,
    )>,
    pub accepted: bool,
}

#[derive(Default)]
pub(super) enum NativeNextInput {
    #[default]
    None,
    Fresh,
    Results {
        messages: Vec<Value>,
        client_outputs: Vec<super::tool_search::ToolSearchItem>,
    },
}

#[derive(Default)]
pub(super) struct NativePendingContext {
    pub live_binding: Option<NativeBlobRef>,
    pub next: NativeNextInput,
}

pub(super) fn native_completed_action(
    whole_view: bool,
    previous: Option<&str>,
) -> Result<super::projection::NativeHistoryAction, NativeUnavailable> {
    use super::projection::NativeHistoryAction;
    match previous {
        Some("strip") => Ok(NativeHistoryAction::Strip),
        Some("F") => Ok(NativeHistoryAction::F),
        None | Some("KEEP") => Ok(if whole_view {
            NativeHistoryAction::Keep
        } else {
            NativeHistoryAction::F
        }),
        Some(_) => Err(NativeUnavailable::InvalidReference),
    }
}

pub(super) fn native_validate_groups(
    source: &NativeSource,
    ledger: &NativeLedger,
    raw: &BTreeMap<u64, super::tool_search::ToolSearchItem>,
    authorship: &BTreeMap<u64, NativeIdAuthorship>,
    same_domain: impl Fn(&NativeBlobRef, &NativeBlobRef) -> bool,
) -> Result<Vec<NativeValidatedGroup>, NativeUnavailable> {
    NativeResource::SourceGroups.admit(ledger.groups.len() as u64, 0)?;
    let mut seen = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    let mut groups = Vec::new();
    for record in &ledger.groups {
        if record.key.is_empty()
            || !keys.insert(&record.key)
            || ledger.lifecycle.groups.get(&record.key) != Some(&record.state)
            || !source
                .producing_bindings
                .iter()
                .any(|binding| binding.origin_ref == record.origin_ref)
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let state = match record.state.as_str() {
            "Completed" => NativeGroupState::Completed,
            "LivePending" => NativeGroupState::LivePending,
            "AbandonedPending" => NativeGroupState::AbandonedPending,
            _ => return Err(NativeUnavailable::InvalidReference),
        };
        NativeResource::GroupMembers.admit(record.members.len() as u64, 0)?;
        if record
            .members
            .windows(2)
            .any(|pair| pair[0].occurrence >= pair[1].occurrence)
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut primary = Vec::new();
        let mut accepted = true;
        let mut duplicate_sources = Vec::new();
        for (duplicate, original) in &record.duplicate_observations {
            if duplicate.occurrence <= original.occurrence
                || duplicate_sources.contains(duplicate)
                || !record.members.contains(duplicate)
                || !record.members.contains(original)
                || record
                    .duplicate_observations
                    .iter()
                    .any(|(other, _)| other == original)
                || raw.get(&duplicate.occurrence).map(|item| item.raw())
                    != raw.get(&original.occurrence).map(|item| item.raw())
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            duplicate_sources.push(duplicate.clone());
        }
        for coordinate in &record.members {
            if seen.contains(coordinate) {
                return Err(NativeUnavailable::InvalidReference);
            }
            seen.push(coordinate.clone());
            let block = source
                .ordered_blocks
                .get(coordinate.occurrence as usize)
                .filter(|block| block.coordinates == *coordinate)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let binding: Vec<_> = source
                .producing_bindings
                .iter()
                .filter(|binding| binding.request == coordinate.producing_binding)
                .collect();
            if binding.len() != 1 || !same_domain(&record.origin_ref, &binding[0].origin_ref) {
                return Err(NativeUnavailable::InvalidReference);
            }
            let item = raw
                .get(&coordinate.occurrence)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let shape = native_item_shape(
                item,
                authorship
                    .get(&coordinate.occurrence)
                    .copied()
                    .unwrap_or(NativeIdAuthorship::Unknown),
            )?;
            let (family, execution) = match shape.family {
                NativeReplayFamily::ResponsesClient => ("Responses", "client"),
                NativeReplayFamily::ResponsesServer => ("Responses", "server"),
                NativeReplayFamily::MessagesServer => ("Messages", "server"),
            };
            if record.family != family
                || record.execution != execution
                || block.state != NativeBlockState::ClosedFaithful
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let response = source
                .responses
                .iter()
                .find(|response| response.occurrence == coordinate.response)
                .ok_or(NativeUnavailable::InvalidReference)?;
            accepted &=
                response.status == NativeResponseStatus::Accepted && response.stream_complete;
            if !duplicate_sources.contains(coordinate) {
                primary.push((coordinate.clone(), item.clone(), shape));
            }
        }
        primary.sort_by_key(|member| member.0.occurrence);
        let expected = if state == NativeGroupState::Completed {
            2
        } else {
            1
        };
        if primary.len() != expected || !primary[0].2.is_call {
            return Err(NativeUnavailable::InvalidReference);
        }
        let call = &primary[0].2;
        if record.validated_join != call.join || record.keyless_server_fifo != call.keyless_server {
            return Err(NativeUnavailable::InvalidReference);
        }
        if let Some(output) = primary.get(1) {
            if output.2.is_call
                || call.family != output.2.family
                || call.join != output.2.join
                || call.keyless_server != output.2.keyless_server
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        groups.push(NativeValidatedGroup {
            record: record.clone(),
            state,
            primary,
            accepted,
        });
    }
    if seen.len() != raw.len()
        || raw.keys().any(|ordinal| {
            !seen
                .iter()
                .any(|coordinate| coordinate.occurrence == *ordinal)
        })
    {
        return Err(NativeUnavailable::InvalidReference);
    }

    let mut ordered: Vec<_> = groups
        .iter()
        .enumerate()
        .flat_map(|(group, record)| record.primary.iter().map(move |member| (group, member)))
        .collect();
    ordered.sort_by_key(|(_, member)| member.0.occurrence);
    let mut pending: Vec<(usize, &NativeSourceCoordinate, &NativeItemShape)> = Vec::new();
    for (group, member) in ordered {
        let matches = |old: &(usize, &NativeSourceCoordinate, &NativeItemShape)| {
            old.2.family == member.2.family
                && old.2.join == member.2.join
                && same_domain(
                    &groups[old.0].record.origin_ref,
                    &groups[group].record.origin_ref,
                )
        };
        if member.2.is_call {
            if member.2.join.is_some() && pending.iter().any(matches) {
                return Err(NativeUnavailable::InvalidReference);
            }
            pending.push((group, &member.0, &member.2));
        } else {
            let candidates: Vec<_> = pending
                .iter()
                .enumerate()
                .filter(|(_, old)| matches(old))
                .map(|(index, _)| index)
                .collect();
            let first = *candidates
                .first()
                .ok_or(NativeUnavailable::InvalidReference)?;
            if (!member.2.keyless_server && candidates.len() != 1) || pending[first].0 != group {
                return Err(NativeUnavailable::InvalidReference);
            }
            pending.remove(first);
        }
    }
    Ok(groups)
}

pub(super) fn native_pending_action(
    group: &NativeValidatedGroup,
    source: &NativeSource,
    ledger: &NativeLedger,
    context: &NativePendingContext,
) -> Result<(super::projection::NativeHistoryAction, Vec<String>), NativeUnavailable> {
    use super::projection::NativeHistoryAction;
    let call = &group.primary[0];
    let response = source
        .responses
        .iter()
        .find(|response| response.occurrence == call.0.response)
        .ok_or(NativeUnavailable::InvalidReference)?;
    let terminal = ledger
        .lifecycle
        .terminal
        .contains_key(&format!("{}:{}", response.request, response.attempt));
    let abandoned = group.state == NativeGroupState::AbandonedPending || terminal;
    if abandoned {
        return if matches!(context.next, NativeNextInput::Fresh) && terminal {
            Ok((NativeHistoryAction::OmitSendOnly, Vec::new()))
        } else {
            Err(NativeUnavailable::Incomplete)
        };
    }
    if matches!(context.next, NativeNextInput::Fresh) {
        return Err(NativeUnavailable::Incomplete);
    }
    if context.live_binding.as_ref() != Some(&group.record.origin_ref) {
        return Err(NativeUnavailable::Incomplete);
    }
    let mut waiting = Vec::new();
    for client in &ledger.lifecycle.waiting_clients {
        if client.source.producing_binding == call.0.producing_binding
            && client.delivery == "Waiting"
        {
            if client.client_id.is_empty() || waiting.contains(&client.client_id) {
                return Err(NativeUnavailable::InvalidReference);
            }
            waiting.push(client.client_id.clone());
        }
    }
    if !waiting.is_empty() {
        let NativeNextInput::Results {
            messages,
            client_outputs,
        } = &context.next
        else {
            return Ok((NativeHistoryAction::AwaitResults, waiting));
        };
        if messages.len() != 1
            || !client_outputs.is_empty()
            || messages[0].get("role").and_then(Value::as_str) != Some("user")
        {
            return Err(NativeUnavailable::Incomplete);
        }
        NativeResource::ResponseBytes
            .admit(native_encoded_len(&messages[0], NATIVE_RESPONSE_BYTES)?, 0)?;
        let results = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(NativeUnavailable::Incomplete)?;
        if results.is_empty() || results.len() != waiting.len() {
            return Err(NativeUnavailable::Incomplete);
        }
        let mut delivered = std::collections::BTreeSet::new();
        for result in results {
            let id = result
                .get("tool_use_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(NativeUnavailable::Incomplete)?;
            if result.get("type").and_then(Value::as_str) != Some("tool_result")
                || !waiting.iter().any(|client| client == id)
                || !delivered.insert(id)
                || result.get("content").is_none()
            {
                return Err(NativeUnavailable::Incomplete);
            }
        }
        return Ok((NativeHistoryAction::ContinueOriginal, waiting));
    }
    if call.2.family == NativeReplayFamily::ResponsesClient {
        let id = call
            .2
            .join
            .as_ref()
            .ok_or(NativeUnavailable::InvalidReference)?;
        let NativeNextInput::Results {
            messages,
            client_outputs,
        } = &context.next
        else {
            return Ok((NativeHistoryAction::AwaitClient, vec![id.clone()]));
        };
        if !messages.is_empty() || client_outputs.len() != 1 {
            return Err(NativeUnavailable::Incomplete);
        }
        let output = native_item_shape(&client_outputs[0], NativeIdAuthorship::ClientOutput)?;
        if output.family != NativeReplayFamily::ResponsesClient
            || output.is_call
            || output.join.as_ref() != Some(id)
            || !output.faithful_identity
        {
            return Err(NativeUnavailable::Incomplete);
        }
        return Ok((NativeHistoryAction::ContinueOriginal, vec![id.clone()]));
    }
    if let NativeNextInput::Results {
        messages,
        client_outputs,
    } = &context.next
        && (!messages.is_empty() || !client_outputs.is_empty())
    {
        return Err(NativeUnavailable::Incomplete);
    }
    Ok((NativeHistoryAction::ContinueOriginal, Vec::new()))
}

pub(super) fn native_validate_waiting_clients(
    source: &NativeSource,
    ledger: &NativeLedger,
    raw: &BTreeMap<u64, Value>,
) -> Result<(), NativeUnavailable> {
    let mut seen = Vec::new();
    for client in &ledger.lifecycle.waiting_clients {
        let identity = (
            client.source.producing_binding.as_str(),
            client.client_id.as_str(),
        );
        if client.client_id.is_empty()
            || seen.contains(&identity)
            || !matches!(client.delivery.as_str(), "Waiting" | "Unavailable")
            || source
                .ordered_blocks
                .get(client.source.occurrence as usize)
                .is_none_or(|block| block.coordinates != client.source)
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let call = raw
            .get(&client.source.occurrence)
            .ok_or(NativeUnavailable::InvalidReference)?;
        if call.get("type").and_then(Value::as_str) != Some("tool_use")
            || call.get("id").and_then(Value::as_str) != Some(client.client_id.as_str())
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        if let Some(coordinate) = &client.result {
            if source
                .ordered_blocks
                .get(coordinate.occurrence as usize)
                .is_none_or(|block| block.coordinates != *coordinate)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let result = raw
                .get(&coordinate.occurrence)
                .ok_or(NativeUnavailable::InvalidReference)?;
            if result.get("type").and_then(Value::as_str) != Some("tool_result")
                || result.get("tool_use_id").and_then(Value::as_str)
                    != Some(client.client_id.as_str())
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        seen.push(identity);
    }
    Ok(())
}

// Contract r2 section 3, limits_revision 1. These are encoded-byte limits, not RSS limits.
pub const NATIVE_BLOCK_BYTES: u64 = 512 * 1024;
pub const NATIVE_RESPONSE_BYTES: u64 = 1024 * 1024;
pub const NATIVE_RESPONSE_BLOCKS: u64 = 128;
pub const NATIVE_GROUP_BYTES: u64 = 2 * 1024 * 1024;
pub const NATIVE_GROUP_MEMBERS: u64 = 256;
pub const NATIVE_EXCHANGE_BYTES: u64 = 4 * 1024 * 1024;
pub const NATIVE_SNAPSHOT_PAYLOAD_BYTES: u64 = 32_000_000;
pub const NATIVE_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
pub const NATIVE_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
pub const NATIVE_SOURCE_RESPONSES: u64 = 1024;
pub const NATIVE_SOURCE_BLOCKS: u64 = 8192;
pub const NATIVE_SOURCE_GROUPS: u64 = 2048;
pub const NATIVE_ACTIVE_BYTES: u64 = 4 * 1024 * 1024;
pub const NATIVE_LEDGER_BYTES: u64 = 16 * 1024 * 1024;
pub const NATIVE_LEDGER_MEMBERS: u64 = 16_384;
pub const NATIVE_LEDGER_GROUPS: u64 = 2048;
pub const NATIVE_ROLE_BYTES: u64 = 84 * 1024 * 1024;
pub const NATIVE_CONTROL_BYTES: u64 = 4 * 1024 * 1024;
pub const NATIVE_LINEAGE_BYTES: u64 = 256 * 1024 * 1024;

pub fn encode_native<T: Serialize>(value: &T, limit: u64) -> Result<Vec<u8>, NativeUnavailable> {
    struct Counter {
        bytes: u64,
        limit: u64,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len() as u64)
                .filter(|total| *total <= self.limit)
                .ok_or_else(|| std::io::Error::other("native evidence capacity"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| NativeUnavailable::Capacity)?;
    let length = usize::try_from(counter.bytes).map_err(|_| NativeUnavailable::Capacity)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| NativeUnavailable::Capacity)?;
    serde_json::to_writer(&mut bytes, value).map_err(|_| NativeUnavailable::Fidelity)?;
    if bytes.len() != length {
        return Err(NativeUnavailable::Fidelity);
    }
    Ok(bytes)
}

#[derive(Clone)]
pub struct NativeCaptureLease(Arc<NativeLeaseState>);

struct NativeLeaseState {
    lineage: String,
    request: String,
    turn: u64,
    ordinal: u64,
    allowance: Mutex<NativeAllowance>,
    view_authorized: std::sync::atomic::AtomicBool,
}

struct NativeAllowance {
    ceiling: u64,
    charged: u64,
    unavailable: Option<NativeUnavailable>,
    ended: bool,
    generation: Option<(u64, u64, bool)>,
}

impl std::fmt::Debug for NativeCaptureLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCaptureLease")
            .field("ordinal", &self.0.ordinal)
            .finish_non_exhaustive()
    }
}

impl NativeCaptureLease {
    pub fn issue(lineage: String, request: String, turn: u64, ordinal: u64, ceiling: u64) -> Self {
        Self(Arc::new(NativeLeaseState {
            lineage,
            request,
            turn,
            ordinal,
            view_authorized: std::sync::atomic::AtomicBool::new(true),
            allowance: Mutex::new(NativeAllowance {
                ceiling,
                charged: 0,
                unavailable: None,
                ended: false,
                generation: None,
            }),
        }))
    }
    pub fn lineage(&self) -> &str {
        &self.0.lineage
    }
    pub fn request(&self) -> &str {
        &self.0.request
    }
    pub fn turn(&self) -> u64 {
        self.0.turn
    }
    pub fn ordinal(&self) -> u64 {
        self.0.ordinal
    }
    pub fn same_lease(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn reserve(&self, bytes: u64) -> Result<(), NativeUnavailable> {
        let mut allowance = self
            .0
            .allowance
            .lock()
            .map_err(|_| NativeUnavailable::Indeterminate)?;
        if allowance.ended || allowance.unavailable.is_some() {
            return Err(allowance
                .unavailable
                .unwrap_or(NativeUnavailable::StaleHistory));
        }
        let Some(charged) = allowance
            .charged
            .checked_add(bytes)
            .filter(|n| *n <= allowance.ceiling)
        else {
            allowance.unavailable = Some(NativeUnavailable::Capacity);
            return Err(NativeUnavailable::Capacity);
        };
        allowance.charged = charged;
        Ok(())
    }
    pub fn reserve_generation(&self) -> Result<(), NativeUnavailable> {
        let mut allowance = self
            .0
            .allowance
            .lock()
            .map_err(|_| NativeUnavailable::Indeterminate)?;
        let Some((copies, minimum, ready)) = allowance.generation.as_mut() else {
            return Ok(());
        };
        if *ready {
            *ready = false;
            return Ok(());
        }
        let copies = *copies;
        let minimum = *minimum;
        let available = allowance
            .ceiling
            .checked_sub(allowance.charged)
            .and_then(|bytes| bytes.checked_sub(copies))
            .filter(|bytes| *bytes >= minimum)
            .ok_or(NativeUnavailable::Capacity)?;
        let _ = available;
        allowance.charged = allowance
            .charged
            .checked_add(copies)
            .ok_or(NativeUnavailable::Capacity)?;
        Ok(())
    }
    pub fn fund_generations(&self, copies: u64, minimum: u64) -> Result<(), NativeUnavailable> {
        let mut allowance = self
            .0
            .allowance
            .lock()
            .map_err(|_| NativeUnavailable::Indeterminate)?;
        allowance.generation = Some((copies, minimum, true));
        Ok(())
    }
    pub fn limit_generations(&self, bytes: u64, copies: u64) -> Result<(), NativeUnavailable> {
        let mut allowance = self
            .0
            .allowance
            .lock()
            .map_err(|_| NativeUnavailable::Indeterminate)?;
        allowance.ceiling = allowance.ceiling.min(
            allowance
                .charged
                .checked_add(bytes)
                .ok_or(NativeUnavailable::Capacity)?,
        );
        if let Some((reserved, _, _)) = &mut allowance.generation {
            *reserved = (*reserved).max(copies);
        }
        Ok(())
    }
    pub fn unavailable(&self, reason: NativeUnavailable) {
        if let Ok(mut allowance) = self.0.allowance.lock() {
            allowance.unavailable.get_or_insert(reason);
        }
    }
    pub fn finish(&self) {
        if let Ok(mut allowance) = self.0.allowance.lock() {
            allowance.ended = true;
        }
    }
    pub fn revoke_view(&self) {
        self.0
            .view_authorized
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn view_authorized(&self) -> bool {
        self.0
            .view_authorized
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn is_finished(&self) -> bool {
        self.0
            .allowance
            .lock()
            .map(|allowance| allowance.ended)
            .unwrap_or(true)
    }
    pub fn ceiling(&self) -> Result<u64, NativeUnavailable> {
        self.0
            .allowance
            .lock()
            .map(|allowance| allowance.ceiling)
            .map_err(|_| NativeUnavailable::Indeterminate)
    }
    pub fn charged(&self) -> Result<u64, NativeUnavailable> {
        self.0
            .allowance
            .lock()
            .map(|allowance| allowance.charged)
            .map_err(|_| NativeUnavailable::Indeterminate)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCapturedBlock {
    pub wire_index: u32,
    pub arrival_ordinal: u64,
    pub raw: String,
    pub partial_json: Option<String>,
    pub state: NativeBlockState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAttemptEvidence {
    pub attempt: u64,
    pub admitted: bool,
    pub response_started: bool,
    pub provider_response_id: Option<Value>,
    pub source_stop_reason: Option<String>,
    pub stream_complete: bool,
    pub status: NativeResponseStatus,
    pub blocks: Vec<NativeCapturedBlock>,
    pub usage: BTreeMap<String, Value>,
    pub latency_ms: Option<u64>,
    pub http_dispatches: u64,
    pub observed_bytes: u64,
    pub failed_block: Option<u32>,
    pub view_roles: Vec<Option<(u64, String)>>,
    pub view_digests: Vec<String>,
}

impl std::fmt::Debug for NativeAttemptEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeAttemptEvidence")
            .field("attempt", &self.attempt)
            .field("admitted", &self.admitted)
            .field("blocks", &self.blocks.len())
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum NativeEvidenceEvent {
    Attempt(Arc<NativeAttemptEvidence>),
    End,
}

pub fn native_encoded_len<T: Serialize>(value: &T, limit: u64) -> Result<u64, NativeUnavailable> {
    struct Counter {
        bytes: u64,
        limit: u64,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len() as u64)
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("native capacity"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| NativeUnavailable::Capacity)?;
    Ok(counter.bytes)
}

/// Count a compact JSON value embedded as a JSON string without allocating it.
pub fn native_encoded_json_string_len<T: Serialize>(
    value: &T,
    limit: u64,
) -> Result<u64, NativeUnavailable> {
    struct Counter {
        bytes: u64,
        limit: u64,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                let width = match byte {
                    b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
                    0..=31 => 6,
                    _ => 1,
                };
                self.bytes = self
                    .bytes
                    .checked_add(width)
                    .filter(|total| *total <= self.limit)
                    .ok_or_else(|| std::io::Error::other("native capacity"))?;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    if limit < 2 {
        return Err(NativeUnavailable::Capacity);
    }
    let mut counter = Counter { bytes: 2, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| NativeUnavailable::Capacity)?;
    Ok(counter.bytes)
}

#[derive(Clone, Copy)]
enum NativeJsonEncoding {
    Value,
    String,
}

/// Count canonical JSON without constructing its candidate object or array values.
pub fn native_json_encoded_len(input: &str, limit: u64) -> Result<u64, NativeUnavailable> {
    native_json_size(input, limit, NativeJsonEncoding::Value)
}

/// Count canonical JSON embedded in a string before materializing tool input.
pub fn native_json_string_encoded_len(input: &str, limit: u64) -> Result<u64, NativeUnavailable> {
    native_json_size(input, limit, NativeJsonEncoding::String)
}

fn native_json_size(
    input: &str,
    limit: u64,
    encoding: NativeJsonEncoding,
) -> Result<u64, NativeUnavailable> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    #[derive(Clone, Copy)]
    struct Size<'a> {
        limit: u64,
        exhausted: &'a std::cell::Cell<bool>,
        encoding: NativeJsonEncoding,
    }
    impl Size<'_> {
        fn checked<E: Error>(&self, bytes: u64) -> Result<u64, E> {
            if bytes > self.limit {
                self.exhausted.set(true);
                Err(E::custom("native capacity"))
            } else {
                Ok(bytes)
            }
        }
        fn serialized<T: Serialize, E: Error>(&self, value: &T) -> Result<u64, E> {
            let size = match self.encoding {
                NativeJsonEncoding::Value => native_encoded_len(value, self.limit),
                NativeJsonEncoding::String => {
                    native_encoded_json_string_len(value, self.limit).map(|bytes| bytes - 2)
                }
            };
            match size {
                Ok(bytes) => Ok(bytes),
                Err(_) => {
                    self.exhausted.set(true);
                    Err(E::custom("native capacity"))
                }
            }
        }
    }
    impl<'de> DeserializeSeed<'de> for Size<'_> {
        type Value = u64;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<u64, D::Error> {
            deserializer.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Size<'_> {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("JSON")
        }
        fn visit_unit<E: Error>(self) -> Result<u64, E> {
            self.checked(4)
        }
        fn visit_bool<E: Error>(self, value: bool) -> Result<u64, E> {
            self.checked(if value { 4 } else { 5 })
        }
        fn visit_i64<E: Error>(self, value: i64) -> Result<u64, E> {
            self.serialized(&value)
        }
        fn visit_u64<E: Error>(self, value: u64) -> Result<u64, E> {
            self.serialized(&value)
        }
        fn visit_f64<E: Error>(self, value: f64) -> Result<u64, E> {
            self.serialized(&value)
        }
        fn visit_str<E: Error>(self, value: &str) -> Result<u64, E> {
            self.serialized(&value)
        }
        fn visit_string<E: Error>(self, value: String) -> Result<u64, E> {
            self.visit_str(&value)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<u64, A::Error> {
            let mut size = 2u64;
            let mut count = 0u64;
            while let Some(value) = sequence.next_element_seed(self)? {
                size = size
                    .checked_add(value)
                    .and_then(|n| n.checked_add(u64::from(count > 0)))
                    .ok_or_else(|| A::Error::custom("native capacity"))?;
                self.checked::<A::Error>(size)?;
                count += 1;
            }
            Ok(size)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<u64, A::Error> {
            let mut entries = BTreeMap::new();
            let mut size = 2u64;
            while let Some(key) = map.next_key::<String>()? {
                let value = map.next_value_seed(self)?;
                let field = self
                    .serialized::<_, A::Error>(&key)?
                    .checked_add(value)
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| A::Error::custom("native capacity"))?;
                if let Some(old) = entries.insert(key, field) {
                    size -= old;
                } else if entries.len() > 1 {
                    size += 1;
                }
                size = size
                    .checked_add(field)
                    .ok_or_else(|| A::Error::custom("native capacity"))?;
                self.checked::<A::Error>(size)?;
            }
            Ok(size)
        }
    }
    let exhausted = std::cell::Cell::new(false);
    let mut decoder = serde_json::Deserializer::from_str(input);
    let result = Size {
        limit,
        exhausted: &exhausted,
        encoding,
    }
    .deserialize(&mut decoder)
    .and_then(|bytes| decoder.end().map(|_| bytes));
    let bytes = result.map_err(|_| {
        if exhausted.get() {
            NativeUnavailable::Capacity
        } else {
            NativeUnavailable::Fidelity
        }
    })?;
    match encoding {
        NativeJsonEncoding::Value => Ok(bytes),
        NativeJsonEncoding::String => bytes
            .checked_add(2)
            .filter(|bytes| *bytes <= limit)
            .ok_or(NativeUnavailable::Capacity),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeResource {
    BlockBytes,
    ResponseBytes,
    ResponseBlocks,
    GroupBytes,
    GroupMembers,
    ExchangeBytes,
    SnapshotPayloadBytes,
    SnapshotBytes,
    SourceBytes,
    SourceResponses,
    SourceBlocks,
    SourceGroups,
    ActiveBytes,
    LedgerBytes,
    LedgerMembers,
    LedgerGroups,
    CurrentBytes,
    StagingBytes,
    BackupBytes,
    ControlBytes,
    LineageBytes,
}

impl NativeResource {
    pub fn ceiling(self) -> u64 {
        match self {
            Self::BlockBytes => NATIVE_BLOCK_BYTES,
            Self::ResponseBytes => NATIVE_RESPONSE_BYTES,
            Self::ResponseBlocks => NATIVE_RESPONSE_BLOCKS,
            Self::GroupBytes => NATIVE_GROUP_BYTES,
            Self::GroupMembers => NATIVE_GROUP_MEMBERS,
            Self::ExchangeBytes => NATIVE_EXCHANGE_BYTES,
            Self::SnapshotPayloadBytes => NATIVE_SNAPSHOT_PAYLOAD_BYTES,
            Self::SnapshotBytes => NATIVE_SNAPSHOT_BYTES,
            Self::SourceBytes => NATIVE_SOURCE_BYTES,
            Self::SourceResponses => NATIVE_SOURCE_RESPONSES,
            Self::SourceBlocks => NATIVE_SOURCE_BLOCKS,
            Self::SourceGroups => NATIVE_SOURCE_GROUPS,
            Self::ActiveBytes => NATIVE_ACTIVE_BYTES,
            Self::LedgerBytes => NATIVE_LEDGER_BYTES,
            Self::LedgerMembers => NATIVE_LEDGER_MEMBERS,
            Self::LedgerGroups => NATIVE_LEDGER_GROUPS,
            Self::CurrentBytes | Self::StagingBytes | Self::BackupBytes => NATIVE_ROLE_BYTES,
            Self::ControlBytes => NATIVE_CONTROL_BYTES,
            Self::LineageBytes => NATIVE_LINEAGE_BYTES,
        }
    }

    pub fn admit(self, used: u64, reserved: u64) -> Result<u64, NativeUnavailable> {
        used.checked_add(reserved)
            .filter(|bytes| *bytes <= self.ceiling())
            .ok_or(NativeUnavailable::Capacity)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRoleReservation {
    pub snapshot_bytes: u64,
    pub source_bytes: u64,
    pub active_bytes: u64,
    pub ledger_bytes: u64,
}

impl NativeRoleReservation {
    pub fn bytes(&self) -> Result<u64, NativeUnavailable> {
        let mut bytes = NativeResource::SnapshotBytes.admit(self.snapshot_bytes, 0)?;
        for (resource, used) in [
            (NativeResource::SourceBytes, self.source_bytes),
            (NativeResource::ActiveBytes, self.active_bytes),
            (NativeResource::LedgerBytes, self.ledger_bytes),
        ] {
            bytes = bytes
                .checked_add(resource.admit(used, 0)?)
                .ok_or(NativeUnavailable::Capacity)?;
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGenerationReservation {
    pub current: NativeRoleReservation,
    pub staging: NativeRoleReservation,
    pub backup: NativeRoleReservation,
    pub control_bytes: u64,
}

impl NativeGenerationReservation {
    pub fn validate(&self) -> Result<u64, NativeUnavailable> {
        let mut total = NativeResource::ControlBytes.admit(self.control_bytes, 0)?;
        for (resource, role) in [
            (NativeResource::CurrentBytes, &self.current),
            (NativeResource::StagingBytes, &self.staging),
            (NativeResource::BackupBytes, &self.backup),
        ] {
            total = total
                .checked_add(resource.admit(role.bytes()?, 0)?)
                .ok_or(NativeUnavailable::Capacity)?;
        }
        NativeResource::LineageBytes.admit(total, 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSnapshot {
    pub body: NativeBlobRef,
    pub tools: Option<NativeToolsRange>,
    pub binding: NativeBlobRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeToolsRange {
    pub offset: u64,
    pub byte_len: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGenerationAuthority {
    pub schema_version: u32,
    pub lineage: String,
    pub high_water: u64,
    pub checkpoint: NativeBlobRef,
    pub transaction: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeNamespaceKind {
    GrokBuild,
    GrokBuildConcise,
    GrokBuildHashline,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    #[serde(rename = "mcp")]
    Mcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEligibleSnapshot {
    pub version: u32,
    pub namespace_kind: NativeNamespaceKind,
    pub qualified_namespace: String,
    pub child_name: String,
    pub schema: NativeBlobRef,
    pub binding: NativeBlobRef,
}

/// Immutable provenance from the shell's one typed registry snapshot.
#[derive(Clone)]
pub struct NativeEligibleEntry {
    position: usize,
    namespace_kind: NativeNamespaceKind,
    qualified_namespace: String,
    child_name: String,
    declaration: super::ToolSpec,
    schema_bytes: Arc<[u8]>,
    schema_digest: String,
}

impl NativeEligibleEntry {
    pub fn checked(
        position: usize,
        namespace_kind: NativeNamespaceKind,
        qualified_namespace: String,
        child_name: String,
        declaration: super::ToolSpec,
        schema_bytes: Arc<[u8]>,
        schema_digest: String,
    ) -> Result<Self, NativeUnavailable> {
        if qualified_namespace.is_empty()
            || child_name.is_empty()
            || schema_digest.len() != 64
            || !schema_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || encode_native(&declaration.parameters, NATIVE_SOURCE_BYTES)?.as_slice()
                != schema_bytes.as_ref()
            || (namespace_kind == NativeNamespaceKind::Mcp
                && declaration.name != format!("{qualified_namespace}__{child_name}"))
            || (namespace_kind != NativeNamespaceKind::Mcp && declaration.name != child_name)
        {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        Ok(Self {
            position,
            namespace_kind,
            qualified_namespace,
            child_name,
            declaration,
            schema_bytes,
            schema_digest,
        })
    }

    pub fn position(&self) -> usize {
        self.position
    }
    pub fn namespace_kind(&self) -> NativeNamespaceKind {
        self.namespace_kind
    }
    pub fn qualified_namespace(&self) -> &str {
        &self.qualified_namespace
    }
    pub fn child_name(&self) -> &str {
        &self.child_name
    }
    pub fn declaration(&self) -> &super::ToolSpec {
        &self.declaration
    }
    pub fn schema_bytes(&self) -> &[u8] {
        &self.schema_bytes
    }
    pub fn schema_digest(&self) -> &str {
        &self.schema_digest
    }

    fn matches(&self, spec: &super::ToolSpec) -> bool {
        self.declaration.name == spec.name
            && self.declaration.description == spec.description
            && self.declaration.parameters == spec.parameters
            && self.declaration.exposure == spec.exposure
    }
}

#[derive(Clone)]
pub struct NativeEligibleInputs {
    entries: Vec<NativeEligibleEntry>,
    candidates: usize,
}

impl std::fmt::Debug for NativeEligibleInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEligibleInputs")
            .field("entries", &self.entries.len())
            .field("candidates", &self.candidates)
            .finish_non_exhaustive()
    }
}

impl NativeEligibleInputs {
    pub fn checked(
        entries: Vec<NativeEligibleEntry>,
        candidates: usize,
    ) -> Result<Self, NativeUnavailable> {
        NativeResource::SourceBlocks.admit(candidates as u64, 0)?;
        let mut positions = std::collections::BTreeSet::new();
        let mut bytes = 0;
        for entry in &entries {
            if entry.position >= candidates || !positions.insert(entry.position) {
                return Err(NativeUnavailable::UnknownIdentity);
            }
            bytes = NativeResource::SourceBytes.admit(bytes, entry.schema_bytes.len() as u64)?;
            bytes = NativeResource::SourceBytes.admit(
                bytes,
                native_encoded_len(&entry.declaration, NATIVE_SOURCE_BYTES)?,
            )?;
            bytes = NativeResource::SourceBytes.admit(
                bytes,
                (entry.qualified_namespace.len()
                    + entry.child_name.len()
                    + entry.schema_digest.len()) as u64,
            )?;
        }
        Ok(Self {
            entries,
            candidates,
        })
    }

    pub fn entries(&self) -> &[NativeEligibleEntry] {
        &self.entries
    }

    pub fn matches_specs(&self, specs: &[super::ToolSpec]) -> bool {
        self.candidates == specs.len()
            && self.entries.iter().all(|entry| {
                specs
                    .get(entry.position)
                    .is_some_and(|spec| entry.matches(spec))
            })
    }

    /// Positions refer to the original snapshot, never a name-based lookup.
    pub fn select(
        &self,
        positions: &[usize],
        specs: &[super::ToolSpec],
    ) -> Result<Self, NativeUnavailable> {
        self.select_inner(positions, specs, false)
    }

    /// The row and preparation selectors may change exposure, not the declaration.
    pub fn select_exposure(
        &self,
        positions: &[usize],
        specs: &[super::ToolSpec],
    ) -> Result<Self, NativeUnavailable> {
        self.select_inner(positions, specs, true)
    }

    fn select_inner(
        &self,
        positions: &[usize],
        specs: &[super::ToolSpec],
        exposure: bool,
    ) -> Result<Self, NativeUnavailable> {
        if positions.len() != specs.len()
            || positions
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != positions.len()
        {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        let mut entries = Vec::new();
        for (position, (original, spec)) in positions.iter().zip(specs).enumerate() {
            if *original >= self.candidates {
                return Err(NativeUnavailable::UnknownIdentity);
            }
            if let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.position == *original)
            {
                let mut entry = entry.clone();
                if exposure {
                    entry.declaration.exposure = spec.exposure;
                }
                if entry.matches(spec) {
                    entry.position = position;
                    entries.push(entry);
                }
            }
        }
        Self::checked(entries, specs.len())
    }

    pub fn with_appended(&self, specs: &[super::ToolSpec]) -> Result<Self, NativeUnavailable> {
        if specs.len() < self.candidates || !self.matches_specs(&specs[..self.candidates]) {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        Self::checked(self.entries.clone(), specs.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEligibleMemberLink {
    pub source: NativeSourceCoordinate,
    pub identity: NativeEligibleMemberIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", deny_unknown_fields)]
pub enum NativeEligibleMemberIdentity {
    Backed { eligible_ref: NativeBlobRef },
    Unavailable { reason: NativeUnavailable },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEligibleMemberAccounting {
    pub source: NativeSourceCoordinate,
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeIdentityExtension {
    Revision1Unknown,
    Revision2 {
        eligible_member_links: Vec<NativeEligibleMemberLink>,
        eligible_member_accounting: Vec<NativeEligibleMemberAccounting>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCandidateReference<'a> {
    Tool {
        namespace: Option<&'a str>,
        name: &'a str,
        declaration: Option<&'a Value>,
    },
    ClientResult {
        join: &'a str,
    },
}

/// Enumerate only modeled candidate leaves, in their raw occurrence order.
pub fn native_candidate_references(
    raw: &Value,
) -> Result<Vec<NativeCandidateReference<'_>>, NativeUnavailable> {
    fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, NativeUnavailable> {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or(NativeUnavailable::UnknownIdentity)
    }
    let mut references = Vec::new();
    match text(raw, "type")? {
        "tool_search_tool_result" => {
            if raw.get("is_error").and_then(Value::as_bool) == Some(true) {
                return Ok(references);
            }
            let content = raw
                .get("content")
                .ok_or(NativeUnavailable::UnknownIdentity)?;
            if content.get("type").and_then(Value::as_str) == Some("tool_search_tool_result_error")
            {
                return Ok(references);
            }
            let leaves = content
                .get("tool_references")
                .and_then(Value::as_array)
                .ok_or(NativeUnavailable::UnknownIdentity)?;
            NativeResource::LedgerMembers.admit(leaves.len() as u64, 0)?;
            for leaf in leaves {
                if text(leaf, "type")? != "tool_reference" {
                    return Err(NativeUnavailable::UnknownIdentity);
                }
                references.push(NativeCandidateReference::Tool {
                    namespace: None,
                    name: text(leaf, "tool_name")?,
                    declaration: None,
                });
            }
        }
        "tool_search_output" => {
            let tools = raw
                .get("tools")
                .and_then(Value::as_array)
                .ok_or(NativeUnavailable::UnknownIdentity)?;
            for tool in tools {
                let (namespace, children) = match text(tool, "type")? {
                    "function" => (None, std::slice::from_ref(tool)),
                    "namespace" => (
                        Some(text(tool, "name")?),
                        tool.get("tools")
                            .and_then(Value::as_array)
                            .ok_or(NativeUnavailable::UnknownIdentity)?
                            .as_slice(),
                    ),
                    _ => return Err(NativeUnavailable::UnknownIdentity),
                };
                NativeResource::LedgerMembers
                    .admit(references.len() as u64, children.len() as u64)?;
                for child in children {
                    if text(child, "type")? != "function" {
                        return Err(NativeUnavailable::UnknownIdentity);
                    }
                    references.push(NativeCandidateReference::Tool {
                        namespace,
                        name: text(child, "name")?,
                        declaration: Some(child),
                    });
                }
            }
        }
        "tool_use" | "function_call" => references.push(NativeCandidateReference::Tool {
            namespace: raw.get("namespace").and_then(Value::as_str),
            name: text(raw, "name")?,
            declaration: None,
        }),
        "tool_result" => references.push(NativeCandidateReference::ClientResult {
            join: text(raw, "tool_use_id")?,
        }),
        "function_call_output" => references.push(NativeCandidateReference::ClientResult {
            join: text(raw, "call_id")?,
        }),
        "server_tool_use" | "tool_search_call" | "text" | "thinking" | "redacted_thinking"
        | "reasoning" | "message" => {}
        _ => return Err(NativeUnavailable::UnknownIdentity),
    }
    Ok(references)
}

#[cfg(test)]
#[test]
fn native_history_identity_shapes_preserve_field_presence() {
    use super::tool_search::ToolSearchItem;
    for id in [
        None,
        Some(Value::Null),
        Some(serde_json::json!("")),
        Some(serde_json::json!(7)),
        Some(serde_json::json!("provider-output")),
    ] {
        let mut raw = serde_json::json!({"type":"tool_search_output","execution":"server","status":"completed","tools":[]});
        if let Some(value) = &id {
            raw["id"] = value.clone();
        }
        let item = ToolSearchItem::from_wire(raw.clone()).unwrap();
        assert_eq!(item.raw(), &raw);
        assert_eq!(item.raw().get("id"), id.as_ref());
        assert_eq!(
            item.id(),
            id.as_ref()
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        );
        assert!(native_candidate_references(item.raw()).unwrap().is_empty());
        let shape = native_item_shape(&item, NativeIdAuthorship::Provider).unwrap();
        assert_eq!(shape.family, NativeReplayFamily::ResponsesServer);
        assert_eq!(shape.item_id_present, id.is_some());
        assert_eq!(
            shape.faithful_identity,
            id.as_ref()
                .is_none_or(|value| value.as_str().is_some_and(|value| !value.is_empty()))
        );
        assert!(shape.keyless_server && shape.join.is_none() && !shape.is_call);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeReplayFamily {
    ResponsesClient,
    ResponsesServer,
    MessagesServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeIdAuthorship {
    Provider,
    ClientOutput,
    ClientMinted,
    Unknown,
}

pub(super) struct NativeIdentityFields<'a> {
    pub kind: super::tool_search::ToolSearchKind,
    pub item_id: Option<&'a Value>,
    pub join_field: Option<&'a Value>,
    pub join: Option<&'a str>,
    pub execution: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeItemShape {
    pub family: NativeReplayFamily,
    pub is_call: bool,
    pub join: Option<String>,
    pub keyless_server: bool,
    pub item_id_present: bool,
    pub faithful_identity: bool,
    pub result_error: bool,
}

pub(super) fn native_item_shape(
    item: &super::tool_search::ToolSearchItem,
    authorship: NativeIdAuthorship,
) -> Result<NativeItemShape, NativeUnavailable> {
    use super::tool_search::ToolSearchKind;
    NativeResource::BlockBytes.admit(native_encoded_len(item.raw(), NATIVE_BLOCK_BYTES)?, 0)?;
    let fields = item.native_identity_fields();
    let family = match fields.kind {
        ToolSearchKind::ServerToolUse | ToolSearchKind::ToolSearchToolResult => {
            NativeReplayFamily::MessagesServer
        }
        ToolSearchKind::Call | ToolSearchKind::Output => match fields.execution {
            Some("client") => NativeReplayFamily::ResponsesClient,
            Some("server") => NativeReplayFamily::ResponsesServer,
            _ => return Err(NativeUnavailable::UnknownIdentity),
        },
    };
    let keyless_server =
        family == NativeReplayFamily::ResponsesServer && fields.join_field.is_none();
    if !keyless_server && fields.join.is_none() {
        return Err(NativeUnavailable::InvalidReference);
    }
    let is_call = matches!(
        fields.kind,
        ToolSearchKind::Call | ToolSearchKind::ServerToolUse
    );
    if is_call {
        let key = if family == NativeReplayFamily::MessagesServer {
            "input"
        } else {
            "arguments"
        };
        if !item.raw().get(key).is_some_and(Value::is_object) {
            return Err(NativeUnavailable::Fidelity);
        }
    } else if family != NativeReplayFamily::MessagesServer
        && !matches!(
            item.status(),
            super::tool_search::ToolSearchStatus::Completed
                | super::tool_search::ToolSearchStatus::Error
        )
    {
        return Err(NativeUnavailable::Incomplete);
    }
    let result_error = !is_call
        && (item.status() == super::tool_search::ToolSearchStatus::Error
            || item.raw().get("is_error").and_then(Value::as_bool) == Some(true)
            || item.raw().pointer("/content/type").and_then(Value::as_str)
                == Some("tool_search_tool_result_error"));
    let valid_optional_id = fields
        .item_id
        .is_none_or(|value| value.as_str().is_some_and(|id| !id.is_empty()));
    let faithful_identity = valid_optional_id
        && (fields.item_id.is_none() || authorship == NativeIdAuthorship::Provider)
        && !(family == NativeReplayFamily::ResponsesClient && !is_call && fields.item_id.is_some())
        && !(fields.kind == ToolSearchKind::ToolSearchToolResult && fields.item_id.is_some())
        && (family != NativeReplayFamily::ResponsesClient
            || is_call
            || authorship == NativeIdAuthorship::ClientOutput);
    Ok(NativeItemShape {
        family,
        is_call,
        join: fields.join.map(str::to_owned),
        keyless_server,
        item_id_present: fields.item_id.is_some(),
        faithful_identity,
        result_error,
    })
}

#[cfg(test)]
#[test]
fn native_history_ledger_group_lifecycle_validation() {
    let coordinate = NativeSourceCoordinate {
        lineage: "u5".into(),
        producing_binding: "request".into(),
        turn: 1,
        item: 0,
        response: 0,
        block: 0,
        submember: None,
        occurrence: 0,
    };
    let binding = NativeBlobRef {
        kind: "producing_binding".into(),
        byte_len: 1,
        digest: "a".repeat(64),
    };
    let group = NativeGroupRecord {
        key: "u5-group".into(),
        family: "Messages".into(),
        execution: "server".into(),
        origin_ref: binding,
        validated_join: Some("srvtoolu_u5".into()),
        keyless_server_fifo: false,
        members: vec![coordinate.clone()],
        duplicate_observations: Vec::new(),
        state: "LivePending".into(),
    };
    let mut ledger = NativeLedger::default();
    ledger.groups.push(group);
    ledger
        .lifecycle
        .groups
        .insert("u5-group".into(), "LivePending".into());
    ledger.lifecycle.waiting_clients.push(NativeWaitingClient {
        client_id: "client_u5".into(),
        source: coordinate,
        result: None,
        delivery: "Waiting".into(),
    });
    let bytes = encode_native(&ledger, NATIVE_LEDGER_BYTES).unwrap();
    assert_eq!(
        serde_json::from_slice::<NativeLedger>(&bytes).unwrap(),
        ledger
    );
    let mut changed = ledger.clone();
    changed.lifecycle.waiting_clients[0].delivery = "Unavailable".into();
    assert_ne!(encode_native(&changed, NATIVE_LEDGER_BYTES).unwrap(), bytes);
    changed = ledger.clone();
    changed.groups[0].state = "AbandonedPending".into();
    assert_ne!(encode_native(&changed, NATIVE_LEDGER_BYTES).unwrap(), bytes);
    changed = ledger.clone();
    let member = changed.groups[0].members[0].clone();
    changed.groups[0]
        .duplicate_observations
        .push((member.clone(), member));
    assert_ne!(encode_native(&changed, NATIVE_LEDGER_BYTES).unwrap(), bytes);
    let mut malformed = serde_json::to_value(&ledger).unwrap();
    malformed["unrecognized_lifecycle"] = serde_json::json!(true);
    assert!(serde_json::from_value::<NativeLedger>(malformed).is_err());
    u5_ledger_semantics();
}

#[cfg(test)]
fn u5_ledger_semantics() {
    // Exact U3 source and ledger bytes; the semantic validator does not re-mint identity.
    let payload: Value = serde_json::from_str(r####"{"source":"{\"schema_version\":2,\"lineage\":\"mock\",\"source_revision\":1,\"producing_bindings\":[{\"request\":\"u3-deterministic\",\"generation\":null,\"origin\":{\"status\":\"Unknown\",\"reason\":\"producing identity unavailable before U3\"},\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"}}],\"responses\":[{\"occurrence\":0,\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"continuation\":0,\"provider_response_id\":\"msg_u3\",\"source_stop_reason\":\"end_turn\",\"block_start\":0,\"block_end\":2,\"stream_complete\":true,\"status\":\"Accepted\"}],\"ordered_blocks\":[{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"wire_index\":0,\"arrival_ordinal\":0,\"block\":{\"kind\":\"raw_block\",\"byte_len\":104,\"digest\":\"d7bcaf820f5be89d8c8060c00e406ada1a63dd0d73c36ef2d787b71f9b28eb52\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":null},{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"wire_index\":1,\"arrival_ordinal\":1,\"block\":{\"kind\":\"raw_block\",\"byte_len\":240,\"digest\":\"34469c5546072d7cce326a0d7bd71814edbd22e0753ec403d4eb085e62b7feff\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":null}],\"eligible_snapshots\":[{\"kind\":\"eligible_snapshot\",\"byte_len\":345,\"digest\":\"fbeb800577aa31e9f3cf6bced5b30a0a94fe15ecf898d48f5c69a4a44ebe549a\"},{\"kind\":\"eligible_snapshot\",\"byte_len\":343,\"digest\":\"f2cd1b35d0416155002174e86365f59eda6bb09db3edd7b9223b165d75179187\"}],\"older_sources\":[],\"eligible_member_links\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":0,\"occurrence\":1},\"identity\":{\"state\":\"Backed\",\"eligible_ref\":{\"kind\":\"eligible_snapshot\",\"byte_len\":345,\"digest\":\"fbeb800577aa31e9f3cf6bced5b30a0a94fe15ecf898d48f5c69a4a44ebe549a\"}}},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":1,\"occurrence\":1},\"identity\":{\"state\":\"Backed\",\"eligible_ref\":{\"kind\":\"eligible_snapshot\",\"byte_len\":343,\"digest\":\"f2cd1b35d0416155002174e86365f59eda6bb09db3edd7b9223b165d75179187\"}}}]}","ledger":"{\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"active_digest\":\"a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09\",\"groups\":[{\"key\":\"u3-deterministic:1:1:0\",\"family\":\"Messages\",\"execution\":\"server\",\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"},\"validated_join\":\"srvtoolu_u3\",\"keyless_server_fifo\":false,\"members\":[{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1}],\"duplicate_observations\":[],\"state\":\"Completed\"}],\"lifecycle\":{\"exchanges\":{\"u3-deterministic\":\"Terminal\"},\"groups\":{\"u3-deterministic:1:1:0\":\"Completed\"},\"waiting_clients\":[],\"terminal\":{}},\"accounting\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null}],\"usage\":[{\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"response_occurrence\":0,\"continuation\":0,\"observed\":{\"input_tokens\":11,\"output_tokens\":7},\"source_stop_reason\":\"end_turn\",\"outcome\":\"Accepted\",\"latency_ms\":1,\"http_dispatches\":1}],\"observations\":[{\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"digest\":\"e2789a08fd081abd3b049eb3fbbafb578d583ac99238b0b8d4da422a09881bad\"}],\"transitions\":[],\"eligible_member_accounting\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":0,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":1,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"}]}","raw":["{\"type\":\"server_tool_use\",\"id\":\"srvtoolu_u3\",\"name\":\"tool_search_tool_regex\",\"input\":{\"query\":\"lookup\"}}","{\"type\":\"tool_search_tool_result\",\"tool_use_id\":\"srvtoolu_u3\",\"content\":{\"type\":\"tool_search_tool_search_result\",\"tool_references\":[{\"type\":\"tool_reference\",\"tool_name\":\"alpha__lookup\"},{\"type\":\"tool_reference\",\"tool_name\":\"zeta__fetch\"}]}}"]}"####).unwrap();
    let (source, _) = serde_json::from_str::<NativeSourceV2>(payload["source"].as_str().unwrap())
        .unwrap()
        .into_parts();
    let (ledger, _) = serde_json::from_str::<NativeLedgerV2>(payload["ledger"].as_str().unwrap())
        .unwrap()
        .into_parts();
    let raw: BTreeMap<_, _> = payload["raw"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            (
                index as u64,
                super::tool_search::ToolSearchItem::from_wire(
                    serde_json::from_str(raw.as_str().unwrap()).unwrap(),
                )
                .unwrap(),
            )
        })
        .collect();
    let authorship = BTreeMap::from([
        (0, NativeIdAuthorship::Provider),
        (1, NativeIdAuthorship::Provider),
    ]);
    assert_eq!(
        native_validate_groups(&source, &ledger, &raw, &authorship, |left, right| left
            == right)
        .unwrap()
        .len(),
        1
    );
    for fault in [
        "state",
        "lifecycle-map",
        "member-order",
        "duplicate-member",
        "duplicate-link",
    ] {
        let mut bad = ledger.clone();
        match fault {
            "state" => bad.groups[0].state = "reopened".into(),
            "lifecycle-map" => {
                bad.lifecycle.groups.clear();
            }
            "member-order" => bad.groups[0].members.swap(0, 1),
            "duplicate-member" => {
                let member = bad.groups[0].members[0].clone();
                bad.groups[0].members.push(member);
            }
            "duplicate-link" => {
                let original = bad.groups[0].members[0].clone();
                bad.groups[0]
                    .duplicate_observations
                    .push((original.clone(), original));
            }
            _ => unreachable!(),
        }
        assert!(
            native_validate_groups(&source, &bad, &raw, &authorship, |left, right| left
                == right)
            .is_err(),
            "full group/lifecycle validation refuses {fault}"
        );
    }
}
