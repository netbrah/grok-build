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
pub enum NativeNextInput {
    #[default]
    None,
    Fresh,
    Results {
        messages: Vec<Value>,
        client_outputs: Vec<super::tool_search::ToolSearchItem>,
    },
}

#[derive(Default)]
pub struct NativePendingContext {
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
pub enum NativeReplayFamily {
    ResponsesClient,
    ResponsesServer,
    MessagesServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeIdAuthorship {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeNeutralStatus {
    Completed,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeNeutralKind {
    Call,
    Result,
    Omission,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeNeutralText {
    pub kind: NativeNeutralKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFDisposition {
    F,
    Strip,
}

/// Request-local provenance; never inferred from model-visible text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFGroup {
    pub key: String,
    pub call: NativeSourceCoordinate,
    pub call_digest: String,
    pub result: NativeSourceCoordinate,
    pub result_digest: String,
    pub disposition: NativeFDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFProvenance {
    pub version: u32,
    pub groups: Vec<NativeFGroup>,
}

impl NativeFProvenance {
    pub fn empty() -> Self {
        Self {
            version: 1,
            groups: Vec::new(),
        }
    }
}

pub use super::projection::{
    DiscoveryOrigin, NativeGroupDecision, NativeHistoryAction, NativeHistoryDecision,
    NativePreparedMember, NativeProjectionInputs, NativePureHistory, NativeQualification,
    NativeReplayDialect, NativeReplayDomain, NativeReplayShape, NativeShapeReceipt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeHistoryReason {
    Kept,
    Unqualified,
    MissingView,
    PriorDisposition,
    FBudget,
    Pending,
    Abandoned,
}

impl NativeHistoryReason {
    fn label(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Unqualified => "unqualified",
            Self::MissingView => "missing_view",
            Self::PriorDisposition => "prior_disposition",
            Self::FBudget => "f_budget",
            Self::Pending => "pending",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeHistoryCounts {
    pub kept: u64,
    pub demoted: u64,
    pub stripped: u64,
    pub omitted: u64,
    pub pending: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeMemberAccounting {
    pub source: NativeSourceCoordinate,
    pub source_digest: String,
    pub origin: Option<NativeBlobRef>,
    pub group: Option<String>,
    pub action: NativeHistoryAction,
    pub reason: NativeHistoryReason,
    pub output: Option<NativeSourceCoordinate>,
    pub output_digest: Option<String>,
    pub lost: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeReferenceAccounting {
    pub source: NativeSourceCoordinate,
    pub source_digest: String,
    pub identity: NativeEligibleMemberIdentity,
    pub group: Option<String>,
    pub action: NativeHistoryAction,
    pub reason: NativeHistoryReason,
    pub output: Option<NativeSourceCoordinate>,
    pub lost: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeGroupQualification {
    pub group: String,
    pub source: Option<NativeReplayDomain>,
    pub target: NativeReplayDomain,
    pub shape: NativeReplayShape,
    pub receipt: Option<NativeShapeReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryAccounting {
    pub history: NativeHistoryBoundary,
    pub source_revision: u64,
    pub source_digest: String,
    pub durable_projection_revision: u64,
    pub durable_dispositions: BTreeMap<String, String>,
    pub target_model: String,
    pub target: Option<NativeReplayDomain>,
    pub qualification: Vec<NativeGroupQualification>,
    pub original_admission: super::responses::SearchAdmission,
    pub groups: NativeHistoryCounts,
    pub members: NativeHistoryCounts,
    pub raw_members: Vec<NativeMemberAccounting>,
    pub references: Vec<NativeReferenceAccounting>,
    pub reasons: BTreeMap<NativeHistoryReason, u64>,
    pub summary: String,
    pub rediscovery: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLoadedDefinition {
    pub namespace_kind: NativeNamespaceKind,
    pub qualified_namespace: String,
    pub child_name: String,
    pub invocation_name: String,
    pub schema_digest: String,
    pub declaration: Value,
    pub definition: Value,
    pub source: NativeSourceCoordinate,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLoadedAuthority {
    pub definitions: Vec<NativeLoadedDefinition>,
    pub names: Vec<(String, String)>,
    pub promotion_names: Vec<String>,
    pub cache_digest: String,
}

#[derive(Clone, PartialEq, Eq)]
struct NativeHistoryCoreProof {
    decision: NativeHistoryDecision,
    loaded: NativeLoadedAuthority,
    accounting: NativeHistoryAccounting,
}

impl std::fmt::Debug for NativeHistoryCoreProof {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeHistoryCoreProof")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryCore {
    pub decision: NativeHistoryDecision,
    pub loaded: NativeLoadedAuthority,
    pub accounting: NativeHistoryAccounting,
    proof: Arc<NativeHistoryCoreProof>,
}

impl NativeHistoryCore {
    fn output_is_intact(&self) -> bool {
        let mut expected_accounting = self.proof.accounting.clone();
        // Ordinary-history rebasing is checked by the plan's occurrence mapping.
        expected_accounting.history = self.accounting.history.clone();
        self.decision == self.proof.decision
            && self.loaded == self.proof.loaded
            && self.accounting == expected_accounting
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryRefusal {
    pub reason: NativeUnavailable,
    pub summary: String,
    pub rediscovery: bool,
}

impl From<NativeUnavailable> for NativeHistoryRefusal {
    fn from(reason: NativeUnavailable) -> Self {
        Self {
            reason,
            summary: format!(
                "request-only native history refused: {reason:?}; no replay authority"
            ),
            rediscovery: true,
        }
    }
}

/// Pure request-local derivation, never a durable transition or production selector.
pub fn prepare_native_history_core(
    history: &NativePureHistory<'_>,
    input: &NativeProjectionInputs<'_>,
    qualification: impl FnMut(&NativeGroupRecord) -> (DiscoveryOrigin, NativeQualification),
) -> Result<NativeHistoryCore, NativeHistoryRefusal> {
    let decision =
        super::projection::prepare_native_history_reproject(history, input, qualification)?;
    let accounting = native_returned_accounting(history, input, &decision)?;
    let loaded = super::tool_search::native_loaded_authority(history, input, &decision)?;
    let proof = Arc::new(NativeHistoryCoreProof {
        decision: decision.clone(),
        loaded: loaded.clone(),
        accounting: accounting.clone(),
    });
    Ok(NativeHistoryCore {
        decision,
        loaded,
        accounting,
        proof,
    })
}

fn native_count_action(counts: &mut NativeHistoryCounts, action: NativeHistoryAction) {
    match action {
        NativeHistoryAction::Keep => counts.kept += 1,
        NativeHistoryAction::F => counts.demoted += 1,
        NativeHistoryAction::Strip => counts.stripped += 1,
        NativeHistoryAction::OmitSendOnly => counts.omitted += 1,
        NativeHistoryAction::AwaitClient
        | NativeHistoryAction::AwaitResults
        | NativeHistoryAction::ContinueOriginal => counts.pending += 1,
    }
}

fn native_returned_accounting(
    history: &NativePureHistory<'_>,
    input: &NativeProjectionInputs<'_>,
    decision: &NativeHistoryDecision,
) -> Result<NativeHistoryAccounting, NativeUnavailable> {
    use super::projection::{native_policy_blob, native_policy_history};
    native_policy_history(history)?;
    let active: NativeActive = serde_json::from_slice(native_policy_blob(
        history,
        &history.record.active,
        "active",
    )?)
    .map_err(|_| NativeUnavailable::InvalidReference)?;
    let target = decision.qualification.first().map(|q| q.target.clone());
    if decision
        .qualification
        .iter()
        .any(|q| Some(&q.target) != target.as_ref())
    {
        return Err(NativeUnavailable::ConflictingObservation);
    }
    let mut accounting = NativeHistoryAccounting {
        history: history.record.history_boundary.clone(),
        source_revision: history.source.source_revision,
        source_digest: history.record.source.digest.clone(),
        durable_projection_revision: active.projection_revision,
        durable_dispositions: active.dispositions.clone(),
        target_model: input.target_model_id.into(),
        target,
        qualification: decision.qualification.clone(),
        original_admission: input.target.admission(),
        groups: NativeHistoryCounts::default(),
        members: NativeHistoryCounts::default(),
        raw_members: Vec::new(),
        references: Vec::new(),
        reasons: BTreeMap::new(),
        summary: String::new(),
        rediscovery: false,
    };
    let reason_for = |group: &NativeGroupDecision| match group.action {
        NativeHistoryAction::Keep => NativeHistoryReason::Kept,
        NativeHistoryAction::OmitSendOnly => NativeHistoryReason::Abandoned,
        NativeHistoryAction::AwaitClient
        | NativeHistoryAction::AwaitResults
        | NativeHistoryAction::ContinueOriginal => NativeHistoryReason::Pending,
        NativeHistoryAction::F | NativeHistoryAction::Strip => {
            let prior_strip = active
                .dispositions
                .get(&group.key)
                .is_some_and(|old| old == "strip")
                || input.prior_f.is_some_and(|prior| {
                    prior.groups.iter().any(|old| {
                        old.key == group.key && old.disposition == NativeFDisposition::Strip
                    })
                });
            if group.action == NativeHistoryAction::Strip && !prior_strip {
                NativeHistoryReason::FBudget
            } else if active
                .dispositions
                .get(&group.key)
                .is_some_and(|old| old == "F" || old == "strip")
                || input
                    .prior_f
                    .is_some_and(|prior| prior.groups.iter().any(|old| old.key == group.key))
            {
                NativeHistoryReason::PriorDisposition
            } else if group
                .members
                .iter()
                .any(|source| !input.selected.contains(source))
            {
                NativeHistoryReason::MissingView
            } else {
                NativeHistoryReason::Unqualified
            }
        }
    };
    for group in &decision.groups {
        native_count_action(&mut accounting.groups, group.action);
        *accounting.reasons.entry(reason_for(group)).or_default() += 1;
    }
    for block in &history.source.ordered_blocks {
        let group = decision
            .groups
            .iter()
            .find(|group| group.members.contains(&block.coordinates));
        let member = decision
            .members
            .iter()
            .find(|member| member.source == block.coordinates);
        let action = member.map_or_else(
            || {
                if input.selected.contains(&block.coordinates) {
                    NativeHistoryAction::Keep
                } else {
                    NativeHistoryAction::OmitSendOnly
                }
            },
            |member| member.action,
        );
        let reason = group.map_or_else(
            || {
                if action == NativeHistoryAction::Keep {
                    NativeHistoryReason::Kept
                } else {
                    NativeHistoryReason::MissingView
                }
            },
            reason_for,
        );
        let lost = matches!(
            action,
            NativeHistoryAction::F | NativeHistoryAction::Strip | NativeHistoryAction::OmitSendOnly
        );
        let output = match member {
            Some(member) if member.raw.is_some() || member.neutral.is_some() => {
                Some(member.source.clone())
            }
            Some(member) => member.duplicate_of.clone().filter(|original| {
                decision.members.iter().any(|other| {
                    &other.source == original && (other.raw.is_some() || other.neutral.is_some())
                })
            }),
            None if action == NativeHistoryAction::Keep => Some(block.coordinates.clone()),
            None => None,
        };
        let output_digest = member
            .and_then(|member| member.neutral.as_ref())
            .map(|text| super::projection::native_content_digest(text.text.as_bytes()))
            .or_else(|| {
                output
                    .as_ref()
                    .filter(|_| !lost)
                    .map(|_| block.block.digest.clone())
            });
        native_count_action(&mut accounting.members, action);
        let witness = history
            .ledger
            .accounting
            .iter()
            .find(|entry| entry.source == block.coordinates)
            .ok_or(NativeUnavailable::InvalidReference)?;
        accounting.raw_members.push(NativeMemberAccounting {
            source: block.coordinates.clone(),
            source_digest: witness.source_digest.clone(),
            origin: history
                .source
                .producing_bindings
                .iter()
                .find(|binding| binding.request == block.coordinates.producing_binding)
                .map(|binding| binding.origin_ref.clone()),
            group: group.map(|group| group.key.clone()),
            action,
            reason,
            output,
            output_digest,
            lost,
        });
    }
    for block in &history.source.ordered_blocks {
        let raw: Value =
            serde_json::from_slice(native_policy_blob(history, &block.block, "raw_block")?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
        let leaves = native_candidate_references(&raw)?;
        let parent = accounting
            .raw_members
            .iter()
            .find(|member| member.source == block.coordinates)
            .ok_or(NativeUnavailable::InvalidReference)?;
        for (ordinal, _) in leaves.iter().enumerate() {
            let mut source = block.coordinates.clone();
            source.submember =
                Some(u64::try_from(ordinal).map_err(|_| NativeUnavailable::Capacity)?);
            let identity = match history.identity {
                NativeIdentityExtension::Revision1Unknown => {
                    NativeEligibleMemberIdentity::Unavailable {
                        reason: NativeUnavailable::UnknownIdentity,
                    }
                }
                NativeIdentityExtension::Revision2 {
                    eligible_member_links,
                    eligible_member_accounting,
                } => {
                    let links: Vec<_> = eligible_member_links
                        .iter()
                        .filter(|link| link.source == source)
                        .collect();
                    let witnesses: Vec<_> = eligible_member_accounting
                        .iter()
                        .filter(|witness| witness.source == source)
                        .collect();
                    if links.len() != 1
                        || witnesses.len() != 1
                        || witnesses[0].source_digest != parent.source_digest
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    links[0].identity.clone()
                }
            };
            accounting.references.push(NativeReferenceAccounting {
                source,
                source_digest: parent.source_digest.clone(),
                identity,
                group: parent.group.clone(),
                action: parent.action,
                reason: parent.reason,
                output: parent.output.clone(),
                lost: parent.lost,
            });
        }
    }
    if let NativeIdentityExtension::Revision2 {
        eligible_member_links,
        eligible_member_accounting,
    } = history.identity
        && (accounting.references.len() != eligible_member_links.len()
            || accounting.references.len() != eligible_member_accounting.len())
    {
        return Err(NativeUnavailable::InvalidReference);
    }
    NativeResource::LedgerMembers.admit(
        accounting.raw_members.len() as u64,
        accounting.references.len() as u64,
    )?;
    accounting.rediscovery =
        accounting.groups.demoted + accounting.groups.stripped + accounting.groups.omitted > 0;
    let g = &accounting.groups;
    let m = &accounting.members;
    let reasons = accounting
        .reasons
        .iter()
        .map(|(reason, count)| format!("{}={count}", reason.label()))
        .collect::<Vec<_>>()
        .join(",");
    accounting.summary = format!(
        "request-only native history; groups keep={} F={} strip={} omit={} pending={}; members keep={} F={} strip={} omit={} pending={}; references={}; reasons={}; aggregate={}; rediscovery={}; durable unchanged",
        g.kept,
        g.demoted,
        g.stripped,
        g.omitted,
        g.pending,
        m.kept,
        m.demoted,
        m.stripped,
        m.omitted,
        m.pending,
        accounting.references.len(),
        reasons,
        decision.groups.len(),
        accounting.rediscovery
    );
    if accounting.summary.len() > 512 || accounting.reasons.len() > 8 {
        return Err(NativeUnavailable::Capacity);
    }
    Ok(accounting)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistoryRequirement {
    Inactive,
    Selected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeHistoryWriteAuthority {
    SendOnly,
    SwitchStore {
        lineage: String,
        binding_generation: u64,
    },
    OwningPrimary {
        lineage: String,
        binding_generation: u64,
        request: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistoryExecution {
    Client,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHistoryVersions {
    pub schema: u32,
    pub policy: u32,
    pub projection: u32,
    pub reader: u32,
    pub limits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryCapture {
    pub history: NativeHistoryBoundary,
    pub history_revision: u64,
    pub lineage: NativeLineage,
    pub native_generation: u64,
    pub high_water: u64,
    pub projection_revision: u64,
    pub strict_append_sequence: u64,
    pub binding_generation: u64,
    pub install_generation: u64,
    pub issued_request: String,
    pub target: NativeReplayDomain,
    pub execution: NativeHistoryExecution,
    pub original_admission: super::responses::SearchAdmission,
    pub versions: NativeHistoryVersions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryRequestContext {
    requirement: NativeHistoryRequirement,
    capture: NativeHistoryCapture,
    authority: NativeHistoryWriteAuthority,
}

impl NativeHistoryRequestContext {
    pub fn captured(
        requirement: NativeHistoryRequirement,
        capture: NativeHistoryCapture,
        authority: NativeHistoryWriteAuthority,
    ) -> Self {
        Self {
            requirement,
            capture,
            authority,
        }
    }

    pub fn requirement(&self) -> NativeHistoryRequirement {
        self.requirement
    }
    pub fn capture(&self) -> &NativeHistoryCapture {
        &self.capture
    }
    pub fn authority(&self) -> &NativeHistoryWriteAuthority {
        &self.authority
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFinalCarrier {
    bytes: Arc<[u8]>,
    digest: String,
    tools: Option<NativeToolsRange>,
}

impl NativeFinalCarrier {
    pub fn capture(bytes: Arc<[u8]>) -> Result<Self, NativeUnavailable> {
        NativeResource::SnapshotPayloadBytes.admit(bytes.len() as u64, 0)?;
        let tools = native_final_tools_range(&bytes)?;
        Ok(Self {
            digest: super::projection::native_content_digest(&bytes),
            bytes,
            tools,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn tools(&self) -> Option<&NativeToolsRange> {
        self.tools.as_ref()
    }
    pub fn declarations(&self) -> Option<&[u8]> {
        self.tools.as_ref().map(|range| {
            &self.bytes[range.offset as usize..(range.offset + range.byte_len) as usize]
        })
    }
}

fn native_final_tools_range(bytes: &[u8]) -> Result<Option<NativeToolsRange>, NativeUnavailable> {
    fn whitespace(bytes: &[u8], at: &mut usize) {
        while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
            *at += 1;
        }
    }
    fn take<'a, T: Deserialize<'a>>(
        bytes: &'a [u8],
        at: &mut usize,
    ) -> Result<T, NativeUnavailable> {
        let mut stream = serde_json::Deserializer::from_slice(&bytes[*at..]).into_iter::<T>();
        let value = stream
            .next()
            .ok_or(NativeUnavailable::Fidelity)?
            .map_err(|_| NativeUnavailable::Fidelity)?;
        *at += stream.byte_offset();
        Ok(value)
    }
    let mut at = 0usize;
    whitespace(bytes, &mut at);
    if bytes.get(at) != Some(&b'{') {
        return Err(NativeUnavailable::Fidelity);
    }
    at += 1;
    let mut keys = std::collections::BTreeSet::new();
    let mut tools = None;
    whitespace(bytes, &mut at);
    if bytes.get(at) == Some(&b'}') {
        at += 1;
    } else {
        loop {
            whitespace(bytes, &mut at);
            let key: String = take(bytes, &mut at)?;
            if !keys.insert(key.clone()) {
                return Err(NativeUnavailable::Fidelity);
            }
            whitespace(bytes, &mut at);
            if bytes.get(at) != Some(&b':') {
                return Err(NativeUnavailable::Fidelity);
            }
            at += 1;
            whitespace(bytes, &mut at);
            let start = at;
            let _: serde::de::IgnoredAny = take(bytes, &mut at)?;
            if key == "tools" {
                if bytes.get(start) != Some(&b'[') {
                    return Err(NativeUnavailable::Fidelity);
                }
                tools = Some(NativeToolsRange {
                    offset: start as u64,
                    byte_len: (at - start) as u64,
                    digest: super::projection::native_content_digest(&bytes[start..at]),
                });
            }
            whitespace(bytes, &mut at);
            match bytes.get(at) {
                Some(b',') => {
                    at += 1;
                }
                Some(b'}') => {
                    at += 1;
                    break;
                }
                _ => return Err(NativeUnavailable::Fidelity),
            }
        }
    }
    whitespace(bytes, &mut at);
    if at != bytes.len() {
        return Err(NativeUnavailable::Fidelity);
    }
    Ok(tools)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDeclarationIdentity {
    pub declaration_index: u64,
    pub child_index: Option<u64>,
    pub namespace_kind: NativeNamespaceKind,
    pub qualified_namespace: String,
    pub child_name: String,
    pub declaration: Value,
    pub schema: NativeBlobRef,
    pub schema_bytes: Arc<[u8]>,
    pub provenance: NativeBlobRef,
    pub provenance_bytes: Arc<[u8]>,
    pub binding_bytes: Option<Arc<[u8]>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativePlanReadiness {
    Tentative,
    SendOnly,
    OwningPrimary {
        preparation: Box<NativeBoundPreparation>,
        certificate: Arc<NativeHistoryCommitCertificate>,
    },
    RequiredGeneration {
        generation: u64,
        high_water: u64,
        authority: Option<NativeGenerationAuthority>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeLoweredHistory {
    conversation: String,
    instructions: Option<String>,
}

impl NativeLoweredHistory {
    fn capture(
        request: &crate::ConversationRequest,
        backend: &crate::ApiBackend,
    ) -> Result<Self, NativePlanRefusal> {
        let body = match backend {
            crate::ApiBackend::Messages => {
                serde_json::to_value(super::messages::build_messages_request(request))
            }
            crate::ApiBackend::Responses => serde_json::to_value(
                async_openai::types::responses::CreateResponse::from(request),
            ),
            crate::ApiBackend::ChatCompletions => {
                return Err(NativePlanRefusal::InvalidQualification);
            }
        }
        .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        Self::from_body(&body, backend)
    }

    fn from_body(body: &Value, backend: &crate::ApiBackend) -> Result<Self, NativePlanRefusal> {
        let (conversation, instructions) = match backend {
            crate::ApiBackend::Messages => ("messages", "system"),
            crate::ApiBackend::Responses => ("input", "instructions"),
            crate::ApiBackend::ChatCompletions => {
                return Err(NativePlanRefusal::InvalidQualification);
            }
        };
        let conversation = body
            .get(conversation)
            .filter(|value| value.is_array())
            .ok_or(NativePlanRefusal::FinalCarrierChanged)?;
        let digest = |value: &Value| {
            serde_json::to_vec(value)
                .map(|bytes| super::projection::native_content_digest(&bytes))
                .map_err(|_| NativePlanRefusal::FinalCarrierChanged)
        };
        Ok(Self {
            conversation: digest(conversation)?,
            instructions: body.get(instructions).map(digest).transpose()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeResponsesInputProjection {
    Preserve,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFinalizationEvidence {
    backend: crate::ApiBackend,
    dialect: NativeReplayDialect,
    history: NativeLoweredHistory,
    carrier: NativeFinalCarrier,
    request_digest: String,
}

impl NativeFinalizationEvidence {
    fn request_digest(
        request: &crate::ConversationRequest,
        backend: &crate::ApiBackend,
    ) -> Result<String, NativePlanRefusal> {
        let body = match backend {
            crate::ApiBackend::Messages => {
                serde_json::to_value(super::messages::build_messages_request(request))
            }
            crate::ApiBackend::Responses => serde_json::to_value(
                async_openai::types::responses::CreateResponse::from(request),
            ),
            crate::ApiBackend::ChatCompletions => {
                return Err(NativePlanRefusal::InvalidQualification);
            }
        }
        .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        let values = [
            serde_json::to_value(&request.items).map_err(|_| NativePlanRefusal::InvalidHistory)?,
            body,
            Value::Array(crate::extra_tool_entries_for_route(
                &request.hosted_tools,
                request.search_admission,
            )),
        ];
        let mut digests = Vec::with_capacity(values.len());
        for value in values {
            let bytes =
                serde_json::to_vec(&value).map_err(|_| NativePlanRefusal::InvalidHistory)?;
            NativeResource::SnapshotPayloadBytes
                .admit(bytes.len() as u64, 0)
                .map_err(|_| NativePlanRefusal::InvalidHistory)?;
            digests.push(super::projection::native_content_digest(&bytes));
        }
        Ok(super::projection::native_content_digest(
            &serde_json::to_vec(&digests).map_err(|_| NativePlanRefusal::InvalidHistory)?,
        ))
    }

    fn validates(
        &self,
        context: &NativeHistoryRequestContext,
        request: &crate::ConversationRequest,
        carrier: &NativeFinalCarrier,
    ) -> Result<(), NativePlanRefusal> {
        if self.backend != context.capture.target.backend
            || self.dialect != context.capture.target.dialect
            || &self.carrier != carrier
            || self.request_digest != Self::request_digest(request, &self.backend)?
        {
            return Err(NativePlanRefusal::FinalCarrierChanged);
        }
        Ok(())
    }
    fn checked_body(
        request: &crate::ConversationRequest,
        expected: &Value,
        backend: crate::ApiBackend,
        dialect: NativeReplayDialect,
        carrier: NativeFinalCarrier,
    ) -> Result<Self, NativePlanRefusal> {
        let actual: Value = serde_json::from_slice(carrier.bytes())
            .map_err(|_| NativePlanRefusal::FinalCarrierChanged)?;
        let history = NativeLoweredHistory::from_body(expected, &backend)?;
        if NativeLoweredHistory::from_body(&actual, &backend)? != history
            || actual.get("model") != expected.get("model")
            || actual.get("tools") != expected.get("tools")
            || (backend == crate::ApiBackend::Responses
                && actual.get("store") != Some(&Value::Bool(false)))
        {
            return Err(NativePlanRefusal::FinalCarrierChanged);
        }
        Ok(Self {
            request_digest: Self::request_digest(request, &backend)?,
            backend,
            dialect,
            history,
            carrier,
        })
    }

    pub fn checked_responses(
        request: &crate::ConversationRequest,
        dialect: crate::ResponsesReplayDialect,
        projection: NativeResponsesInputProjection,
        carrier: NativeFinalCarrier,
    ) -> Result<Self, NativePlanRefusal> {
        let mut expected = serde_json::to_value(
            async_openai::types::responses::CreateResponse::from(request),
        )
        .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        let extras =
            crate::extra_tool_entries_for_route(&request.hosted_tools, request.search_admission);
        if !extras.is_empty() {
            let (declarations, hosted): (Vec<_>, Vec<_>) = extras.into_iter().partition(|entry| {
                entry.get("type").and_then(Value::as_str)
                    == Some(crate::TOOL_SEARCH_DECLARATION_TYPE)
            });
            let tools = expected
                .as_object_mut()
                .ok_or(NativePlanRefusal::InvalidHistory)?
                .entry("tools")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or(NativePlanRefusal::InvalidDeclarations)?;
            tools.splice(0..0, declarations);
            tools.extend(hosted);
        }
        if dialect == crate::ResponsesReplayDialect::Codex
            && let Some(tools) = expected.get_mut("tools").and_then(Value::as_array_mut)
        {
            for tool in tools {
                if tool.get("type").and_then(Value::as_str) == Some("web_search")
                    && let Some(tool) = tool.as_object_mut()
                {
                    tool.entry("external_web_access")
                        .or_insert(Value::Bool(true));
                }
            }
        }
        crate::patch_reasoning_text_types(&mut expected);
        crate::patch_reasoning_empty_ids(
            &mut expected,
            request.model.as_deref().unwrap_or_default(),
        );
        crate::strip_sentinel_web_search_actions(&mut expected);
        for replacement in request.raw_responses_input_replacements(dialect) {
            let input = expected
                .get_mut("input")
                .and_then(Value::as_array_mut)
                .ok_or(NativePlanRefusal::InvalidHistory)?;
            *input
                .get_mut(replacement.input_item_index)
                .ok_or(NativePlanRefusal::InvalidHistory)? = replacement.value;
        }
        let input = expected
            .get_mut("input")
            .and_then(Value::as_array_mut)
            .ok_or(NativePlanRefusal::InvalidHistory)?;
        let proactive = request.reasoning_effort == Some(crate::ReasoningEffort::Ultra);
        let mode = match (dialect == crate::ResponsesReplayDialect::Codex, proactive) {
            (true, false) => "explicit_request_only",
            (true, true) => "proactive",
            (false, false) => {
                "Multi-agent mode: explicit_request_only — subagents may be spawned only when the user explicitly requests."
            }
            (false, true) => {
                "Multi-agent mode: proactive — subagents may be spawned proactively when the task benefits from delegation."
            }
        };
        let anchor = request
            .x_grok_conv_id
            .as_deref()
            .is_some_and(|id| !id.is_empty());
        let at = if !anchor
            && input
                .last()
                .is_some_and(|item| item.get("role").and_then(Value::as_str) == Some("user"))
        {
            input.len() - 1
        } else {
            input.len()
        };
        input.insert(
            at,
            serde_json::json!({"type":"message","role":"developer", "content":[{
                "type":"input_text", "text":format!("<multi_agent_mode>{mode}</multi_agent_mode>"),
            }]}),
        );
        if projection == NativeResponsesInputProjection::Strict {
            for item in input {
                if item.get("type").and_then(Value::as_str) == Some("reasoning")
                    && let Some(item) = item.as_object_mut()
                {
                    item.remove("content");
                    item.remove("id");
                }
            }
        }
        Self::checked_body(
            request,
            &expected,
            crate::ApiBackend::Responses,
            NativeReplayDialect::Responses(dialect),
            carrier,
        )
    }

    pub fn checked_native_messages(
        request: &crate::ConversationRequest,
        core: &NativeHistoryCore,
        carrier: NativeFinalCarrier,
    ) -> Result<Self, NativePlanRefusal> {
        if !core.output_is_intact() {
            return Err(NativePlanRefusal::InvalidAccounting);
        }
        let kept: Vec<_> = core
            .decision
            .members
            .iter()
            .filter(|member| member.action == NativeHistoryAction::Keep)
            .filter_map(|member| member.raw.as_ref())
            .collect();
        let mut marked = request.clone();
        let mut replacements = Vec::new();
        for item in &mut marked.items {
            if let super::ConversationItem::Discovery { item: discovery } = item {
                let ordinal = replacements.len();
                if kept.get(ordinal).copied() != Some(discovery.raw()) {
                    return Err(NativePlanRefusal::InvalidHistory);
                }
                let marker = format!("native-history-validation-{}-{ordinal}", carrier.digest());
                replacements.push((marker.clone(), discovery.raw().clone()));
                *item = super::ConversationItem::assistant(marker);
            }
        }
        if replacements.len() != kept.len() {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        let mut expected = serde_json::to_value(super::messages::build_messages_request(&marked))
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        let mut seen = std::collections::BTreeSet::new();
        for message in expected
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or(NativePlanRefusal::InvalidHistory)?
        {
            let content = message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or(NativePlanRefusal::InvalidHistory)?;
            for part in content {
                if let Some(index) = replacements.iter().position(|(marker, _)| {
                    part.get("text").and_then(Value::as_str) == Some(marker.as_str())
                }) {
                    if !seen.insert(index) {
                        return Err(NativePlanRefusal::InvalidHistory);
                    }
                    *part = replacements[index].1.clone();
                }
            }
        }
        if seen.len() != replacements.len() {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        Self::checked_body(
            request,
            &expected,
            crate::ApiBackend::Messages,
            NativeReplayDialect::Messages,
            carrier,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePlanProducingBinding {
    request: String,
    generation: Option<u64>,
    origin: Value,
}

#[derive(Serialize)]
struct NativePlanGroupIndex<'a> {
    key: &'a str,
    action: String,
    members: &'a [NativeSourceCoordinate],
    waiting_clients: &'a [String],
}

#[derive(Serialize)]
struct NativePlanFIndex<'a> {
    key: &'a str,
    call: &'a NativeSourceCoordinate,
    call_digest: &'a str,
    result: &'a NativeSourceCoordinate,
    result_digest: &'a str,
    disposition: &'static str,
}

#[derive(Serialize)]
struct NativePlanActiveView<'a> {
    view_links: Vec<&'a NativeViewLink>,
    outputs: &'a BTreeMap<usize, Option<super::ConversationItem>>,
    groups: Vec<NativePlanGroupIndex<'a>>,
    f_version: u32,
    f_groups: Vec<NativePlanFIndex<'a>>,
    duplicates: Vec<(&'a NativeSourceCoordinate, &'a NativeSourceCoordinate)>,
}

impl NativePlanActiveView<'_> {
    fn check(
        ledger: &NativeLedger,
        decision: &NativeHistoryDecision,
        outputs: &BTreeMap<usize, Option<super::ConversationItem>>,
    ) -> Result<(), NativePlanRefusal> {
        let view = NativePlanActiveView {
            view_links: ledger
                .accounting
                .iter()
                .filter(|link| {
                    decision
                        .members
                        .iter()
                        .any(|member| member.source == link.source)
                })
                .collect(),
            outputs,
            groups: decision
                .groups
                .iter()
                .map(|group| NativePlanGroupIndex {
                    key: &group.key,
                    action: format!("{:?}", group.action),
                    members: &group.members,
                    waiting_clients: &group.waiting_clients,
                })
                .collect(),
            f_version: decision.f_provenance.version,
            f_groups: decision
                .f_provenance
                .groups
                .iter()
                .map(|group| NativePlanFIndex {
                    key: &group.key,
                    call: &group.call,
                    call_digest: &group.call_digest,
                    result: &group.result,
                    result_digest: &group.result_digest,
                    disposition: match group.disposition {
                        NativeFDisposition::F => "F",
                        NativeFDisposition::Strip => "strip",
                    },
                })
                .collect(),
            duplicates: decision
                .members
                .iter()
                .filter_map(|member| {
                    member
                        .duplicate_of
                        .as_ref()
                        .map(|original| (&member.source, original))
                })
                .collect(),
        };
        native_encoded_len(&view, NATIVE_ACTIVE_BYTES)
            .map(|_| ())
            .map_err(|_| NativePlanRefusal::InvalidHistory)
    }
}

pub struct NativeHistoryPlanInput<'a> {
    pub record: NativeSearchHistoryV1,
    pub input_history_bytes: Arc<[u8]>,
    pub ledger_bytes: Arc<[u8]>,
    pub lowering_request: &'a crate::ConversationRequest,
    pub post_history_bytes: Arc<[u8]>,
    pub carrier: NativeFinalCarrier,
    pub identities: Vec<NativeDeclarationIdentity>,
    pub qualification: Vec<NativeGroupQualification>,
    pub core: NativeHistoryCore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryPlan {
    context: NativeHistoryRequestContext,
    record: NativeSearchHistoryV1,
    input_history_bytes: Arc<[u8]>,
    ledger_bytes: Arc<[u8]>,
    post_history: NativeHistoryBoundary,
    post_history_bytes: Arc<[u8]>,
    lowered_history: NativeLoweredHistory,
    carrier: NativeFinalCarrier,
    identities: Vec<NativeDeclarationIdentity>,
    qualification: Vec<NativeGroupQualification>,
    core: NativeHistoryCore,
    readiness: NativePlanReadiness,
    finalization: Option<NativeFinalizationEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistoryReady {
    Inactive,
    SendOnly,
    OwningPrimary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePlanRefusal {
    MissingPlan,
    InactivePlan,
    StaleContext,
    UnsupportedVersion,
    InvalidHistory,
    InvalidDeclarations,
    InvalidQualification,
    InvalidAccounting,
    NotReady,
    AuthorityUnverified,
    KeepCarrierUnverified,
    FinalCarrierChanged,
}

impl NativeHistoryPlan {
    pub fn checked(
        context: NativeHistoryRequestContext,
        input: NativeHistoryPlanInput<'_>,
    ) -> Result<Self, NativePlanRefusal> {
        Self::checked_with_finalization(context, input, None)
    }

    pub fn checked_finalized(
        context: NativeHistoryRequestContext,
        input: NativeHistoryPlanInput<'_>,
        finalization: NativeFinalizationEvidence,
    ) -> Result<Self, NativePlanRefusal> {
        finalization.validates(&context, input.lowering_request, &input.carrier)?;
        if input
            .core
            .decision
            .groups
            .iter()
            .any(|group| group.action == NativeHistoryAction::Keep)
            && !input.core.loaded.definitions.is_empty()
        {
            let current = input
                .lowering_request
                .native_eligible_inputs
                .as_ref()
                .filter(|current| current.matches_specs(&input.lowering_request.tools))
                .ok_or(NativePlanRefusal::InvalidDeclarations)?;
            for loaded in &input.core.loaded.definitions {
                let matches = current
                    .entries()
                    .iter()
                    .filter(|entry| {
                        entry.namespace_kind() == loaded.namespace_kind
                            && entry.qualified_namespace() == loaded.qualified_namespace
                            && entry.child_name() == loaded.child_name
                            && entry.schema_digest() == loaded.schema_digest
                            && serde_json::to_value(entry.declaration()).ok().as_ref()
                                == Some(&loaded.declaration)
                    })
                    .count();
                if matches != 1 {
                    return Err(NativePlanRefusal::InvalidDeclarations);
                }
            }
        }
        Self::checked_with_finalization(context, input, Some(finalization))
    }

    fn checked_with_finalization(
        context: NativeHistoryRequestContext,
        input: NativeHistoryPlanInput<'_>,
        finalization: Option<NativeFinalizationEvidence>,
    ) -> Result<Self, NativePlanRefusal> {
        let NativeHistoryPlanInput {
            record,
            input_history_bytes,
            ledger_bytes,
            lowering_request,
            post_history_bytes,
            carrier,
            identities,
            qualification,
            core,
        } = input;
        NativeResource::SnapshotPayloadBytes
            .admit(post_history_bytes.len() as u64, 0)
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        let post: Vec<super::ConversationItem> = serde_json::from_slice(&post_history_bytes)
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        if lowering_request.model.as_deref() != Some(context.capture.target.model.as_str())
            || lowering_request.search_admission != Some(context.capture.original_admission)
            || serde_json::to_value(&lowering_request.items)
                .map_err(|_| NativePlanRefusal::InvalidHistory)?
                != serde_json::to_value(&post).map_err(|_| NativePlanRefusal::InvalidHistory)?
        {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        let lowered_history = if let Some(evidence) = &finalization {
            evidence.history.clone()
        } else {
            NativeLoweredHistory::capture(lowering_request, &context.capture.target.backend)?
        };
        let plan = Self {
            context,
            record,
            input_history_bytes,
            ledger_bytes,
            post_history: NativeHistoryBoundary {
                items: post.len() as u64,
                digest: super::projection::native_content_digest(&post_history_bytes),
            },
            post_history_bytes,
            lowered_history,
            carrier,
            identities,
            qualification,
            core,
            readiness: NativePlanReadiness::Tentative,
            finalization,
        };
        plan.validate_binding()?;
        Ok(plan)
    }

    pub fn send_only(mut self) -> Result<Self, NativePlanRefusal> {
        if self.context.authority != NativeHistoryWriteAuthority::SendOnly {
            return Err(NativePlanRefusal::AuthorityUnverified);
        }
        self.validate_binding()?;
        self.readiness = NativePlanReadiness::SendOnly;
        Ok(self)
    }

    pub fn require_generation(
        mut self,
        generation: u64,
        high_water: u64,
        authority: Option<NativeGenerationAuthority>,
    ) -> Self {
        self.readiness = NativePlanReadiness::RequiredGeneration {
            generation,
            high_water,
            authority,
        };
        self
    }

    pub fn context(&self) -> &NativeHistoryRequestContext {
        &self.context
    }
    pub fn post_history(&self) -> &NativeHistoryBoundary {
        &self.post_history
    }
    pub fn post_history_bytes(&self) -> &[u8] {
        &self.post_history_bytes
    }
    pub fn carrier(&self) -> &NativeFinalCarrier {
        &self.carrier
    }
    pub fn identities(&self) -> &[NativeDeclarationIdentity] {
        &self.identities
    }
    pub fn qualification(&self) -> &[NativeGroupQualification] {
        &self.qualification
    }
    pub fn core(&self) -> &NativeHistoryCore {
        &self.core
    }
    pub fn readiness(&self) -> &NativePlanReadiness {
        &self.readiness
    }

    fn validate_binding(&self) -> Result<(), NativePlanRefusal> {
        let capture = &self.context.capture;
        if self.context.requirement != NativeHistoryRequirement::Selected {
            return Err(NativePlanRefusal::InactivePlan);
        }
        if self.finalization.is_none()
            && self
                .core
                .decision
                .groups
                .iter()
                .any(|group| group.action == NativeHistoryAction::Keep)
        {
            return Err(NativePlanRefusal::KeepCarrierUnverified);
        }
        let versions = capture.versions;
        if !matches!(
            (
                versions.schema,
                versions.policy,
                versions.reader,
                versions.limits
            ),
            (1, 1, 1, 1) | (2, 1, 2, 1)
        ) || versions.projection != 1
            || (
                versions.schema,
                versions.policy,
                versions.reader,
                versions.limits,
            ) != (
                self.record.schema_version,
                self.record.policy_revision,
                self.record.reader_floor,
                self.record.limits_revision,
            )
        {
            return Err(NativePlanRefusal::UnsupportedVersion);
        }
        let digest = |text: &str| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !digest(&capture.history.digest)
            || !digest(&capture.lineage.source_digest)
            || capture.lineage.id.is_empty()
            || capture.issued_request.is_empty()
            || !capture.target.complete()
            || self.record.lineage != capture.lineage
            || self.record.source.digest != capture.lineage.source_digest
            || self.record.history_boundary != capture.history
            || self.record.generation != capture.native_generation
            || self.record.high_water != capture.high_water
            || self.post_history.digest
                != super::projection::native_content_digest(&self.post_history_bytes)
            || self.core.accounting.history != capture.history
            || self.core.accounting.source_revision != capture.lineage.source_revision
            || self.core.accounting.source_digest != capture.lineage.source_digest
            || self.core.accounting.durable_projection_revision != capture.projection_revision
            || self.core.accounting.target_model != capture.target.model
            || self.core.accounting.original_admission != capture.original_admission
        {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        let body: Value = serde_json::from_slice(self.carrier.bytes())
            .map_err(|_| NativePlanRefusal::FinalCarrierChanged)?;
        if body.get("model").and_then(Value::as_str) != Some(capture.target.model.as_str())
            || (capture.target.backend == crate::ApiBackend::Responses
                && body.get("store") != Some(&Value::Bool(false)))
        {
            return Err(NativePlanRefusal::FinalCarrierChanged);
        }
        let post: Vec<super::ConversationItem> = serde_json::from_slice(&self.post_history_bytes)
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        if post.len() as u64 != self.post_history.items {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        self.validate_output_mapping(&post)?;
        if NativeLoweredHistory::from_body(&body, &capture.target.backend)? != self.lowered_history
        {
            return Err(NativePlanRefusal::FinalCarrierChanged);
        }
        self.validate_declarations()?;
        self.validate_decisions()?;
        if !self.core.output_is_intact() {
            return Err(NativePlanRefusal::InvalidAccounting);
        }
        if self
            .finalization
            .as_ref()
            .is_some_and(|evidence| evidence.carrier != self.carrier)
        {
            return Err(NativePlanRefusal::FinalCarrierChanged);
        }
        Ok(())
    }

    fn validate_output_mapping(
        &self,
        post: &[super::ConversationItem],
    ) -> Result<(), NativePlanRefusal> {
        use super::ConversationItem;
        NativeResource::LedgerBytes
            .admit(self.ledger_bytes.len() as u64, 0)
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        if self.record.ledger.kind != "ledger"
            || self.record.ledger.byte_len != self.ledger_bytes.len() as u64
            || self.record.ledger.digest
                != super::projection::native_content_digest(&self.ledger_bytes)
            || self.context.capture.history.digest
                != super::projection::native_content_digest(&self.input_history_bytes)
        {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        let ledger = match self.record.schema_version {
            1 => serde_json::from_slice::<NativeLedger>(&self.ledger_bytes),
            2 => serde_json::from_slice::<NativeLedgerV2>(&self.ledger_bytes)
                .map(|ledger| ledger.into_parts().0),
            _ => return Err(NativePlanRefusal::UnsupportedVersion),
        }
        .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        let input: Vec<ConversationItem> = serde_json::from_slice(&self.input_history_bytes)
            .map_err(|_| NativePlanRefusal::InvalidHistory)?;
        if input.len() as u64 != self.context.capture.history.items
            || ledger.source_digest != self.record.source.digest
            || ledger.active_digest != self.record.active.digest
        {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        for group in &self.core.decision.groups {
            let source_groups: Vec<_> = ledger
                .groups
                .iter()
                .filter(|source| source.key == group.key)
                .collect();
            if source_groups.len() != 1 || source_groups[0].members != group.members {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
        }
        let mut outputs = BTreeMap::new();
        for member in &self.core.decision.members {
            let links: Vec<_> = ledger
                .accounting
                .iter()
                .filter(|link| link.source == member.source)
                .collect();
            if links.len() != 1 {
                return Err(NativePlanRefusal::InvalidHistory);
            }
            let link = links[0];
            let duplicate = ledger
                .groups
                .iter()
                .flat_map(|group| &group.duplicate_observations)
                .find(|(source, _)| source == &member.source)
                .map(|(_, original)| original);
            if member.duplicate_of.as_ref() != duplicate
                || !self
                    .core
                    .accounting
                    .raw_members
                    .iter()
                    .any(|row| row.source == link.source && row.source_digest == link.source_digest)
            {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
            let output = match (&member.neutral, &member.raw) {
                (Some(neutral), None) => Some(ConversationItem::assistant(neutral.text.clone())),
                (None, Some(raw)) => Some(ConversationItem::Discovery {
                    item: super::tool_search::ToolSearchItem::from_wire(raw.clone())
                        .map_err(|_| NativePlanRefusal::InvalidHistory)?,
                }),
                (None, None) => None,
                (Some(_), Some(_)) => return Err(NativePlanRefusal::InvalidHistory),
            };
            if duplicate.is_some() && output.is_some() {
                return Err(NativePlanRefusal::InvalidHistory);
            }
            let Some(index) = link.view_item else {
                if output.is_some() {
                    return Err(NativePlanRefusal::InvalidHistory);
                }
                continue;
            };
            let index = usize::try_from(index).map_err(|_| NativePlanRefusal::InvalidHistory)?;
            let original = input.get(index).ok_or(NativePlanRefusal::InvalidHistory)?;
            let digest = super::projection::native_content_digest(
                &serde_json::to_vec(original).map_err(|_| NativePlanRefusal::InvalidHistory)?,
            );
            let native = match original {
                ConversationItem::Discovery { item } => {
                    member.raw.as_ref().is_none_or(|raw| item.raw() == raw)
                }
                ConversationItem::Assistant(assistant) => {
                    link.role == "ProjectedText"
                        && assistant.tool_calls.is_empty()
                        && self.core.decision.f_provenance.version == 1
                        && self.core.decision.f_provenance.groups.iter().any(|group| {
                            group.call == member.source || group.result == member.source
                        })
                }
                _ => false,
            };
            // Durable views hash a one-item vector; send views hash the item.
            let durable_digest = self
                .finalization
                .as_ref()
                .map(|_| {
                    serde_json::to_vec(std::slice::from_ref(original))
                        .map(|bytes| super::projection::native_content_digest(&bytes))
                        .map_err(|_| NativePlanRefusal::InvalidHistory)
                })
                .transpose()?;
            if (link.view_digest.as_ref() != Some(&digest)
                && link.view_digest.as_ref() != durable_digest.as_ref())
                || !native
            {
                return Err(NativePlanRefusal::InvalidHistory);
            }
            match outputs.entry(index) {
                std::collections::btree_map::Entry::Occupied(_) => {
                    if output.is_some()
                        || !duplicate.is_some_and(|source| {
                            ledger.accounting.iter().any(|link| {
                                &link.source == source && link.view_item == Some(index as u64)
                            })
                        })
                    {
                        return Err(NativePlanRefusal::InvalidHistory);
                    }
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(output);
                }
            }
        }
        NativePlanActiveView::check(&ledger, &self.core.decision, &outputs)?;
        let mut expected = Vec::new();
        for (index, item) in input.into_iter().enumerate() {
            if let Some(output) = outputs.get(&index) {
                expected.extend(output.iter().cloned());
            } else if matches!(item, ConversationItem::Discovery { .. }) {
                return Err(NativePlanRefusal::InvalidHistory);
            } else {
                expected.push(item);
            }
        }
        if serde_json::to_value(&expected).map_err(|_| NativePlanRefusal::InvalidHistory)?
            != serde_json::to_value(post).map_err(|_| NativePlanRefusal::InvalidHistory)?
        {
            return Err(NativePlanRefusal::InvalidHistory);
        }
        Ok(())
    }

    fn validate_declarations(&self) -> Result<(), NativePlanRefusal> {
        let declarations: Vec<Value> = self
            .carrier
            .declarations()
            .map(serde_json::from_slice)
            .transpose()
            .map_err(|_| NativePlanRefusal::InvalidDeclarations)?
            .unwrap_or_default();
        let mut positions = std::collections::BTreeSet::new();
        for identity in &self.identities {
            let position = usize::try_from(identity.declaration_index)
                .map_err(|_| NativePlanRefusal::InvalidDeclarations)?;
            let top = declarations
                .get(position)
                .ok_or(NativePlanRefusal::InvalidDeclarations)?;
            let declaration = if let Some(child) = identity.child_index {
                if top.get("type").and_then(Value::as_str) != Some("namespace")
                    || top.get("name").and_then(Value::as_str)
                        != Some(identity.qualified_namespace.as_str())
                {
                    return Err(NativePlanRefusal::InvalidDeclarations);
                }
                top.get("tools")
                    .and_then(Value::as_array)
                    .and_then(|tools| tools.get(child as usize))
                    .ok_or(NativePlanRefusal::InvalidDeclarations)?
            } else {
                top
            };
            if !positions.insert((position, identity.child_index))
                || declaration != &identity.declaration
                || identity.qualified_namespace.is_empty()
                || identity.child_name.is_empty()
                || identity.schema.kind != "eligible_schema"
                || identity.schema.byte_len != identity.schema_bytes.len() as u64
                || identity.schema.digest
                    != super::projection::native_content_digest(&identity.schema_bytes)
                || identity.provenance.kind != "eligible_snapshot"
                || identity.provenance.byte_len != identity.provenance_bytes.len() as u64
                || identity.provenance.digest
                    != super::projection::native_content_digest(&identity.provenance_bytes)
            {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
            let schema: Value = serde_json::from_slice(&identity.schema_bytes)
                .map_err(|_| NativePlanRefusal::InvalidDeclarations)?;
            let snapshot: NativeEligibleSnapshot =
                serde_json::from_slice(&identity.provenance_bytes)
                    .map_err(|_| NativePlanRefusal::InvalidDeclarations)?;
            let binding_bytes = identity
                .binding_bytes
                .as_deref()
                .ok_or(NativePlanRefusal::InvalidDeclarations)?;
            if snapshot.binding.kind != "producing_binding"
                || snapshot.binding.byte_len != binding_bytes.len() as u64
                || snapshot.binding.digest
                    != super::projection::native_content_digest(binding_bytes)
            {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
            let binding: NativePlanProducingBinding = serde_json::from_slice(binding_bytes)
                .map_err(|_| NativePlanRefusal::InvalidDeclarations)?;
            if binding.request != self.context.capture.issued_request
                || binding.generation != Some(self.context.capture.binding_generation)
                || !binding.origin.is_object()
            {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
            let name = if identity.child_index.is_none()
                && identity.namespace_kind == NativeNamespaceKind::Mcp
            {
                format!("{}__{}", identity.qualified_namespace, identity.child_name)
            } else {
                identity.child_name.clone()
            };
            if declaration
                .get("input_schema")
                .or_else(|| declaration.get("parameters"))
                != Some(&schema)
                || declaration.get("name").and_then(Value::as_str) != Some(name.as_str())
                || snapshot.version != 1
                || snapshot.schema != identity.schema
                || snapshot.namespace_kind != identity.namespace_kind
                || snapshot.qualified_namespace != identity.qualified_namespace
                || snapshot.child_name != identity.child_name
            {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
        }
        for (position, declaration) in declarations.iter().enumerate() {
            if let Some(children) = declaration.get("tools").and_then(Value::as_array) {
                for (child, _) in children.iter().enumerate() {
                    if !positions.contains(&(position, Some(child as u64))) {
                        return Err(NativePlanRefusal::InvalidDeclarations);
                    }
                }
            } else if (declaration.get("input_schema").is_some()
                || declaration.get("parameters").is_some())
                && !positions.contains(&(position, None))
            {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
        }
        for loaded in &self.core.loaded.definitions {
            if !self.identities.iter().any(|identity| {
                identity.namespace_kind == loaded.namespace_kind
                    && identity.qualified_namespace == loaded.qualified_namespace
                    && identity.child_name == loaded.child_name
                    && identity.schema.digest == loaded.schema_digest
            }) {
                return Err(NativePlanRefusal::InvalidDeclarations);
            }
        }
        Ok(())
    }

    fn validate_decisions(&self) -> Result<(), NativePlanRefusal> {
        let accounting = &self.core.accounting;
        if accounting.target.as_ref() != Some(&self.context.capture.target)
            || self.qualification != accounting.qualification
            || accounting.qualification != self.core.decision.qualification
        {
            return Err(NativePlanRefusal::InvalidQualification);
        }
        if accounting.summary.is_empty()
            || accounting.summary.len() > 512
            || accounting.reasons.len() > 8
            || self.qualification.len() != self.core.decision.groups.len()
        {
            return Err(NativePlanRefusal::InvalidAccounting);
        }
        let mut groups = NativeHistoryCounts::default();
        let mut members = NativeHistoryCounts::default();
        let mut seen = Vec::new();
        for group in &self.core.decision.groups {
            native_count_action(&mut groups, group.action);
            if group.key.is_empty()
                || group.members.is_empty()
                || seen.contains(&group.key.as_str())
            {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
            seen.push(group.key.as_str());
            let qualification: Vec<_> = self
                .qualification
                .iter()
                .filter(|q| q.group == group.key)
                .collect();
            if qualification.len() != 1 {
                return Err(NativePlanRefusal::InvalidQualification);
            }
            let qualification = qualification[0];
            if qualification.target != self.context.capture.target
                || qualification.shape.referenced_members
                    != accounting
                        .references
                        .iter()
                        .filter(|reference| {
                            let mut parent = reference.source.clone();
                            parent.submember = None;
                            reference.group.as_ref() == Some(&group.key)
                                && self.core.decision.members.iter().any(|member| {
                                    member.source == parent && member.duplicate_of.is_none()
                                })
                        })
                        .count() as u64
                || !qualification.shape.completed
            {
                return Err(NativePlanRefusal::InvalidQualification);
            }
            for source in &group.members {
                let decision: Vec<_> = self
                    .core
                    .decision
                    .members
                    .iter()
                    .filter(|member| &member.source == source)
                    .collect();
                let witnesses: Vec<_> = accounting
                    .raw_members
                    .iter()
                    .filter(|member| &member.source == source)
                    .collect();
                if decision.len() != 1
                    || witnesses.len() != 1
                    || decision[0].action != group.action
                    || witnesses[0].action != group.action
                    || witnesses[0].group.as_ref() != Some(&group.key)
                {
                    return Err(NativePlanRefusal::InvalidAccounting);
                }
            }
            if group.action == NativeHistoryAction::Keep {
                if qualification.source.as_ref() != Some(&qualification.target)
                    || !self.context.capture.original_admission.admitted()
                    || !qualification.receipt.as_ref().is_some_and(|receipt| {
                        receipt.target == qualification.target
                            && receipt.shape == qualification.shape
                            && !receipt.receipt_id.is_empty()
                    })
                {
                    return Err(NativePlanRefusal::InvalidQualification);
                }
                let supported = matches!(
                    (
                        &qualification.target.dialect,
                        qualification.shape.family,
                        self.context.capture.execution
                    ),
                    (
                        NativeReplayDialect::Messages,
                        NativeReplayFamily::MessagesServer,
                        NativeHistoryExecution::Server
                    ) | (
                        NativeReplayDialect::Responses(super::ResponsesReplayDialect::Codex),
                        NativeReplayFamily::ResponsesClient,
                        NativeHistoryExecution::Client
                    ) | (
                        NativeReplayDialect::Responses(super::ResponsesReplayDialect::Codex),
                        NativeReplayFamily::ResponsesServer,
                        NativeHistoryExecution::Server
                    )
                );
                if !supported {
                    return Err(NativePlanRefusal::InvalidQualification);
                }
            }
        }
        let mut raw_seen = Vec::new();
        for member in &accounting.raw_members {
            if raw_seen.contains(&member.source)
                || member.source.lineage != self.context.capture.lineage.id
            {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
            raw_seen.push(member.source.clone());
            native_count_action(&mut members, member.action);
        }
        if groups != accounting.groups || members != accounting.members {
            return Err(NativePlanRefusal::InvalidAccounting);
        }
        let mut references = Vec::new();
        for reference in &accounting.references {
            let mut parent = reference.source.clone();
            parent.submember = None;
            if reference.source.submember.is_none()
                || references.contains(&reference.source)
                || !accounting.raw_members.iter().any(|member| {
                    member.source == parent
                        && member.action == reference.action
                        && member.group == reference.group
                        && member.source_digest == reference.source_digest
                        && member.lost == reference.lost
                })
            {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
            references.push(reference.source.clone());
        }
        for member in &self.core.decision.members {
            if !accounting
                .raw_members
                .iter()
                .any(|row| row.source == member.source && row.action == member.action)
            {
                return Err(NativePlanRefusal::InvalidAccounting);
            }
        }
        if self
            .core
            .decision
            .groups
            .iter()
            .all(|group| group.action != NativeHistoryAction::Keep)
            && (!self.core.loaded.definitions.is_empty()
                || !self.core.loaded.names.is_empty()
                || !self.core.loaded.promotion_names.is_empty())
        {
            return Err(NativePlanRefusal::InvalidAccounting);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct NativeHistoryAuthorityIssuer {
    identity: Arc<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistorySealPurpose {
    OwningPrimary,
    SwitchStore,
    BoundBuild,
    CompoundCommit,
}

#[derive(Clone)]
pub struct NativeHistoryIssuedSeal {
    issuer: Arc<()>,
    live: Arc<std::sync::atomic::AtomicBool>,
    reported: Arc<std::sync::atomic::AtomicBool>,
    purpose: NativeHistorySealPurpose,
    actor_nonce: u64,
    lineage: String,
    run: u64,
    capture: Option<NativeHistoryCapture>,
}

impl std::fmt::Debug for NativeHistoryIssuedSeal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeHistoryIssuedSeal")
            .field("purpose", &self.purpose)
            .field("live", &self.is_live())
            .finish_non_exhaustive()
    }
}

impl PartialEq for NativeHistoryIssuedSeal {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.issuer, &other.issuer)
            && Arc::ptr_eq(&self.live, &other.live)
            && self.purpose == other.purpose
            && self.actor_nonce == other.actor_nonce
            && self.lineage == other.lineage
            && self.run == other.run
            && self.capture == other.capture
    }
}
impl Eq for NativeHistoryIssuedSeal {}

impl NativeHistoryIssuedSeal {
    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn finish(&self) {
        self.live.store(false, std::sync::atomic::Ordering::Release);
    }
    pub fn purpose(&self) -> NativeHistorySealPurpose {
        self.purpose
    }
    pub fn actor_nonce(&self) -> u64 {
        self.actor_nonce
    }
    pub fn lineage(&self) -> &str {
        &self.lineage
    }
    pub fn run(&self) -> u64 {
        self.run
    }
    pub fn capture(&self) -> Option<&NativeHistoryCapture> {
        self.capture.as_ref()
    }
}

impl NativeHistoryAuthorityIssuer {
    pub fn owns(&self, seal: &NativeHistoryIssuedSeal) -> bool {
        Arc::ptr_eq(&self.identity, &seal.issuer) && seal.is_live()
    }

    pub fn issue_owner(
        &self,
        purpose: NativeHistorySealPurpose,
        actor_nonce: u64,
        lineage: String,
        run: u64,
    ) -> Result<NativeHistoryIssuedSeal, NativeUnavailable> {
        if !matches!(
            purpose,
            NativeHistorySealPurpose::OwningPrimary | NativeHistorySealPurpose::SwitchStore
        ) || actor_nonce == 0
            || lineage.is_empty()
            || run == 0
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        Ok(NativeHistoryIssuedSeal {
            issuer: self.identity.clone(),
            live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            reported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            purpose,
            actor_nonce,
            lineage,
            run,
            capture: None,
        })
    }

    pub fn issue_bound(
        &self,
        owner: &NativeHistoryIssuedSeal,
        capture: NativeHistoryCapture,
    ) -> Result<NativeHistoryIssuedSeal, NativeUnavailable> {
        if !self.owns(owner)
            || owner.lineage != capture.lineage.id
            || !matches!(
                owner.purpose,
                NativeHistorySealPurpose::OwningPrimary | NativeHistorySealPurpose::SwitchStore
            )
        {
            return Err(NativeUnavailable::Incomplete);
        }
        Ok(NativeHistoryIssuedSeal {
            issuer: self.identity.clone(),
            live: owner.live.clone(),
            reported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            purpose: NativeHistorySealPurpose::BoundBuild,
            actor_nonce: owner.actor_nonce,
            lineage: owner.lineage.clone(),
            run: owner.run,
            capture: Some(capture),
        })
    }
}

/// Authority class named by a preparation's captured intent.
///
/// This is a *name*, not verified issuance: it records which class of write the
/// caller claimed. waj2-U9's owner/revision checks are what turn a claim into
/// durable authority; nothing here may be read as proof of either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePreparationGrant {
    SendOnly,
    OwningPrimary,
    SwitchStore,
}

/// Tentative, host-only preparation carried by a Selected bound request.
///
/// Captured independently of any final plan: a Selected request keeps its
/// requirement, the actor's per-build counters, the frozen retained-history
/// boundary and the durable→send mapping even after the plan is dropped or the
/// carrier invalidated. The constructor is public only because the owning actor
/// lives in another crate; the values grant nothing on their own —
/// `NativeHistoryReady` still comes from the checked plan, never from the
/// presence of this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBoundPreparation {
    requirement: NativeHistoryRequirement,
    grant: NativePreparationGrant,
    issued_request: String,
    history: NativeHistoryBoundary,
    mapping: NativeHistoryTransformMapping,
    history_revision: Option<u64>,
    install_generation: Option<u64>,
    build_sequence: u64,
    actor_nonce: u64,
    context: Option<NativeHistoryRequestContext>,
    seal: Option<NativeHistoryIssuedSeal>,
    durable_history: Option<Arc<[u8]>>,
    switch_projection: Option<NativeSwitchProjection>,
}

impl NativeBoundPreparation {
    /// `history_revision` and `install_generation` are `None` until their own
    /// counters exist; the caller may not substitute a look-alike value.
    #[allow(clippy::too_many_arguments)]
    pub fn issued(
        requirement: NativeHistoryRequirement,
        grant: NativePreparationGrant,
        issued_request: String,
        history: NativeHistoryBoundary,
        mapping: NativeHistoryTransformMapping,
        history_revision: Option<u64>,
        install_generation: Option<u64>,
        build_sequence: u64,
        actor_nonce: u64,
    ) -> Self {
        Self {
            requirement,
            grant,
            issued_request,
            history,
            mapping,
            history_revision,
            install_generation,
            build_sequence,
            actor_nonce,
            context: None,
            seal: None,
            durable_history: None,
            switch_projection: None,
        }
    }

    pub fn requirement(&self) -> NativeHistoryRequirement {
        self.requirement
    }
    pub fn grant(&self) -> NativePreparationGrant {
        self.grant
    }
    pub fn issued_request(&self) -> &str {
        &self.issued_request
    }
    pub fn history(&self) -> &NativeHistoryBoundary {
        &self.history
    }
    pub fn mapping(&self) -> &NativeHistoryTransformMapping {
        &self.mapping
    }
    /// The durable-history revision counter is not implemented on this branch:
    /// `ConversationItem` length and the native `source_revision`
    /// (`NativeLineage`) are different quantities and neither may be recorded
    /// here, so the preparation carries `None` with this reason rather than a
    /// look-alike value. A later unit owns the real counter.
    pub fn history_revision(&self) -> Option<u64> {
        self.history_revision
    }
    /// No production site advances an install generation this actor can read
    /// (the transport binding carries config and credentials only), so the
    /// capture is `None` with this reason instead of an aliased counter.
    pub fn install_generation(&self) -> Option<u64> {
        self.install_generation
    }
    pub fn build_sequence(&self) -> u64 {
        self.build_sequence
    }
    pub fn actor_nonce(&self) -> u64 {
        self.actor_nonce
    }
}

/// The builder seam a durable occurrence passed through on its way to the send
/// view of one bound build.
///
/// Every arm names one actual mutation site in
/// `xai-chat-state/src/actor/request_builder.rs`; an occurrence the builder did
/// not touch carries [`Self::Kept`] and nothing else. None of these arms removes
/// an item: pruning rewrites `ToolResult.content` in place and the image budget
/// evicts image parts in place, so a `Pruned*` or `ImageBudgetCompacted` arm is
/// a content transform at the same durable coordinate, never a durable strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistoryTransform {
    Kept,
    ImageBudgetCompacted,
    PrunedHardClear,
    PrunedSoftTrim,
    RequestOnlyReminder,
    RequestOnlyReminderUpsert,
    AgentMessageLabelRepaired,
}

impl NativeHistoryTransform {
    /// The two arms that name a request-only memory reminder as their input.
    pub fn names_reminder(self) -> bool {
        matches!(
            self,
            Self::RequestOnlyReminder | Self::RequestOnlyReminderUpsert
        )
    }
}

/// One occurrence of [`NativeHistoryTransformMapping`].
///
/// `before_digest` is present exactly for an occurrence that existed in the
/// frozen durable history; a request-only insertion carries no durable
/// coordinate. Any entry whose transforms name a reminder records the actual
/// reminder text it was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryMappingEntry {
    durable_index: Option<u32>,
    send_index: Option<u32>,
    transforms: Vec<NativeHistoryTransform>,
    before_digest: Option<String>,
    after_digest: String,
    reminder_input: Option<String>,
}

impl NativeHistoryMappingEntry {
    /// An occurrence that was present in `D_pre`.
    pub fn durable(
        durable_index: u32,
        before_digest: String,
        after_digest: String,
        transforms: Vec<NativeHistoryTransform>,
        reminder_input: Option<String>,
    ) -> Self {
        Self {
            durable_index: Some(durable_index),
            send_index: None,
            transforms,
            before_digest: Some(before_digest),
            after_digest,
            reminder_input,
        }
    }

    /// An occurrence the builder created for this request only.
    pub fn request_only_insertion(
        after_digest: String,
        transforms: Vec<NativeHistoryTransform>,
        reminder_input: Option<String>,
    ) -> Self {
        Self {
            durable_index: None,
            send_index: None,
            transforms,
            before_digest: None,
            after_digest,
            reminder_input,
        }
    }

    pub fn durable_index(&self) -> Option<u32> {
        self.durable_index
    }
    pub fn send_index(&self) -> Option<u32> {
        self.send_index
    }
    pub fn transforms(&self) -> &[NativeHistoryTransform] {
        &self.transforms
    }
    pub fn before_digest(&self) -> Option<&str> {
        self.before_digest.as_deref()
    }
    pub fn after_digest(&self) -> &str {
        &self.after_digest
    }
    pub fn reminder_input(&self) -> Option<&str> {
        self.reminder_input.as_deref()
    }

    fn with_send_index(mut self, send_index: u32) -> Self {
        self.send_index = Some(send_index);
        self
    }
}

/// Host-only, occurrence-indexed relation between the frozen durable history
/// (`D_pre`) and the actual send history (`S_pre`) of one bound build
/// (design r4 §4.1).
///
/// It is complete by construction: every durable occurrence and every send
/// occurrence appears exactly once, each with its before/after item digest and
/// the single seam that changed it. `Checked` is the only way to obtain one and
/// it refuses an incomplete or ambiguous relation. The entry count is bounded by
/// the request's own item count plus at most one request-only insertion per
/// seam, so it adds no new retention surface — nothing here is serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryTransformMapping {
    durable: NativeHistoryBoundary,
    send: NativeHistoryBoundary,
    entries: Vec<NativeHistoryMappingEntry>,
}

impl NativeHistoryTransformMapping {
    /// Verifies the relation covers both boundaries completely and that every
    /// entry is self-consistent, then binds the send coordinates in order.
    ///
    /// Refusals: `InvalidReference` for a malformed entry (a digest without a
    /// durable coordinate, a duplicate durable coordinate, an insertion credited
    /// to a seam other than the request-only reminder, a `Kept` entry whose
    /// digests differ or a changed entry claimed as `Kept`, a reminder arm
    /// without its reminder text); `Incomplete` for an uncovered durable
    /// coordinate, an empty transform list, or a send count that differs from
    /// `S_pre`'s item count.
    pub fn checked(
        durable: NativeHistoryBoundary,
        send: NativeHistoryBoundary,
        entries: Vec<NativeHistoryMappingEntry>,
    ) -> Result<Self, NativeUnavailable> {
        let durable_len =
            usize::try_from(durable.items).map_err(|_| NativeUnavailable::Incomplete)?;
        let send_len = usize::try_from(send.items).map_err(|_| NativeUnavailable::Incomplete)?;
        let mut seen_durable = vec![false; durable_len];
        for entry in &entries {
            let has_before = entry.before_digest.is_some();
            if has_before != entry.durable_index.is_some() {
                return Err(NativeUnavailable::InvalidReference);
            }
            if entry.transforms.is_empty() {
                return Err(NativeUnavailable::Incomplete);
            }
            let names_reminder = entry.transforms.iter().any(|t| t.names_reminder());
            if names_reminder != entry.reminder_input.is_some()
                || entry.reminder_input.as_deref().is_some_and(str::is_empty)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            if entry.transforms.contains(&NativeHistoryTransform::Kept)
                && entry.transforms.len() > 1
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            if entry.durable_index.is_none()
                && entry.transforms.as_slice() != [NativeHistoryTransform::RequestOnlyReminder]
            {
                // Only the insertion seam may create a send occurrence with no
                // durable coordinate.
                return Err(NativeUnavailable::InvalidReference);
            }
            if !is_hex64(&entry.after_digest) {
                return Err(NativeUnavailable::InvalidReference);
            }
            let is_kept = entry.transforms.as_slice() == [NativeHistoryTransform::Kept];
            if let Some(before) = entry.before_digest.as_deref() {
                if !is_hex64(before) {
                    return Err(NativeUnavailable::InvalidReference);
                }
                if is_kept != (before == entry.after_digest.as_str()) {
                    // `Kept` means the builder copied the bytes unchanged; any
                    // other relation must show a digest change it observed.
                    return Err(NativeUnavailable::Fidelity);
                }
            } else if is_kept {
                return Err(NativeUnavailable::InvalidReference);
            }
            if let Some(index) = entry.durable_index {
                // The builder never drops a durable occurrence on the way to the
                // send view, so every durable coordinate must be carried.
                let slot = usize::try_from(index).map_err(|_| NativeUnavailable::Incomplete)?;
                let seen = seen_durable
                    .get_mut(slot)
                    .ok_or(NativeUnavailable::Incomplete)?;
                if *seen {
                    return Err(NativeUnavailable::InvalidReference);
                }
                *seen = true;
            }
        }
        if seen_durable.iter().any(|seen| !seen) {
            return Err(NativeUnavailable::Incomplete);
        }
        // The caller lists entries in send order; the send coordinates are
        // bound here so a relation cannot carry an index the builder never
        // emitted, and `S_pre`'s item count is what the list must match.
        if entries.len() != send_len {
            return Err(NativeUnavailable::Incomplete);
        }
        let mut bound = Vec::with_capacity(entries.len());
        for (position, entry) in entries.into_iter().enumerate() {
            bound.push(entry.with_send_index(position as u32));
        }
        Ok(Self {
            durable,
            send,
            entries: bound,
        })
    }

    pub fn durable(&self) -> &NativeHistoryBoundary {
        &self.durable
    }
    pub fn send(&self) -> &NativeHistoryBoundary {
        &self.send
    }
    pub fn entries(&self) -> &[NativeHistoryMappingEntry] {
        &self.entries
    }
    pub fn send_for_durable(&self, durable_index: u32) -> Option<u32> {
        self.entries
            .iter()
            .find(|entry| entry.durable_index == Some(durable_index))
            .and_then(|entry| entry.send_index)
    }
    pub fn durable_for_send(&self, send_index: u32) -> Option<u32> {
        self.entries
            .iter()
            .find(|entry| entry.send_index == Some(send_index))
            .and_then(|entry| entry.durable_index)
    }
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn validate_native_history_plan(
    context: &NativeHistoryRequestContext,
    plan: Option<&NativeHistoryPlan>,
    carrier: &NativeFinalCarrier,
) -> Result<NativeHistoryReady, NativePlanRefusal> {
    match (context.requirement, plan) {
        (NativeHistoryRequirement::Inactive, None) => Ok(NativeHistoryReady::Inactive),
        (NativeHistoryRequirement::Inactive, Some(_)) => Err(NativePlanRefusal::InactivePlan),
        (NativeHistoryRequirement::Selected, None) => Err(NativePlanRefusal::MissingPlan),
        (NativeHistoryRequirement::Selected, Some(plan)) => {
            if plan.context != *context {
                return Err(NativePlanRefusal::StaleContext);
            }
            plan.validate_binding()?;
            if plan.carrier != *carrier {
                return Err(NativePlanRefusal::FinalCarrierChanged);
            }
            match (&context.authority, &plan.readiness) {
                (NativeHistoryWriteAuthority::SendOnly, NativePlanReadiness::SendOnly) => {
                    Ok(NativeHistoryReady::SendOnly)
                }
                (
                    NativeHistoryWriteAuthority::OwningPrimary { .. },
                    NativePlanReadiness::OwningPrimary {
                        preparation,
                        certificate,
                    },
                ) if certificate.validates(preparation) => Ok(NativeHistoryReady::OwningPrimary),
                (_, NativePlanReadiness::OwningPrimary { .. }) => Err(NativePlanRefusal::NotReady),
                (_, NativePlanReadiness::Tentative) => Err(NativePlanRefusal::NotReady),
                _ => Err(NativePlanRefusal::AuthorityUnverified),
            }
        }
    }
}

impl NativeHistoryPlan {
    pub fn checked_bound(
        preparation: NativeBoundPreparation,
        certificate: NativeHistoryCommitCertificate,
        input: NativeHistoryPlanInput<'_>,
    ) -> Result<Self, NativePlanRefusal> {
        if preparation.grant() != NativePreparationGrant::OwningPrimary
            || !certificate.validates(&preparation)
            || certificate.acknowledgement().before() != preparation.history()
            || super::projection::native_content_digest(&input.input_history_bytes)
                != preparation.mapping().send().digest
            || input.core.accounting.history != *preparation.mapping().send()
        {
            return Err(NativePlanRefusal::AuthorityUnverified);
        }
        let mut context = preparation
            .context()
            .cloned()
            .ok_or(NativePlanRefusal::AuthorityUnverified)?;
        if !matches!(
            context.authority(),
            NativeHistoryWriteAuthority::OwningPrimary { .. }
        ) {
            return Err(NativePlanRefusal::AuthorityUnverified);
        }
        context.capture.history = preparation.mapping().send().clone();
        let mut plan = Self::checked(context, input)?;
        plan.readiness = NativePlanReadiness::OwningPrimary {
            preparation: Box::new(preparation),
            certificate: Arc::new(certificate),
        };
        plan.validate_binding()?;
        Ok(plan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSwitchProjection {
    target_model: String,
    target_pin: Option<String>,
    backend: crate::ApiBackend,
    boundary: super::projection::Boundary,
    admission: super::responses::SearchAdmission,
}
impl NativeSwitchProjection {
    pub fn captured(
        target_model: String,
        target_pin: Option<String>,
        route: &super::projection::TargetRoute,
    ) -> Self {
        Self {
            target_model,
            target_pin,
            backend: route.backend(),
            boundary: route.boundary(),
            admission: route.admission(),
        }
    }
    pub fn project(
        &self,
        items: &[super::ConversationItem],
    ) -> super::projection::ProjectedHistory {
        let route = super::projection::TargetRoute::new(
            self.backend.clone(),
            self.boundary,
            self.admission,
        );
        super::projection::project_switch_history(
            items,
            &self.target_model,
            self.boundary,
            self.target_pin.as_deref(),
            &route,
        )
    }
}
impl NativeBoundPreparation {
    pub fn with_switch_projection(
        mut self,
        projection: NativeSwitchProjection,
    ) -> Result<Self, NativeUnavailable> {
        if self.grant != NativePreparationGrant::SwitchStore || self.switch_projection.is_some() {
            return Err(NativeUnavailable::InvalidReference);
        }
        self.switch_projection = Some(projection);
        Ok(self)
    }
    pub fn switch_projection(&self) -> Option<&NativeSwitchProjection> {
        self.switch_projection.as_ref()
    }
}

#[derive(Clone)]
pub struct NativeHistoryRegistrationKey {
    issuer: Arc<()>,
}
impl std::fmt::Debug for NativeHistoryRegistrationKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeHistoryRegistrationKey { .. }")
    }
}
impl NativeHistoryAuthorityIssuer {
    pub fn registration_key(&self) -> NativeHistoryRegistrationKey {
        NativeHistoryRegistrationKey {
            issuer: self.identity.clone(),
        }
    }
    pub fn accepts_registration_key(&self, key: &NativeHistoryRegistrationKey) -> bool {
        Arc::ptr_eq(&self.identity, &key.issuer)
    }
}

fn native_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryBefore {
    pub boundary: NativeHistoryBoundary,
    pub byte_len: u64,
    pub raw_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryAfterImage {
    pub format_version: u32,
    pub blob: NativeBlobRef,
    pub boundary: NativeHistoryBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompoundOperation {
    Projection,
    NativeCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCompoundCommit {
    pub record_kind: String,
    pub schema_version: u32,
    pub policy_revision: u32,
    pub reader_floor: u32,
    pub limits_revision: u32,
    pub lineage: String,
    pub high_water: u64,
    pub checkpoint: NativeBlobRef,
    pub transaction: String,
    pub operation: NativeCompoundOperation,
    pub previous_generation: u64,
    #[serde(deserialize_with = "native_required_option")]
    pub previous_checkpoint: Option<NativeBlobRef>,
    #[serde(deserialize_with = "native_required_option")]
    pub previous_authority_digest: Option<String>,
    pub before: NativeHistoryBefore,
    #[serde(deserialize_with = "native_required_option")]
    pub after_image: Option<NativeHistoryAfterImage>,
    #[serde(deserialize_with = "native_required_option")]
    pub projection: Option<NativeBlobRef>,
}

impl NativeCompoundCommit {
    pub fn validate(&self) -> Result<(), NativeUnavailable> {
        let reference = |reference: &NativeBlobRef, kind: &str, limit: u64| {
            reference.kind == kind && reference.byte_len <= limit && is_hex64(&reference.digest)
        };
        if self.record_kind != "native_compound_commit"
            || (
                self.schema_version,
                self.policy_revision,
                self.reader_floor,
                self.limits_revision,
            ) != (3, 1, 3, 2)
            || self.lineage.is_empty()
            || self.transaction.is_empty()
            || self.previous_generation.checked_add(1) != Some(self.high_water)
            || !reference(&self.checkpoint, "checkpoint", NATIVE_CONTROL_BYTES)
            || !is_hex64(&self.before.boundary.digest)
            || !is_hex64(&self.before.raw_digest)
            || (self.previous_generation == 0) != self.previous_checkpoint.is_none()
            || (self.previous_generation == 0) != self.previous_authority_digest.is_none()
            || self
                .previous_checkpoint
                .as_ref()
                .is_some_and(|previous| !reference(previous, "checkpoint", NATIVE_CONTROL_BYTES))
            || self
                .previous_authority_digest
                .as_deref()
                .is_some_and(|digest| !is_hex64(digest))
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        match (self.operation, &self.after_image, &self.projection) {
            (NativeCompoundOperation::Projection, Some(image), Some(metadata))
                if image.format_version == 1
                    && reference(&image.blob, "ordinary_history_jsonl", NATIVE_ROLE_BYTES)
                    && is_hex64(&image.boundary.digest)
                    && reference(metadata, "projection_metadata", NATIVE_LEDGER_BYTES)
                    && self.projection_transaction()? == self.transaction =>
            {
                Ok(())
            }
            (NativeCompoundOperation::NativeCheckpoint, None, None) => Ok(()),
            _ => Err(NativeUnavailable::InvalidReference),
        }
    }

    pub fn projection_transaction(&self) -> Result<String, NativeUnavailable> {
        #[derive(Serialize)]
        struct Key<'a> {
            lineage: &'a str,
            previous_generation: u64,
            previous_checkpoint_digest: Option<&'a str>,
            previous_authority_digest: Option<&'a str>,
            before: &'a NativeHistoryBefore,
            after_image: &'a NativeHistoryAfterImage,
            checkpoint_digest: &'a str,
            projection_digest: &'a str,
        }
        let bytes = serde_json::to_vec(&Key {
            lineage: &self.lineage,
            previous_generation: self.previous_generation,
            previous_checkpoint_digest: self
                .previous_checkpoint
                .as_ref()
                .map(|reference| reference.digest.as_str()),
            previous_authority_digest: self.previous_authority_digest.as_deref(),
            before: &self.before,
            after_image: self
                .after_image
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?,
            checkpoint_digest: &self.checkpoint.digest,
            projection_digest: &self
                .projection
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?
                .digest,
        })
        .map_err(|_| NativeUnavailable::Fidelity)?;
        Ok(format!(
            "s3p:{}",
            super::projection::native_content_digest(&bytes)
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeMaterialized {
    pub format_version: u32,
    pub authority_digest: String,
    pub history_digest: String,
    pub history_byte_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDomainBackend {
    Responses,
    Messages,
    ChatCompletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResponseDialect {
    Codex,
    Xai,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeDomainDialect {
    Messages,
    Responses(NativeResponseDialect),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionDomain {
    pub backend: NativeDomainBackend,
    pub dialect: NativeDomainDialect,
    pub model: String,
    pub row: String,
    pub provider: String,
    pub deployment: String,
    pub deployment_revision: String,
    pub endpoint: String,
    pub principal: String,
}

impl From<&NativeReplayDomain> for NativeProjectionDomain {
    fn from(domain: &NativeReplayDomain) -> Self {
        Self {
            backend: match domain.backend {
                crate::ApiBackend::Responses => NativeDomainBackend::Responses,
                crate::ApiBackend::Messages => NativeDomainBackend::Messages,
                crate::ApiBackend::ChatCompletions => NativeDomainBackend::ChatCompletions,
            },
            dialect: match &domain.dialect {
                NativeReplayDialect::Messages => NativeDomainDialect::Messages,
                NativeReplayDialect::Responses(dialect) => {
                    NativeDomainDialect::Responses(match dialect {
                        super::ResponsesReplayDialect::Codex => NativeResponseDialect::Codex,
                        super::ResponsesReplayDialect::Xai => NativeResponseDialect::Xai,
                        super::ResponsesReplayDialect::Other => NativeResponseDialect::Other,
                    })
                }
            },
            model: domain.model.clone(),
            row: domain.row.clone(),
            provider: domain.provider.clone(),
            deployment: domain.deployment.clone(),
            deployment_revision: domain.deployment_revision.clone(),
            endpoint: domain.endpoint.clone(),
            principal: domain.principal.clone(),
        }
    }
}

impl NativeProjectionDomain {
    pub fn domain(&self) -> NativeReplayDomain {
        NativeReplayDomain {
            backend: match self.backend {
                NativeDomainBackend::Responses => crate::ApiBackend::Responses,
                NativeDomainBackend::Messages => crate::ApiBackend::Messages,
                NativeDomainBackend::ChatCompletions => crate::ApiBackend::ChatCompletions,
            },
            dialect: match self.dialect {
                NativeDomainDialect::Messages => NativeReplayDialect::Messages,
                NativeDomainDialect::Responses(dialect) => {
                    NativeReplayDialect::Responses(match dialect {
                        NativeResponseDialect::Codex => super::ResponsesReplayDialect::Codex,
                        NativeResponseDialect::Xai => super::ResponsesReplayDialect::Xai,
                        NativeResponseDialect::Other => super::ResponsesReplayDialect::Other,
                    })
                }
            },
            model: self.model.clone(),
            row: self.row.clone(),
            provider: self.provider.clone(),
            deployment: self.deployment.clone(),
            deployment_revision: self.deployment_revision.clone(),
            endpoint: self.endpoint.clone(),
            principal: self.principal.clone(),
        }
    }
    pub fn validate(&self) -> Result<(), NativeUnavailable> {
        if !self.domain().complete() || self.endpoint.contains(['?', '#', '@']) {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeProjectionOwner {
    SwitchStore,
    OwningPrimary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionVersions {
    pub schema: u32,
    pub policy: u32,
    pub projection: u32,
    pub reader: u32,
    pub limits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionAdmission {
    pub supports_search_tool: bool,
    pub has_searchable_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionShape {
    pub family: String,
    pub keyless_server: bool,
    pub output_id_present: bool,
    pub completed: bool,
    pub result_error: bool,
    pub referenced_members: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionReceipt {
    pub target: NativeProjectionDomain,
    pub shape: NativeProjectionShape,
    pub receipt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionQualification {
    pub group: u32,
    #[serde(deserialize_with = "native_required_option")]
    pub source: Option<NativeProjectionDomain>,
    pub target: NativeProjectionDomain,
    pub shape: NativeProjectionShape,
    #[serde(deserialize_with = "native_required_option")]
    pub receipt: Option<NativeProjectionReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionGroup {
    pub key: u32,
    pub from: String,
    pub to: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionMember {
    pub source: u32,
    pub source_digest: String,
    #[serde(deserialize_with = "native_required_option")]
    pub group: Option<u32>,
    pub action: String,
    pub output_items: Vec<u64>,
    pub output_digests: Vec<String>,
    pub loss: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionFGroup {
    pub key: u32,
    pub call: u32,
    pub call_digest: String,
    pub result: u32,
    pub result_digest: String,
    pub disposition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionFProvenance {
    pub version: u32,
    pub groups: Vec<NativeProjectionFGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProjectionMetadata {
    pub record_kind: String,
    pub format_version: u32,
    pub owner_class: NativeProjectionOwner,
    pub issued_request: String,
    pub binding_generation: u64,
    pub original_admission: NativeProjectionAdmission,
    pub target: NativeProjectionDomain,
    pub versions: NativeProjectionVersions,
    pub qualification: Vec<NativeProjectionQualification>,
    pub input_history: NativeHistoryBoundary,
    pub post_history: NativeHistoryBoundary,
    pub groups: Vec<NativeProjectionGroup>,
    pub members: Vec<NativeProjectionMember>,
    pub f_provenance: NativeProjectionFProvenance,
    pub loaded_invalidations: Vec<u32>,
}

impl NativeProjectionShape {
    pub fn from_shape(shape: &NativeReplayShape) -> Self {
        Self {
            family: match shape.family {
                NativeReplayFamily::ResponsesClient => "responses_client",
                NativeReplayFamily::ResponsesServer => "responses_server",
                NativeReplayFamily::MessagesServer => "messages_server",
            }
            .into(),
            keyless_server: shape.keyless_server,
            output_id_present: shape.output_id_present,
            completed: shape.completed,
            result_error: shape.result_error,
            referenced_members: shape.referenced_members,
        }
    }
    pub fn shape(&self) -> Result<NativeReplayShape, NativeUnavailable> {
        Ok(NativeReplayShape {
            family: match self.family.as_str() {
                "responses_client" => NativeReplayFamily::ResponsesClient,
                "responses_server" => NativeReplayFamily::ResponsesServer,
                "messages_server" => NativeReplayFamily::MessagesServer,
                _ => return Err(NativeUnavailable::InvalidReference),
            },
            keyless_server: self.keyless_server,
            output_id_present: self.output_id_present,
            completed: self.completed,
            result_error: self.result_error,
            referenced_members: self.referenced_members,
        })
    }
}

impl NativeProjectionQualification {
    pub fn qualification(&self, key: &str) -> Result<NativeGroupQualification, NativeUnavailable> {
        self.target.validate()?;
        if let Some(source) = &self.source {
            source.validate()?;
        }
        Ok(NativeGroupQualification {
            group: key.into(),
            source: self.source.as_ref().map(NativeProjectionDomain::domain),
            target: self.target.domain(),
            shape: self.shape.shape()?,
            receipt: self
                .receipt
                .as_ref()
                .map(|receipt| {
                    Ok(NativeShapeReceipt {
                        target: receipt.target.domain(),
                        shape: receipt.shape.shape()?,
                        receipt_id: receipt.receipt_id.clone(),
                    })
                })
                .transpose()?,
        })
    }
}

impl NativeProjectionMetadata {
    pub fn validate_namespace(
        &self,
        record: &NativeSearchHistoryV1,
        ledger: &NativeLedger,
        identity: &NativeIdentityExtension,
    ) -> Result<(), NativeUnavailable> {
        if self.record_kind != "native_projection_metadata"
            || self.format_version != 1
            || self.issued_request.is_empty()
            || self.binding_generation == 0
            || (
                self.versions.schema,
                self.versions.policy,
                self.versions.projection,
                self.versions.reader,
                self.versions.limits,
            ) != (
                record.schema_version,
                record.policy_revision,
                1,
                record.reader_floor,
                record.limits_revision,
            )
            || self.post_history != record.history_boundary
            || !is_hex64(&self.input_history.digest)
            || !is_hex64(&self.post_history.digest)
            || self.f_provenance.version != 1
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        self.target.validate()?;
        let mut namespace: Vec<_> = ledger
            .accounting
            .iter()
            .map(|member| (&member.source, member.source_digest.as_str()))
            .collect();
        if let NativeIdentityExtension::Revision2 {
            eligible_member_accounting,
            ..
        } = identity
        {
            namespace.extend(
                eligible_member_accounting
                    .iter()
                    .map(|member| (&member.source, member.source_digest.as_str())),
            );
        }
        NativeResource::LedgerMembers.admit(namespace.len() as u64, 0)?;
        NativeResource::LedgerGroups.admit(ledger.groups.len() as u64, 0)?;
        if self.members.len() != namespace.len() {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut seen = std::collections::BTreeSet::new();
        for member in &self.members {
            let (source, source_digest) = namespace
                .get(member.source as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let mut parent = (*source).clone();
            parent.submember = None;
            let actual_group = ledger
                .groups
                .iter()
                .position(|group| group.members.contains(&parent));
            if !seen.insert(member.source)
                || member.source_digest != *source_digest
                || member.group.map(|group| group as usize) != actual_group
                || member.output_items.len() != member.output_digests.len()
                || member
                    .output_items
                    .iter()
                    .any(|index| *index >= self.post_history.items)
                || member.output_digests.iter().any(|digest| !is_hex64(digest))
                || !matches!(
                    member.action.as_str(),
                    "Keep"
                        | "F"
                        | "Strip"
                        | "OmitSendOnly"
                        | "AwaitClient"
                        | "AwaitResults"
                        | "ContinueOriginal"
                )
                || member.action == "Strip" && (!member.output_items.is_empty() || !member.loss)
                || matches!(
                    member.action.as_str(),
                    "AwaitClient" | "AwaitResults" | "ContinueOriginal"
                ) && member.loss
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        let mut groups = std::collections::BTreeSet::new();
        for transition in &self.groups {
            let group = ledger
                .groups
                .get(transition.key as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            if !groups.insert(transition.key)
                || group.state != "Completed"
                || !matches!(
                    transition.from.as_str(),
                    "KEEP" | "F" | "strip" | "Unrecorded"
                )
                || !matches!(transition.to.as_str(), "KEEP" | "F" | "strip")
                || !matches!(
                    transition.reason.as_str(),
                    "kept"
                        | "unqualified"
                        | "missing_view"
                        | "prior_disposition"
                        | "f_budget"
                        | "pending"
                        | "abandoned"
                )
                || transition.from == "strip" && transition.to != "strip"
                || transition.from == "F" && transition.to == "KEEP"
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        if groups.len()
            != ledger
                .groups
                .iter()
                .filter(|group| group.state == "Completed")
                .count()
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut qualifications = std::collections::BTreeSet::new();
        for qualification in &self.qualification {
            let group = ledger
                .groups
                .get(qualification.group as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            if !qualifications.insert(qualification.group) || qualification.target != self.target {
                return Err(NativeUnavailable::InvalidReference);
            }
            let checked = qualification.qualification(&group.key)?;
            if let Some(receipt) = &checked.receipt {
                if receipt.target != checked.target
                    || receipt.shape != checked.shape
                    || receipt.receipt_id.is_empty()
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
        }
        if qualifications.len() != ledger.groups.len() {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut provenance = std::collections::BTreeSet::new();
        for group in &self.f_provenance.groups {
            let current = ledger
                .groups
                .get(group.key as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let call = ledger
                .accounting
                .get(group.call as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let result = ledger
                .accounting
                .get(group.result as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            if !provenance.insert(group.key)
                || !current.members.contains(&call.source)
                || !current.members.contains(&result.source)
                || group.call == group.result
                || !is_hex64(&group.call_digest)
                || !is_hex64(&group.result_digest)
                || !matches!(group.disposition.as_str(), "F" | "Strip")
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        if self
            .groups
            .iter()
            .filter(|group| group.to != "KEEP")
            .any(|group| !provenance.contains(&group.key))
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut invalidations = std::collections::BTreeSet::new();
        for group in &self.loaded_invalidations {
            if !invalidations.insert(*group) || ledger.groups.get(*group as usize).is_none() {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCompoundAck {
    transaction: String,
    generation: u64,
    checkpoint: NativeBlobRef,
    authority_digest: String,
    before: NativeHistoryBoundary,
    after: NativeHistoryBoundary,
}

impl NativeCompoundAck {
    pub fn from_commit(
        commit: &NativeCompoundCommit,
        bytes: &[u8],
    ) -> Result<Self, NativeUnavailable> {
        commit.validate()?;
        if serde_json::from_slice::<NativeCompoundCommit>(bytes)
            .map_err(|_| NativeUnavailable::Fidelity)?
            != *commit
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let after = commit.after_image.as_ref().map_or_else(
            || commit.before.boundary.clone(),
            |image| image.boundary.clone(),
        );
        Ok(Self {
            transaction: commit.transaction.clone(),
            generation: commit.high_water,
            checkpoint: commit.checkpoint.clone(),
            authority_digest: super::projection::native_content_digest(bytes),
            before: commit.before.boundary.clone(),
            after,
        })
    }
    pub fn transaction(&self) -> &str {
        &self.transaction
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn checkpoint(&self) -> &NativeBlobRef {
        &self.checkpoint
    }
    pub fn authority_digest(&self) -> &str {
        &self.authority_digest
    }
    pub fn before(&self) -> &NativeHistoryBoundary {
        &self.before
    }
    pub fn after(&self) -> &NativeHistoryBoundary {
        &self.after
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryCommitCertificate {
    seal: NativeHistoryIssuedSeal,
    ack: NativeCompoundAck,
}

impl NativeHistoryAuthorityIssuer {
    pub fn confirm_compound(
        &self,
        preparation: &NativeBoundPreparation,
        ack: NativeCompoundAck,
        expected_post: &NativeHistoryBoundary,
    ) -> Result<NativeHistoryCommitCertificate, NativeUnavailable> {
        let seal = preparation.seal().ok_or(NativeUnavailable::Incomplete)?;
        let context = preparation.context().ok_or(NativeUnavailable::Incomplete)?;
        if !self.owns(seal)
            || seal.purpose != NativeHistorySealPurpose::BoundBuild
            || seal.capture.as_ref() != Some(context.capture())
            || preparation.grant == NativePreparationGrant::SendOnly
            || ack.before() != preparation.history()
            || ack.after() != expected_post
            || context.capture().native_generation.checked_add(1) != Some(ack.generation())
        {
            return Err(NativeUnavailable::StaleHistory);
        }
        let mut seal = seal.clone();
        seal.purpose = NativeHistorySealPurpose::CompoundCommit;
        Ok(NativeHistoryCommitCertificate { seal, ack })
    }
}

impl NativeHistoryCommitCertificate {
    pub fn mark_reported_once(&self) -> bool {
        self.is_live()
            && self
                .seal
                .reported
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
    }
    pub fn acknowledgement(&self) -> &NativeCompoundAck {
        &self.ack
    }
    pub fn is_live(&self) -> bool {
        self.seal.is_live()
    }
    pub fn validates(&self, preparation: &NativeBoundPreparation) -> bool {
        preparation.seal().is_some_and(|bound| {
            Arc::ptr_eq(&self.seal.issuer, &bound.issuer)
                && Arc::ptr_eq(&self.seal.live, &bound.live)
                && self.seal.capture == bound.capture
                && self.seal.run == bound.run
                && self.seal.actor_nonce == bound.actor_nonce
                && self.seal.purpose == NativeHistorySealPurpose::CompoundCommit
                && self.seal.is_live()
                && self.ack.before() == preparation.history()
        })
    }
}

impl NativeBoundPreparation {
    pub fn with_issued_context(
        mut self,
        context: NativeHistoryRequestContext,
        seal: NativeHistoryIssuedSeal,
        durable_history: Arc<[u8]>,
    ) -> Result<Self, NativeUnavailable> {
        let history: Vec<super::ConversationItem> =
            serde_json::from_slice(&durable_history).map_err(|_| NativeUnavailable::Fidelity)?;
        if context.requirement() != self.requirement
            || context.capture().history != self.history
            || self.mapping.durable() != &self.history
            || seal.capture() != Some(context.capture())
            || seal.purpose() != NativeHistorySealPurpose::BoundBuild
            || seal.actor_nonce() != self.actor_nonce
            || !seal.is_live()
            || super::projection::native_content_digest(&durable_history) != self.history.digest
            || history.len() as u64 != self.history.items
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        self.context = Some(context);
        self.seal = Some(seal);
        self.durable_history = Some(durable_history);
        Ok(self)
    }
    pub fn context(&self) -> Option<&NativeHistoryRequestContext> {
        self.context.as_ref()
    }
    pub fn seal(&self) -> Option<&NativeHistoryIssuedSeal> {
        self.seal.as_ref()
    }
    pub fn durable_history_bytes(&self) -> Option<&[u8]> {
        self.durable_history.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_history_owner_seals_cannot_cross_issuers_or_survive_release() {
        let issuer = NativeHistoryAuthorityIssuer::default();
        let foreign = NativeHistoryAuthorityIssuer::default();
        let owner = issuer
            .issue_owner(
                NativeHistorySealPurpose::OwningPrimary,
                1,
                "owner-lineage".into(),
                1,
            )
            .unwrap();
        assert!(issuer.owns(&owner));
        assert!(!foreign.owns(&owner));
        let borrowed = owner.clone();
        owner.finish();
        assert!(!issuer.owns(&borrowed));
        assert!(!borrowed.is_live());
    }

    #[test]
    fn native_history_invalidation_retains_captured_preparation() {
        let boundary = NativeHistoryBoundary {
            items: 0,
            digest: "a".repeat(64),
        };
        let mapping =
            NativeHistoryTransformMapping::checked(boundary.clone(), boundary.clone(), Vec::new())
                .unwrap();
        let preparation = NativeBoundPreparation::issued(
            NativeHistoryRequirement::Selected,
            NativePreparationGrant::SendOnly,
            "invalidation-owner".into(),
            boundary,
            mapping,
            None,
            None,
            1,
            1,
        );
        let mut request = crate::ConversationRequest {
            native_bound_preparation: Some(preparation.clone()),
            ..Default::default()
        };
        request.invalidate_encoded();
        assert_eq!(request.native_bound_preparation, Some(preparation));
    }

    #[test]
    fn native_history_mapping_kept_occurrence_needs_no_reminder() {
        let boundary = NativeHistoryBoundary {
            items: 1,
            digest: "a".repeat(64),
        };
        let mapping = NativeHistoryTransformMapping::checked(
            boundary.clone(),
            boundary,
            vec![NativeHistoryMappingEntry::durable(
                0,
                "b".repeat(64),
                "b".repeat(64),
                vec![NativeHistoryTransform::Kept],
                None,
            )],
        )
        .expect("unchanged durable occurrence requires no reminder input");
        assert_eq!(mapping.send_for_durable(0), Some(0));
        assert_eq!(mapping.durable_for_send(0), Some(0));
    }

    use crate::ConversationItem;
    use crate::conversation::projection::{Boundary, TargetRoute};
    use serde_json::json;

    enum PassiveMembers {
        Pair,
        DuplicateResult,
        ResponsesPair,
        MessagesKeep,
        ResponsesKeep,
    }

    fn passive_plan_fixture() -> (NativeHistoryRequestContext, NativeHistoryPlan) {
        passive_plan_fixture_for(PassiveMembers::Pair).unwrap()
    }

    fn passive_plan_fixture_for(
        members: PassiveMembers,
    ) -> Result<(NativeHistoryRequestContext, NativeHistoryPlan), NativePlanRefusal> {
        let (context, plan) = passive_plan_candidate_for(members)?;
        Ok((context, passive_recheck(&plan)?))
    }

    fn passive_plan_candidate_for(
        members: PassiveMembers,
    ) -> Result<(NativeHistoryRequestContext, NativeHistoryPlan), NativePlanRefusal> {
        let keep = matches!(
            members,
            PassiveMembers::MessagesKeep | PassiveMembers::ResponsesKeep
        );
        let responses_keep = matches!(members, PassiveMembers::ResponsesKeep);
        let duplicate_result = matches!(members, PassiveMembers::DuplicateResult);
        let backend = if matches!(
            members,
            PassiveMembers::ResponsesPair | PassiveMembers::ResponsesKeep
        ) {
            crate::ApiBackend::Responses
        } else {
            crate::ApiBackend::Messages
        };

        let digest = super::super::projection::native_content_digest;
        let mut blobs: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
        let mut put = |kind: &str, value: Value| {
            let bytes = serde_json::to_vec(&value).unwrap();
            let reference = NativeBlobRef {
                kind: kind.into(),
                byte_len: bytes.len() as u64,
                digest: digest(&bytes),
            };
            blobs.insert(reference.digest.clone(), bytes.into());
            reference
        };
        let coordinates = |occurrence| NativeSourceCoordinate {
            lineage: "passive-lineage".into(),
            producing_binding: "issued-A".into(),
            turn: 1,
            item: 0,
            response: 0,
            block: occurrence,
            submember: None,
            occurrence,
        };
        let call = json!({"type":"server_tool_use","id":"srvtoolu_passive","name":"tool_search_tool_regex","input":{"query":"secret query never rendered"}});
        let references = if duplicate_result {
            vec![json!({"type":"tool_reference","tool_name":"alpha__lookup"})]
        } else {
            Vec::new()
        };
        let result = json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_passive","content":{"type":"tool_search_tool_search_result","tool_references":references}});
        let mut raw = if responses_keep {
            vec![
                json!({"type":"tool_search_call","id":"tsc_passive","call_id":"call_passive","execution":"server","status":"completed","arguments":{"query":"secret query never rendered"}}),
                json!({"type":"tool_search_output","id":"tso_passive","call_id":"call_passive","execution":"server","status":"completed","tools":[]}),
            ]
        } else {
            vec![call, result]
        };
        if duplicate_result {
            raw.push(raw[1].clone());
        }
        let original: Vec<_> = raw
            .iter()
            .map(|raw| ConversationItem::Discovery {
                item: super::super::tool_search::ToolSearchItem::from_wire(raw.clone()).unwrap(),
            })
            .collect();
        let member_count = original.len() as u64;
        let history = NativeHistoryBoundary {
            items: member_count,
            digest: digest(&serde_json::to_vec(&original).unwrap()),
        };
        let origin = put(
            "producing_binding",
            json!({"request":"issued-A","generation":17,"origin":{"status":"Unknown"}}),
        );
        let blocks = raw
            .into_iter()
            .enumerate()
            .map(|(index, raw)| NativeRawBlock {
                coordinates: coordinates(index as u64),
                wire_index: index as u32,
                arrival_ordinal: index as u64,
                block: put("raw_block", raw),
                partial: None,
                state: NativeBlockState::ClosedFaithful,
                eligible_ref: None,
                eligible_unavailable: (duplicate_result && index > 0)
                    .then_some(NativeUnavailable::UnknownIdentity),
            })
            .collect();
        let source = NativeSource {
            schema_version: 2,
            lineage: "passive-lineage".into(),
            source_revision: 3,
            producing_bindings: vec![NativeProducingBinding {
                request: "issued-A".into(),
                generation: Some(17),
                origin: json!({"status":"Unknown"}),
                origin_ref: origin.clone(),
            }],
            responses: vec![NativeResponseBoundary {
                occurrence: 0,
                request: "issued-A".into(),
                lease: 1,
                attempt: 1,
                continuation: 0,
                provider_response_id: Some(json!("msg_passive")),
                source_stop_reason: Some("end_turn".into()),
                block_start: 0,
                block_end: member_count,
                stream_complete: true,
                status: NativeResponseStatus::Accepted,
            }],
            ordered_blocks: blocks,
            eligible_snapshots: Vec::new(),
            older_sources: Vec::new(),
        };
        let reference_links: Vec<_> = if duplicate_result {
            [1, 2]
                .into_iter()
                .map(|index| {
                    let mut source = coordinates(index);
                    source.submember = Some(0);
                    NativeEligibleMemberLink {
                        source,
                        identity: NativeEligibleMemberIdentity::Unavailable {
                            reason: NativeUnavailable::UnknownIdentity,
                        },
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let source_ref = put(
            "source",
            serde_json::to_value(NativeSourceV2::from_parts(
                source.clone(),
                reference_links.clone(),
            ))
            .unwrap(),
        );
        let active = NativeActive::default();
        let active_ref = put("active", serde_json::to_value(&active).unwrap());
        let group = NativeGroupRecord {
            key: "passive-group".into(),
            family: if responses_keep {
                "Responses"
            } else {
                "Messages"
            }
            .into(),
            execution: "server".into(),
            origin_ref: origin,
            validated_join: Some(
                if responses_keep {
                    "call_passive"
                } else {
                    "srvtoolu_passive"
                }
                .into(),
            ),
            keyless_server_fifo: false,
            members: (0..member_count).map(coordinates).collect(),
            duplicate_observations: if duplicate_result {
                vec![(coordinates(2), coordinates(1))]
            } else {
                Vec::new()
            },
            state: "Completed".into(),
        };
        let mut ledger = NativeLedger {
            source_digest: source_ref.digest.clone(),
            active_digest: active_ref.digest.clone(),
            groups: vec![group],
            ..Default::default()
        };
        ledger
            .lifecycle
            .groups
            .insert("passive-group".into(), "Completed".into());
        ledger.accounting = (0..member_count)
            .map(|index| NativeViewLink {
                source: coordinates(index),
                source_digest: source_ref.digest.clone(),
                view_item: Some(index),
                view_digest: Some(digest(
                    &serde_json::to_vec(&original[index as usize]).unwrap(),
                )),
                role: "Discovery".into(),
                source_subrange: None,
            })
            .collect();
        let reference_accounting: Vec<_> = reference_links
            .iter()
            .map(|link| NativeEligibleMemberAccounting {
                source: link.source.clone(),
                source_digest: source_ref.digest.clone(),
            })
            .collect();
        let ledger_ref = put(
            "ledger",
            serde_json::to_value(NativeLedgerV2::from_parts(
                ledger.clone(),
                reference_accounting.clone(),
            ))
            .unwrap(),
        );
        let record = NativeSearchHistoryV1 {
            schema_version: 2,
            policy_revision: 1,
            reader_floor: 2,
            limits_revision: 1,
            lineage: NativeLineage {
                id: "passive-lineage".into(),
                source_revision: 3,
                source_digest: source_ref.digest.clone(),
            },
            generation: 5,
            high_water: 7,
            source: source_ref,
            active: active_ref,
            ledger: ledger_ref,
            history_boundary: history.clone(),
            exchange_snapshot: None,
            previous_checkpoint: None,
            capacity: BTreeMap::new(),
        };
        let domain = NativeReplayDomain {
            backend: backend.clone(),
            dialect: if backend == crate::ApiBackend::Responses {
                NativeReplayDialect::Responses(crate::ResponsesReplayDialect::Codex)
            } else {
                NativeReplayDialect::Messages
            },
            model: "passive-target".into(),
            row: "passive-target".into(),
            provider: "offline".into(),
            deployment: "offline".into(),
            deployment_revision: "1".into(),
            endpoint: "https://offline.invalid/v1".into(),
            principal: "fixture".into(),
        };
        let admission = crate::SearchAdmission {
            supports_search_tool: keep,
            has_searchable_tools: keep,
        };
        let target = TargetRoute::new(
            backend.clone(),
            if backend == crate::ApiBackend::Messages {
                Boundary::Vertex
            } else {
                Boundary::AzStrict
            },
            admission,
        );
        let shape = NativeReplayShape {
            family: if responses_keep {
                NativeReplayFamily::ResponsesServer
            } else {
                NativeReplayFamily::MessagesServer
            },
            keyless_server: false,
            output_id_present: responses_keep,
            completed: true,
            result_error: false,
            referenced_members: u64::from(duplicate_result),
        };
        let identity = NativeIdentityExtension::Revision2 {
            eligible_member_links: reference_links,
            eligible_member_accounting: reference_accounting,
        };
        let selected: Vec<_> = (0..member_count).map(coordinates).collect();
        let authorship = (0..member_count)
            .map(|index| (index, NativeIdAuthorship::Provider))
            .collect();
        let core = prepare_native_history_core(
            &NativePureHistory {
                record: &record,
                source: &source,
                ledger: &ledger,
                identity: &identity,
                blobs: &blobs,
                authorship: &authorship,
            },
            &NativeProjectionInputs {
                target_model_id: "passive-target",
                target: &target,
                tools: &[],
                current: None,
                selected: &selected,
                origin_domains: &Default::default(),
                pending: &Default::default(),
                prior_f: None,
            },
            |_| {
                (
                    DiscoveryOrigin {
                        boundary: if responses_keep {
                            Boundary::AzStrict
                        } else {
                            Boundary::Vertex
                        },
                        owner_model_id: keep.then(|| domain.model.clone()),
                    },
                    NativeQualification {
                        source: keep.then(|| domain.clone()),
                        target: Some(domain.clone()),
                        request: Some(domain.clone()),
                        original_admission: admission,
                        shape: shape.clone(),
                        receipt: keep.then(|| NativeShapeReceipt {
                            target: domain.clone(),
                            shape: shape.clone(),
                            receipt_id: "offline-structural-keep".into(),
                        }),
                        intact_membership: true,
                        current_backing: false,
                        faithful_identity: true,
                    },
                )
            },
        )
        .unwrap();
        let post: Vec<_> = core
            .decision
            .members
            .iter()
            .filter_map(|member| {
                member
                    .neutral
                    .as_ref()
                    .map(|neutral| ConversationItem::assistant(neutral.text.clone()))
                    .or_else(|| {
                        member.raw.as_ref().map(|raw| ConversationItem::Discovery {
                            item: super::super::tool_search::ToolSearchItem::from_wire(raw.clone())
                                .unwrap(),
                        })
                    })
            })
            .collect();
        let request = crate::ConversationRequest {
            items: post.clone(),
            model: Some("passive-target".into()),
            search_admission: Some(admission),
            max_output_tokens: Some(4096),
            ..Default::default()
        };
        let carrier = NativeFinalCarrier::capture(
            serde_json::to_vec(&passive_lower(&request, backend.clone()))
                .unwrap()
                .into(),
        )
        .unwrap();
        let identities = if keep && backend == crate::ApiBackend::Messages {
            let declarations: Vec<Value> =
                serde_json::from_slice(carrier.declarations().unwrap()).unwrap();
            let declaration = declarations[0].clone();
            assert_eq!(declaration["name"], "DeferredToolPlaceholder");
            let schema_bytes: Arc<[u8]> = serde_json::to_vec(&declaration["input_schema"])
                .unwrap()
                .into();
            let schema = NativeBlobRef {
                kind: "eligible_schema".into(),
                byte_len: schema_bytes.len() as u64,
                digest: digest(&schema_bytes),
            };
            let binding = source.producing_bindings[0].origin_ref.clone();
            let binding_bytes = blobs[&binding.digest].clone();
            let snapshot = NativeEligibleSnapshot {
                version: 1,
                namespace_kind: NativeNamespaceKind::GrokBuild,
                qualified_namespace: "grok_build".into(),
                child_name: "DeferredToolPlaceholder".into(),
                schema: schema.clone(),
                binding,
            };
            let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
            vec![NativeDeclarationIdentity {
                declaration_index: 0,
                child_index: None,
                namespace_kind: snapshot.namespace_kind,
                qualified_namespace: snapshot.qualified_namespace,
                child_name: snapshot.child_name,
                declaration,
                schema,
                schema_bytes,
                provenance: NativeBlobRef {
                    kind: "eligible_snapshot".into(),
                    byte_len: provenance_bytes.len() as u64,
                    digest: digest(&provenance_bytes),
                },
                provenance_bytes,
                binding_bytes: Some(binding_bytes),
            }]
        } else {
            Vec::new()
        };
        let context = NativeHistoryRequestContext::captured(
            NativeHistoryRequirement::Selected,
            NativeHistoryCapture {
                history,
                history_revision: 11,
                lineage: record.lineage.clone(),
                native_generation: 5,
                high_water: 7,
                projection_revision: 0,
                strict_append_sequence: 13,
                binding_generation: 17,
                install_generation: 19,
                issued_request: "issued-A".into(),
                target: domain.clone(),
                execution: NativeHistoryExecution::Server,
                original_admission: admission,
                versions: NativeHistoryVersions {
                    schema: 2,
                    policy: 1,
                    projection: 1,
                    reader: 2,
                    limits: 1,
                },
            },
            NativeHistoryWriteAuthority::SendOnly,
        );
        let post_history_bytes: Arc<[u8]> = serde_json::to_vec(&post).unwrap().into();
        let plan = NativeHistoryPlan {
            context: context.clone(),
            input_history_bytes: serde_json::to_vec(&original).unwrap().into(),
            ledger_bytes: blobs[&record.ledger.digest].clone(),
            record,
            post_history: NativeHistoryBoundary {
                items: post.len() as u64,
                digest: digest(&post_history_bytes),
            },
            post_history_bytes,
            lowered_history: NativeLoweredHistory::capture(&request, &backend)?,
            carrier,
            identities,
            qualification: core.accounting.qualification.clone(),
            core,
            readiness: NativePlanReadiness::Tentative,
            finalization: None,
        };
        Ok((context, plan))
    }

    #[test]
    fn native_history_passive_plan_context_contract() {
        let (context, plan) = passive_plan_fixture();
        let carrier = plan.carrier.clone();
        assert_eq!(
            validate_native_history_plan(&context, Some(&plan), &carrier),
            Ok(NativeHistoryReady::SendOnly)
        );
        assert_eq!(
            validate_native_history_plan(&context, None, &carrier),
            Err(NativePlanRefusal::MissingPlan),
            "Selected without a plan must never become Inactive"
        );
        let mut inactive = context.clone();
        inactive.requirement = NativeHistoryRequirement::Inactive;
        assert_eq!(
            validate_native_history_plan(&inactive, None, &carrier),
            Ok(NativeHistoryReady::Inactive)
        );
        assert_eq!(
            validate_native_history_plan(&inactive, Some(&plan), &carrier),
            Err(NativePlanRefusal::InactivePlan)
        );
        let mut changed_body: Value = serde_json::from_slice(carrier.bytes()).unwrap();
        changed_body["max_tokens"] = json!(4095);
        let changed =
            NativeFinalCarrier::capture(serde_json::to_vec(&changed_body).unwrap().into()).unwrap();
        assert_eq!(
            validate_native_history_plan(&context, Some(&plan), &changed),
            Err(NativePlanRefusal::FinalCarrierChanged),
            "changed actual carrier refuses even when tools and context match"
        );
        let mut spaced = carrier.bytes().to_vec();
        spaced.push(b' ');
        let spaced = NativeFinalCarrier::capture(spaced.into()).unwrap();
        assert_ne!(spaced.digest(), carrier.digest());
        assert_eq!(
            validate_native_history_plan(&context, Some(&plan), &spaced),
            Err(NativePlanRefusal::FinalCarrierChanged)
        );
        for field in [
            "history",
            "lineage",
            "source",
            "projection",
            "g",
            "high_water",
            "q",
            "binding",
            "install",
            "issued",
            "target",
            "admission",
            "authority",
            "requirement",
        ] {
            let mut changed = context.clone();
            match field {
                "history" => changed.capture.history_revision += 1,
                "lineage" => changed.capture.lineage.id.push('x'),
                "source" => changed.capture.lineage.source_revision += 1,
                "projection" => changed.capture.projection_revision += 1,
                "g" => changed.capture.native_generation += 1,
                "high_water" => changed.capture.high_water += 1,
                "q" => changed.capture.strict_append_sequence += 1,
                "binding" => changed.capture.binding_generation += 1,
                "install" => changed.capture.install_generation += 1,
                "issued" => changed.capture.issued_request = "installed-B".into(),
                "target" => changed.capture.target.deployment.push('B'),
                "admission" => changed.capture.original_admission.supports_search_tool = true,
                "authority" => {
                    changed.authority = NativeHistoryWriteAuthority::OwningPrimary {
                        lineage: changed.capture.lineage.id.clone(),
                        binding_generation: changed.capture.binding_generation,
                        request: "issued-A".into(),
                    }
                }
                "requirement" => changed.requirement = NativeHistoryRequirement::Inactive,
                _ => unreachable!(),
            }
            assert!(
                validate_native_history_plan(&changed, Some(&plan), &carrier).is_err(),
                "retained captured field {field} cannot change"
            );
        }
        for version in ["schema", "policy", "projection", "reader", "limits"] {
            let mut changed = plan.clone();
            match version {
                "schema" => changed.context.capture.versions.schema += 1,
                "policy" => changed.context.capture.versions.policy += 1,
                "projection" => changed.context.capture.versions.projection += 1,
                "reader" => changed.context.capture.versions.reader += 1,
                "limits" => changed.context.capture.versions.limits += 1,
                _ => unreachable!(),
            }
            assert_eq!(
                validate_native_history_plan(&changed.context, Some(&changed), &carrier),
                Err(NativePlanRefusal::UnsupportedVersion)
            );
        }
        let mut tentative = plan.clone();
        tentative.readiness = NativePlanReadiness::Tentative;
        assert_eq!(
            validate_native_history_plan(&context, Some(&tentative), &carrier),
            Err(NativePlanRefusal::NotReady)
        );
        let mut owning = plan.clone();
        owning.context.authority = NativeHistoryWriteAuthority::OwningPrimary {
            lineage: context.capture.lineage.id.clone(),
            binding_generation: 17,
            request: "issued-A".into(),
        };
        let checkpoint_ref = owning.record.source.clone();
        owning = owning.require_generation(
            5,
            7,
            Some(NativeGenerationAuthority {
                schema_version: 2,
                lineage: context.capture.lineage.id.clone(),
                high_water: 7,
                checkpoint: checkpoint_ref,
                transaction: "structural-not-acknowledged".into(),
            }),
        );
        assert_eq!(
            validate_native_history_plan(&owning.context, Some(&owning), &carrier),
            Err(NativePlanRefusal::AuthorityUnverified),
            "stored descriptors do not issue owning-primary permission"
        );
        let tools = NativeFinalCarrier::capture(Arc::from(br#"{ "tools" : [ { "name":"f", "parameters":{} } ], "nested":{"tools":[]}, "model":"m" }"#.as_slice())).unwrap();
        assert_eq!(
            tools.declarations().unwrap(),
            br#"[ { "name":"f", "parameters":{} } ]"#
        );
        assert_eq!(
            tools.tools().unwrap().digest,
            super::super::projection::native_content_digest(tools.declarations().unwrap())
        );
        assert!(
            NativeFinalCarrier::capture(Arc::from(br#"{"tools":[],"tools":[]}"#.as_slice()))
                .is_err()
        );
        assert!(NativeFinalCarrier::capture(Arc::from(br#"{"tools":null}"#.as_slice())).is_err());
        assert!(NativeFinalCarrier::capture(Arc::from(br#"{"tools":[],}"#.as_slice())).is_err());
        assert!(
            NativeFinalCarrier::capture(Arc::from(br#"{"tools":[]} trailing"#.as_slice())).is_err()
        );
        assert!(carrier.tools().is_none());
        let mut missing_shape = plan.clone();
        missing_shape.qualification.clear();
        assert!(validate_native_history_plan(&context, Some(&missing_shape), &carrier).is_err());
        let mut missing_member = plan.clone();
        missing_member.core.accounting.raw_members.pop();
        assert!(validate_native_history_plan(&context, Some(&missing_member), &carrier).is_err());
        let mut false_count = plan.clone();
        false_count.core.accounting.groups.demoted += 1;
        assert!(validate_native_history_plan(&context, Some(&false_count), &carrier).is_err());
        let default = crate::ConversationRequest::default();
        let cloned = default.clone();
        assert_eq!(
            serde_json::to_value(crate::conversation::messages::build_messages_request(
                &default
            ))
            .unwrap(),
            serde_json::to_value(crate::conversation::messages::build_messages_request(
                &cloned
            ))
            .unwrap()
        );
        passive_declarations_and_binding();
    }

    fn passive_declaration_fixture() -> NativeHistoryPlan {
        let (_, mut plan) = passive_plan_fixture();
        let schema = json!({"type":"object","description":"full schema","properties":{"limit":{"type":"integer","default":7}},"required":["limit"],"additionalProperties":false});
        let schema_bytes: Arc<[u8]> = serde_json::to_vec(&schema).unwrap().into();
        let schema_ref = NativeBlobRef {
            kind: "eligible_schema".into(),
            byte_len: schema_bytes.len() as u64,
            digest: super::super::projection::native_content_digest(&schema_bytes),
        };
        let binding_bytes: Arc<[u8]> = serde_json::to_vec(&json!({
            "request":"issued-A", "generation":17, "origin":{"status":"Unknown"}
        }))
        .unwrap()
        .into();
        let snapshot = NativeEligibleSnapshot {
            version: 1,
            namespace_kind: NativeNamespaceKind::Mcp,
            qualified_namespace: "alpha".into(),
            child_name: "lookup".into(),
            schema: schema_ref.clone(),
            binding: NativeBlobRef {
                kind: "producing_binding".into(),
                byte_len: binding_bytes.len() as u64,
                digest: super::super::projection::native_content_digest(&binding_bytes),
            },
        };
        let provenance_bytes: Arc<[u8]> = serde_json::to_vec(&snapshot).unwrap().into();
        let provenance = NativeBlobRef {
            kind: "eligible_snapshot".into(),
            byte_len: provenance_bytes.len() as u64,
            digest: super::super::projection::native_content_digest(&provenance_bytes),
        };
        let declaration =
            json!({"name":"alpha__lookup","description":"exact declaration","input_schema":schema});
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        body["tools"] = json!([declaration.clone()]);
        plan.carrier =
            NativeFinalCarrier::capture(serde_json::to_vec_pretty(&body).unwrap().into()).unwrap();
        plan.identities.push(NativeDeclarationIdentity {
            declaration_index: 0,
            child_index: None,
            namespace_kind: NativeNamespaceKind::Mcp,
            qualified_namespace: "alpha".into(),
            child_name: "lookup".into(),
            declaration,
            schema: schema_ref,
            schema_bytes,
            provenance,
            provenance_bytes,
            binding_bytes: Some(binding_bytes),
        });
        plan
    }

    fn passive_declarations_and_binding() {
        let plan = passive_declaration_fixture();
        let context = plan.context.clone();
        assert_eq!(
            validate_native_history_plan(&context, Some(&plan), &plan.carrier),
            Ok(NativeHistoryReady::SendOnly)
        );
        for field in [
            "identity",
            "schema",
            "description",
            "name",
            "kind",
            "provenance",
            "post",
            "target",
        ] {
            let mut bad = plan.clone();
            match field {
                "identity" => bad.identities.clear(),
                "schema" => bad.identities[0].schema.digest = "a".repeat(64),
                "description" => bad.identities[0].declaration["description"] = json!("different"),
                "name" => bad.identities[0].child_name.push('x'),
                "kind" => bad.identities[0].namespace_kind = NativeNamespaceKind::GrokBuild,
                "provenance" => bad.identities[0].provenance_bytes = Arc::from(b"{}".as_slice()),
                "post" => {
                    bad.post_history_bytes = Arc::from(b"[]".as_slice());
                    bad.post_history = NativeHistoryBoundary {
                        items: 0,
                        digest: super::super::projection::native_content_digest(b"[]"),
                    };
                }
                "target" => {
                    let mut body: Value = serde_json::from_slice(bad.carrier.bytes()).unwrap();
                    body["model"] = json!("installed-B");
                    bad.carrier =
                        NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into())
                            .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                validate_native_history_plan(&context, Some(&bad), &bad.carrier).is_err(),
                "changed passive field {field} cannot become Ready"
            );
        }
        let mut switched = plan.clone();
        switched.context.authority = NativeHistoryWriteAuthority::SwitchStore {
            lineage: context.capture.lineage.id.clone(),
            binding_generation: context.capture.binding_generation,
        };
        assert_eq!(
            validate_native_history_plan(&switched.context, Some(&switched), &switched.carrier),
            Err(NativePlanRefusal::AuthorityUnverified)
        );
        let mut shapes = plan.clone();
        shapes.qualification[0].target.deployment.push('x');
        assert_eq!(
            validate_native_history_plan(&context, Some(&shapes), &shapes.carrier),
            Err(NativePlanRefusal::InvalidQualification)
        );
    }
    fn passive_request(plan: &NativeHistoryPlan) -> crate::ConversationRequest {
        crate::ConversationRequest {
            items: serde_json::from_slice(&plan.post_history_bytes).unwrap(),
            model: Some(plan.context.capture.target.model.clone()),
            search_admission: Some(plan.context.capture.original_admission),
            max_output_tokens: Some(4096),
            ..Default::default()
        }
    }

    fn passive_lower(request: &crate::ConversationRequest, backend: crate::ApiBackend) -> Value {
        match backend {
            crate::ApiBackend::Messages => serde_json::to_value(
                crate::conversation::messages::build_messages_request(request),
            )
            .unwrap(),
            crate::ApiBackend::Responses => {
                let mut request = async_openai::types::responses::CreateResponse::from(request);
                request.store = Some(false);
                serde_json::to_value(request).unwrap()
            }
            _ => unreachable!(),
        }
    }

    fn passive_recheck(plan: &NativeHistoryPlan) -> Result<NativeHistoryPlan, NativePlanRefusal> {
        let request = passive_request(plan);
        NativeHistoryPlan::checked(
            plan.context.clone(),
            NativeHistoryPlanInput {
                record: plan.record.clone(),
                input_history_bytes: plan.input_history_bytes.clone(),
                ledger_bytes: plan.ledger_bytes.clone(),
                post_history_bytes: plan.post_history_bytes.clone(),
                lowering_request: &request,
                carrier: plan.carrier.clone(),
                identities: plan.identities.clone(),
                qualification: plan.qualification.clone(),
                core: plan.core.clone(),
            },
        )?
        .send_only()
    }

    fn passive_replace_post(plan: &mut NativeHistoryPlan, post: &[ConversationItem]) {
        plan.post_history_bytes = serde_json::to_vec(post).unwrap().into();
        plan.post_history = NativeHistoryBoundary {
            items: post.len() as u64,
            digest: super::super::projection::native_content_digest(&plan.post_history_bytes),
        };
        plan.carrier = NativeFinalCarrier::capture(
            serde_json::to_vec(&passive_lower(
                &passive_request(plan),
                plan.context.capture.target.backend.clone(),
            ))
            .unwrap()
            .into(),
        )
        .unwrap();
    }

    fn passive_append_ordinary(plan: &mut NativeHistoryPlan, ordinary: ConversationItem) {
        let mut input: Vec<ConversationItem> =
            serde_json::from_slice(&plan.input_history_bytes).unwrap();
        input.push(ordinary.clone());
        plan.input_history_bytes = serde_json::to_vec(&input).unwrap().into();
        let history = NativeHistoryBoundary {
            items: input.len() as u64,
            digest: super::super::projection::native_content_digest(&plan.input_history_bytes),
        };
        plan.context.capture.history = history.clone();
        plan.record.history_boundary = history.clone();
        plan.core.accounting.history = history;
        let mut post: Vec<ConversationItem> =
            serde_json::from_slice(&plan.post_history_bytes).unwrap();
        post.push(ordinary);
        passive_replace_post(plan, &post);
    }

    #[test]
    fn native_history_plan_rejects_original_discovery_carrier() {
        let (_, mut plan) = passive_plan_fixture();
        let original: Vec<ConversationItem> =
            serde_json::from_slice(&plan.input_history_bytes).unwrap();
        let raw: Vec<_> = original
            .iter()
            .map(|item| match item {
                ConversationItem::Discovery { item } => item.raw().clone(),
                _ => unreachable!(),
            })
            .collect();
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        let assistant = body["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|message| message["role"] == "assistant")
            .unwrap();
        assistant["content"] = json!(raw);
        plan.carrier =
            NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::FinalCarrierChanged),
            "F1 original Discovery carrier cannot certify a neutral-F post-history"
        );
    }

    #[test]
    fn native_history_plan_rejects_empty_carrier_conversation() {
        let (_, mut plan) = passive_plan_fixture();
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        body["messages"] = json!([]);
        plan.carrier =
            NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::FinalCarrierChanged),
            "F1 an empty conversation cannot certify emitted neutral halves"
        );
    }

    #[test]
    fn native_history_plan_rejects_extra_native_carrier_block() {
        let (_, mut plan) = passive_plan_fixture();
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        let original: Vec<ConversationItem> =
            serde_json::from_slice(&plan.input_history_bytes).unwrap();
        let ConversationItem::Discovery { item } = &original[0] else {
            unreachable!()
        };
        let assistant = body["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|message| message["role"] == "assistant")
            .unwrap();
        assistant["content"]
            .as_array_mut()
            .unwrap()
            .push(item.raw().clone());
        plan.carrier =
            NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::FinalCarrierChanged),
            "F1 extra native search material cannot ride alongside neutral F"
        );
    }

    #[test]
    fn native_history_plan_rejects_duplicate_neutral_post() {
        let (_, mut plan) = passive_plan_fixture();
        let mut post: Vec<ConversationItem> =
            serde_json::from_slice(&plan.post_history_bytes).unwrap();
        post.insert(0, post[0].clone());
        passive_replace_post(&mut plan, &post);
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidHistory),
            "F1 an additional neutral wrapper violates exact source-to-output mapping"
        );
    }

    #[test]
    fn native_history_plan_rejects_relocated_neutral_post() {
        let (_, mut plan) = passive_plan_fixture();
        passive_append_ordinary(
            &mut plan,
            ConversationItem::user("ordinary suffix remains after both halves"),
        );
        assert!(passive_recheck(&plan).is_ok());
        let mut post: Vec<ConversationItem> =
            serde_json::from_slice(&plan.post_history_bytes).unwrap();
        let ordinary = post.pop().unwrap();
        post.insert(1, ordinary);
        passive_replace_post(&mut plan, &post);
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidHistory),
            "F1 preserving native subsequence order is not preserving its positions"
        );
    }

    fn passive_binding_change(plan: &mut NativeHistoryPlan, field: &str, value: Value) {
        let identity = &mut plan.identities[0];
        let mut binding: Value =
            serde_json::from_slice(identity.binding_bytes.as_ref().unwrap()).unwrap();
        binding[field] = value;
        let bytes: Arc<[u8]> = serde_json::to_vec(&binding).unwrap().into();
        let mut snapshot: NativeEligibleSnapshot =
            serde_json::from_slice(&identity.provenance_bytes).unwrap();
        snapshot.binding.byte_len = bytes.len() as u64;
        snapshot.binding.digest = super::super::projection::native_content_digest(&bytes);
        identity.binding_bytes = Some(bytes);
        identity.provenance_bytes = serde_json::to_vec(&snapshot).unwrap().into();
        identity.provenance.byte_len = identity.provenance_bytes.len() as u64;
        identity.provenance.digest =
            super::super::projection::native_content_digest(&identity.provenance_bytes);
    }

    #[test]
    fn native_history_plan_rejects_missing_binding_descriptor() {
        let mut plan = passive_declaration_fixture();
        plan.identities[0].binding_bytes = None;
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidDeclarations),
            "F2 a snapshot must resolve its actual producing-binding descriptor"
        );
    }

    #[test]
    fn native_history_plan_rejects_foreign_binding_request() {
        let mut plan = passive_declaration_fixture();
        passive_binding_change(&mut plan, "request", json!("installed-B"));
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidDeclarations),
            "F2 a hash-consistent foreign request cannot supply producing provenance"
        );
    }

    #[test]
    fn native_history_plan_rejects_foreign_binding_generation() {
        let mut plan = passive_declaration_fixture();
        passive_binding_change(&mut plan, "generation", json!(18));
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidDeclarations),
            "F2 a hash-consistent foreign generation cannot supply producing provenance"
        );
    }

    #[test]
    fn native_history_plan_rejects_binding_digest_mismatch() {
        let mut plan = passive_declaration_fixture();
        let mut bytes = plan.identities[0].binding_bytes.as_ref().unwrap().to_vec();
        bytes.push(b' ');
        plan.identities[0].binding_bytes = Some(bytes.into());
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidDeclarations),
            "F2 binding digests cover actual descriptor bytes, not equivalent reserialization"
        );
    }

    #[test]
    fn native_history_plan_accepts_large_ordinary_history() {
        let (_, mut plan) = passive_plan_fixture();
        for index in 0..17 {
            let text = "x".repeat(256 * 1024);
            let item = if index % 2 == 0 {
                ConversationItem::user(text)
            } else {
                ConversationItem::assistant(text)
            };
            passive_append_ordinary(&mut plan, item);
        }
        assert!(plan.post_history_bytes.len() as u64 > NATIVE_ACTIVE_BYTES);
        assert!((plan.carrier.bytes().len() as u64) < NATIVE_SNAPSHOT_PAYLOAD_BYTES);
        let checked = passive_recheck(&plan);
        assert!(
            checked.is_ok(),
            "F3 ordinary history is not the native-active partition: {checked:?}"
        );
        let checked = checked.unwrap();
        assert_eq!(
            validate_native_history_plan(&checked.context, Some(&checked), &checked.carrier),
            Ok(NativeHistoryReady::SendOnly)
        );
    }

    #[test]
    fn native_history_plan_rejects_post_above_snapshot_envelope() {
        let (_, mut plan) = passive_plan_fixture();
        passive_append_ordinary(
            &mut plan,
            ConversationItem::assistant_with_model(
                "ordinary model metadata is not copied to the carrier",
                "x".repeat(NATIVE_SNAPSHOT_PAYLOAD_BYTES as usize),
            ),
        );
        assert!(plan.post_history_bytes.len() as u64 > NATIVE_SNAPSHOT_PAYLOAD_BYTES);
        assert!((plan.carrier.bytes().len() as u64) < NATIVE_SNAPSHOT_PAYLOAD_BYTES);
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidHistory),
            "F3 the full post-history remains bounded by the snapshot envelope"
        );
    }

    #[test]
    fn native_history_plan_rejects_reasserted_qualification() {
        let (_, mut plan) = passive_plan_fixture();
        plan.qualification[0].shape.keyless_server = true;
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::InvalidQualification),
            "F4 the passive plan cannot reassert qualification different from the common core"
        );
    }

    #[test]
    fn native_history_plan_accounts_duplicate_result_once_logically() {
        let result = passive_plan_fixture_for(PassiveMembers::DuplicateResult);
        assert!(
            result.is_ok(),
            "F5 duplicate observation charges are not logical result cardinality: {result:?}"
        );
        let (context, plan) = result.unwrap();
        assert_eq!(plan.core.accounting.references.len(), 2);
        assert_eq!(
            plan.core.accounting.qualification[0]
                .shape
                .referenced_members,
            1
        );
        assert_eq!(
            validate_native_history_plan(&context, Some(&plan), &plan.carrier),
            Ok(NativeHistoryReady::SendOnly)
        );
    }

    #[test]
    fn native_history_plan_binds_both_real_dialect_lowerers() {
        for members in [PassiveMembers::Pair, PassiveMembers::ResponsesPair] {
            let (_, mut plan) = passive_plan_fixture_for(members).unwrap();
            passive_append_ordinary(
                &mut plan,
                ConversationItem::user("ordinary suffix is also bound"),
            );
            let valid = passive_recheck(&plan).unwrap();
            assert_eq!(
                validate_native_history_plan(&valid.context, Some(&valid), &valid.carrier),
                Ok(NativeHistoryReady::SendOnly)
            );
            let key = match plan.context.capture.target.backend {
                crate::ApiBackend::Messages => "messages",
                crate::ApiBackend::Responses => "input",
                _ => unreachable!(),
            };
            for change in ["empty", "extra_native", "ordinary"] {
                let mut bad = plan.clone();
                let mut body: Value = serde_json::from_slice(bad.carrier.bytes()).unwrap();
                match change {
                    "empty" => body[key] = json!([]),
                    "extra_native" => body[key].as_array_mut().unwrap().push(json!({
                        "type":"tool_search_call", "execution":"server", "arguments":{"query":"unprepared"}
                    })),
                    "ordinary" => {
                        let last = body[key].as_array_mut().unwrap().last_mut().unwrap();
                        last["content"] = json!("changed ordinary suffix");
                    }
                    _ => unreachable!(),
                }
                bad.carrier =
                    NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
                assert_eq!(
                    passive_recheck(&bad),
                    Err(NativePlanRefusal::FinalCarrierChanged),
                    "F1 {key}/{change} is compared with the real lowerer without exclusions"
                );
            }
        }
    }

    fn assert_unverified_keep_refused(plan: NativeHistoryPlan) {
        assert!(
            plan.core
                .decision
                .groups
                .iter()
                .any(|group| group.action == NativeHistoryAction::Keep)
        );
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::KeepCarrierUnverified),
            "R3 a KEEP plan cannot certify an unverified native carrier"
        );
        assert_eq!(
            plan.clone().send_only(),
            Err(NativePlanRefusal::KeepCarrierUnverified)
        );
        let mut unchecked_ready = plan;
        unchecked_ready.readiness = NativePlanReadiness::SendOnly;
        assert_eq!(
            validate_native_history_plan(
                &unchecked_ready.context,
                Some(&unchecked_ready),
                &unchecked_ready.carrier
            ),
            Err(NativePlanRefusal::KeepCarrierUnverified)
        );
        let context = unchecked_ready.context.clone();
        let required = unchecked_ready.require_generation(5, 7, None);
        assert_eq!(
            validate_native_history_plan(&context, Some(&required), &required.carrier),
            Err(NativePlanRefusal::KeepCarrierUnverified)
        );
    }

    #[test]
    fn native_history_plan_rejects_messages_keep_legacy_carrier() {
        let (_, plan) = passive_plan_candidate_for(PassiveMembers::MessagesKeep).unwrap();
        let body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        assert!(!body["messages"].as_array().unwrap().is_empty());
        assert_unverified_keep_refused(plan);
    }

    #[test]
    fn native_history_plan_rejects_responses_keep_placeholder_carrier() {
        let (_, plan) = passive_plan_candidate_for(PassiveMembers::ResponsesKeep).unwrap();
        let body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        assert!(
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] == "message")
        );
        assert_unverified_keep_refused(plan);
    }

    #[test]
    fn native_history_plan_rejects_responses_keep_raw_spliced_carrier() {
        let (_, mut plan) = passive_plan_candidate_for(PassiveMembers::ResponsesKeep).unwrap();
        let request = passive_request(&plan);
        let replacements =
            request.raw_responses_input_replacements(crate::ResponsesReplayDialect::Codex);
        assert_eq!(replacements.len(), 2);
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        for replacement in replacements {
            body["input"][replacement.input_item_index] = replacement.value;
        }
        assert_eq!(body["input"][0]["type"], "tool_search_call");
        assert_eq!(body["input"][1]["type"], "tool_search_output");
        plan.carrier =
            NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
        assert_unverified_keep_refused(plan);
    }

    #[test]
    fn native_history_plan_rejects_responses_reasoning_finalize_rewrite() {
        use async_openai::types::responses as rs;
        let (_, mut plan) = passive_plan_fixture_for(PassiveMembers::ResponsesPair).unwrap();
        passive_append_ordinary(
            &mut plan,
            ConversationItem::Reasoning(
                rs::ReasoningItem {
                    id: None,
                    summary: Vec::new(),
                    content: Some(vec![rs::ReasoningItemContent::ReasoningText(
                        rs::ReasoningTextContent {
                            text: "ordinary retained reasoning".into(),
                        },
                    )]),
                    encrypted_content: None,
                    status: Some(rs::OutputStatus::Completed),
                }
                .into(),
            ),
        );
        plan = passive_recheck(&plan).unwrap();
        assert_eq!(
            validate_native_history_plan(&plan.context, Some(&plan), &plan.carrier),
            Ok(NativeHistoryReady::SendOnly)
        );
        let mut body: Value = serde_json::from_slice(plan.carrier.bytes()).unwrap();
        let typed_input = body["input"].clone();
        crate::conversation::patch_reasoning_empty_ids(&mut body, "passive-target");
        assert_ne!(
            body["input"], typed_input,
            "the actual reasoning finalize patch must change the input"
        );
        let finalized =
            NativeFinalCarrier::capture(serde_json::to_vec(&body).unwrap().into()).unwrap();
        assert_eq!(
            validate_native_history_plan(&plan.context, Some(&plan), &finalized),
            Err(NativePlanRefusal::FinalCarrierChanged)
        );
        plan.carrier = finalized;
        assert_eq!(
            passive_recheck(&plan),
            Err(NativePlanRefusal::FinalCarrierChanged),
            "R3 finalize-aware binding is deferred, not silently accepted"
        );
    }
}
