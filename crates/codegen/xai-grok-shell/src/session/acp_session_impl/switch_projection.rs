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

use crate::session::acp_session::SessionActor;

impl SessionActor {
    /// Post-wall hook for `handle_set_session_model` (sdd-71 §9 step 5): project the
    /// stored history for the cross-wire switch to `target_model`, persisting the
    /// result through the backup-gated, disk-acked seam.
    ///
    /// `target_pin` (XW-ENC-AFFINITY-1, apex-mf6): the target row's
    /// `x-litellm-tags` pin (`None` = untagged, empty-string normalized) —
    /// the switch-time gate on the AZ->AZ row consults it against each
    /// item's mint tag to decide store retention of the ciphertext.
    pub(crate) async fn apply_switch_projection(&self, target_model: &str, target_pin: Option<&str>) {
        let outcome = self
            .chat_state_handle
            .project_switch_history(target_model, target_pin)
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
