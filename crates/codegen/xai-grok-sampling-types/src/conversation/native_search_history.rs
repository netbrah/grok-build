//! Passive host-only native-search evidence. This record grants no replay authority.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NATIVE_SCHEMA_VERSION: u32 = 1;
pub const NATIVE_POLICY_REVISION: u32 = 1;
pub const NATIVE_READER_FLOOR: u32 = 1;
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
