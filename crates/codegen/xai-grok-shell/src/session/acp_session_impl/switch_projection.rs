//! Shell policy for the switch-time projector (XW-PROJECT-1, apex-ayl.71): fire the
//! in-actor projection once per cross-model switch and report the disk-acknowledged
//! outcome (XSWITCH-1 backup-gated contract; chat_history.jsonl only).
//!
//! - The projection itself is `project_switch_history` in
//!   `xai-grok-sampling-types::conversation::projection`, computed IN-ACTOR so it
//!   serializes with turn pushes (no read-then-write window across a new prompt).
//! - The write is a no-op (reply `NoMatch`) when the projection changed nothing:
//!   re-applying the same model never touches disk (idempotence, sdd-71 §4.3).
//! - The write is gated on a backup and acknowledged from disk
//!   ([`StripOutcome`]); only `Applied` claims the stored conversation changed.
//! - Silent: no user notification — the switch is user-initiated and the TUI
//!   already shows the model line change.

use xai_chat_state::StripOutcome;
use xai_grok_sampling_types::conversation::native_search_history::NativeHistoryRequirement;
use xai_grok_sampling_types::conversation::projection::TargetRoute;

use crate::session::acp_session::SessionActor;

impl SessionActor {
    async fn apply_native_switch_projection(
        &self,
        target_model: &str,
        target_pin: Option<&str>,
        route: &TargetRoute,
    ) -> Result<bool, xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable>
    {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        #[cfg(test)]
        let target = HISTORY_S3_TEST_DOMAIN.try_with(Clone::clone).ok();
        #[cfg(not(test))]
        let target: Option<NativeReplayDomain> = None;
        let target = target.ok_or(NativeUnavailable::UnknownIdentity)?;
        if target.model != target_model || target.backend != route.backend() {
            return Err(NativeUnavailable::UnknownIdentity);
        }
        let (prepared, _) = self.prepare_native_tool_candidates_timed().await;
        let bundle = self.prepared_tool_bundle(&prepared);
        let projection = super::BridgeDeclarationProjection::prepared(bundle, Default::default());
        let candidates = projection.candidates();
        let config = self
            .chat_state_handle
            .get_sampling_config()
            .await
            .ok_or(NativeUnavailable::Incomplete)?;
        let selected = candidates.clone().select_for_row(&config);
        let admission = xai_grok_sampling_types::SearchAdmission::for_row(
            config.supports_search_tool,
            &selected,
        );
        let native_route = TargetRoute::new(route.backend(), route.boundary(), admission);
        let guard = self
            .chat_state_handle
            .register_native_history_owner(
                self.session_id_string(),
                format!("switch:{target_model}"),
                target,
                NativeProjectionOwner::SwitchStore,
                self.session_turn_active.clone(),
            )
            .await?;
        let candidates =
            candidates.with_history_entry(xai_chat_state::NativeHistoryEntry::captured(
                NativeHistoryRequirement::Selected,
                xai_chat_state::NativeHistoryEntryIntent::SwitchStore(guard.token()),
            ))?;
        let completion = self
            .chat_state_handle
            .project_native_switch_history(
                candidates,
                target_model.into(),
                target_pin.map(str::to_owned),
                native_route,
            )
            .await?;
        match completion {
            Some(completion) => {
                self.publish_native_history_accounting(
                    &completion.core.accounting,
                    &completion.certificate,
                    "switch_store",
                )?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Post-wall hook for `handle_set_session_model` (sdd-71 §9 step 5): project the
    /// stored history for the cross-wire switch to `target_model`, persisting the
    /// result through the backup-gated, disk-acked seam.
    ///
    /// `target_pin` (XW-ENC-AFFINITY-1, apex-mf6): the target row's
    /// `x-litellm-tags` pin (`None` = untagged, empty-string normalized) —
    /// the switch-time gate on the AZ->AZ row consults it against each
    /// item's mint tag to decide store retention of the ciphertext.
    ///
    /// `route` (apex-waj.35): the target row's route tuple, built by the caller from the
    /// row the switch lands on (`model_switch.rs::target_route_from_row`). It rides unchanged to the
    /// projector; this seam must not re-derive anything from `target_model`, which is why it
    /// takes the tuple rather than the flags separately.
    pub(crate) async fn apply_switch_projection(
        &self,
        target_model: &str,
        target_pin: Option<&str>,
        route: &TargetRoute,
        requirement: NativeHistoryRequirement,
    ) {
        if requirement == NativeHistoryRequirement::Selected {
            let result = self
                .apply_native_switch_projection(target_model, target_pin, route)
                .await;
            xai_grok_telemetry::unified_log::warn(
                "shell.turn.native_switch_projection",
                Some(self.session_info.id.0.as_ref()),
                Some(
                    serde_json::json!({"model_id":target_model,"outcome":match result { Ok(true) => "applied", Ok(false) => "no_match", Err(_) => "refused" }}),
                ),
            );
            return;
        }
        let outcome = self
            .chat_state_handle
            .project_switch_history(target_model, target_pin, route)
            .await;
        let (outcome_label, changed) = match outcome {
            StripOutcome::Applied { stripped } => ("applied", stripped),
            StripOutcome::NoMatch => ("no_match", 0),
            StripOutcome::WriteFailed { stripped } => ("write_failed", stripped),
            StripOutcome::ActorUnavailable => ("actor_unavailable", 0),
        };
        xai_grok_telemetry::unified_log::warn(
            "shell.turn.switch_projection_persisted",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "model_id": target_model,
                "outcome": outcome_label,
                "changed": changed,
            })),
        );
    }
}

/// The single production selector for the native-history requirement (waj2-U8).
///
/// Exactly one place decides the mode for a fresh bound request, so the
/// activation cannot be scattered across the turn, switch and auxiliary
/// callers. It stays `Inactive` for the whole of history-S3: the Selected route
/// only becomes real once the joint U9 owner/compound-readiness path exists,
/// and nothing here may send Selected before that cutover.
pub(crate) const PRODUCTION_NATIVE_HISTORY_MODE: NativeHistoryRequirement =
    NativeHistoryRequirement::Inactive;

/// Read the sole production mode. Crate-internal on purpose: tests drive the
/// Selected arm through the same private path rather than a post-build callback.
#[cfg(test)]
tokio::task_local! {
    pub(crate) static HISTORY_S3_TEST_REQUIREMENT: NativeHistoryRequirement;
    pub(crate) static HISTORY_S3_TEST_DOMAIN: xai_grok_sampling_types::conversation::native_search_history::NativeReplayDomain;
}

pub(crate) fn select_native_history() -> NativeHistoryRequirement {
    #[cfg(test)]
    if let Ok(requirement) = HISTORY_S3_TEST_REQUIREMENT.try_with(|requirement| *requirement) {
        return requirement;
    }
    PRODUCTION_NATIVE_HISTORY_MODE
}

/// Adapter for an explicitly captured entry whose build produced no native
/// preparation (the Inactive route, and every legacy-shaped request).
///
/// Absence of a preparation is never a mode decision: the caller keeps its own
/// captured entry and asks this adapter, which refuses only what is genuinely
/// inconsistent — a Selected caller with no preparation.
pub(crate) fn native_history_inactive_adapter(
    entry: &xai_chat_state::NativeHistoryEntry,
    preparation: Option<
        &xai_grok_sampling_types::conversation::native_search_history::NativeBoundPreparation,
    >,
) -> Result<(), xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable> {
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    match (entry.requirement(), preparation) {
        (NativeHistoryRequirement::Inactive, _) => Ok(()),
        (NativeHistoryRequirement::Selected, None) => Err(NativeUnavailable::Incomplete),
        (NativeHistoryRequirement::Selected, Some(_)) => Ok(()),
    }
}

pub(crate) fn native_history_accounting_report(
    accounting: &xai_grok_sampling_types::conversation::native_search_history::NativeHistoryAccounting,
    site: &str,
) -> Result<String, xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable>
{
    use xai_grok_sampling_types::conversation::native_search_history::{
        NativeHistoryReason, NativeUnavailable,
    };
    if accounting.reasons.len() > 8 {
        return Err(NativeUnavailable::Capacity);
    }
    let reasons = accounting
        .reasons
        .iter()
        .map(|(reason, count)| {
            let label = match reason {
                NativeHistoryReason::Kept => "kept",
                NativeHistoryReason::Unqualified => "unqualified",
                NativeHistoryReason::MissingView => "missing_view",
                NativeHistoryReason::PriorDisposition => "prior_disposition",
                NativeHistoryReason::FBudget => "f_budget",
                NativeHistoryReason::Pending => "pending",
                NativeHistoryReason::Abandoned => "abandoned",
            };
            format!("{label}={count}")
        })
        .collect::<Vec<_>>()
        .join(",");
    let g = &accounting.groups;
    let m = &accounting.members;
    let aggregate = g.kept + g.demoted + g.stripped + g.omitted + g.pending;
    let report = format!(
        "Native history {site}: groups KEEP={} F={} strip={} omit={} pending={}; members KEEP={} F={} strip={} omit={} pending={}; reasons={reasons}; aggregate={aggregate}; rediscovery={}",
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
        accounting.rediscovery
    );
    if report.len() > 512 {
        return Err(NativeUnavailable::Capacity);
    }
    Ok(report)
}

impl SessionActor {
    pub(crate) fn publish_native_history_accounting(
        &self,
        accounting: &xai_grok_sampling_types::conversation::native_search_history::NativeHistoryAccounting,
        certificate: &xai_grok_sampling_types::conversation::native_search_history::NativeHistoryCommitCertificate,
        site: &str,
    ) -> Result<(), xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable>
    {
        let report = native_history_accounting_report(accounting, site)?;
        if !certificate.mark_reported_once() {
            return Ok(());
        }
        let update = agent_client_protocol::SessionUpdate::AgentMessageChunk(
            agent_client_protocol::ContentChunk::new(agent_client_protocol::ContentBlock::Text(
                agent_client_protocol::TextContent::new(report.clone()),
            )),
        );
        let metadata = serde_json::json!({"nativeHistoryAccounting": {
            "site":site, "summary":report, "generation":certificate.acknowledgement().generation(),
            "reasons":accounting.reasons.len(), "rediscovery":accounting.rediscovery,
        }})
        .as_object()
        .cloned();
        self.emit_transient_notification(
            agent_client_protocol::SessionNotification::new(self.session_info.id.clone(), update)
                .meta(metadata),
        );
        Ok(())
    }
}
