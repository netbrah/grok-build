//! Chat persistence trait and mock implementation.
//!
//! The actor owns persistence exclusively (`Box<dyn ChatPersistence>`), so the
//! trait uses `&mut self` — no locks, no atomics, no shared state.
//! The mock uses a channel to report records to the test, keeping everything
//! in the actor / message-passing paradigm.

use std::io;

use tokio::sync::{mpsc, oneshot};
use xai_grok_sampling_types::ConversationItem;

use crate::commands::{StrictAppendAck, StrictAppendError};

/// Abstraction over chat-specific persistence. The actor owns this exclusively via `Box<dyn ChatPersistence>`.
/// Methods take `&mut self` — no interior mutability. The real impl wraps a sender the actor solely owns.
pub trait ChatPersistence: Send + 'static {
    /// Persist a single conversation item (append to chat_history.jsonl).
    fn persist_message(&mut self, item: &ConversationItem);

    /// Persist one working-directory switch generation and report commit status.
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>>;

    /// Replace the entire chat history (compaction / rewind).
    fn replace_history(&mut self, items: &[ConversationItem]);

    /// Destructive image-strip rewrite: back up on-disk history, then replace it, acking the disk outcome.
    /// A failed backup gates off the rewrite so recoverability never silently evaporates.
    /// Backends without a recoverable store may no-op the backup but must ack the write.
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>>;

    /// Flush pending writes to disk.
    fn flush(&mut self);

    fn read_native_history(
        &mut self,
    ) -> oneshot::Receiver<
        Result<
            native::NativeRead,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence));
        rx
    }

    fn persist_native_generation_and_ack(
        &mut self,
        _write: native::NativeWrite,
    ) -> oneshot::Receiver<
        Result<
            native::NativeAppendAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence));
        rx
    }
}
/// Host-only native generations; ordinary chat snapshots do not carry this authority.
pub mod native {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use xai_grok_sampling_types::ConversationItem;
    use xai_grok_sampling_types::conversation::native_search_history::*;

    #[derive(Clone)]
    pub struct NativeCheckpoint {
        pub record: NativeSearchHistoryV1,
        pub source: NativeSource,
        pub active: NativeActive,
        pub ledger: NativeLedger,
        pub bytes: Arc<[u8]>,
        pub blobs: BTreeMap<String, Arc<[u8]>>,
        pub blob_kinds: BTreeMap<String, String>,
    }

    impl std::fmt::Debug for NativeCheckpoint {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NativeCheckpoint")
                .field("generation", &self.record.generation)
                .field("responses", &self.source.responses.len())
                .field("blocks", &self.source.ordered_blocks.len())
                .finish_non_exhaustive()
        }
    }

    #[derive(Debug, Clone)]
    pub struct NativeRead {
        pub lineage: String,
        pub checkpoint: Option<Arc<NativeCheckpoint>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NativeAppendAck {
        Applied { generation: u64, digest: String },
        Unchanged,
        Released,
    }

    #[derive(Debug, Clone)]
    pub struct NativeWrite {
        pub previous_generation: u64,
        pub transaction: String,
        pub checkpoint: Arc<NativeCheckpoint>,
    }

    #[derive(Default)]
    pub(crate) struct NativeActorState {
        pub lineage: Option<String>,
        pub checkpoint: Option<Arc<NativeCheckpoint>>,
        pub unavailable: Option<NativeUnavailable>,
        pub leases: BTreeMap<u64, NativeActorLease>,
        pub next_lease: u64,
    }

    /// Owns a lease until its reply is consumed by the requesting handle.
    #[derive(Debug)]
    pub struct NativeCaptureReply {
        pub(crate) lease: Option<NativeCaptureLease>,
    }

    impl Drop for NativeCaptureReply {
        fn drop(&mut self) {
            if let Some(lease) = &self.lease {
                lease.finish();
            }
        }
    }

    pub(crate) struct NativeActorLease {
        pub capability: NativeCaptureLease,
        pub pending: Vec<Arc<NativeAttemptEvidence>>,
        pub admitted: bool,
        pub reserved_bytes: u64,
    }

    pub fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn history_boundary(
        items: &[ConversationItem],
    ) -> Result<NativeHistoryBoundary, NativeUnavailable> {
        // The ordinary history is hashed but is not copied into the native source partition.
        struct HashWriter(Sha256);
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, items).map_err(|_| NativeUnavailable::Fidelity)?;
        Ok(NativeHistoryBoundary {
            items: items.len() as u64,
            digest: format!("{:x}", writer.0.finalize()),
        })
    }

    fn blob_resource(kind: &str) -> Result<NativeResource, NativeUnavailable> {
        match kind {
            "raw_block" | "partial_input_json" | "producing_binding" | "eligible_schema"
            | "eligible_snapshot" | "source" => Ok(NativeResource::SourceBytes),
            "active" => Ok(NativeResource::ActiveBytes),
            "ledger" => Ok(NativeResource::LedgerBytes),
            "snapshot" | "snapshot_body" => Ok(NativeResource::SnapshotBytes),
            "checkpoint" => Ok(NativeResource::ControlBytes),
            _ => Err(NativeUnavailable::UnsupportedVersion),
        }
    }

    fn blob_limit(kind: &str) -> Result<u64, NativeUnavailable> {
        Ok(match kind {
            "raw_block" | "partial_input_json" => NATIVE_BLOCK_BYTES,
            "snapshot_body" => NATIVE_SNAPSHOT_PAYLOAD_BYTES,
            _ => blob_resource(kind)?.ceiling(),
        })
    }

    fn partition_bytes(
        blobs: &BTreeMap<String, Arc<[u8]>>,
        kinds: &BTreeMap<String, String>,
        resource: NativeResource,
    ) -> Result<u64, NativeUnavailable> {
        blobs.iter().try_fold(0u64, |bytes, (key, value)| {
            let kind = kinds.get(key).ok_or(NativeUnavailable::InvalidReference)?;
            if blob_resource(kind)? == resource {
                resource.admit(bytes, value.len() as u64)
            } else {
                Ok(bytes)
            }
        })
    }

    fn reserve_blob(
        blobs: &BTreeMap<String, Arc<[u8]>>,
        kinds: &BTreeMap<String, String>,
        kind: &str,
        bytes: u64,
    ) -> Result<(), NativeUnavailable> {
        if bytes > blob_limit(kind)? {
            return Err(NativeUnavailable::Capacity);
        }
        let resource = blob_resource(kind)?;
        resource.admit(partition_bytes(blobs, kinds, resource)?, bytes)?;
        Ok(())
    }

    fn add_blob(
        blobs: &mut BTreeMap<String, Arc<[u8]>>,
        kinds: &mut BTreeMap<String, String>,
        kind: &str,
        bytes: Vec<u8>,
    ) -> Result<NativeBlobRef, NativeUnavailable> {
        let reference = NativeBlobRef {
            kind: kind.into(),
            byte_len: bytes.len() as u64,
            digest: digest(&bytes),
        };
        if let Some(old) = blobs.get(&reference.digest) {
            if old.as_ref() != bytes.as_slice()
                || kinds.get(&reference.digest).map(String::as_str) != Some(kind)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        } else {
            reserve_blob(blobs, kinds, kind, reference.byte_len)?;
            kinds.insert(reference.digest.clone(), kind.into());
            blobs.insert(reference.digest.clone(), bytes.into());
        }
        Ok(reference)
    }

    fn add_raw_blob(
        blobs: &mut BTreeMap<String, Arc<[u8]>>,
        kinds: &mut BTreeMap<String, String>,
        kind: &str,
        bytes: &[u8],
    ) -> Result<NativeBlobRef, NativeUnavailable> {
        let hash = digest(bytes);
        if let Some(old) = blobs.get(&hash) {
            if old.as_ref() != bytes || kinds.get(&hash).map(String::as_str) != Some(kind) {
                return Err(NativeUnavailable::InvalidReference);
            }
            return Ok(NativeBlobRef {
                kind: kind.into(),
                byte_len: bytes.len() as u64,
                digest: hash,
            });
        }
        reserve_blob(blobs, kinds, kind, bytes.len() as u64)?;
        add_blob(blobs, kinds, kind, bytes.to_vec())
    }

    fn add_json<T: Serialize>(
        blobs: &mut BTreeMap<String, Arc<[u8]>>,
        kinds: &mut BTreeMap<String, String>,
        kind: &str,
        value: &T,
        limit: u64,
    ) -> Result<NativeBlobRef, NativeUnavailable> {
        let length = native_encoded_len(value, limit)?;
        struct HashWriter(Sha256);
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut hash = HashWriter(Sha256::new());
        serde_json::to_writer(&mut hash, value).map_err(|_| NativeUnavailable::Fidelity)?;
        let key = format!("{:x}", hash.0.finalize());
        if !blobs.contains_key(&key) {
            reserve_blob(blobs, kinds, kind, length)?;
        }
        add_blob(blobs, kinds, kind, encode_native(value, limit)?)
    }

    fn read_blob(
        reference: &NativeBlobRef,
        blobs: &mut BTreeMap<String, Arc<[u8]>>,
        kinds: &mut BTreeMap<String, String>,
        load: &mut impl FnMut(&NativeBlobRef) -> Result<Vec<u8>, NativeUnavailable>,
    ) -> Result<Arc<[u8]>, NativeUnavailable> {
        if reference.digest.len() != 64
            || !reference
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        if let Some(bytes) = blobs.get(&reference.digest) {
            if bytes.len() as u64 != reference.byte_len
                || kinds.get(&reference.digest) != Some(&reference.kind)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            return Ok(bytes.clone());
        }
        reserve_blob(blobs, kinds, &reference.kind, reference.byte_len)?;
        let bytes = load(reference)?;
        if bytes.len() as u64 != reference.byte_len || digest(&bytes) != reference.digest {
            return Err(NativeUnavailable::InvalidReference);
        }
        let bytes: Arc<[u8]> = bytes.into();
        kinds.insert(reference.digest.clone(), reference.kind.clone());
        blobs.insert(reference.digest.clone(), bytes.clone());
        Ok(bytes)
    }

    fn check_record(record: &NativeSearchHistoryV1) -> Result<(), NativeUnavailable> {
        if record.schema_version != NATIVE_SCHEMA_VERSION
            || record.policy_revision != NATIVE_POLICY_REVISION
            || record.reader_floor != NATIVE_READER_FLOOR
            || record.limits_revision != NATIVE_LIMITS_REVISION
            || record.generation == 0
            || record.high_water != record.generation
        {
            return Err(NativeUnavailable::UnsupportedVersion);
        }
        if record.lineage.id.is_empty()
            || record.source.kind != "source"
            || record.active.kind != "active"
            || record.ledger.kind != "ledger"
            || record.source.digest != record.lineage.source_digest
            || record
                .exchange_snapshot
                .as_ref()
                .is_some_and(|r| r.kind != "snapshot")
            || record
                .previous_checkpoint
                .as_ref()
                .is_some_and(|r| r.kind != "checkpoint")
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        Ok(())
    }

    impl NativeCheckpoint {
        pub fn decode(
            bytes: Vec<u8>,
            mut load: impl FnMut(&NativeBlobRef) -> Result<Vec<u8>, NativeUnavailable>,
        ) -> Result<Self, NativeUnavailable> {
            NativeResource::ControlBytes.admit(bytes.len() as u64, 0)?;
            let record: NativeSearchHistoryV1 = serde_json::from_slice(&bytes)
                .map_err(|_| NativeUnavailable::UnsupportedVersion)?;
            check_record(&record)?;
            let mut blobs = BTreeMap::new();
            let mut blob_kinds = BTreeMap::new();
            let mut queue = std::collections::VecDeque::new();
            let enqueue_record =
                |record: &NativeSearchHistoryV1, queue: &mut std::collections::VecDeque<_>| {
                    for reference in [&record.source, &record.active, &record.ledger]
                        .into_iter()
                        .chain(record.exchange_snapshot.as_ref())
                        .chain(record.previous_checkpoint.as_ref())
                    {
                        queue.push_back((
                            reference.clone(),
                            record.generation,
                            record.lineage.source_revision,
                        ));
                    }
                };
            enqueue_record(&record, &mut queue);
            while let Some((reference, generation, revision)) = queue.pop_front() {
                let seen = blobs.contains_key(&reference.digest);
                let payload = read_blob(&reference, &mut blobs, &mut blob_kinds, &mut load)?;
                match reference.kind.as_str() {
                    "checkpoint" => {
                        let older: NativeSearchHistoryV1 = serde_json::from_slice(&payload)
                            .map_err(|_| NativeUnavailable::InvalidReference)?;
                        check_record(&older)?;
                        if older.generation >= generation || older.lineage.id != record.lineage.id {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        if !seen {
                            enqueue_record(&older, &mut queue);
                        }
                    }
                    "source" => {
                        let source: NativeSource = serde_json::from_slice(&payload)
                            .map_err(|_| NativeUnavailable::InvalidReference)?;
                        if source.schema_version != NATIVE_SCHEMA_VERSION
                            || source.lineage != record.lineage.id
                            || source.source_revision == 0
                            || source.source_revision > revision
                        {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        if !seen {
                            for block in &source.ordered_blocks {
                                if block.block.kind != "raw_block"
                                    || block
                                        .partial
                                        .as_ref()
                                        .is_some_and(|r| r.kind != "partial_input_json")
                                {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                for reference in std::iter::once(&block.block)
                                    .chain(block.partial.as_ref())
                                    .chain(block.eligible_ref.as_ref())
                                {
                                    queue.push_back((
                                        reference.clone(),
                                        generation,
                                        source.source_revision,
                                    ));
                                }
                            }
                            for binding in &source.producing_bindings {
                                if binding.origin_ref.kind != "producing_binding" {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                queue.push_back((
                                    binding.origin_ref.clone(),
                                    generation,
                                    source.source_revision,
                                ));
                            }
                            for identity in &source.eligible_snapshots {
                                if identity.kind != "eligible_snapshot" {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                queue.push_back((
                                    identity.clone(),
                                    generation,
                                    source.source_revision,
                                ));
                            }
                            for older in &source.older_sources {
                                if older.kind != "source" {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                queue.push_back((
                                    older.clone(),
                                    generation,
                                    source.source_revision - 1,
                                ));
                            }
                        }
                    }
                    "snapshot" if !seen => {
                        let snapshot: NativeSnapshot = serde_json::from_slice(&payload)
                            .map_err(|_| NativeUnavailable::InvalidReference)?;
                        if snapshot.body.kind != "snapshot_body"
                            || snapshot.binding.kind != "producing_binding"
                        {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        queue.push_back((snapshot.body, generation, revision));
                        queue.push_back((snapshot.binding, generation, revision));
                    }
                    "eligible_snapshot" if !seen => {
                        let identity: NativeEligibleSnapshot = serde_json::from_slice(&payload)
                            .map_err(|_| NativeUnavailable::InvalidReference)?;
                        if identity.version != 1
                            || identity.schema.kind != "eligible_schema"
                            || identity.binding.kind != "producing_binding"
                        {
                            return Err(NativeUnavailable::UnsupportedVersion);
                        }
                        queue.push_back((identity.schema, generation, revision));
                        queue.push_back((identity.binding, generation, revision));
                    }
                    _ => {}
                }
            }
            let parse = |reference: &NativeBlobRef| {
                blobs
                    .get(&reference.digest)
                    .ok_or(NativeUnavailable::InvalidReference)
            };
            let source: NativeSource = serde_json::from_slice(parse(&record.source)?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            let active: NativeActive = serde_json::from_slice(parse(&record.active)?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            let ledger: NativeLedger = serde_json::from_slice(parse(&record.ledger)?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            let checkpoint = Self {
                record,
                source,
                active,
                ledger,
                bytes: bytes.into(),
                blobs,
                blob_kinds,
            };
            checkpoint.validate()?;
            Ok(checkpoint)
        }

        pub fn matches_history(&self, items: &[ConversationItem]) -> Result<(), NativeUnavailable> {
            self.matches_pending_history(items)?;
            for link in &self.active.view_links {
                let index = usize::try_from(link.view_item.ok_or(NativeUnavailable::StaleHistory)?)
                    .map_err(|_| NativeUnavailable::StaleHistory)?;
                if items.get(index).is_none() {
                    return Err(NativeUnavailable::StaleHistory);
                }
            }
            Ok(())
        }

        /// Evidence precedes the ordinary response commit; missing new views are not readable yet.
        pub fn matches_pending_history(
            &self,
            items: &[ConversationItem],
        ) -> Result<(), NativeUnavailable> {
            let count = usize::try_from(self.record.history_boundary.items)
                .map_err(|_| NativeUnavailable::StaleHistory)?;
            let prefix = items.get(..count).ok_or(NativeUnavailable::StaleHistory)?;
            if history_boundary(prefix)? != self.record.history_boundary {
                return Err(NativeUnavailable::StaleHistory);
            }
            for link in &self.active.view_links {
                let index = usize::try_from(link.view_item.ok_or(NativeUnavailable::StaleHistory)?)
                    .map_err(|_| NativeUnavailable::StaleHistory)?;
                let expected = link
                    .view_digest
                    .as_ref()
                    .ok_or(NativeUnavailable::StaleHistory)?;
                if let Some(item) = items.get(index)
                    && history_boundary(std::slice::from_ref(item))?.digest != *expected
                {
                    return Err(NativeUnavailable::StaleHistory);
                }
            }
            Ok(())
        }
    }

    impl NativeCheckpoint {
        pub fn append(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            attempts: &[Arc<NativeAttemptEvidence>],
            items: &[ConversationItem],
        ) -> Result<Self, NativeUnavailable> {
            let generation = previous
                .map_or(0, |p| p.record.generation)
                .checked_add(1)
                .ok_or(NativeUnavailable::Capacity)?;
            if let Some(previous) = previous {
                previous.validate()?;
                previous.matches_history(items)?;
                if previous.source.lineage != lease.lineage() {
                    return Err(NativeUnavailable::StaleHistory);
                }
            }
            let mut blobs = previous.map_or_else(BTreeMap::new, |p| p.blobs.clone());
            let mut blob_kinds = previous.map_or_else(BTreeMap::new, |p| p.blob_kinds.clone());
            let mut source = previous.map_or_else(
                || NativeSource {
                    schema_version: NATIVE_SCHEMA_VERSION,
                    lineage: lease.lineage().into(),
                    source_revision: 0,
                    producing_bindings: Vec::new(),
                    responses: Vec::new(),
                    ordered_blocks: Vec::new(),
                    eligible_snapshots: Vec::new(),
                    older_sources: Vec::new(),
                },
                |p| p.source.clone(),
            );
            let mut active = previous.map_or_else(NativeActive::default, |p| p.active.clone());
            let mut ledger = previous.map_or_else(NativeLedger::default, |p| p.ledger.clone());
            let old_link_count = active.view_links.len();
            let old_accounting_count = ledger.accounting.len();
            let origin = serde_json::json!({"status":"Unknown","reason":"producing identity unavailable before U3"});
            let descriptor =
                serde_json::json!({"request":lease.request(),"generation":null,"origin":origin});
            let origin_ref = add_json(
                &mut blobs,
                &mut blob_kinds,
                "producing_binding",
                &descriptor,
                NATIVE_SOURCE_BYTES,
            )?;
            if !source
                .producing_bindings
                .iter()
                .any(|p| p.request == lease.request())
            {
                source.producing_bindings.push(NativeProducingBinding {
                    request: lease.request().into(),
                    generation: None,
                    origin,
                    origin_ref: origin_ref.clone(),
                });
            }
            for attempt in attempts {
                let observation_bytes = encode_native(attempt.as_ref(), NATIVE_RESPONSE_BYTES)?;
                let observation_digest = digest(&observation_bytes);
                if let Some(old) = ledger.observations.iter().find(|old| {
                    old.request == lease.request()
                        && old.lease == lease.ordinal()
                        && old.attempt == attempt.attempt
                }) {
                    if old.digest != observation_digest {
                        return Err(NativeUnavailable::ConflictingObservation);
                    }
                    continue;
                }
                let response_occurrence = (attempt.response_started && attempt.admitted)
                    .then_some(source.responses.len() as u64);
                let block_start = source.ordered_blocks.len() as u64;
                let mut discoveries = Vec::new();
                let mut discovery_coordinates = Vec::new();
                let mut abandoned = false;
                for (index, block) in attempt.blocks.iter().enumerate() {
                    let raw: serde_json::Value = serde_json::from_str(&block.raw)
                        .map_err(|_| NativeUnavailable::Fidelity)?;
                    if block.raw.len() as u64 > NATIVE_BLOCK_BYTES {
                        return Err(NativeUnavailable::Capacity);
                    }
                    let reference = add_raw_blob(
                        &mut blobs,
                        &mut blob_kinds,
                        "raw_block",
                        block.raw.as_bytes(),
                    )?;
                    let partial = block
                        .partial_json
                        .as_ref()
                        .map(|partial| {
                            add_json(
                                &mut blobs,
                                &mut blob_kinds,
                                "partial_input_json",
                                partial,
                                NATIVE_BLOCK_BYTES,
                            )
                        })
                        .transpose()?;
                    if reference
                        .byte_len
                        .checked_add(partial.as_ref().map_or(0, |p| p.byte_len))
                        .filter(|bytes| *bytes <= NATIVE_BLOCK_BYTES)
                        .is_none()
                    {
                        return Err(NativeUnavailable::Capacity);
                    }
                    let coordinates = NativeSourceCoordinate {
                        lineage: lease.lineage().into(),
                        producing_binding: lease.request().into(),
                        turn: lease.turn(),
                        item: items.len() as u64,
                        response: response_occurrence.ok_or(NativeUnavailable::Fidelity)?,
                        block: index as u64,
                        submember: None,
                        occurrence: source.ordered_blocks.len() as u64,
                    };
                    let discovery =
                        xai_grok_sampling_types::tool_search::ToolSearchItem::from_wire(
                            raw.clone(),
                        )
                        .ok();
                    if let Some(discovery) = discovery {
                        discoveries.push(discovery);
                        discovery_coordinates.push(coordinates.clone());
                    }
                    let raw_type = raw
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if raw_type == "tool_use"
                        && let Some(id) = raw
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .filter(|id| !id.is_empty())
                    {
                        ledger.lifecycle.waiting_clients.push(NativeWaitingClient {
                            client_id: id.into(),
                            source: coordinates.clone(),
                            result: None,
                            delivery: if matches!(attempt.status, NativeResponseStatus::Accepted) {
                                "Waiting"
                            } else {
                                "Unavailable"
                            }
                            .into(),
                        });
                    }
                    let tool_member = matches!(
                        raw_type,
                        "server_tool_use" | "tool_search_tool_result" | "tool_use" | "tool_result"
                    );
                    source.ordered_blocks.push(NativeRawBlock {
                        coordinates: coordinates.clone(),
                        wire_index: block.wire_index,
                        arrival_ordinal: block.arrival_ordinal,
                        block: reference,
                        partial,
                        state: block.state.clone(),
                        eligible_ref: None,
                        eligible_unavailable: tool_member
                            .then_some(NativeUnavailable::UnknownIdentity),
                    });
                    let view = attempt.view_roles.get(index).and_then(Option::as_ref);
                    let view = view.filter(|_| {
                        matches!(attempt.status, NativeResponseStatus::Accepted)
                            && lease.view_authorized()
                    });
                    let view_digest = view
                        .map(|(item, _)| {
                            usize::try_from(*item)
                                .ok()
                                .and_then(|index| attempt.view_digests.get(index))
                                .cloned()
                                .ok_or(NativeUnavailable::Fidelity)
                        })
                        .transpose()?;
                    let link = NativeViewLink {
                        source: coordinates,
                        source_digest: String::new(),
                        view_item: if matches!(attempt.status, NativeResponseStatus::Accepted)
                            && lease.view_authorized()
                        {
                            view.map(|(item, _)| items.len() as u64 + item)
                        } else {
                            None
                        },
                        view_digest,
                        role: view.map_or_else(|| "SourceOnly".into(), |(_, role)| role.clone()),
                        source_subrange: None,
                    };
                    ledger.accounting.push(link.clone());
                    if link.view_item.is_some() {
                        active.view_links.push(link);
                    }
                }
                for (ordinal, group) in
                    xai_grok_sampling_types::tool_search::call_id_groups(&discoveries)
                        .into_iter()
                        .enumerate()
                {
                    let members: Vec<_> = group
                        .iter()
                        .map(|i| discovery_coordinates[*i].clone())
                        .collect();
                    let completed = group.iter().any(|i| discoveries[*i].kind() == xai_grok_sampling_types::tool_search::ToolSearchKind::ServerToolUse)
                        && group.iter().any(|i| discoveries[*i].kind() == xai_grok_sampling_types::tool_search::ToolSearchKind::ToolSearchToolResult);
                    let state = if completed {
                        "Completed"
                    } else if matches!(attempt.status, NativeResponseStatus::Accepted)
                        && attempt.source_stop_reason.as_deref() == Some("tool_use")
                    {
                        "LivePending"
                    } else {
                        "AbandonedPending"
                    };
                    abandoned |= state == "AbandonedPending";
                    let key = format!(
                        "{}:{}:{}:{ordinal}",
                        lease.request(),
                        lease.ordinal(),
                        attempt.attempt
                    );
                    ledger.lifecycle.groups.insert(key.clone(), state.into());
                    ledger.groups.push(NativeGroupRecord {
                        key,
                        family: "Messages".into(),
                        execution: "server".into(),
                        origin_ref: origin_ref.clone(),
                        validated_join: discoveries[group[0]].call_id().map(str::to_owned),
                        keyless_server_fifo: false,
                        members,
                        duplicate_observations: Vec::new(),
                        state: state.into(),
                    });
                }
                if let Some(occurrence) = response_occurrence {
                    source.responses.push(NativeResponseBoundary {
                        occurrence,
                        request: lease.request().into(),
                        lease: lease.ordinal(),
                        attempt: attempt.attempt,
                        continuation: 0,
                        provider_response_id: attempt.provider_response_id.clone(),
                        source_stop_reason: attempt.source_stop_reason.clone(),
                        block_start,
                        block_end: source.ordered_blocks.len() as u64,
                        stream_complete: attempt.stream_complete,
                        status: attempt.status.clone(),
                    });
                }
                let terminal_reason = match attempt.status {
                    NativeResponseStatus::PartialTerminal(reason) => Some(reason),
                    _ => abandoned.then_some(NativeUnavailable::Incomplete),
                };
                if let Some(reason) = terminal_reason {
                    ledger
                        .lifecycle
                        .exchanges
                        .insert(lease.request().into(), "Terminal".into());
                    ledger.lifecycle.terminal.insert(
                        format!("{}:{}", lease.request(), attempt.attempt),
                        NativeTerminalMarker {
                            outcome: reason,
                            last_accepted_response: source
                                .responses
                                .iter()
                                .rev()
                                .find(|r| r.status == NativeResponseStatus::Accepted)
                                .map(|r| r.occurrence),
                            observed_bytes: attempt.observed_bytes,
                            failed_block: attempt.failed_block,
                        },
                    );
                } else {
                    ledger.lifecycle.exchanges.insert(
                        lease.request().into(),
                        if attempt.source_stop_reason.as_deref() == Some("tool_use") {
                            "LiveBound"
                        } else {
                            "Terminal"
                        }
                        .into(),
                    );
                }
                ledger.usage.push(NativeResponseAccounting {
                    request: lease.request().into(),
                    lease: lease.ordinal(),
                    attempt: attempt.attempt,
                    response_occurrence,
                    continuation: 0,
                    observed: attempt.usage.clone(),
                    source_stop_reason: attempt.source_stop_reason.clone(),
                    outcome: attempt.status.clone(),
                    latency_ms: attempt.latency_ms,
                    http_dispatches: attempt.http_dispatches,
                });
                ledger.observations.push(NativeObservation {
                    request: lease.request().into(),
                    lease: lease.ordinal(),
                    attempt: attempt.attempt,
                    digest: observation_digest,
                });
            }
            source.source_revision = source
                .source_revision
                .checked_add(1)
                .ok_or(NativeUnavailable::Capacity)?;
            if let Some(previous) = previous {
                source.older_sources.push(previous.record.source.clone());
            }
            let source_ref = add_json(
                &mut blobs,
                &mut blob_kinds,
                "source",
                &source,
                NATIVE_SOURCE_BYTES,
            )?;
            for link in &mut active.view_links[old_link_count..] {
                link.source_digest = source_ref.digest.clone();
            }
            for link in &mut ledger.accounting[old_accounting_count..] {
                link.source_digest = source_ref.digest.clone();
            }
            let active_ref = add_json(
                &mut blobs,
                &mut blob_kinds,
                "active",
                &active,
                NATIVE_ACTIVE_BYTES,
            )?;
            ledger.source_digest = source_ref.digest.clone();
            ledger.active_digest = active_ref.digest.clone();
            let ledger_ref = add_json(
                &mut blobs,
                &mut blob_kinds,
                "ledger",
                &ledger,
                NATIVE_LEDGER_BYTES,
            )?;
            let previous_checkpoint = previous
                .map(|previous| {
                    add_raw_blob(&mut blobs, &mut blob_kinds, "checkpoint", &previous.bytes)
                })
                .transpose()?;
            let record = NativeSearchHistoryV1 {
                schema_version: NATIVE_SCHEMA_VERSION,
                policy_revision: NATIVE_POLICY_REVISION,
                reader_floor: NATIVE_READER_FLOOR,
                limits_revision: NATIVE_LIMITS_REVISION,
                lineage: NativeLineage {
                    id: lease.lineage().into(),
                    source_revision: source.source_revision,
                    source_digest: source_ref.digest.clone(),
                },
                generation,
                high_water: generation,
                source: source_ref,
                active: active_ref,
                ledger: ledger_ref,
                history_boundary: history_boundary(items)?,
                exchange_snapshot: None,
                previous_checkpoint,
                capacity: BTreeMap::new(),
            };
            let bytes = encode_native(&record, NATIVE_CONTROL_BYTES)?;
            let mut checkpoint = Self {
                record,
                source,
                active,
                ledger,
                bytes: bytes.into(),
                blobs,
                blob_kinds,
            };
            checkpoint.seal_capacity()?;
            checkpoint.validate()?;
            Ok(checkpoint)
        }

        pub fn validate(&self) -> Result<(), NativeUnavailable> {
            check_record(&self.record)?;
            self.reservation()?;
            for (resource, count) in [
                (
                    NativeResource::SourceResponses,
                    self.source.responses.len() as u64,
                ),
                (
                    NativeResource::SourceBlocks,
                    self.source.ordered_blocks.len() as u64,
                ),
                (
                    NativeResource::SourceGroups,
                    self.ledger.groups.len() as u64,
                ),
                (
                    NativeResource::LedgerGroups,
                    self.ledger.groups.len() as u64,
                ),
                (
                    NativeResource::LedgerMembers,
                    Self::ledger_members(&self.ledger)?,
                ),
            ] {
                resource.admit(count, 0)?;
            }
            if self.record.capacity != self.capacity()?
                || self.source.schema_version != NATIVE_SCHEMA_VERSION
                || self.source.lineage != self.record.lineage.id
                || self.source.source_revision != self.record.lineage.source_revision
                || self.ledger.source_digest != self.record.source.digest
                || self.ledger.active_digest != self.record.active.digest
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let value = |reference: &NativeBlobRef| -> Result<&[u8], NativeUnavailable> {
                let bytes = self
                    .blobs
                    .get(&reference.digest)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                if bytes.len() as u64 != reference.byte_len
                    || self.blob_kinds.get(&reference.digest) != Some(&reference.kind)
                    || digest(bytes) != reference.digest
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
                Ok(bytes)
            };
            if serde_json::from_slice::<NativeSearchHistoryV1>(&self.bytes)
                .ok()
                .as_ref()
                != Some(&self.record)
                || serde_json::from_slice::<NativeSource>(value(&self.record.source)?)
                    .ok()
                    .as_ref()
                    != Some(&self.source)
                || serde_json::from_slice::<NativeActive>(value(&self.record.active)?)
                    .ok()
                    .as_ref()
                    != Some(&self.active)
                || serde_json::from_slice::<NativeLedger>(value(&self.record.ledger)?)
                    .ok()
                    .as_ref()
                    != Some(&self.ledger)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            for (key, bytes) in &self.blobs {
                if digest(bytes) != *key {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            let block_bytes = |block: &NativeRawBlock| -> Result<u64, NativeUnavailable> {
                let raw = value(&block.block)?;
                serde_json::from_slice::<serde_json::Value>(raw)
                    .map_err(|_| NativeUnavailable::Fidelity)?;
                let partial = block
                    .partial
                    .as_ref()
                    .map(|reference| value(reference).map(|bytes| bytes.len() as u64))
                    .transpose()?
                    .unwrap_or(0);
                let total = NativeResource::BlockBytes.admit(raw.len() as u64, partial)?;
                NativeResource::BlockBytes
                    .admit(total, native_encoded_len(block, NATIVE_BLOCK_BYTES)?)
            };
            let mut exchanges = BTreeMap::new();
            let mut block_end = 0;
            for (ordinal, response) in self.source.responses.iter().enumerate() {
                if response.occurrence != ordinal as u64
                    || response.block_start != block_end
                    || response.block_end < response.block_start
                    || response.block_end as usize > self.source.ordered_blocks.len()
                    || response.status == NativeResponseStatus::Collecting
                    || (response.status == NativeResponseStatus::Accepted
                        && !response.stream_complete)
                {
                    return Err(NativeUnavailable::Fidelity);
                }
                NativeResource::ResponseBlocks
                    .admit(response.block_end - response.block_start, 0)?;
                let mut bytes = native_encoded_len(response, NATIVE_RESPONSE_BYTES)?;
                for (index, block) in self.source.ordered_blocks
                    [response.block_start as usize..response.block_end as usize]
                    .iter()
                    .enumerate()
                {
                    if block.coordinates.lineage != self.source.lineage
                        || block.coordinates.response != ordinal as u64
                        || block.coordinates.block != index as u64
                        || block.coordinates.occurrence != response.block_start + index as u64
                        || block.coordinates.producing_binding != response.request
                        || (response.status == NativeResponseStatus::Accepted
                            && block.state != NativeBlockState::ClosedFaithful)
                    {
                        return Err(NativeUnavailable::Fidelity);
                    }
                    bytes = NativeResource::ResponseBytes.admit(bytes, block_bytes(block)?)?;
                }
                if response.status == NativeResponseStatus::Accepted {
                    let exchange = exchanges.entry(&response.request).or_insert(0);
                    *exchange = NativeResource::ExchangeBytes.admit(*exchange, bytes)?;
                }
                block_end = response.block_end;
            }
            if block_end as usize != self.source.ordered_blocks.len() {
                return Err(NativeUnavailable::InvalidReference);
            }
            for usage in &self.ledger.usage {
                if matches!(usage.outcome, NativeResponseStatus::PartialTerminal(_))
                    && !self
                        .ledger
                        .lifecycle
                        .terminal
                        .contains_key(&format!("{}:{}", usage.request, usage.attempt))
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for binding in &self.source.producing_bindings {
                let descriptor: serde_json::Value =
                    serde_json::from_slice(value(&binding.origin_ref)?)
                        .map_err(|_| NativeUnavailable::InvalidReference)?;
                if descriptor
                    != serde_json::json!({"request":binding.request,"generation":binding.generation,"origin":binding.origin})
                    || binding.generation.is_none()
                        && binding
                            .origin
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            != Some("Unknown")
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            let source_member = |coordinate: &NativeSourceCoordinate| -> Result<&NativeRawBlock, NativeUnavailable> {
                self.source.ordered_blocks.get(coordinate.occurrence as usize)
                    .filter(|block| block.coordinates == *coordinate).ok_or(NativeUnavailable::InvalidReference)
            };
            let mut group_keys = std::collections::BTreeSet::new();
            for group in &self.ledger.groups {
                NativeResource::GroupMembers.admit(group.members.len() as u64, 0)?;
                if !group_keys.insert(&group.key)
                    || self.ledger.lifecycle.groups.get(&group.key) != Some(&group.state)
                    || !matches!(
                        group.state.as_str(),
                        "Completed" | "LivePending" | "AbandonedPending"
                    )
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
                let mut bytes = native_encoded_len(group, NATIVE_GROUP_BYTES)?;
                for member in &group.members {
                    bytes = NativeResource::GroupBytes
                        .admit(bytes, block_bytes(source_member(member)?)?)?;
                }
                for (observation, original) in &group.duplicate_observations {
                    source_member(observation)?;
                    source_member(original)?;
                }
            }
            for waiting in &self.ledger.lifecycle.waiting_clients {
                if waiting.client_id.is_empty() {
                    return Err(NativeUnavailable::InvalidReference);
                }
                source_member(&waiting.source)?;
                if let Some(result) = &waiting.result {
                    source_member(result)?;
                }
            }
            if self.ledger.accounting.len() != self.source.ordered_blocks.len() {
                return Err(NativeUnavailable::InvalidReference);
            }
            let mut admitted_sources = BTreeMap::new();
            for link in &self.ledger.accounting {
                source_member(&link.source)?;
                let admitted = match admitted_sources.entry(link.source_digest.as_str()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let manifest = self
                            .blobs
                            .get(&link.source_digest)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        entry.insert(
                            serde_json::from_slice::<NativeSource>(manifest)
                                .map_err(|_| NativeUnavailable::InvalidReference)?,
                        )
                    }
                };
                if admitted
                    .ordered_blocks
                    .get(link.source.occurrence as usize)
                    .map(|b| &b.coordinates)
                    != Some(&link.source)
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for link in &self.active.view_links {
                if !self.ledger.accounting.contains(link)
                    || link.view_item.is_none()
                    || !link.view_digest.as_ref().is_some_and(|digest| {
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                    || self.source.responses[link.source.response as usize].status
                        != NativeResponseStatus::Accepted
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            if let Some(snapshot_ref) = &self.record.exchange_snapshot {
                let snapshot: NativeSnapshot = serde_json::from_slice(value(snapshot_ref)?)
                    .map_err(|_| NativeUnavailable::InvalidReference)?;
                let body = value(&snapshot.body)?;
                NativeResource::SnapshotPayloadBytes.admit(body.len() as u64, 0)?;
                NativeResource::SnapshotBytes
                    .admit(body.len() as u64, value(snapshot_ref)?.len() as u64)?;
                validate_snapshot_tools(body, snapshot.tools.as_ref())?;
                value(&snapshot.binding)?;
            }
            if let Some(previous) = &self.record.previous_checkpoint {
                let record: NativeSearchHistoryV1 = serde_json::from_slice(value(previous)?)
                    .map_err(|_| NativeUnavailable::InvalidReference)?;
                let old = Self {
                    source: serde_json::from_slice(value(&record.source)?)
                        .map_err(|_| NativeUnavailable::InvalidReference)?,
                    active: serde_json::from_slice(value(&record.active)?)
                        .map_err(|_| NativeUnavailable::InvalidReference)?,
                    ledger: serde_json::from_slice(value(&record.ledger)?)
                        .map_err(|_| NativeUnavailable::InvalidReference)?,
                    record,
                    bytes: value(previous)?.into(),
                    blobs: BTreeMap::new(),
                    blob_kinds: BTreeMap::new(),
                };
                self.extends(&old)?;
            }
            Ok(())
        }
    }
    impl NativeCheckpoint {
        fn role_bytes(&self) -> Result<NativeRoleReservation, NativeUnavailable> {
            Ok(NativeRoleReservation {
                snapshot_bytes: partition_bytes(
                    &self.blobs,
                    &self.blob_kinds,
                    NativeResource::SnapshotBytes,
                )?,
                source_bytes: partition_bytes(
                    &self.blobs,
                    &self.blob_kinds,
                    NativeResource::SourceBytes,
                )?,
                active_bytes: partition_bytes(
                    &self.blobs,
                    &self.blob_kinds,
                    NativeResource::ActiveBytes,
                )?,
                ledger_bytes: partition_bytes(
                    &self.blobs,
                    &self.blob_kinds,
                    NativeResource::LedgerBytes,
                )?,
            })
        }

        fn ledger_members(ledger: &NativeLedger) -> Result<u64, NativeUnavailable> {
            let mut count = 0u64;
            for n in [
                ledger.accounting.len(),
                ledger.usage.len(),
                ledger.observations.len(),
                ledger.transitions.len(),
                ledger.lifecycle.waiting_clients.len(),
                ledger.lifecycle.terminal.len(),
            ] {
                count = count
                    .checked_add(n as u64)
                    .ok_or(NativeUnavailable::Capacity)?;
            }
            for group in &ledger.groups {
                count = count
                    .checked_add(group.duplicate_observations.len() as u64)
                    .ok_or(NativeUnavailable::Capacity)?;
            }
            Ok(count)
        }

        fn terminal_reserve(&self) -> Result<(u64, u64, u64), NativeUnavailable> {
            let pending = self
                .ledger
                .groups
                .iter()
                .any(|group| group.state == "LivePending")
                || self
                    .ledger
                    .lifecycle
                    .exchanges
                    .values()
                    .any(|state| state == "LiveBound")
                || self
                    .ledger
                    .lifecycle
                    .waiting_clients
                    .iter()
                    .any(|waiting| waiting.delivery == "Waiting");
            if !pending {
                return Ok((0, 0, Self::ledger_members(&self.ledger)?));
            }
            self.terminal_copies()
        }

        fn terminal_copies(&self) -> Result<(u64, u64, u64), NativeUnavailable> {
            let mut ledger = self.ledger.clone();
            let mut active = self.active.clone();
            for group in &mut ledger.groups {
                if group.state == "LivePending" {
                    group.state = "AbandonedPending".into();
                    ledger
                        .lifecycle
                        .groups
                        .insert(group.key.clone(), group.state.clone());
                    active.loaded_invalidations.push(group.key.clone());
                }
            }
            for waiting in &mut ledger.lifecycle.waiting_clients {
                if waiting.delivery == "Waiting" {
                    waiting.delivery = "Unavailable".into();
                }
            }
            for usage in &mut ledger.usage {
                if ledger
                    .lifecycle
                    .exchanges
                    .get(&usage.request)
                    .is_some_and(|state| state == "LiveBound")
                {
                    for name in [
                        "input_tokens",
                        "output_tokens",
                        "reasoning_tokens",
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens",
                    ] {
                        usage
                            .observed
                            .insert(name.into(), serde_json::json!(u64::MAX));
                    }
                    usage.observed.insert(
                        "cache_creation".into(),
                        serde_json::json!({
                        "ephemeral_5m_input_tokens":u64::MAX,"ephemeral_1h_input_tokens":u64::MAX}),
                    );
                    usage.outcome =
                        NativeResponseStatus::PartialTerminal(NativeUnavailable::Indeterminate);
                    ledger
                        .lifecycle
                        .terminal
                        .entry(format!("{}:{}", usage.request, usage.attempt))
                        .or_insert(NativeTerminalMarker {
                            outcome: NativeUnavailable::Indeterminate,
                            last_accepted_response: Some(u64::MAX),
                            observed_bytes: u64::MAX,
                            failed_block: Some(u32::MAX),
                        });
                }
            }
            for state in ledger.lifecycle.exchanges.values_mut() {
                if state == "LiveBound" {
                    *state = "Terminal".into();
                }
            }
            Ok((
                native_encoded_len(&active, NATIVE_ACTIVE_BYTES)?,
                native_encoded_len(&ledger, NATIVE_LEDGER_BYTES)?,
                Self::ledger_members(&ledger)?,
            ))
        }

        pub fn reservation(&self) -> Result<NativeGenerationReservation, NativeUnavailable> {
            let mut role = self.role_bytes()?;
            let (active, ledger, members) = self.terminal_reserve()?;
            role.active_bytes = NativeResource::ActiveBytes.admit(role.active_bytes, active)?;
            role.ledger_bytes = NativeResource::LedgerBytes.admit(role.ledger_bytes, ledger)?;
            NativeResource::LedgerMembers.admit(members, 0)?;
            let reservation = NativeGenerationReservation {
                current: role.clone(),
                staging: role.clone(),
                backup: role,
                control_bytes: NATIVE_CONTROL_BYTES,
            };
            reservation.validate()?;
            Ok(reservation)
        }

        fn capacity(&self) -> Result<BTreeMap<String, u64>, NativeUnavailable> {
            let role = self.role_bytes()?;
            let reservation = self.reservation()?;
            let (terminal_active, terminal_ledger, members) = self.terminal_reserve()?;
            let mut control =
                partition_bytes(&self.blobs, &self.blob_kinds, NativeResource::ControlBytes)?;
            control = NativeResource::ControlBytes.admit(control, self.bytes.len() as u64)?;
            Ok(BTreeMap::from([
                ("source_bytes".into(), role.source_bytes),
                (
                    "source_responses".into(),
                    self.source.responses.len() as u64,
                ),
                (
                    "source_blocks".into(),
                    self.source.ordered_blocks.len() as u64,
                ),
                ("source_groups".into(), self.ledger.groups.len() as u64),
                ("active_bytes".into(), role.active_bytes),
                ("ledger_bytes".into(), role.ledger_bytes),
                ("ledger_members".into(), members),
                ("ledger_groups".into(), self.ledger.groups.len() as u64),
                ("snapshot_bytes".into(), role.snapshot_bytes),
                ("terminal_active_bytes".into(), terminal_active),
                ("terminal_ledger_bytes".into(), terminal_ledger),
                ("control_bytes".into(), control),
                (
                    "reserved_current_bytes".into(),
                    reservation.current.bytes()?,
                ),
                (
                    "reserved_staging_bytes".into(),
                    reservation.staging.bytes()?,
                ),
                ("reserved_backup_bytes".into(), reservation.backup.bytes()?),
                ("reserved_control_bytes".into(), reservation.control_bytes),
                ("reserved_lineage_bytes".into(), reservation.validate()?),
            ]))
        }

        pub(crate) fn seal_capacity(&mut self) -> Result<(), NativeUnavailable> {
            for _ in 0..8 {
                let capacity = self.capacity()?;
                if capacity == self.record.capacity {
                    return Ok(());
                }
                self.record.capacity = capacity;
                self.bytes = encode_native(&self.record, NATIVE_CONTROL_BYTES)?.into();
            }
            Err(NativeUnavailable::Fidelity)
        }

        pub fn capture_allocation(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            items: &[ConversationItem],
        ) -> Result<(u64, u64, u64), NativeUnavailable> {
            let terminal = Arc::new(NativeAttemptEvidence {
                attempt: u64::MAX,
                admitted: true,
                response_started: true,
                provider_response_id: Some(serde_json::json!("")),
                source_stop_reason: Some("model_context_window_exceeded".into()),
                stream_complete: false,
                status: NativeResponseStatus::PartialTerminal(NativeUnavailable::Capacity),
                blocks: Vec::new(),
                usage: serde_json::from_value(serde_json::json!({
                    "input_tokens":u64::MAX,"output_tokens":u64::MAX,"thinking_tokens":u64::MAX,
                    "cache_read_input_tokens":u64::MAX,"cache_creation_input_tokens":u64::MAX,
                    "cache_creation":{"ephemeral_5m_input_tokens":u64::MAX,"ephemeral_1h_input_tokens":u64::MAX},
                    "output_tokens_details":{"thinking_tokens":u64::MAX}
                })).map_err(|_| NativeUnavailable::Fidelity)?,
                latency_ms: Some(u64::MAX),
                http_dispatches: u64::MAX,
                observed_bytes: u64::MAX,
                failed_block: Some(u32::MAX),
                view_roles: Vec::new(),
                view_digests: Vec::new(),
            });
            // Count the same next-generation encodings used by the real append.
            let minimum = Self::append(previous, lease, std::slice::from_ref(&terminal), items)?;
            let old_role = previous
                .map(Self::role_bytes)
                .transpose()?
                .unwrap_or_default();
            let new_role = minimum.role_bytes()?;
            let reservation = minimum.capture_reservation()?;
            let source_copy = new_role
                .source_bytes
                .checked_sub(old_role.source_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let active_copy = minimum.record.active.byte_len.max(
                new_role
                    .active_bytes
                    .checked_sub(old_role.active_bytes)
                    .ok_or(NativeUnavailable::Capacity)?,
            );
            let ledger_copy = new_role
                .ledger_bytes
                .checked_sub(old_role.ledger_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let source = NATIVE_SOURCE_BYTES
                .checked_sub(reservation.current.source_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let active = NATIVE_ACTIVE_BYTES
                .checked_sub(reservation.current.active_bytes)
                .and_then(|bytes| {
                    bytes.checked_sub(active_copy.saturating_sub(
                        new_role.active_bytes.saturating_sub(old_role.active_bytes),
                    ))
                })
                .ok_or(NativeUnavailable::Capacity)?;
            let ledger = NATIVE_LEDGER_BYTES
                .checked_sub(reservation.current.ledger_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let control = NATIVE_CONTROL_BYTES
                .checked_sub(minimum.record.capacity["control_bytes"])
                .ok_or(NativeUnavailable::Capacity)?;
            let ceiling = source.min(active).min(ledger).min(control);
            let copy = source_copy
                .max(active_copy)
                .max(ledger_copy)
                .max(minimum.bytes.len() as u64);
            let response = minimum
                .source
                .responses
                .last()
                .ok_or(NativeUnavailable::Fidelity)?;
            let usage = minimum
                .ledger
                .usage
                .last()
                .ok_or(NativeUnavailable::Fidelity)?;
            let marker = minimum
                .ledger
                .lifecycle
                .terminal
                .get(&format!("{}:{}", lease.request(), u64::MAX))
                .ok_or(NativeUnavailable::Fidelity)?;
            let mut minimum_bytes = native_encoded_len(terminal.as_ref(), NATIVE_RESPONSE_BYTES)?;
            for bytes in [
                native_encoded_len(response, NATIVE_RESPONSE_BYTES)?,
                native_encoded_len(usage, NATIVE_RESPONSE_BYTES)?,
                native_encoded_len(marker, NATIVE_RESPONSE_BYTES)?,
            ] {
                minimum_bytes = minimum_bytes
                    .checked_add(bytes)
                    .ok_or(NativeUnavailable::Capacity)?;
            }
            if ceiling < minimum_bytes {
                return Err(NativeUnavailable::Capacity);
            }
            Ok((ceiling, copy, minimum_bytes))
        }

        fn capture_reservation(&self) -> Result<NativeGenerationReservation, NativeUnavailable> {
            let mut role = self.role_bytes()?;
            // Any admitted response can open a mixed handoff, even on a completed lineage.
            let (active, ledger, members) = self.terminal_copies()?;
            role.active_bytes = NativeResource::ActiveBytes.admit(role.active_bytes, active)?;
            role.ledger_bytes = NativeResource::LedgerBytes.admit(role.ledger_bytes, ledger)?;
            NativeResource::LedgerMembers.admit(members, NATIVE_RESPONSE_BLOCKS * 2 + 3)?;
            let reservation = NativeGenerationReservation {
                current: role.clone(),
                staging: role.clone(),
                backup: role,
                control_bytes: NATIVE_CONTROL_BYTES,
            };
            reservation.validate()?;
            Ok(reservation)
        }

        pub fn available_capture_bytes(&self) -> Result<u64, NativeUnavailable> {
            let reservation = self.capture_reservation()?;
            let source = NATIVE_SOURCE_BYTES
                .checked_sub(reservation.current.source_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let ledger = NATIVE_LEDGER_BYTES
                .checked_sub(reservation.current.ledger_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            let active = NATIVE_ACTIVE_BYTES
                .checked_sub(reservation.current.active_bytes)
                .ok_or(NativeUnavailable::Capacity)?;
            Ok(source.min(ledger).min(active))
        }

        pub fn extends(&self, previous: &Self) -> Result<(), NativeUnavailable> {
            if self.record.lineage.id != previous.record.lineage.id
                || self.record.generation
                    != previous
                        .record
                        .generation
                        .checked_add(1)
                        .ok_or(NativeUnavailable::Capacity)?
                || self
                    .record
                    .previous_checkpoint
                    .as_ref()
                    .map(|r| r.digest.as_str())
                    != Some(digest(&previous.bytes).as_str())
                || !self
                    .source
                    .responses
                    .starts_with(&previous.source.responses)
                || !self
                    .source
                    .ordered_blocks
                    .starts_with(&previous.source.ordered_blocks)
                || !self
                    .source
                    .producing_bindings
                    .starts_with(&previous.source.producing_bindings)
                || !self
                    .source
                    .eligible_snapshots
                    .starts_with(&previous.source.eligible_snapshots)
                || !self
                    .ledger
                    .observations
                    .starts_with(&previous.ledger.observations)
                || !self.ledger.usage.starts_with(&previous.ledger.usage)
                || !self
                    .ledger
                    .accounting
                    .starts_with(&previous.ledger.accounting)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            for (key, terminal) in &previous.ledger.lifecycle.terminal {
                if self.ledger.lifecycle.terminal.get(key) != Some(terminal) {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for group in &previous.ledger.groups {
                let current = self
                    .ledger
                    .groups
                    .iter()
                    .find(|current| current.key == group.key)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                if !current.members.starts_with(&group.members)
                    || !current
                        .duplicate_observations
                        .starts_with(&group.duplicate_observations)
                    || matches!(group.state.as_str(), "Completed" | "AbandonedPending")
                        && current.state != group.state
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for (key, disposition) in &previous.active.dispositions {
                let rank = |value: &str| match value {
                    "KEEP" => Some(0),
                    "F" => Some(1),
                    "strip" => Some(2),
                    _ => None,
                };
                if rank(disposition)
                    .zip(
                        self.active
                            .dispositions
                            .get(key)
                            .and_then(|value| rank(value)),
                    )
                    .is_none_or(|(old, new)| new < old)
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            Ok(())
        }
    }

    fn snapshot_tools(body: &[u8]) -> Result<Option<NativeToolsRange>, NativeUnavailable> {
        NativeResource::SnapshotPayloadBytes.admit(body.len() as u64, 0)?;
        let mut at = 0;
        let whitespace = |at: &mut usize| {
            while body.get(*at).is_some_and(u8::is_ascii_whitespace) {
                *at += 1;
            }
        };
        whitespace(&mut at);
        if body.get(at) != Some(&b'{') {
            return Err(NativeUnavailable::Fidelity);
        }
        at += 1;
        whitespace(&mut at);
        let mut tools = None;
        let mut keys = std::collections::BTreeSet::new();
        if body.get(at) != Some(&b'}') {
            loop {
                let mut key =
                    serde_json::Deserializer::from_slice(&body[at..]).into_iter::<String>();
                let name = key
                    .next()
                    .ok_or(NativeUnavailable::Fidelity)?
                    .map_err(|_| NativeUnavailable::Fidelity)?;
                if !keys.insert(name.clone()) {
                    return Err(NativeUnavailable::Fidelity);
                }
                at += key.byte_offset();
                whitespace(&mut at);
                if body.get(at) != Some(&b':') {
                    return Err(NativeUnavailable::Fidelity);
                }
                at += 1;
                whitespace(&mut at);
                let start = at;
                let mut value = serde_json::Deserializer::from_slice(&body[at..])
                    .into_iter::<serde_json::Value>();
                value
                    .next()
                    .ok_or(NativeUnavailable::Fidelity)?
                    .map_err(|_| NativeUnavailable::Fidelity)?;
                at += value.byte_offset();
                if name == "tools" {
                    tools = Some(NativeToolsRange {
                        offset: start as u64,
                        byte_len: (at - start) as u64,
                        digest: digest(&body[start..at]),
                    });
                }
                whitespace(&mut at);
                match body.get(at) {
                    Some(b',') => {
                        at += 1;
                        whitespace(&mut at);
                    }
                    Some(b'}') => break,
                    _ => return Err(NativeUnavailable::Fidelity),
                }
            }
        }
        if body.get(at) != Some(&b'}') {
            return Err(NativeUnavailable::Fidelity);
        }
        at += 1;
        whitespace(&mut at);
        if at != body.len() {
            return Err(NativeUnavailable::Fidelity);
        }
        Ok(tools)
    }

    fn validate_snapshot_tools(
        body: &[u8],
        tools: Option<&NativeToolsRange>,
    ) -> Result<(), NativeUnavailable> {
        if snapshot_tools(body)?.as_ref() != tools {
            return Err(NativeUnavailable::InvalidReference);
        }
        Ok(())
    }

    impl NativeCheckpoint {
        pub fn with_snapshot(
            &self,
            body: &[u8],
            binding: NativeBlobRef,
        ) -> Result<Self, NativeUnavailable> {
            self.validate()?;
            let tools = snapshot_tools(body)?;
            if binding.kind != "producing_binding"
                || self
                    .blobs
                    .get(&binding.digest)
                    .is_none_or(|bytes| bytes.len() as u64 != binding.byte_len)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let mut next = self.clone();
            let body = add_raw_blob(&mut next.blobs, &mut next.blob_kinds, "snapshot_body", body)?;
            let snapshot = NativeSnapshot {
                body,
                tools,
                binding,
            };
            next.record.exchange_snapshot = Some(add_json(
                &mut next.blobs,
                &mut next.blob_kinds,
                "snapshot",
                &snapshot,
                NATIVE_SNAPSHOT_BYTES,
            )?);
            next.record.previous_checkpoint = Some(add_raw_blob(
                &mut next.blobs,
                &mut next.blob_kinds,
                "checkpoint",
                &self.bytes,
            )?);
            next.record.generation = self
                .record
                .generation
                .checked_add(1)
                .ok_or(NativeUnavailable::Capacity)?;
            next.record.high_water = next.record.generation;
            next.seal_capacity()?;
            next.validate()?;
            Ok(next)
        }
    }
}

/// Outcome of a conversation image strip, as acknowledged by the actor.
/// Typed so a dead actor can never masquerade as "stripped nothing".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripOutcome {
    /// Stripped and durably persisted; `stripped` counts the stored
    /// occurrences replaced (a URL stored twice counts twice).
    Applied { stripped: usize },
    /// No stored image matched the requested URLs; nothing changed.
    NoMatch,
    /// Stripped in memory, but the backup or disk write failed, or the
    /// acknowledgement was lost mid-flight. Treated as not persisted: the
    /// stored file may still carry the images and the next load re-poisons.
    WriteFailed { stripped: usize },
    /// The chat-state actor is gone; the strip may not have happened at all.
    ActorUnavailable,
}

// ============================================================================
// Mock (test double) — channel-based, no locks, no atomics
// ============================================================================

/// A record of a persistence call, sent over a channel to the test.
#[derive(Debug, Clone)]
pub enum PersistenceRecord {
    /// A single message was persisted.
    Message(ConversationItem),
    /// A persistence-acknowledged switch append was requested.
    AcknowledgedMessage(ConversationItem),
    /// The full history was replaced.
    ReplaceHistory(Vec<ConversationItem>),
    /// A backup-gated, disk-acknowledged strip rewrite was requested.
    ReplaceHistoryForStrip(Vec<ConversationItem>),
    /// A flush was requested.
    Flush,
    NativeGeneration(std::sync::Arc<native::NativeCheckpoint>),
}

/// Test implementation: sends every call as a [`PersistenceRecord`] over a
/// channel. The test holds the [`MockPersistenceReceiver`] to inspect what
/// the actor did. No locks, no atomics — just message passing.
pub struct MockChatPersistence {
    tx: mpsc::UnboundedSender<PersistenceRecord>,
    /// When set, strip rewrites ack an I/O error instead of success:
    /// pins the honest-failure half of the [`StripOutcome`] contract.
    fail_strip_writes: bool,
    persistence_ack_tx:
        Option<mpsc::UnboundedSender<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>>>,
    persisted_working_directory_switches: Vec<ConversationItem>,
}

/// Receiver side of the mock. Held by the test to drain and inspect records.
pub struct MockPersistenceReceiver {
    rx: mpsc::UnboundedReceiver<PersistenceRecord>,
    persistence_ack_rx: Option<
        mpsc::UnboundedReceiver<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>>,
    >,
}

impl MockChatPersistence {
    /// Create a paired (mock, receiver). Give the mock to the actor, keep the
    /// receiver in the test.
    pub fn new() -> (Self, MockPersistenceReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                fail_strip_writes: false,
                persistence_ack_tx: None,
                persisted_working_directory_switches: Vec::new(),
            },
            MockPersistenceReceiver {
                rx,
                persistence_ack_rx: None,
            },
        )
    }

    /// Create a mock whose strip rewrites fail at "disk": the ack carries
    /// an error, so callers must surface `StripOutcome::WriteFailed`.
    pub fn new_failing_strip_writes() -> (Self, MockPersistenceReceiver) {
        let (mut mock, rx) = Self::new();
        mock.fail_strip_writes = true;
        (mock, rx)
    }

    /// Create a mock whose persistence acknowledgement is test-controlled.
    pub fn new_with_manual_persistence_ack() -> (Self, MockPersistenceReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (persistence_ack_tx, persistence_ack_rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                fail_strip_writes: false,
                persistence_ack_tx: Some(persistence_ack_tx),
                persisted_working_directory_switches: Vec::new(),
            },
            MockPersistenceReceiver {
                rx,
                persistence_ack_rx: Some(persistence_ack_rx),
            },
        )
    }
}

impl MockPersistenceReceiver {
    /// Drain all pending records from the channel.
    pub fn drain(&mut self) -> Vec<PersistenceRecord> {
        let mut records = Vec::new();
        while let Ok(record) = self.rx.try_recv() {
            records.push(record);
        }
        records
    }

    /// Receive the next manual persistence acknowledgement sender.
    pub async fn next_persistence_ack(
        &mut self,
    ) -> Option<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>> {
        match &mut self.persistence_ack_rx {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }

    /// Collect all `Message` items received so far (drains the channel).
    pub fn messages(&mut self) -> Vec<ConversationItem> {
        self.drain()
            .into_iter()
            .filter_map(|r| match r {
                PersistenceRecord::Message(item) => Some(item),
                _ => None,
            })
            .collect()
    }
}

impl ChatPersistence for MockChatPersistence {
    fn read_native_history(
        &mut self,
    ) -> oneshot::Receiver<
        Result<
            native::NativeRead,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Ok(native::NativeRead {
            lineage: "mock".into(),
            checkpoint: None,
        }));
        rx
    }
    fn persist_native_generation_and_ack(
        &mut self,
        write: native::NativeWrite,
    ) -> oneshot::Receiver<
        Result<
            native::NativeAppendAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (tx, rx) = oneshot::channel();
        let result = if self.fail_strip_writes {
            Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence)
        } else {
            self.tx.send(PersistenceRecord::NativeGeneration(write.checkpoint.clone()))
                .map(|_| native::NativeAppendAck::Applied {
                    generation: write.checkpoint.record.generation,
                    digest: native::digest(&write.checkpoint.bytes),
                })
                .map_err(|_| xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence)
        };
        let _ = tx.send(result);
        rx
    }
    fn persist_message(&mut self, item: &ConversationItem) {
        let _ = self.tx.send(PersistenceRecord::Message(item.clone()));
    }

    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>> {
        let (reply, receiver) = oneshot::channel();
        let sent = self
            .tx
            .send(PersistenceRecord::AcknowledgedMessage(item.clone()))
            .map_err(|_| {
                StrictAppendError::NotCommitted(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "mock persistence closed",
                ))
            });
        if let Err(error) = sent {
            let _ = reply.send(Err(error));
        } else if let Some(ack_tx) = &self.persistence_ack_tx {
            let _ = ack_tx.send(reply);
        } else {
            let generation = item.working_directory_switch_generation();
            let acknowledgement = self
                .persisted_working_directory_switches
                .iter()
                .find(|persisted| persisted.working_directory_switch_generation() == generation)
                .cloned()
                .map_or(StrictAppendAck::Appended, StrictAppendAck::AlreadyPresent);
            if matches!(&acknowledgement, StrictAppendAck::Appended) {
                self.persisted_working_directory_switches.push(item.clone());
            }
            let _ = reply.send(Ok(acknowledgement));
        }
        receiver
    }

    fn replace_history(&mut self, items: &[ConversationItem]) {
        let _ = self
            .tx
            .send(PersistenceRecord::ReplaceHistory(items.to_vec()));
    }

    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, receiver) = oneshot::channel();
        let _ = self
            .tx
            .send(PersistenceRecord::ReplaceHistoryForStrip(items.to_vec()));
        let ack = if self.fail_strip_writes {
            Err(io::Error::new(io::ErrorKind::StorageFull, "mock disk full"))
        } else {
            Ok(())
        };
        let _ = reply.send(ack);
        receiver
    }

    fn flush(&mut self) {
        let _ = self.tx.send(PersistenceRecord::Flush);
    }
}

// ============================================================================
// Null (noop) — for benchmarks / scenarios where persistence is unwanted
// ============================================================================

/// No-op implementation: discards everything (for benchmarks / noop scenarios).
pub struct NullChatPersistence;

impl ChatPersistence for NullChatPersistence {
    fn persist_message(&mut self, _item: &ConversationItem) {}
    fn persist_working_directory_switch_and_ack(
        &mut self,
        _item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>> {
        let (reply, receiver) = oneshot::channel();
        let _ = reply.send(Ok(StrictAppendAck::Appended));
        receiver
    }
    fn replace_history(&mut self, _items: &[ConversationItem]) {}
    fn replace_history_for_strip_and_ack(
        &mut self,
        _items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, receiver) = oneshot::channel();
        let _ = reply.send(Ok(()));
        receiver
    }
    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_persistence_records_messages() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        let item = ConversationItem::system("test");
        mock.persist_message(&item);
        let records = rx.drain();
        assert_eq!(records.len(), 1);
        assert!(matches!(&records[0], PersistenceRecord::Message(_)));
    }

    #[test]
    fn mock_persistence_records_multiple_messages() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.persist_message(&ConversationItem::system("a"));
        mock.persist_message(&ConversationItem::user("b"));
        mock.persist_message(&ConversationItem::assistant("c"));
        assert_eq!(rx.messages().len(), 3);
    }

    #[test]
    fn mock_persistence_records_replace_history() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.replace_history(&[ConversationItem::system("a"), ConversationItem::system("b")]);
        let records = rx.drain();
        assert_eq!(records.len(), 1);
        match &records[0] {
            PersistenceRecord::ReplaceHistory(items) => assert_eq!(items.len(), 2),
            other => panic!("expected ReplaceHistory, got {other:?}"),
        }
    }

    #[test]
    fn mock_persistence_records_flush() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.flush();
        mock.flush();
        let records = rx.drain();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|r| matches!(r, PersistenceRecord::Flush))
        );
    }

    #[tokio::test]
    async fn mock_persistence_deduplicates_working_directory_generation() {
        let (mut mock, _rx) = MockChatPersistence::new();
        let first = ConversationItem::working_directory_switch("authoritative", 4);
        assert!(matches!(
            mock.persist_working_directory_switch_and_ack(&first)
                .await
                .unwrap()
                .unwrap(),
            StrictAppendAck::Appended
        ));
        assert!(matches!(
            mock.persist_working_directory_switch_and_ack(
                &ConversationItem::working_directory_switch("retry", 4),
            )
            .await
            .unwrap()
            .unwrap(),
            StrictAppendAck::AlreadyPresent(item) if item.text_content() == "authoritative"
        ));
    }
}
