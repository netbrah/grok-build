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
    fn persist_native_projection_and_ack(
        &mut self,
        _write: native::NativeProjectionWrite,
    ) -> oneshot::Receiver<
        Result<
            xai_grok_sampling_types::conversation::native_search_history::NativeCompoundAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (reply, response) = oneshot::channel();
        let _ = reply.send(Err(xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable::Persistence));
        response
    }
    fn persist_native_checkpoint_and_ack(
        &mut self,
        write: native::NativeWrite,
        _pending: native::NativePendingProtection,
    ) -> oneshot::Receiver<
        Result<
            native::NativeAppendAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        self.persist_native_generation_and_ack(write)
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
        pub identity: NativeIdentityExtension,
        pub bytes: Arc<[u8]>,
        pub blobs: BTreeMap<String, Arc<[u8]>>,
        pub blob_kinds: BTreeMap<String, String>,
        pub compound_proof: Option<Arc<ValidatedCompoundProof>>,
        pub authority_digest: Option<String>,
        pub compound_lineage: bool,
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
        pub issuer: NativeHistoryAuthorityIssuer,
        pub owner: Option<crate::NativeHistoryOwnerToken>,
        pub owner_target: Option<NativeReplayDomain>,
        pub run_active: Option<Arc<std::sync::atomic::AtomicBool>>,
        pub issued: Option<NativeBoundPreparation>,
        pub history_revision: u64,
        pub install_generation: u64,
        pub binding_generation: u64,
        pub next_run: u64,
        pub loaded: NativeLoadedAuthority,
        pub pending_projection: Option<NativePendingProjection>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct NativePendingProjection {
        pub preparation: NativeBoundPreparation,
        pub stage: NativePreparedProjection,
    }

    #[derive(Debug, Clone)]
    pub struct NativeProjectionCompletion {
        pub preparation: NativeBoundPreparation,
        pub certificate: NativeHistoryCommitCertificate,
        pub send_record: NativeSearchHistoryV1,
        pub send_ledger_bytes: Arc<[u8]>,
        pub send_before: Arc<[u8]>,
        pub send_after: Vec<ConversationItem>,
        pub core: NativeHistoryCore,
    }

    #[derive(Debug, Clone)]
    pub struct NativeCompoundProofInput {
        pub commit: NativeCompoundCommit,
        pub authority_bytes: Arc<[u8]>,
        pub metadata_bytes: Arc<[u8]>,
        pub image_bytes: Arc<[u8]>,
    }

    #[derive(Debug, Clone)]
    pub struct ValidatedCompoundProof {
        metadata: NativeProjectionMetadata,
        commit: NativeCompoundCommit,
        image_bytes: Arc<[u8]>,
    }

    impl ValidatedCompoundProof {
        pub fn metadata(&self) -> &NativeProjectionMetadata {
            &self.metadata
        }
        pub fn commit(&self) -> &NativeCompoundCommit {
            &self.commit
        }
    }

    #[derive(Debug, Clone, Default)]
    pub struct NativePendingProtection {
        pub transaction: Option<String>,
        pub dependencies: std::collections::BTreeSet<String>,
        pub temporaries: std::collections::BTreeSet<String>,
    }

    #[derive(Debug, Clone)]
    pub struct NativeProjectionWrite {
        pub commit: NativeCompoundCommit,
        pub authority_bytes: Arc<[u8]>,
        pub checkpoint: Arc<NativeCheckpoint>,
        pub metadata_bytes: Arc<[u8]>,
        pub image_bytes: Arc<[u8]>,
        pub pending: NativePendingProtection,
    }

    #[derive(Debug, Clone)]
    pub struct NativePreparedProjection {
        pub write: NativeProjectionWrite,
        pub send_record: NativeSearchHistoryV1,
        pub send_ledger_bytes: Arc<[u8]>,
        pub send_before: Arc<[u8]>,
        pub send_after: Vec<ConversationItem>,
        pub send_core: NativeHistoryCore,
        pub durable_after: Vec<ConversationItem>,
    }

    pub fn history_jsonl(items: &[ConversationItem]) -> Result<Vec<u8>, NativeUnavailable> {
        let mut bytes = Vec::new();
        for item in items {
            serde_json::to_writer(&mut bytes, item).map_err(|_| NativeUnavailable::Fidelity)?;
            bytes.push(b'\n');
        }
        Ok(bytes)
    }

    pub fn strict_history_jsonl(bytes: &[u8]) -> Result<Vec<ConversationItem>, NativeUnavailable> {
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            return Err(NativeUnavailable::Fidelity);
        }
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        bytes[..bytes.len() - 1]
            .split(|byte| *byte == b'\n')
            .map(|line| serde_json::from_slice(line).map_err(|_| NativeUnavailable::Fidelity))
            .collect()
    }

    impl NativeProjectionWrite {
        pub fn validate(&self) -> Result<(), NativeUnavailable> {
            self.commit.validate()?;
            let metadata: NativeProjectionMetadata =
                serde_json::from_slice(&self.metadata_bytes)
                    .map_err(|_| NativeUnavailable::InvalidReference)?;
            let history = strict_history_jsonl(&self.image_bytes)?;
            let image = self
                .commit
                .after_image
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            let projection = self
                .commit
                .projection
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            if serde_json::from_slice::<NativeCompoundCommit>(&self.authority_bytes)
                .map_err(|_| NativeUnavailable::InvalidReference)?
                != self.commit
                || digest(&self.checkpoint.bytes) != self.commit.checkpoint.digest
                || self.checkpoint.bytes.len() as u64 != self.commit.checkpoint.byte_len
                || digest(&self.metadata_bytes) != projection.digest
                || self.metadata_bytes.len() as u64 != projection.byte_len
                || digest(&self.image_bytes) != image.blob.digest
                || self.image_bytes.len() as u64 != image.blob.byte_len
                || history_boundary(&history)? != image.boundary
                || metadata.input_history != self.commit.before.boundary
                || metadata.post_history != image.boundary
                || self.pending.transaction.as_deref() != Some(self.commit.transaction.as_str())
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            metadata.validate_namespace(
                &self.checkpoint.record,
                &self.checkpoint.ledger,
                &self.checkpoint.identity,
            )?;
            self.checkpoint.validate()
        }
    }

    impl NativeCheckpoint {
        fn core_for_request(
            &self,
            request: &xai_grok_sampling_types::ConversationRequest,
            route: &xai_grok_sampling_types::conversation::projection::TargetRoute,
            domain: &NativeReplayDomain,
        ) -> Result<NativeHistoryCore, NativeUnavailable> {
            use xai_grok_sampling_types::conversation::projection::Boundary;
            let selected: Vec<_> = self
                .source
                .ordered_blocks
                .iter()
                .map(|block| block.coordinates.clone())
                .collect();
            let authorship = self
                .source
                .ordered_blocks
                .iter()
                .map(|block| (block.coordinates.occurrence, NativeIdAuthorship::Provider))
                .collect();
            let history = NativePureHistory {
                record: &self.record,
                source: &self.source,
                ledger: &self.ledger,
                identity: &self.identity,
                blobs: &self.blobs,
                authorship: &authorship,
            };
            prepare_native_history_core(
                &history,
                &NativeProjectionInputs {
                    target_model_id: &domain.model,
                    target: route,
                    tools: &request.tools,
                    current: request.native_eligible_inputs.as_deref(),
                    selected: &selected,
                    origin_domains: &BTreeMap::new(),
                    pending: &NativePendingContext::default(),
                    prior_f: None,
                },
                |group| {
                    (
                        DiscoveryOrigin {
                            boundary: Boundary::Vertex,
                            owner_model_id: None,
                        },
                        NativeQualification {
                            source: None,
                            target: Some(domain.clone()),
                            request: Some(domain.clone()),
                            original_admission: route.admission(),
                            shape: NativeReplayShape {
                                family: if group.family == "Messages" {
                                    NativeReplayFamily::MessagesServer
                                } else {
                                    NativeReplayFamily::ResponsesClient
                                },
                                keyless_server: group.keyless_server_fifo,
                                output_id_present: false,
                                completed: group.state == "Completed",
                                result_error: false,
                                referenced_members: 0,
                            },
                            receipt: None,
                            intact_membership: true,
                            current_backing: false,
                            faithful_identity: false,
                        },
                    )
                },
            )
            .map_err(|refusal| refusal.reason)
        }

        fn native_postimage(
            &self,
            before: &[ConversationItem],
            core: &NativeHistoryCore,
        ) -> Result<(Vec<ConversationItem>, NativeActive, NativeLedger), NativeUnavailable>
        {
            let mut replacements = BTreeMap::new();
            for member in &core.decision.members {
                let link = self
                    .ledger
                    .accounting
                    .iter()
                    .find(|link| link.source == member.source)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                let Some(index) = link.view_item else {
                    continue;
                };
                let index =
                    usize::try_from(index).map_err(|_| NativeUnavailable::InvalidReference)?;
                if before.get(index).is_none() {
                    return Err(NativeUnavailable::StaleHistory);
                }
                let output = if let Some(neutral) = &member.neutral {
                    Some(ConversationItem::assistant(neutral.text.clone()))
                } else if let Some(raw) = &member.raw {
                    Some(ConversationItem::Discovery { item: xai_grok_sampling_types::conversation::tool_search::ToolSearchItem::from_wire(raw.clone()).map_err(|_| NativeUnavailable::Fidelity)? })
                } else if matches!(
                    member.action,
                    NativeHistoryAction::AwaitClient
                        | NativeHistoryAction::AwaitResults
                        | NativeHistoryAction::ContinueOriginal
                ) {
                    Some(before[index].clone())
                } else {
                    None
                };
                if replacements
                    .insert(index, (output, member.action))
                    .is_some()
                    && member.duplicate_of.is_none()
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            let mut positions = Vec::with_capacity(before.len());
            let mut after = Vec::new();
            for (index, item) in before.iter().enumerate() {
                match replacements.get(&index) {
                    Some((None, _)) => positions.push(None),
                    Some((Some(output), _)) => {
                        positions.push(Some(after.len() as u64));
                        after.push(output.clone());
                    }
                    None => {
                        positions.push(Some(after.len() as u64));
                        after.push(item.clone());
                    }
                }
            }
            let mut ledger = self.ledger.clone();
            for link in &mut ledger.accounting {
                let Some(old_index) = link.view_item else {
                    continue;
                };
                let old_index =
                    usize::try_from(old_index).map_err(|_| NativeUnavailable::InvalidReference)?;
                let new_index = *positions
                    .get(old_index)
                    .ok_or(NativeUnavailable::StaleHistory)?;
                link.view_item = new_index;
                link.view_digest = new_index
                    .map(|index| {
                        history_boundary(&after[index as usize..index as usize + 1])
                            .map(|boundary| boundary.digest)
                    })
                    .transpose()?;
                if replacements
                    .get(&old_index)
                    .is_some_and(|(_, action)| *action == NativeHistoryAction::F)
                {
                    link.role = "ProjectedText".into();
                }
                if new_index.is_none() {
                    link.role = "SourceOnly".into();
                }
            }
            let mut active = self.active.clone();
            active.projection_revision = active
                .projection_revision
                .checked_add(1)
                .ok_or(NativeUnavailable::Capacity)?;
            active.view_links = self
                .active
                .view_links
                .iter()
                .map(|old| {
                    ledger
                        .accounting
                        .iter()
                        .find(|new| new.source == old.source)
                        .cloned()
                        .ok_or(NativeUnavailable::InvalidReference)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|link| link.view_item.is_some())
                .collect();
            for group in &core.decision.groups {
                let disposition = match group.action {
                    NativeHistoryAction::Keep => Some("KEEP"),
                    NativeHistoryAction::F => Some("F"),
                    NativeHistoryAction::Strip => Some("strip"),
                    _ => None,
                };
                if let Some(disposition) = disposition {
                    active
                        .dispositions
                        .insert(group.key.clone(), disposition.into());
                    if disposition != "KEEP" && !active.loaded_invalidations.contains(&group.key) {
                        active.loaded_invalidations.push(group.key.clone());
                    }
                }
            }
            Ok((after, active, ledger))
        }

        fn replace_partitions(
            &mut self,
            active: NativeActive,
            mut ledger: NativeLedger,
            history: &[ConversationItem],
        ) -> Result<(), NativeUnavailable> {
            self.record.active = add_json(
                &mut self.blobs,
                &mut self.blob_kinds,
                "active",
                &active,
                NATIVE_ACTIVE_BYTES,
            )?;
            ledger.active_digest = self.record.active.digest.clone();
            self.record.ledger = add_blob(
                &mut self.blobs,
                &mut self.blob_kinds,
                "ledger",
                ledger_encoding(&ledger, &self.identity)?,
            )?;
            self.active = active;
            self.ledger = ledger;
            self.record.history_boundary = history_boundary(history)?;
            self.bytes = encode_native(&self.record, NATIVE_CONTROL_BYTES)?.into();
            self.seal_capacity()?;
            Ok(())
        }
    }

    impl NativeCheckpoint {
        fn verify_compound_policy(
            &self,
            previous: &Self,
            metadata: &NativeProjectionMetadata,
            post: &[ConversationItem],
        ) -> Result<(), NativeUnavailable> {
            use xai_grok_sampling_types::conversation::projection::{TargetRoute, model_boundary_class};
            let mut record = previous.record.clone();
            record.history_boundary = metadata.input_history.clone();
            let selected: Vec<_> = previous
                .source
                .ordered_blocks
                .iter()
                .filter(|block| {
                    !metadata
                        .groups
                        .iter()
                        .filter(|transition| transition.to == "KEEP")
                        .any(|transition| {
                            previous.ledger.groups[transition.key as usize]
                                .members
                                .contains(&block.coordinates)
                        })
                })
                .map(|block| block.coordinates.clone())
                .collect();
            let authorship = previous
                .source
                .ordered_blocks
                .iter()
                .map(|block| (block.coordinates.occurrence, NativeIdAuthorship::Provider))
                .collect();
            let history = NativePureHistory {
                record: &record,
                source: &previous.source,
                ledger: &previous.ledger,
                identity: &previous.identity,
                blobs: &self.blobs,
                authorship: &authorship,
            };
            let admission = xai_grok_sampling_types::SearchAdmission {
                supports_search_tool: metadata.original_admission.supports_search_tool,
                has_searchable_tools: metadata.original_admission.has_searchable_tools,
            };
            let domain = metadata.target.domain();
            let route = TargetRoute::new(
                domain.backend.clone(),
                model_boundary_class(&domain.model),
                admission,
            );
            let checked = prepare_native_history_core(
                &history,
                &NativeProjectionInputs {
                    target_model_id: &domain.model,
                    target: &route,
                    tools: &[],
                    current: None,
                    selected: &selected,
                    origin_domains: &BTreeMap::new(),
                    pending: &NativePendingContext::default(),
                    prior_f: None,
                },
                |group| {
                    (
                        DiscoveryOrigin {
                            boundary: route.boundary(),
                            owner_model_id: None,
                        },
                        NativeQualification {
                            source: None,
                            target: Some(domain.clone()),
                            request: Some(domain.clone()),
                            original_admission: admission,
                            shape: NativeReplayShape {
                                family: if group.family == "Messages" {
                                    NativeReplayFamily::MessagesServer
                                } else {
                                    NativeReplayFamily::ResponsesClient
                                },
                                keyless_server: group.keyless_server_fifo,
                                output_id_present: false,
                                completed: group.state == "Completed",
                                result_error: false,
                                referenced_members: 0,
                            },
                            receipt: None,
                            intact_membership: true,
                            current_backing: false,
                            faithful_identity: false,
                        },
                    )
                },
            )
            .map_err(|reason| reason.reason)?;
            for transition in &metadata.groups {
                let group = &self.ledger.groups[transition.key as usize];
                let qualification = metadata
                    .qualification
                    .iter()
                    .find(|qualification| qualification.group == transition.key)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                let shape = checked
                    .accounting
                    .qualification
                    .iter()
                    .find(|qualification| qualification.group == group.key)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                if qualification.shape.shape()? != shape.shape {
                    return Err(NativeUnavailable::InvalidReference);
                }
                if transition.to == "KEEP" {
                    if !admission.admitted()
                        || qualification.source.as_ref() != Some(&qualification.target)
                        || qualification.receipt.as_ref().is_none_or(|receipt| {
                            receipt.target != qualification.target
                                || receipt.shape != qualification.shape
                                || receipt.receipt_id.is_empty()
                        })
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    let origin = self
                        .source
                        .producing_bindings
                        .iter()
                        .find(|binding| binding.origin_ref == group.origin_ref)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    if origin
                        .origin
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        != Some("Known")
                    {
                        return Err(NativeUnavailable::UnknownIdentity);
                    }
                    for source in &group.members {
                        let link = self
                            .ledger
                            .accounting
                            .iter()
                            .find(|link| &link.source == source)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        if let Some(index) = link.view_item {
                            let original = self
                                .source
                                .ordered_blocks
                                .iter()
                                .find(|block| &block.coordinates == source)
                                .ok_or(NativeUnavailable::InvalidReference)?;
                            let raw: serde_json::Value = serde_json::from_slice(
                                self.blobs
                                    .get(&original.block.digest)
                                    .ok_or(NativeUnavailable::InvalidReference)?,
                            )
                            .map_err(|_| NativeUnavailable::Fidelity)?;
                            if !matches!(post.get(index as usize), Some(ConversationItem::Discovery { item }) if item.raw() == &raw)
                            {
                                return Err(NativeUnavailable::Fidelity);
                            }
                        }
                    }
                } else {
                    let expected = checked
                        .decision
                        .groups
                        .iter()
                        .find(|expected| expected.key == group.key)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    let to = match expected.action {
                        NativeHistoryAction::F => "F",
                        NativeHistoryAction::Strip => "strip",
                        _ => return Err(NativeUnavailable::InvalidReference),
                    };
                    if transition.to != to {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    for expected in checked
                        .decision
                        .members
                        .iter()
                        .filter(|member| group.members.contains(&member.source))
                    {
                        let link = self
                            .ledger
                            .accounting
                            .iter()
                            .find(|link| link.source == expected.source)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        match (link.view_item, expected.neutral.as_ref(), expected.action) {
                            (Some(index), Some(neutral), NativeHistoryAction::F)
                                if expected.duplicate_of.is_none() =>
                            {
                                if !matches!(post.get(index as usize), Some(ConversationItem::Assistant(item)) if item.content.as_ref() == neutral.text && item.tool_calls.is_empty())
                                {
                                    return Err(NativeUnavailable::Fidelity);
                                }
                            }
                            (None, _, _) => {}
                            _ => return Err(NativeUnavailable::InvalidReference),
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn projection_metadata(
        preparation: &NativeBoundPreparation,
        previous: &NativeCheckpoint,
        checkpoint: &NativeCheckpoint,
        core: &NativeHistoryCore,
        post: &[ConversationItem],
    ) -> Result<NativeProjectionMetadata, NativeUnavailable> {
        let context = preparation.context().ok_or(NativeUnavailable::Incomplete)?;
        let group_ordinal = |key: &str| -> Result<u32, NativeUnavailable> {
            checkpoint
                .ledger
                .groups
                .iter()
                .position(|group| group.key == key)
                .ok_or(NativeUnavailable::InvalidReference)
                .and_then(|index| u32::try_from(index).map_err(|_| NativeUnavailable::Capacity))
        };
        let mut members = Vec::new();
        for (index, link) in checkpoint.ledger.accounting.iter().enumerate() {
            let facts = core
                .accounting
                .raw_members
                .iter()
                .find(|member| member.source == link.source)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let group = facts.group.as_deref().map(group_ordinal).transpose()?;
            let output_items: Vec<_> = link.view_item.into_iter().collect();
            let output_digests = output_items
                .iter()
                .map(|index| {
                    let item = post
                        .get(*index as usize)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    Ok(digest(
                        &serde_json::to_vec(item).map_err(|_| NativeUnavailable::Fidelity)?,
                    ))
                })
                .collect::<Result<Vec<_>, NativeUnavailable>>()?;
            members.push(NativeProjectionMember {
                source: u32::try_from(index).map_err(|_| NativeUnavailable::Capacity)?,
                source_digest: facts.source_digest.clone(),
                group,
                action: format!("{:?}", facts.action),
                output_items,
                output_digests,
                loss: facts.lost,
            });
            if let NativeIdentityExtension::Revision2 {
                eligible_member_accounting,
                ..
            } = &checkpoint.identity
            {
                for (reference_index, reference) in eligible_member_accounting.iter().enumerate() {
                    let mut parent = reference.source.clone();
                    parent.submember = None;
                    if parent != link.source {
                        continue;
                    }
                    let facts = core
                        .accounting
                        .references
                        .iter()
                        .find(|member| member.source == reference.source)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    let output_items: Vec<_> = if facts.action == NativeHistoryAction::Keep {
                        link.view_item.into_iter().collect()
                    } else {
                        Vec::new()
                    };
                    let output_digests = output_items
                        .iter()
                        .map(|index| {
                            let item = post
                                .get(*index as usize)
                                .ok_or(NativeUnavailable::InvalidReference)?;
                            Ok(digest(
                                &serde_json::to_vec(item)
                                    .map_err(|_| NativeUnavailable::Fidelity)?,
                            ))
                        })
                        .collect::<Result<Vec<_>, NativeUnavailable>>()?;
                    members.push(NativeProjectionMember {
                        source: u32::try_from(checkpoint.ledger.accounting.len() + reference_index)
                            .map_err(|_| NativeUnavailable::Capacity)?,
                        source_digest: facts.source_digest.clone(),
                        group: facts.group.as_deref().map(group_ordinal).transpose()?,
                        action: format!("{:?}", facts.action),
                        output_items,
                        output_digests,
                        loss: facts.lost,
                    });
                }
            }
        }
        let reason = |reason: NativeHistoryReason| match reason {
            NativeHistoryReason::Kept => "kept",
            NativeHistoryReason::Unqualified => "unqualified",
            NativeHistoryReason::MissingView => "missing_view",
            NativeHistoryReason::PriorDisposition => "prior_disposition",
            NativeHistoryReason::FBudget => "f_budget",
            NativeHistoryReason::Pending => "pending",
            NativeHistoryReason::Abandoned => "abandoned",
        };
        let groups = core
            .decision
            .groups
            .iter()
            .filter_map(|group| {
                let to = match group.action {
                    NativeHistoryAction::Keep => "KEEP",
                    NativeHistoryAction::F => "F",
                    NativeHistoryAction::Strip => "strip",
                    _ => return None,
                };
                Some((group, to))
            })
            .map(|(group, to)| {
                let facts = core
                    .accounting
                    .raw_members
                    .iter()
                    .find(|member| member.group.as_deref() == Some(group.key.as_str()))
                    .ok_or(NativeUnavailable::InvalidReference)?;
                Ok(NativeProjectionGroup {
                    key: group_ordinal(&group.key)?,
                    from: previous
                        .active
                        .dispositions
                        .get(&group.key)
                        .map_or("Unrecorded", String::as_str)
                        .into(),
                    to: to.into(),
                    reason: reason(facts.reason).into(),
                })
            })
            .collect::<Result<Vec<_>, NativeUnavailable>>()?;
        let qualification = core
            .accounting
            .qualification
            .iter()
            .map(|qualification| {
                Ok(NativeProjectionQualification {
                    group: group_ordinal(&qualification.group)?,
                    source: qualification
                        .source
                        .as_ref()
                        .map(NativeProjectionDomain::from),
                    target: NativeProjectionDomain::from(&qualification.target),
                    shape: NativeProjectionShape::from_shape(&qualification.shape),
                    receipt: qualification.receipt.as_ref().map(|receipt| {
                        NativeProjectionReceipt {
                            target: NativeProjectionDomain::from(&receipt.target),
                            shape: NativeProjectionShape::from_shape(&receipt.shape),
                            receipt_id: receipt.receipt_id.clone(),
                        }
                    }),
                })
            })
            .collect::<Result<Vec<_>, NativeUnavailable>>()?;
        let raw_ordinal = |source: &NativeSourceCoordinate| -> Result<u32, NativeUnavailable> {
            checkpoint
                .ledger
                .accounting
                .iter()
                .position(|member| member.source == *source)
                .ok_or(NativeUnavailable::InvalidReference)
                .and_then(|index| u32::try_from(index).map_err(|_| NativeUnavailable::Capacity))
        };
        let f_provenance = NativeProjectionFProvenance {
            version: 1,
            groups: core
                .decision
                .f_provenance
                .groups
                .iter()
                .map(|group| {
                    Ok(NativeProjectionFGroup {
                        key: group_ordinal(&group.key)?,
                        call: raw_ordinal(&group.call)?,
                        call_digest: group.call_digest.clone(),
                        result: raw_ordinal(&group.result)?,
                        result_digest: group.result_digest.clone(),
                        disposition: match group.disposition {
                            NativeFDisposition::F => "F",
                            NativeFDisposition::Strip => "Strip",
                        }
                        .into(),
                    })
                })
                .collect::<Result<Vec<_>, NativeUnavailable>>()?,
        };
        let metadata = NativeProjectionMetadata {
            record_kind: "native_projection_metadata".into(),
            format_version: 1,
            owner_class: match preparation.grant() {
                NativePreparationGrant::OwningPrimary => NativeProjectionOwner::OwningPrimary,
                NativePreparationGrant::SwitchStore => NativeProjectionOwner::SwitchStore,
                NativePreparationGrant::SendOnly => return Err(NativeUnavailable::Incomplete),
            },
            issued_request: preparation.issued_request().into(),
            binding_generation: context.capture().binding_generation,
            original_admission: NativeProjectionAdmission {
                supports_search_tool: context.capture().original_admission.supports_search_tool,
                has_searchable_tools: context.capture().original_admission.has_searchable_tools,
            },
            target: NativeProjectionDomain::from(&context.capture().target),
            versions: NativeProjectionVersions {
                schema: checkpoint.record.schema_version,
                policy: 1,
                projection: 1,
                reader: checkpoint.record.reader_floor,
                limits: checkpoint.record.limits_revision,
            },
            qualification,
            input_history: preparation.history().clone(),
            post_history: checkpoint.record.history_boundary.clone(),
            groups,
            members,
            f_provenance,
            loaded_invalidations: checkpoint
                .active
                .loaded_invalidations
                .iter()
                .map(|key| group_ordinal(key))
                .collect::<Result<Vec<_>, _>>()?,
        };
        metadata.validate_namespace(
            &checkpoint.record,
            &checkpoint.ledger,
            &checkpoint.identity,
        )?;
        Ok(metadata)
    }

    impl NativeCheckpoint {
        fn projection_extends(&self, previous: &Self) -> Result<(), NativeUnavailable> {
            let proof = self
                .compound_proof
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            let metadata = &proof.metadata;
            metadata.validate_namespace(&self.record, &self.ledger, &self.identity)?;
            if self.source != previous.source
                || self.identity != previous.identity
                || self.record.source != previous.record.source
                || self.ledger.groups != previous.ledger.groups
                || self.ledger.lifecycle != previous.ledger.lifecycle
                || self.ledger.usage != previous.ledger.usage
                || self.ledger.observations != previous.ledger.observations
                || self.ledger.transitions != previous.ledger.transitions
                || self.ledger.accounting.len() != previous.ledger.accounting.len()
                || self.record.previous_checkpoint != proof.commit.previous_checkpoint
                || self.record.generation != proof.commit.high_water
                || metadata.input_history != proof.commit.before.boundary
                || proof
                    .commit
                    .after_image
                    .as_ref()
                    .map(|image| &image.boundary)
                    != Some(&self.record.history_boundary)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let post = strict_history_jsonl(&proof.image_bytes)?;
            self.verify_compound_policy(previous, metadata, &post)?;
            if history_boundary(&post)? != self.record.history_boundary {
                return Err(NativeUnavailable::InvalidReference);
            }
            for (before, after) in previous
                .ledger
                .accounting
                .iter()
                .zip(&self.ledger.accounting)
            {
                if before.source != after.source
                    || before.source_digest != after.source_digest
                    || before.source_subrange != after.source_subrange
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for transition in &metadata.groups {
                let group = self
                    .ledger
                    .groups
                    .get(transition.key as usize)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                if transition.from
                    != previous
                        .active
                        .dispositions
                        .get(&group.key)
                        .map_or("Unrecorded", String::as_str)
                    || self.active.dispositions.get(&group.key) != Some(&transition.to)
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            for group in &metadata.f_provenance.groups {
                for (ordinal, expected) in [
                    (group.call, &group.call_digest),
                    (group.result, &group.result_digest),
                ] {
                    let member = self
                        .ledger
                        .accounting
                        .get(ordinal as usize)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    let raw = self
                        .source
                        .ordered_blocks
                        .iter()
                        .find(|raw| raw.coordinates == member.source)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    if &raw.block.digest != expected {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                }
            }
            for member in &metadata.members {
                if member.source as usize >= self.ledger.accounting.len() {
                    continue;
                }
                let link = &self.ledger.accounting[member.source as usize];
                if member.output_items != link.view_item.into_iter().collect::<Vec<_>>() {
                    return Err(NativeUnavailable::InvalidReference);
                }
                for (index, expected) in member.output_items.iter().zip(&member.output_digests) {
                    let item = post
                        .get(*index as usize)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    if &digest(&serde_json::to_vec(item).map_err(|_| NativeUnavailable::Fidelity)?)
                        != expected
                        || link.view_digest.as_ref()
                            != Some(&history_boundary(std::slice::from_ref(item))?.digest)
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                }
            }
            let expected_invalidations: Vec<_> = metadata
                .loaded_invalidations
                .iter()
                .map(|index| {
                    self.ledger
                        .groups
                        .get(*index as usize)
                        .map(|group| group.key.clone())
                        .ok_or(NativeUnavailable::InvalidReference)
                })
                .collect::<Result<_, _>>()?;
            if expected_invalidations != self.active.loaded_invalidations {
                return Err(NativeUnavailable::InvalidReference);
            }
            Ok(())
        }

        fn install_compound_proof(
            &mut self,
            input: NativeCompoundProofInput,
        ) -> Result<(), NativeUnavailable> {
            input.commit.validate()?;
            let image = input
                .commit
                .after_image
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            let metadata_ref = input
                .commit
                .projection
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            let metadata: NativeProjectionMetadata = serde_json::from_slice(&input.metadata_bytes)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            if input.commit.operation != NativeCompoundOperation::Projection
                || serde_json::from_slice::<NativeCompoundCommit>(&input.authority_bytes)
                    .map_err(|_| NativeUnavailable::InvalidReference)?
                    != input.commit
                || digest(&self.bytes) != input.commit.checkpoint.digest
                || self.bytes.len() as u64 != input.commit.checkpoint.byte_len
                || self.record.lineage.id != input.commit.lineage
                || self.record.high_water != input.commit.high_water
                || self.record.previous_checkpoint != input.commit.previous_checkpoint
                || digest(&input.metadata_bytes) != metadata_ref.digest
                || input.metadata_bytes.len() as u64 != metadata_ref.byte_len
                || digest(&input.image_bytes) != image.blob.digest
                || input.image_bytes.len() as u64 != image.blob.byte_len
                || history_boundary(&strict_history_jsonl(&input.image_bytes)?)? != image.boundary
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            self.authority_digest = Some(digest(&input.authority_bytes));
            self.compound_lineage = true;
            self.compound_proof = Some(Arc::new(ValidatedCompoundProof {
                metadata,
                commit: input.commit,
                image_bytes: input.image_bytes,
            }));
            Ok(())
        }
    }

    impl NativeCheckpoint {
        pub fn prepare_projection(
            &self,
            preparation: &NativeBoundPreparation,
            request: &xai_grok_sampling_types::ConversationRequest,
            route: &xai_grok_sampling_types::conversation::projection::TargetRoute,
        ) -> Result<NativePreparedProjection, NativeUnavailable> {
            if !self.is_empty_projection_input() {
                self.validate()?;
            }
            let context = preparation.context().ok_or(NativeUnavailable::Incomplete)?;
            if preparation.grant() == NativePreparationGrant::SendOnly
                || !preparation
                    .seal()
                    .is_some_and(NativeHistoryIssuedSeal::is_live)
            {
                return Err(NativeUnavailable::Incomplete);
            }
            let durable: Vec<ConversationItem> = serde_json::from_slice(
                preparation
                    .durable_history_bytes()
                    .ok_or(NativeUnavailable::Incomplete)?,
            )
            .map_err(|_| NativeUnavailable::Fidelity)?;
            if history_boundary(&durable)? != *preparation.history()
                || history_boundary(&request.items)? != *preparation.mapping().send()
            {
                return Err(NativeUnavailable::StaleHistory);
            }
            let mut durable_view = self.clone();
            let mut policy_input = durable.clone();
            if let Some(switch) = preparation.switch_projection() {
                let ordinary = switch.project(&durable);
                let drops: std::collections::BTreeSet<_> =
                    ordinary.drops.iter().map(|drop| drop.index).collect();
                if drops.len() != ordinary.drops.len()
                    || ordinary.items.len() + drops.len() != durable.len()
                    || drops.iter().any(|index| *index >= durable.len())
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
                let mut positions = Vec::with_capacity(durable.len());
                let mut survivor = 0u64;
                for index in 0..durable.len() {
                    if drops.contains(&index) {
                        positions.push(None);
                    } else {
                        positions.push(Some(survivor));
                        survivor += 1;
                    }
                }
                let mut ledger = durable_view.ledger.clone();
                for link in &mut ledger.accounting {
                    if let Some(index) = link.view_item {
                        let new = *positions
                            .get(index as usize)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        link.view_item = new;
                        link.view_digest = new
                            .map(|index| {
                                history_boundary(
                                    &ordinary.items[index as usize..index as usize + 1],
                                )
                                .map(|boundary| boundary.digest)
                            })
                            .transpose()?;
                        if new.is_none() {
                            link.role = "SourceOnly".into();
                        }
                    }
                }
                let mut active = durable_view.active.clone();
                active.view_links = active
                    .view_links
                    .iter()
                    .map(|old| {
                        ledger
                            .accounting
                            .iter()
                            .find(|new| new.source == old.source)
                            .cloned()
                            .ok_or(NativeUnavailable::InvalidReference)
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|link| link.view_item.is_some())
                    .collect();
                policy_input = ordinary.items;
                durable_view.replace_partitions(active, ledger, &policy_input)?;
            } else {
                durable_view.record.history_boundary = preparation.history().clone();
            }
            let core = durable_view.core_for_request(request, route, &context.capture().target)?;
            let (durable_after, active, ledger) =
                durable_view.native_postimage(&policy_input, &core)?;
            let mut checkpoint = self.clone();
            if self.is_empty_projection_input() {
                // Synthetic genesis inputs have no retained native predecessor.
                for digest in [&self.record.active.digest, &self.record.ledger.digest] {
                    checkpoint.blobs.remove(digest);
                    checkpoint.blob_kinds.remove(digest);
                }
            }
            checkpoint.record.generation = checkpoint
                .record
                .generation
                .checked_add(1)
                .ok_or(NativeUnavailable::Capacity)?;
            checkpoint.record.high_water = checkpoint.record.generation;
            checkpoint.record.previous_checkpoint = if self.record.generation == 0 {
                None
            } else {
                Some(add_raw_blob(
                    &mut checkpoint.blobs,
                    &mut checkpoint.blob_kinds,
                    "checkpoint",
                    &self.bytes,
                )?)
            };
            checkpoint.replace_partitions(active, ledger, &durable_after)?;
            let metadata =
                projection_metadata(preparation, self, &checkpoint, &core, &durable_after)?;
            let metadata_bytes: Arc<[u8]> = encode_native(&metadata, NATIVE_LEDGER_BYTES)?.into();
            let image_bytes: Arc<[u8]> = history_jsonl(&durable_after)?.into();
            NativeResource::CurrentBytes.admit(image_bytes.len() as u64, 0)?;
            let before_bytes = history_jsonl(&durable)?;
            let previous_authority_digest = if self.record.generation == 0 {
                None
            } else {
                Some(
                    self.authority_digest
                        .clone()
                        .ok_or(NativeUnavailable::Incomplete)?,
                )
            };
            let mut commit = NativeCompoundCommit {
                record_kind: "native_compound_commit".into(),
                schema_version: 3,
                policy_revision: 1,
                reader_floor: 3,
                limits_revision: 2,
                lineage: checkpoint.record.lineage.id.clone(),
                high_water: checkpoint.record.high_water,
                checkpoint: NativeBlobRef {
                    kind: "checkpoint".into(),
                    byte_len: checkpoint.bytes.len() as u64,
                    digest: digest(&checkpoint.bytes),
                },
                transaction: String::new(),
                operation: NativeCompoundOperation::Projection,
                previous_generation: self.record.generation,
                previous_checkpoint: checkpoint.record.previous_checkpoint.clone(),
                previous_authority_digest,
                before: NativeHistoryBefore {
                    boundary: preparation.history().clone(),
                    byte_len: before_bytes.len() as u64,
                    raw_digest: digest(&before_bytes),
                },
                after_image: Some(NativeHistoryAfterImage {
                    format_version: 1,
                    blob: NativeBlobRef {
                        kind: "ordinary_history_jsonl".into(),
                        byte_len: image_bytes.len() as u64,
                        digest: digest(&image_bytes),
                    },
                    boundary: checkpoint.record.history_boundary.clone(),
                }),
                projection: Some(NativeBlobRef {
                    kind: "projection_metadata".into(),
                    byte_len: metadata_bytes.len() as u64,
                    digest: digest(&metadata_bytes),
                }),
            };
            commit.transaction = commit.projection_transaction()?;
            commit.validate()?;
            let authority_bytes: Arc<[u8]> = encode_native(&commit, NATIVE_CONTROL_BYTES)?.into();
            checkpoint.compound_proof = Some(Arc::new(ValidatedCompoundProof {
                metadata,
                commit: commit.clone(),
                image_bytes: image_bytes.clone(),
            }));
            checkpoint.authority_digest = Some(digest(&authority_bytes));
            checkpoint.compound_lineage = true;
            checkpoint.validate()?;
            let mut send_view = self.clone();
            let mut ledger = send_view.ledger.clone();
            for link in &mut ledger.accounting {
                if let Some(index) = link.view_item {
                    let mapped = preparation
                        .mapping()
                        .send_for_durable(
                            u32::try_from(index)
                                .map_err(|_| NativeUnavailable::InvalidReference)?,
                        )
                        .ok_or(NativeUnavailable::Incomplete)?;
                    link.view_item = Some(mapped as u64);
                    link.view_digest = Some(digest(
                        &serde_json::to_vec(
                            request
                                .items
                                .get(mapped as usize)
                                .ok_or(NativeUnavailable::Incomplete)?,
                        )
                        .map_err(|_| NativeUnavailable::Fidelity)?,
                    ));
                }
            }
            let mut active = send_view.active.clone();
            active.view_links = active
                .view_links
                .iter()
                .map(|old| {
                    ledger
                        .accounting
                        .iter()
                        .find(|link| link.source == old.source)
                        .cloned()
                        .ok_or(NativeUnavailable::InvalidReference)
                })
                .collect::<Result<Vec<_>, _>>()?;
            send_view.replace_partitions(active, ledger, &request.items)?;
            let send_core = if preparation.grant() == NativePreparationGrant::SwitchStore {
                core.clone()
            } else {
                send_view.core_for_request(request, route, &context.capture().target)?
            };
            if preparation.grant() != NativePreparationGrant::SwitchStore
                && send_core.decision != core.decision
            {
                return Err(NativeUnavailable::Fidelity);
            }
            let send_after = if preparation.grant() == NativePreparationGrant::SwitchStore {
                durable_after.clone()
            } else {
                send_view.native_postimage(&request.items, &send_core)?.0
            };
            let mut pending = NativePendingProtection {
                transaction: Some(commit.transaction.clone()),
                ..Default::default()
            };
            pending
                .dependencies
                .extend(checkpoint.blobs.keys().cloned());
            pending.dependencies.extend([
                digest(&checkpoint.bytes),
                digest(&metadata_bytes),
                digest(&image_bytes),
                digest(&authority_bytes),
            ]);
            let write = NativeProjectionWrite {
                commit,
                authority_bytes,
                checkpoint: Arc::new(checkpoint),
                metadata_bytes,
                image_bytes,
                pending,
            };
            write.validate()?;
            Ok(NativePreparedProjection {
                write,
                send_record: send_view.record.clone(),
                send_ledger_bytes: send_view.blobs[&send_view.record.ledger.digest].clone(),
                send_before: serde_json::to_vec(&request.items)
                    .map_err(|_| NativeUnavailable::Fidelity)?
                    .into(),
                send_after,
                send_core,
                durable_after,
            })
        }
    }

    impl NativeCheckpoint {
        pub fn empty_projection_input(
            lineage: String,
            history: &[ConversationItem],
        ) -> Result<Self, NativeUnavailable> {
            let source = NativeSource {
                schema_version: 1,
                lineage: lineage.clone(),
                source_revision: 1,
                producing_bindings: Vec::new(),
                responses: Vec::new(),
                ordered_blocks: Vec::new(),
                eligible_snapshots: Vec::new(),
                older_sources: Vec::new(),
            };
            let active = NativeActive::default();
            let identity = NativeIdentityExtension::Revision1Unknown;
            let mut blobs = BTreeMap::new();
            let mut blob_kinds = BTreeMap::new();
            let source_ref = add_blob(
                &mut blobs,
                &mut blob_kinds,
                "source",
                source_encoding(&source, &identity)?,
            )?;
            let active_ref = add_json(
                &mut blobs,
                &mut blob_kinds,
                "active",
                &active,
                NATIVE_ACTIVE_BYTES,
            )?;
            let ledger = NativeLedger {
                source_digest: source_ref.digest.clone(),
                active_digest: active_ref.digest.clone(),
                ..Default::default()
            };
            let ledger_ref = add_blob(
                &mut blobs,
                &mut blob_kinds,
                "ledger",
                ledger_encoding(&ledger, &identity)?,
            )?;
            let record = NativeSearchHistoryV1 {
                schema_version: 1,
                policy_revision: 1,
                reader_floor: 1,
                limits_revision: 1,
                lineage: NativeLineage {
                    id: lineage,
                    source_revision: 1,
                    source_digest: source_ref.digest.clone(),
                },
                generation: 0,
                high_water: 0,
                source: source_ref,
                active: active_ref,
                ledger: ledger_ref,
                history_boundary: history_boundary(history)?,
                exchange_snapshot: None,
                previous_checkpoint: None,
                capacity: BTreeMap::new(),
            };
            let bytes = encode_native(&record, NATIVE_CONTROL_BYTES)?.into();
            Ok(Self {
                record,
                source,
                active,
                ledger,
                identity,
                bytes,
                blobs,
                blob_kinds,
                compound_proof: None,
                authority_digest: None,
                compound_lineage: false,
            })
        }

        fn is_empty_projection_input(&self) -> bool {
            self.record.generation == 0
                && self.record.high_water == 0
                && self.record.previous_checkpoint.is_none()
                && self.source.producing_bindings.is_empty()
                && self.source.responses.is_empty()
                && self.source.ordered_blocks.is_empty()
                && self.source.eligible_snapshots.is_empty()
                && self.source.older_sources.is_empty()
                && self.ledger.groups.is_empty()
                && self.ledger.accounting.is_empty()
                && self.ledger.usage.is_empty()
                && self.ledger.observations.is_empty()
                && self.ledger.transitions.is_empty()
                && self.active.view_links.is_empty()
                && self.active.dispositions.is_empty()
        }
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
        pub inputs: Option<Arc<NativeEligibleInputs>>,
        pub publication: NativePrimaryPublication,
        pub primary_pending: Option<NativePrimaryPending>,
        pub primary_decision: Option<NativePrimaryDecision>,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NativePrimaryPublication {
        Legacy,
        Deferred,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NativePrimaryDecision {
        Committed,
        Rejected,
    }

    impl NativePrimaryDecision {
        fn as_str(self) -> &'static str {
            match self {
                Self::Committed => "Committed",
                Self::Rejected => "Rejected",
            }
        }
    }

    #[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct NativePrimaryRecord {
        primary_resolution: String,
        request: String,
        lease: u64,
        attempt: u64,
        occurrence: u64,
        authorized_at_evidence: bool,
        intended_digest: String,
        ordinary_digest: String,
        intended_members: u64,
        ordinary_items: u64,
        observed_bytes: u64,
    }

    #[derive(Clone)]
    pub(crate) struct NativePrimaryPending {
        record: NativePrimaryRecord,
        source_digest: String,
        intended: Vec<NativeViewLink>,
        ordinary_digests: Vec<String>,
        history_start: u64,
    }

    impl NativePrimaryPending {
        pub(crate) fn authorized(&self) -> bool {
            self.record.authorized_at_evidence
        }
        pub(crate) fn attempt(&self) -> u64 {
            self.record.attempt
        }
        pub(crate) fn occurrence(&self) -> u64 {
            self.record.occurrence
        }
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
        if !matches!(
            (
                record.schema_version,
                record.policy_revision,
                record.reader_floor,
                record.limits_revision
            ),
            (1, 1, 1, 1) | (2, 1, 2, 1)
        ) || record.generation == 0
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

    fn source_parts(
        bytes: &[u8],
    ) -> Result<(NativeSource, Option<Vec<NativeEligibleMemberLink>>), NativeUnavailable> {
        NativeResource::SourceBytes.admit(bytes.len() as u64, 0)?;
        let version = serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|_| NativeUnavailable::InvalidReference)?["schema_version"]
            .as_u64();
        match version {
            Some(1) => serde_json::from_slice::<NativeSource>(bytes)
                .map(|source| (source, None))
                .map_err(|_| NativeUnavailable::InvalidReference),
            Some(2) => serde_json::from_slice::<NativeSourceV2>(bytes)
                .map(|source| {
                    let (source, links) = source.into_parts();
                    (source, Some(links))
                })
                .map_err(|_| NativeUnavailable::InvalidReference),
            _ => Err(NativeUnavailable::UnsupportedVersion),
        }
    }

    fn ledger_parts(
        bytes: &[u8],
        links: Option<Vec<NativeEligibleMemberLink>>,
    ) -> Result<(NativeLedger, NativeIdentityExtension), NativeUnavailable> {
        NativeResource::LedgerBytes.admit(bytes.len() as u64, 0)?;
        match links {
            None => serde_json::from_slice::<NativeLedger>(bytes)
                .map(|ledger| (ledger, NativeIdentityExtension::Revision1Unknown))
                .map_err(|_| NativeUnavailable::InvalidReference),
            Some(eligible_member_links) => serde_json::from_slice::<NativeLedgerV2>(bytes)
                .map(|wire| {
                    let (ledger, eligible_member_accounting) = wire.into_parts();
                    (
                        ledger,
                        NativeIdentityExtension::Revision2 {
                            eligible_member_links,
                            eligible_member_accounting,
                        },
                    )
                })
                .map_err(|_| NativeUnavailable::InvalidReference),
        }
    }

    fn source_encoding(
        source: &NativeSource,
        identity: &NativeIdentityExtension,
    ) -> Result<Vec<u8>, NativeUnavailable> {
        match identity {
            NativeIdentityExtension::Revision1Unknown => encode_native(source, NATIVE_SOURCE_BYTES),
            NativeIdentityExtension::Revision2 {
                eligible_member_links,
                ..
            } => encode_native(
                &NativeSourceV2::from_parts(source.clone(), eligible_member_links.clone()),
                NATIVE_SOURCE_BYTES,
            ),
        }
    }

    fn ledger_encoding(
        ledger: &NativeLedger,
        identity: &NativeIdentityExtension,
    ) -> Result<Vec<u8>, NativeUnavailable> {
        match identity {
            NativeIdentityExtension::Revision1Unknown => encode_native(ledger, NATIVE_LEDGER_BYTES),
            NativeIdentityExtension::Revision2 {
                eligible_member_accounting,
                ..
            } => encode_native(
                &NativeLedgerV2::from_parts(ledger.clone(), eligible_member_accounting.clone()),
                NATIVE_LEDGER_BYTES,
            ),
        }
    }

    fn identity_links(identity: &NativeIdentityExtension) -> &[NativeEligibleMemberLink] {
        match identity {
            NativeIdentityExtension::Revision1Unknown => &[],
            NativeIdentityExtension::Revision2 {
                eligible_member_links,
                ..
            } => eligible_member_links,
        }
    }

    fn identity_accounting(
        identity: &NativeIdentityExtension,
    ) -> &[NativeEligibleMemberAccounting] {
        match identity {
            NativeIdentityExtension::Revision1Unknown => &[],
            NativeIdentityExtension::Revision2 {
                eligible_member_accounting,
                ..
            } => eligible_member_accounting,
        }
    }

    fn member_coordinate(
        parent: &NativeSourceCoordinate,
        ordinal: usize,
    ) -> Result<NativeSourceCoordinate, NativeUnavailable> {
        let mut coordinate = parent.clone();
        coordinate.submember =
            Some(u64::try_from(ordinal).map_err(|_| NativeUnavailable::Capacity)?);
        Ok(coordinate)
    }

    impl NativeCheckpoint {
        /// Legacy records expose explicit Unknown members, never scalar-derived backing.
        pub fn eligible_members(&self) -> Result<Vec<NativeEligibleMemberLink>, NativeUnavailable> {
            if let NativeIdentityExtension::Revision2 {
                eligible_member_links,
                ..
            } = &self.identity
            {
                return Ok(eligible_member_links.clone());
            }
            let mut links = Vec::new();
            for block in &self.source.ordered_blocks {
                let raw: serde_json::Value = serde_json::from_slice(
                    self.blobs
                        .get(&block.block.digest)
                        .ok_or(NativeUnavailable::InvalidReference)?,
                )
                .map_err(|_| NativeUnavailable::Fidelity)?;
                for (ordinal, _) in native_candidate_references(&raw)?.iter().enumerate() {
                    NativeResource::LedgerMembers.admit(links.len() as u64, 1)?;
                    links.push(NativeEligibleMemberLink {
                        source: member_coordinate(&block.coordinates, ordinal)?,
                        identity: NativeEligibleMemberIdentity::Unavailable {
                            reason: NativeUnavailable::UnknownIdentity,
                        },
                    });
                }
            }
            Ok(links)
        }
    }

    fn reference_matches(
        snapshot: &NativeEligibleSnapshot,
        schema: &[u8],
        leaf: &NativeCandidateReference<'_>,
    ) -> Result<bool, NativeUnavailable> {
        let NativeCandidateReference::Tool {
            namespace,
            name,
            declaration,
        } = leaf
        else {
            return Ok(false);
        };
        let qualified = if snapshot.namespace_kind == NativeNamespaceKind::Mcp {
            format!("{}__{}", snapshot.qualified_namespace, snapshot.child_name)
        } else {
            snapshot.child_name.clone()
        };
        let names_match = match namespace {
            Some(namespace) => {
                *namespace == snapshot.qualified_namespace && *name == snapshot.child_name
            }
            None => *name == qualified,
        };
        if !names_match {
            return Ok(false);
        }
        if let Some(declaration) = declaration {
            let parameters = declaration
                .get("parameters")
                .ok_or(NativeUnavailable::UnknownIdentity)?;
            if encode_native(parameters, NATIVE_SOURCE_BYTES)?.as_slice() != schema {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn client_reference<'a>(
        raw: &'a [serde_json::Value],
        at: usize,
        join: &str,
    ) -> Result<NativeCandidateReference<'a>, NativeUnavailable> {
        let mut found = None;
        for value in raw.get(..at).ok_or(NativeUnavailable::InvalidReference)? {
            let matches = match value.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => {
                    value.get("id").and_then(serde_json::Value::as_str) == Some(join)
                }
                Some("function_call") => {
                    value.get("call_id").and_then(serde_json::Value::as_str) == Some(join)
                }
                _ => false,
            };
            if matches {
                if found.is_some() {
                    return Err(NativeUnavailable::UnknownIdentity);
                }
                let leaves = native_candidate_references(value)?;
                if leaves.len() != 1 {
                    return Err(NativeUnavailable::UnknownIdentity);
                }
                found = leaves.into_iter().next();
            }
        }
        found.ok_or(NativeUnavailable::UnknownIdentity)
    }

    impl NativeCheckpoint {
        pub fn decode(
            bytes: Vec<u8>,
            load: impl FnMut(&NativeBlobRef) -> Result<Vec<u8>, NativeUnavailable>,
        ) -> Result<Self, NativeUnavailable> {
            Self::decode_impl(bytes, None, load)
        }

        pub fn decode_with_compound_proof(
            bytes: Vec<u8>,
            proof: NativeCompoundProofInput,
            load: impl FnMut(&NativeBlobRef) -> Result<Vec<u8>, NativeUnavailable>,
        ) -> Result<Self, NativeUnavailable> {
            Self::decode_impl(bytes, Some(proof), load)
        }

        fn decode_impl(
            bytes: Vec<u8>,
            proof: Option<NativeCompoundProofInput>,
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
                        let (source, links) = source_parts(&payload)?;
                        if source.schema_version > record.schema_version
                            || source.lineage != record.lineage.id
                            || source.source_revision == 0
                            || source.source_revision > revision
                        {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        if !seen {
                            for link in links.as_deref().unwrap_or_default() {
                                if let NativeEligibleMemberIdentity::Backed { eligible_ref } =
                                    &link.identity
                                {
                                    if eligible_ref.kind != "eligible_snapshot"
                                        || !source.eligible_snapshots.contains(eligible_ref)
                                    {
                                        return Err(NativeUnavailable::InvalidReference);
                                    }
                                    queue.push_back((
                                        eligible_ref.clone(),
                                        generation,
                                        source.source_revision,
                                    ));
                                }
                            }
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
            let (source, links) = source_parts(parse(&record.source)?)?;
            let active: NativeActive = serde_json::from_slice(parse(&record.active)?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            let (ledger, identity) = ledger_parts(parse(&record.ledger)?, links)?;
            let mut checkpoint = Self {
                record,
                source,
                active,
                ledger,
                identity,
                bytes: bytes.into(),
                blobs,
                blob_kinds,
                compound_proof: None,
                authority_digest: None,
                compound_lineage: false,
            };
            if let Some(proof) = proof {
                checkpoint.install_compound_proof(proof)?;
            }
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

    fn primary_records(
        ledger: &NativeLedger,
    ) -> Result<Vec<(usize, NativePrimaryRecord)>, NativeUnavailable> {
        let mut records = Vec::new();
        for (index, value) in ledger.transitions.iter().enumerate() {
            if value.get("primary_resolution").is_none() {
                continue;
            }
            let record: NativePrimaryRecord = serde_json::from_value(value.clone())
                .map_err(|_| NativeUnavailable::InvalidReference)?;
            if !matches!(
                record.primary_resolution.as_str(),
                "Pending" | "Committed" | "Rejected"
            ) || record.request.is_empty()
                || record.intended_digest.len() != 64
                || record.ordinary_digest.len() != 64
                || records
                    .iter()
                    .any(|(_, old): &(usize, NativePrimaryRecord)| {
                        old.request == record.request
                            && old.lease == record.lease
                            && old.attempt == record.attempt
                    })
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            records.push((index, record));
        }
        Ok(records)
    }

    impl NativeCheckpoint {
        fn primary_intent(
            lease: &NativeCaptureLease,
            attempt: &NativeAttemptEvidence,
            items: &[ConversationItem],
            occurrence: u64,
            block_start: u64,
        ) -> Result<NativePrimaryPending, NativeUnavailable> {
            let mut intended = Vec::new();
            for (index, _) in attempt.blocks.iter().enumerate() {
                let Some((offset, role)) = attempt.view_roles.get(index).and_then(Option::as_ref)
                else {
                    continue;
                };
                let offset_index =
                    usize::try_from(*offset).map_err(|_| NativeUnavailable::Capacity)?;
                let view_digest = attempt
                    .view_digests
                    .get(offset_index)
                    .ok_or(NativeUnavailable::Fidelity)?
                    .clone();
                let source = NativeSourceCoordinate {
                    lineage: lease.lineage().into(),
                    producing_binding: lease.request().into(),
                    turn: lease.turn(),
                    item: items.len() as u64,
                    response: occurrence,
                    block: index as u64,
                    submember: None,
                    occurrence: block_start
                        .checked_add(index as u64)
                        .ok_or(NativeUnavailable::Capacity)?,
                };
                intended.push(NativeViewLink {
                    source,
                    source_digest: String::new(),
                    view_item: Some(
                        (items.len() as u64)
                            .checked_add(*offset)
                            .ok_or(NativeUnavailable::Capacity)?,
                    ),
                    view_digest: Some(view_digest),
                    role: role.clone(),
                    source_subrange: None,
                });
            }
            let record = NativePrimaryRecord {
                primary_resolution: "Pending".into(),
                request: lease.request().into(),
                lease: lease.ordinal(),
                attempt: attempt.attempt,
                occurrence,
                authorized_at_evidence: lease.view_authorized(),
                intended_digest: digest(&encode_native(&intended, NATIVE_LEDGER_BYTES)?),
                ordinary_digest: digest(&encode_native(
                    &attempt.view_digests,
                    NATIVE_LEDGER_BYTES,
                )?),
                intended_members: intended.len() as u64,
                ordinary_items: attempt.view_digests.len() as u64,
                observed_bytes: attempt.observed_bytes,
            };
            Ok(NativePrimaryPending {
                record,
                source_digest: String::new(),
                intended,
                ordinary_digests: attempt.view_digests.clone(),
                history_start: items.len() as u64,
            })
        }

        pub(crate) fn pending_primary(
            &self,
            lease: &NativeCaptureLease,
            attempts: &[Arc<NativeAttemptEvidence>],
            items: &[ConversationItem],
        ) -> Result<Option<NativePrimaryPending>, NativeUnavailable> {
            let mut records = primary_records(&self.ledger)?
                .into_iter()
                .map(|(_, record)| record)
                .filter(|record| {
                    record.request == lease.request()
                        && record.lease == lease.ordinal()
                        && record.primary_resolution == "Pending"
                });
            let Some(record) = records.next() else {
                return Ok(None);
            };
            if records.next().is_some() {
                return Err(NativeUnavailable::ConflictingObservation);
            }
            let response = self.validate_primary_record(&record)?;
            let attempt = attempts
                .iter()
                .find(|attempt| attempt.attempt == record.attempt)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let mut pending = Self::primary_intent(
                lease,
                attempt,
                items,
                record.occurrence,
                response.block_start,
            )?;
            if pending.record != record {
                return Err(NativeUnavailable::InvalidReference);
            }
            pending.source_digest = self.record.source.digest.clone();
            Ok(Some(pending))
        }

        fn validate_primary_record(
            &self,
            record: &NativePrimaryRecord,
        ) -> Result<&NativeResponseBoundary, NativeUnavailable> {
            let response = self
                .source
                .responses
                .get(record.occurrence as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            if response.occurrence != record.occurrence
                || response.request != record.request
                || response.lease != record.lease
                || response.attempt != record.attempt
                || response.status != NativeResponseStatus::Accepted
                || !response.stream_complete
                || record.intended_members > response.block_end - response.block_start
                || record.ordinary_items > NATIVE_RESPONSE_BLOCKS
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            Ok(response)
        }
    }

    impl NativeCheckpoint {
        fn primary_accounting_extends(&self, previous: &Self) -> Result<(), NativeUnavailable> {
            if self.compound_proof.is_some() {
                return self.projection_extends(previous);
            }
            let old_records = primary_records(&previous.ledger)?;
            let new_records = primary_records(&self.ledger)?;
            if !old_records
                .iter()
                .all(|(index, _)| new_records.iter().any(|(next, _)| index == next))
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let mut changed = None;
            for (index, old) in &old_records {
                let (_, new) = new_records
                    .iter()
                    .find(|(next, _)| next == index)
                    .ok_or(NativeUnavailable::InvalidReference)?;
                self.validate_primary_record(new)?;
                if old != new {
                    let mut expected = old.clone();
                    expected.primary_resolution = new.primary_resolution.clone();
                    if old.primary_resolution != "Pending"
                        || !matches!(new.primary_resolution.as_str(), "Committed" | "Rejected")
                        || *new != expected
                        || changed.replace((old, new)).is_some()
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                }
            }
            for (index, value) in previous.ledger.transitions.iter().enumerate() {
                if !old_records.iter().any(|(at, _)| *at == index)
                    && self.ledger.transitions.get(index) != Some(value)
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            let Some((old, new)) = changed else {
                return if self
                    .ledger
                    .accounting
                    .starts_with(&previous.ledger.accounting)
                {
                    Ok(())
                } else {
                    Err(NativeUnavailable::InvalidReference)
                };
            };
            if self.source != previous.source
                || self.identity != previous.identity
                || self.record.source != previous.record.source
                || self.ledger.accounting.len() != previous.ledger.accounting.len()
                || self.ledger.transitions.len() != previous.ledger.transitions.len()
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let response = self
                .source
                .responses
                .get(old.occurrence as usize)
                .ok_or(NativeUnavailable::InvalidReference)?;
            let mut intended = Vec::new();
            let mut own_members = 0;
            for (before, after) in previous
                .ledger
                .accounting
                .iter()
                .zip(&self.ledger.accounting)
            {
                if before.source.response != response.occurrence {
                    if before != after {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    continue;
                }
                own_members += 1;
                if before.source != after.source
                    || before.source_digest != after.source_digest
                    || before.source.producing_binding != old.request
                    || before.role != "SourceOnly"
                    || before.view_item.is_some()
                    || before.view_digest.is_some()
                    || before.source_subrange != after.source_subrange
                {
                    return Err(NativeUnavailable::InvalidReference);
                }
                if new.primary_resolution == "Rejected" {
                    if before != after {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                } else if after.view_item.is_some() {
                    if !old.authorized_at_evidence
                        || after
                            .view_digest
                            .as_ref()
                            .is_none_or(|value| value.len() != 64)
                        || !matches!(
                            after.role.as_str(),
                            "Discovery" | "Text" | "Client" | "Reasoning"
                        )
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    let mut link = after.clone();
                    link.source_digest.clear();
                    intended.push(link);
                } else if before != after {
                    return Err(NativeUnavailable::InvalidReference);
                }
            }
            if own_members != response.block_end - response.block_start {
                return Err(NativeUnavailable::InvalidReference);
            }
            if new.primary_resolution == "Committed"
                && (intended.len() as u64 != old.intended_members
                    || digest(&encode_native(&intended, NATIVE_LEDGER_BYTES)?)
                        != old.intended_digest)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let mut active = previous.active.view_links.clone();
            if new.primary_resolution == "Committed" {
                active.extend(
                    self.ledger
                        .accounting
                        .iter()
                        .filter(|entry| {
                            entry.source.response == old.occurrence && entry.view_item.is_some()
                        })
                        .cloned(),
                );
            }
            if self.active.view_links != active
                || self.active.dispositions != previous.active.dispositions
                || self.active.loaded_invalidations != previous.active.loaded_invalidations
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            let mut expected_lifecycle = previous.ledger.lifecycle.clone();
            let mut expected_groups = previous.ledger.groups.clone();
            if new.primary_resolution == "Rejected" {
                for waiting in &mut expected_lifecycle.waiting_clients {
                    if waiting.source.response == old.occurrence
                        && waiting.source.producing_binding == old.request
                    {
                        waiting.delivery = "Unavailable".into();
                    }
                }
                for group in &mut expected_groups {
                    if group
                        .members
                        .iter()
                        .any(|member| member.response == old.occurrence)
                        && group.state == "LivePending"
                    {
                        group.state = "AbandonedPending".into();
                        expected_lifecycle
                            .groups
                            .insert(group.key.clone(), group.state.clone());
                    }
                }
                expected_lifecycle
                    .exchanges
                    .insert(old.request.clone(), "Terminal".into());
                expected_lifecycle
                    .terminal
                    .entry(format!("{}:{}", old.request, old.attempt))
                    .or_insert(NativeTerminalMarker {
                        outcome: NativeUnavailable::Cancelled,
                        last_accepted_response: Some(response.occurrence),
                        observed_bytes: old.observed_bytes,
                        failed_block: None,
                    });
            }
            if self.ledger.lifecycle != expected_lifecycle || self.ledger.groups != expected_groups
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            Ok(())
        }

        pub(crate) fn resolve_primary(
            &self,
            pending: &NativePrimaryPending,
            decision: NativePrimaryDecision,
            items: &[ConversationItem],
        ) -> Result<Self, NativeUnavailable> {
            self.validate()?;
            self.matches_history(items)?;
            if self.record.source.digest != pending.source_digest {
                return Err(NativeUnavailable::StaleHistory);
            }
            let (index, original) = primary_records(&self.ledger)?
                .into_iter()
                .find(|(_, record)| {
                    record.request == pending.record.request
                        && record.lease == pending.record.lease
                        && record.attempt == pending.record.attempt
                })
                .ok_or(NativeUnavailable::StaleHistory)?;
            if original != pending.record || original.primary_resolution != "Pending" {
                return Err(NativeUnavailable::ConflictingObservation);
            }
            self.validate_primary_record(&original)?;
            let mut next = self.clone();
            next.compound_proof = None;
            next.authority_digest = None;
            let response = &self.source.responses[original.occurrence as usize];
            if decision == NativePrimaryDecision::Committed {
                if !original.authorized_at_evidence {
                    return Err(NativeUnavailable::Cancelled);
                }
                let start = usize::try_from(pending.history_start)
                    .map_err(|_| NativeUnavailable::Capacity)?;
                let end = start
                    .checked_add(pending.ordinary_digests.len())
                    .ok_or(NativeUnavailable::Capacity)?;
                let actual = items
                    .get(start..end)
                    .ok_or(NativeUnavailable::StaleHistory)?;
                for (item, expected) in actual.iter().zip(&pending.ordinary_digests) {
                    if history_boundary(std::slice::from_ref(item))?.digest != *expected {
                        return Err(NativeUnavailable::StaleHistory);
                    }
                }
                for link in &pending.intended {
                    let entry = next
                        .ledger
                        .accounting
                        .iter_mut()
                        .find(|entry| entry.source == link.source)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    let digest = entry.source_digest.clone();
                    *entry = link.clone();
                    entry.source_digest = digest;
                    next.active.view_links.push(entry.clone());
                }
                next.record.history_boundary = history_boundary(items)?;
            } else {
                for waiting in &mut next.ledger.lifecycle.waiting_clients {
                    if waiting.source.response == original.occurrence
                        && waiting.source.producing_binding == original.request
                    {
                        waiting.delivery = "Unavailable".into();
                    }
                }
                for group in &mut next.ledger.groups {
                    if group
                        .members
                        .iter()
                        .any(|member| member.response == original.occurrence)
                        && group.state == "LivePending"
                    {
                        group.state = "AbandonedPending".into();
                        next.ledger
                            .lifecycle
                            .groups
                            .insert(group.key.clone(), group.state.clone());
                    }
                }
                next.ledger
                    .lifecycle
                    .exchanges
                    .insert(original.request.clone(), "Terminal".into());
                next.ledger
                    .lifecycle
                    .terminal
                    .entry(format!("{}:{}", original.request, original.attempt))
                    .or_insert(NativeTerminalMarker {
                        outcome: NativeUnavailable::Cancelled,
                        last_accepted_response: Some(response.occurrence),
                        observed_bytes: original.observed_bytes,
                        failed_block: None,
                    });
            }
            let mut resolved = original;
            resolved.primary_resolution = decision.as_str().into();
            next.ledger.transitions[index] =
                serde_json::to_value(resolved).map_err(|_| NativeUnavailable::Fidelity)?;
            next.record.active = add_json(
                &mut next.blobs,
                &mut next.blob_kinds,
                "active",
                &next.active,
                NATIVE_ACTIVE_BYTES,
            )?;
            next.ledger.active_digest = next.record.active.digest.clone();
            next.record.ledger = add_blob(
                &mut next.blobs,
                &mut next.blob_kinds,
                "ledger",
                ledger_encoding(&next.ledger, &next.identity)?,
            )?;
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
            next.bytes = encode_native(&next.record, NATIVE_CONTROL_BYTES)?.into();
            next.seal_capacity()?;
            next.validate()?;
            next.matches_history(items)?;
            Ok(next)
        }
    }

    impl NativeCheckpoint {
        pub fn append(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            attempts: &[Arc<NativeAttemptEvidence>],
            items: &[ConversationItem],
        ) -> Result<Self, NativeUnavailable> {
            Self::append_with_identity(previous, lease, attempts, items, None)
        }

        pub fn append_with_identity(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            attempts: &[Arc<NativeAttemptEvidence>],
            items: &[ConversationItem],
            inputs: Option<&NativeEligibleInputs>,
        ) -> Result<Self, NativeUnavailable> {
            Self::append_with_publication(
                previous,
                lease,
                attempts,
                items,
                inputs,
                NativePrimaryPublication::Legacy,
            )
        }

        pub(crate) fn append_with_publication(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            attempts: &[Arc<NativeAttemptEvidence>],
            items: &[ConversationItem],
            inputs: Option<&NativeEligibleInputs>,
            publication: NativePrimaryPublication,
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
                    schema_version: if inputs.is_some() { 2 } else { 1 },
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
            let mut identity = previous
                .map_or(NativeIdentityExtension::Revision1Unknown, |previous| {
                    previous.identity.clone()
                });
            if inputs.is_some() && matches!(identity, NativeIdentityExtension::Revision1Unknown) {
                let links = previous
                    .map(NativeCheckpoint::eligible_members)
                    .transpose()?
                    .unwrap_or_default();
                let mut witnesses = Vec::new();
                for link in &links {
                    let mut parent = link.source.clone();
                    parent.submember = None;
                    let accounting = ledger
                        .accounting
                        .iter()
                        .find(|entry| entry.source == parent)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    witnesses.push(NativeEligibleMemberAccounting {
                        source: link.source.clone(),
                        source_digest: accounting.source_digest.clone(),
                    });
                }
                identity = NativeIdentityExtension::Revision2 {
                    eligible_member_links: links,
                    eligible_member_accounting: witnesses,
                };
            }
            source.schema_version = if matches!(identity, NativeIdentityExtension::Revision1Unknown)
            {
                1
            } else {
                2
            };
            let mut snapshots = Vec::new();
            if let Some(inputs) = inputs {
                for entry in inputs.entries() {
                    if digest(entry.schema_bytes()) != entry.schema_digest() {
                        return Err(NativeUnavailable::UnknownIdentity);
                    }
                    let schema = add_raw_blob(
                        &mut blobs,
                        &mut blob_kinds,
                        "eligible_schema",
                        entry.schema_bytes(),
                    )?;
                    let snapshot = NativeEligibleSnapshot {
                        version: 1,
                        namespace_kind: entry.namespace_kind(),
                        qualified_namespace: entry.qualified_namespace().into(),
                        child_name: entry.child_name().into(),
                        schema,
                        binding: origin_ref.clone(),
                    };
                    let reference = add_json(
                        &mut blobs,
                        &mut blob_kinds,
                        "eligible_snapshot",
                        &snapshot,
                        NATIVE_SOURCE_BYTES,
                    )?;
                    if !source.eligible_snapshots.contains(&reference) {
                        source.eligible_snapshots.push(reference.clone());
                    }
                    snapshots.push((entry.position(), reference));
                }
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
                if publication == NativePrimaryPublication::Deferred
                    && attempt.status == NativeResponseStatus::Accepted
                {
                    if primary_records(&ledger)?.iter().any(|(_, record)| {
                        record.request == lease.request() && record.primary_resolution == "Pending"
                    }) {
                        return Err(NativeUnavailable::ConflictingObservation);
                    }
                    let pending = Self::primary_intent(
                        lease,
                        attempt,
                        items,
                        response_occurrence.ok_or(NativeUnavailable::Fidelity)?,
                        source.ordered_blocks.len() as u64,
                    )?;
                    let bytes = encode_native(&pending.record, NATIVE_LEDGER_BYTES)?;
                    let transient = native_encoded_len(&pending.intended, NATIVE_LEDGER_BYTES)?
                        .checked_add(native_encoded_len(
                            &pending.ordinary_digests,
                            NATIVE_LEDGER_BYTES,
                        )?)
                        .ok_or(NativeUnavailable::Capacity)?;
                    lease.reserve(
                        (bytes.len() as u64)
                            .checked_mul(3)
                            .and_then(|bytes| bytes.checked_add(transient))
                            .ok_or(NativeUnavailable::Capacity)?,
                    )?;
                    ledger.transitions.push(
                        serde_json::from_slice(&bytes).map_err(|_| NativeUnavailable::Fidelity)?,
                    );
                }
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
                    let leaves = if source.schema_version == 2 {
                        native_candidate_references(&raw)?
                    } else {
                        Vec::new()
                    };
                    let identity_preflight = |source: u64, witness: u64| {
                        let bytes = witness
                            .checked_mul(2)
                            .and_then(|copies| source.checked_add(copies))
                            .ok_or(NativeUnavailable::Capacity)?;
                        if bytes == 0 {
                            Ok(())
                        } else {
                            lease.reserve(bytes)
                        }
                    };
                    let source_cost =
                        partition_bytes(&blobs, &blob_kinds, NativeResource::SourceBytes)?
                            .checked_add(source_encoding(&source, &identity)?.len() as u64)
                            .ok_or(NativeUnavailable::Capacity)?;
                    let ledger_cost =
                        partition_bytes(&blobs, &blob_kinds, NativeResource::LedgerBytes)?
                            .checked_add(ledger_encoding(&ledger, &identity)?.len() as u64)
                            .ok_or(NativeUnavailable::Capacity)?;
                    let member_cost = Self::ledger_members(&ledger, &identity)?;
                    NativeResource::LedgerMembers.admit(member_cost, leaves.len() as u64)?;
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
                    let mut resolved_raw = Vec::new();
                    let mut resolved_leaves = Vec::new();
                    for leaf in &leaves {
                        if let NativeCandidateReference::ClientResult { .. } = leaf {
                            for old in &source.ordered_blocks {
                                resolved_raw.push(
                                    serde_json::from_slice::<serde_json::Value>(
                                        blobs
                                            .get(&old.block.digest)
                                            .ok_or(NativeUnavailable::InvalidReference)?,
                                    )
                                    .map_err(|_| NativeUnavailable::Fidelity)?,
                                );
                            }
                            break;
                        }
                    }
                    for leaf in leaves {
                        let resolved = match leaf {
                            NativeCandidateReference::ClientResult { join } => {
                                client_reference(&resolved_raw, resolved_raw.len(), join)?
                            }
                            tool => tool,
                        };
                        resolved_leaves.push(resolved);
                    }
                    fn append_identity_links(
                        identity: &mut NativeIdentityExtension,
                        coordinates: &NativeSourceCoordinate,
                        leaves: &[NativeCandidateReference<'_>],
                        inputs: Option<&NativeEligibleInputs>,
                        snapshots: &[(usize, NativeBlobRef)],
                        source_bytes: u64,
                        ledger_bytes: u64,
                        ledger_members: u64,
                        preflight: impl FnOnce(u64, u64) -> Result<(), NativeUnavailable>,
                    ) -> Result<Option<NativeUnavailable>, NativeUnavailable> {
                        let NativeIdentityExtension::Revision2 {
                            eligible_member_links,
                            eligible_member_accounting,
                        } = identity
                        else {
                            return Ok(None);
                        };
                        let make = |ordinal, leaf: &NativeCandidateReference<'_>| -> Result<NativeEligibleMemberLink, NativeUnavailable> {
                            let mut matching = Vec::new();
                            if let Some(inputs) = inputs {
                                for entry in inputs.entries() {
                                    let NativeCandidateReference::Tool { namespace, name, declaration } = leaf else { continue; };
                                    let names_match = namespace.map_or(entry.declaration().name == *name, |namespace|
                                        namespace == entry.qualified_namespace() && *name == entry.child_name());
                                    if !names_match { continue; }
                                    if let Some(declaration) = declaration {
                                        if declaration.get("parameters") != Some(&entry.declaration().parameters)
                                            || declaration.get("description").and_then(serde_json::Value::as_str) != entry.declaration().description.as_deref()
                                        { continue; }
                                    }
                                    if let Some((_, reference)) = snapshots.iter().find(|(position, _)| *position == entry.position()) {
                                        matching.push(reference.clone());
                                    }
                                }
                            }
                            let identity = if matching.len() == 1 {
                                NativeEligibleMemberIdentity::Backed { eligible_ref: matching.remove(0) }
                            } else { NativeEligibleMemberIdentity::Unavailable { reason: NativeUnavailable::UnknownIdentity } };
                            Ok(NativeEligibleMemberLink { source: member_coordinate(coordinates, ordinal)?, identity })
                        };
                        let mut link_bytes = 0;
                        let mut witness_bytes = 0;
                        for (ordinal, leaf) in leaves.iter().enumerate() {
                            let link = make(ordinal, leaf)?;
                            link_bytes = NativeResource::BlockBytes.admit(
                                link_bytes,
                                native_encoded_len(&link, NATIVE_BLOCK_BYTES)?
                                    + u64::from(!eligible_member_links.is_empty() || ordinal > 0),
                            )?;
                            let witness = NativeEligibleMemberAccounting {
                                source: link.source,
                                source_digest: "0".repeat(64),
                            };
                            witness_bytes = NativeResource::LedgerBytes.admit(
                                witness_bytes,
                                native_encoded_len(&witness, NATIVE_LEDGER_BYTES)?
                                    + u64::from(
                                        !eligible_member_accounting.is_empty() || ordinal > 0,
                                    ),
                            )?;
                        }
                        NativeResource::SourceBytes.admit(source_bytes, link_bytes)?;
                        NativeResource::LedgerBytes.admit(
                            ledger_bytes,
                            witness_bytes
                                .checked_mul(2)
                                .ok_or(NativeUnavailable::Capacity)?,
                        )?;
                        NativeResource::LedgerMembers.admit(ledger_members, leaves.len() as u64)?;
                        preflight(link_bytes, witness_bytes)?;
                        let mut unavailable = None;
                        for (ordinal, leaf) in leaves.iter().enumerate() {
                            let link = make(ordinal, leaf)?;
                            if matches!(
                                link.identity,
                                NativeEligibleMemberIdentity::Unavailable { .. }
                            ) {
                                unavailable = Some(NativeUnavailable::UnknownIdentity);
                            }
                            eligible_member_accounting.push(NativeEligibleMemberAccounting {
                                source: link.source.clone(),
                                source_digest: String::new(),
                            });
                            eligible_member_links.push(link);
                        }
                        Ok(unavailable)
                    }
                    let summary = append_identity_links(
                        &mut identity,
                        &coordinates,
                        &resolved_leaves,
                        inputs,
                        &snapshots,
                        source_cost,
                        ledger_cost,
                        member_cost,
                        identity_preflight,
                    )?;
                    source.ordered_blocks.push(NativeRawBlock {
                        coordinates: coordinates.clone(),
                        wire_index: block.wire_index,
                        arrival_ordinal: block.arrival_ordinal,
                        block: reference,
                        partial,
                        state: block.state.clone(),
                        eligible_ref: None,
                        eligible_unavailable: if source.schema_version == 2 {
                            summary
                        } else {
                            tool_member.then_some(NativeUnavailable::UnknownIdentity)
                        },
                    });
                    let view = attempt.view_roles.get(index).and_then(Option::as_ref);
                    let view = view.filter(|_| {
                        matches!(attempt.status, NativeResponseStatus::Accepted)
                            && lease.view_authorized()
                            && publication == NativePrimaryPublication::Legacy
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
            let source_ref = add_blob(
                &mut blobs,
                &mut blob_kinds,
                "source",
                source_encoding(&source, &identity)?,
            )?;
            if let NativeIdentityExtension::Revision2 {
                eligible_member_accounting,
                ..
            } = &mut identity
            {
                for witness in eligible_member_accounting
                    .iter_mut()
                    .filter(|witness| witness.source_digest.is_empty())
                {
                    witness.source_digest = source_ref.digest.clone();
                }
            }
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
            let ledger_ref = add_blob(
                &mut blobs,
                &mut blob_kinds,
                "ledger",
                ledger_encoding(&ledger, &identity)?,
            )?;
            let previous_checkpoint = previous
                .map(|previous| {
                    add_raw_blob(&mut blobs, &mut blob_kinds, "checkpoint", &previous.bytes)
                })
                .transpose()?;
            let record = NativeSearchHistoryV1 {
                schema_version: source.schema_version,
                policy_revision: NATIVE_POLICY_REVISION,
                reader_floor: source.schema_version,
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
                identity,
                bytes: bytes.into(),
                blobs,
                blob_kinds,
                compound_proof: None,
                authority_digest: None,
                compound_lineage: false,
            };
            checkpoint.compound_lineage =
                previous.is_some_and(|previous| previous.compound_lineage);
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
                    Self::ledger_members(&self.ledger, &self.identity)?,
                ),
            ] {
                resource.admit(count, 0)?;
            }
            if self.record.capacity != self.capacity()?
                || self.source.schema_version != self.record.schema_version
                || (matches!(self.identity, NativeIdentityExtension::Revision1Unknown)
                    != (self.record.schema_version == 1))
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
                || source_parts(value(&self.record.source)?)?.0 != self.source
                || source_parts(value(&self.record.source)?)?.1.as_deref()
                    != match &self.identity {
                        NativeIdentityExtension::Revision1Unknown => None,
                        NativeIdentityExtension::Revision2 {
                            eligible_member_links,
                            ..
                        } => Some(eligible_member_links.as_slice()),
                    }
                || serde_json::from_slice::<NativeActive>(value(&self.record.active)?)
                    .ok()
                    .as_ref()
                    != Some(&self.active)
                || ledger_parts(
                    value(&self.record.ledger)?,
                    source_parts(value(&self.record.source)?)?.1,
                )? != (self.ledger.clone(), self.identity.clone())
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
                let mut total = NativeResource::BlockBytes.admit(raw.len() as u64, partial)?;
                for link in identity_links(&self.identity)
                    .iter()
                    .filter(|link| link.source.occurrence == block.coordinates.occurrence)
                {
                    total = NativeResource::BlockBytes
                        .admit(total, native_encoded_len(link, NATIVE_BLOCK_BYTES)?)?;
                }
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
            fn validate_identity_links(
                checkpoint: &NativeCheckpoint,
            ) -> Result<(), NativeUnavailable> {
                let NativeIdentityExtension::Revision2 {
                    eligible_member_links: links,
                    eligible_member_accounting: witnesses,
                } = &checkpoint.identity
                else {
                    return Ok(());
                };
                if links.len() != witnesses.len() {
                    return Err(NativeUnavailable::InvalidReference);
                }
                let mut raw = Vec::new();
                for block in &checkpoint.source.ordered_blocks {
                    raw.push(
                        serde_json::from_slice::<serde_json::Value>(
                            checkpoint
                                .blobs
                                .get(&block.block.digest)
                                .ok_or(NativeUnavailable::InvalidReference)?,
                        )
                        .map_err(|_| NativeUnavailable::Fidelity)?,
                    );
                }
                let mut cursor = 0;
                let mut admitting_sources = BTreeMap::new();
                for (at, block) in checkpoint.source.ordered_blocks.iter().enumerate() {
                    let leaves = native_candidate_references(&raw[at])?;
                    let parent_links: Vec<_> = checkpoint
                        .ledger
                        .accounting
                        .iter()
                        .filter(|link| link.source == block.coordinates)
                        .collect();
                    if parent_links.len() != 1 {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                    let admitted = checkpoint
                        .blobs
                        .get(&parent_links[0].source_digest)
                        .ok_or(NativeUnavailable::InvalidReference)?;
                    let (admitting, admitting_links) =
                        match admitting_sources.entry(parent_links[0].source_digest.as_str()) {
                            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(source_parts(admitted)?)
                            }
                        };
                    let migrated = admitting.schema_version == 1;
                    let mut missing = false;
                    for (ordinal, original) in leaves.iter().enumerate() {
                        let expected = member_coordinate(&block.coordinates, ordinal)?;
                        let link = links
                            .get(cursor)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        let witness = witnesses
                            .get(cursor)
                            .ok_or(NativeUnavailable::InvalidReference)?;
                        if link.source != expected
                            || witness.source != expected
                            || witness.source_digest != parent_links[0].source_digest
                        {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        if let Some(admitting_links) = &admitting_links {
                            if admitting_links
                                .iter()
                                .filter(|admitted| admitted.source == expected)
                                .collect::<Vec<_>>()
                                != vec![link]
                            {
                                return Err(NativeUnavailable::InvalidReference);
                            }
                        } else if !matches!(
                            link.identity,
                            NativeEligibleMemberIdentity::Unavailable {
                                reason: NativeUnavailable::UnknownIdentity
                            }
                        ) {
                            return Err(NativeUnavailable::InvalidReference);
                        }
                        match &link.identity {
                            NativeEligibleMemberIdentity::Unavailable {
                                reason: NativeUnavailable::UnknownIdentity,
                            } => missing = true,
                            NativeEligibleMemberIdentity::Unavailable { .. } => {
                                return Err(NativeUnavailable::InvalidReference);
                            }
                            NativeEligibleMemberIdentity::Backed { eligible_ref } => {
                                if migrated
                                    || eligible_ref.kind != "eligible_snapshot"
                                    || !checkpoint.source.eligible_snapshots.contains(eligible_ref)
                                {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                let bytes = checkpoint
                                    .blobs
                                    .get(&eligible_ref.digest)
                                    .ok_or(NativeUnavailable::InvalidReference)?;
                                if bytes.len() as u64 != eligible_ref.byte_len
                                    || digest(bytes) != eligible_ref.digest
                                {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                let snapshot: NativeEligibleSnapshot =
                                    serde_json::from_slice(bytes)
                                        .map_err(|_| NativeUnavailable::InvalidReference)?;
                                let bindings: Vec<_> = checkpoint
                                    .source
                                    .producing_bindings
                                    .iter()
                                    .filter(|binding| {
                                        binding.request == block.coordinates.producing_binding
                                    })
                                    .collect();
                                if bindings.len() != 1
                                    || snapshot.binding != bindings[0].origin_ref
                                    || snapshot.version != 1
                                    || snapshot.schema.kind != "eligible_schema"
                                    || snapshot.qualified_namespace.is_empty()
                                    || snapshot.child_name.is_empty()
                                {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                let schema = checkpoint
                                    .blobs
                                    .get(&snapshot.schema.digest)
                                    .ok_or(NativeUnavailable::InvalidReference)?;
                                if schema.len() as u64 != snapshot.schema.byte_len
                                    || digest(schema) != snapshot.schema.digest
                                {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                let resolved;
                                let leaf = if let NativeCandidateReference::ClientResult { join } =
                                    original
                                {
                                    resolved = client_reference(&raw, at, join)?;
                                    &resolved
                                } else {
                                    original
                                };
                                if !reference_matches(&snapshot, schema, leaf)? {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                                let mut matches = 0usize;
                                for reference in &checkpoint.source.eligible_snapshots {
                                    let candidate: NativeEligibleSnapshot = serde_json::from_slice(
                                        checkpoint
                                            .blobs
                                            .get(&reference.digest)
                                            .ok_or(NativeUnavailable::InvalidReference)?,
                                    )
                                    .map_err(|_| NativeUnavailable::InvalidReference)?;
                                    let candidate_schema = checkpoint
                                        .blobs
                                        .get(&candidate.schema.digest)
                                        .ok_or(NativeUnavailable::InvalidReference)?;
                                    if candidate.binding == snapshot.binding
                                        && reference_matches(&candidate, candidate_schema, leaf)?
                                    {
                                        matches += 1;
                                    }
                                }
                                if matches != 1 {
                                    return Err(NativeUnavailable::InvalidReference);
                                }
                            }
                        }
                        cursor += 1;
                    }
                    if !migrated
                        && (block.eligible_ref.is_some()
                            || block.eligible_unavailable
                                != missing.then_some(NativeUnavailable::UnknownIdentity))
                    {
                        return Err(NativeUnavailable::InvalidReference);
                    }
                }
                if cursor != links.len() {
                    return Err(NativeUnavailable::InvalidReference);
                }
                Ok(())
            }
            validate_identity_links(self)?;
            let mut group_keys = std::collections::BTreeSet::new();
            for group in &self.ledger.groups {
                let references = identity_links(&self.identity)
                    .iter()
                    .filter(|link| {
                        let mut parent = link.source.clone();
                        parent.submember = None;
                        group.members.contains(&parent)
                    })
                    .count() as u64;
                NativeResource::GroupMembers.admit(group.members.len() as u64, references)?;
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
                        entry.insert(source_parts(manifest)?.0)
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
                let (source, links) = source_parts(value(&record.source)?)?;
                let (ledger, identity) = ledger_parts(value(&record.ledger)?, links)?;
                let old = Self {
                    source,
                    active: serde_json::from_slice(value(&record.active)?)
                        .map_err(|_| NativeUnavailable::InvalidReference)?,
                    ledger,
                    identity,
                    record,
                    bytes: value(previous)?.into(),
                    blobs: BTreeMap::new(),
                    blob_kinds: BTreeMap::new(),
                    compound_proof: None,
                    authority_digest: None,
                    compound_lineage: false,
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

        fn ledger_members(
            ledger: &NativeLedger,
            identity: &NativeIdentityExtension,
        ) -> Result<u64, NativeUnavailable> {
            let mut count = identity_accounting(identity).len() as u64;
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
                    .any(|waiting| waiting.delivery == "Waiting")
                || primary_records(&self.ledger)?
                    .iter()
                    .any(|(_, record)| record.primary_resolution == "Pending");
            if !pending {
                return Ok((0, 0, Self::ledger_members(&self.ledger, &self.identity)?));
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
            let mut primary_active = 0u64;
            let mut primary_ledger = 0u64;
            for (index, mut record) in primary_records(&ledger)? {
                if record.primary_resolution != "Pending" {
                    continue;
                }
                let response = self.validate_primary_record(&record)?;
                let mut links = Vec::new();
                for block in &self.source.ordered_blocks
                    [response.block_start as usize..response.block_end as usize]
                {
                    links.push(NativeViewLink {
                        source: block.coordinates.clone(),
                        source_digest: "0".repeat(64),
                        view_item: Some(u64::MAX),
                        view_digest: Some("0".repeat(64)),
                        role: "Discovery".into(),
                        source_subrange: None,
                    });
                }
                let encoded = native_encoded_len(&links, NATIVE_ACTIVE_BYTES)?;
                primary_active = NativeResource::ActiveBytes.admit(primary_active, encoded)?;
                primary_ledger = NativeResource::LedgerBytes.admit(primary_ledger, encoded)?;
                ledger
                    .lifecycle
                    .terminal
                    .entry(format!("{}:{}", record.request, record.attempt))
                    .or_insert(NativeTerminalMarker {
                        outcome: NativeUnavailable::Indeterminate,
                        last_accepted_response: Some(u64::MAX),
                        observed_bytes: u64::MAX,
                        failed_block: Some(u32::MAX),
                    });
                record.primary_resolution = "Committed".into();
                ledger.transitions[index] =
                    serde_json::to_value(record).map_err(|_| NativeUnavailable::Fidelity)?;
            }
            Ok((
                NativeResource::ActiveBytes.admit(
                    native_encoded_len(&active, NATIVE_ACTIVE_BYTES)?,
                    primary_active,
                )?,
                NativeResource::LedgerBytes.admit(
                    ledger_encoding(&ledger, &self.identity)?.len() as u64,
                    primary_ledger,
                )?,
                Self::ledger_members(&ledger, &self.identity)?,
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
            Self::capture_allocation_with_identity(previous, lease, items, None)
        }

        pub(crate) fn capture_allocation_primary(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            items: &[ConversationItem],
            inputs: Option<&NativeEligibleInputs>,
        ) -> Result<(u64, u64, u64), NativeUnavailable> {
            let (ceiling, copies, minimum) =
                Self::capture_allocation_with_identity(previous, lease, items, inputs)?;
            let probe = NativeCaptureLease::issue(
                lease.lineage().into(),
                lease.request().into(),
                lease.turn(),
                lease.ordinal(),
                NATIVE_SOURCE_BYTES,
            );
            let accepted = Arc::new(NativeAttemptEvidence {
                attempt: u64::MAX,
                admitted: true,
                response_started: true,
                provider_response_id: Some(serde_json::json!("")),
                source_stop_reason: Some("end_turn".into()),
                stream_complete: true,
                status: NativeResponseStatus::Accepted,
                blocks: Vec::new(),
                usage: BTreeMap::new(),
                latency_ms: Some(u64::MAX),
                http_dispatches: u64::MAX,
                observed_bytes: u64::MAX,
                failed_block: None,
                view_roles: Vec::new(),
                view_digests: Vec::new(),
            });
            let candidate = Self::append_with_publication(
                previous,
                &probe,
                std::slice::from_ref(&accepted),
                items,
                inputs,
                NativePrimaryPublication::Deferred,
            )?;
            let pending = candidate
                .pending_primary(&probe, std::slice::from_ref(&accepted), items)?
                .ok_or(NativeUnavailable::Incomplete)?;
            let committed =
                candidate.resolve_primary(&pending, NativePrimaryDecision::Committed, items)?;
            let rejected =
                candidate.resolve_primary(&pending, NativePrimaryDecision::Rejected, items)?;
            let mut extra = probe.charged()?;
            let mut maximum = copies;
            for checkpoint in [&candidate, &committed, &rejected] {
                let bytes = checkpoint
                    .record
                    .active
                    .byte_len
                    .checked_add(checkpoint.record.ledger.byte_len)
                    .and_then(|bytes| bytes.checked_add(checkpoint.bytes.len() as u64))
                    .ok_or(NativeUnavailable::Capacity)?;
                extra = extra
                    .checked_add(bytes)
                    .ok_or(NativeUnavailable::Capacity)?;
                maximum = maximum.max(bytes);
            }
            let minimum = minimum
                .checked_add(extra)
                .ok_or(NativeUnavailable::Capacity)?;
            let ceiling = ceiling
                .checked_sub(extra)
                .filter(|remaining| *remaining >= minimum)
                .ok_or(NativeUnavailable::Capacity)?;
            Ok((ceiling, maximum, minimum))
        }

        pub fn capture_allocation_with_identity(
            previous: Option<&Self>,
            lease: &NativeCaptureLease,
            items: &[ConversationItem],
            inputs: Option<&NativeEligibleInputs>,
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
            let minimum = Self::append_with_identity(
                previous,
                lease,
                std::slice::from_ref(&terminal),
                items,
                inputs,
            )?;
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
            if self.record.schema_version < previous.record.schema_version
                || !identity_links(&self.identity).starts_with(identity_links(&previous.identity))
                || !identity_accounting(&self.identity)
                    .starts_with(identity_accounting(&previous.identity))
            {
                return Err(NativeUnavailable::InvalidReference);
            }
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
                || self.primary_accounting_extends(previous).is_err()
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
            next.compound_proof = None;
            next.authority_digest = None;
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
