//! Shell policy for the sampler's model-bound strip (XSWITCH-1, apex-ayl.58): which strips may
//! rewrite stored history, when, and what the user is told.
//!
//! - The sampler emits `ModelBoundStateStripped` only after the strip actually removed items
//!   (the `RetryWithModelBoundStateStrip` arm is fail-closed: `strip_model_bound_state`
//!   returned > 0), so every event is persistable — no reason-based gating like images.
//! - The rewrite waits for that request to terminal (`Completed` or `Failed`). The write is
//!   awaited before the drain barrier releases so the next prompt cannot reread the markers.
//! - The write is gated on a backup and acknowledged from disk ([`StripOutcome`]); only
//!   `Applied` claims the stored conversation changed.
//! - Scope: `chat_history.jsonl` only.
//!   A rebuild replaying `updates.jsonl` (e.g. a remote pull) restores the markers and pays
//!   one more 503 + strip cycle.
//! - Silent recovery: no user notification (CROSSWIRE-1 — the user already sees the transient
//!   `RetryState::Retrying`; the one-shot strip notification is the carried N1 follow-up).

use xai_chat_state::StripOutcome;
use xai_grok_sampler::RequestId;

use crate::session::acp_session::{PendingModelBoundStrip, SessionActor};

const MAX_PENDING_MODEL_BOUND_STRIPS: usize = 16;

fn enforce_pending_model_bound_strip_bound(
    pending: &mut std::collections::HashMap<RequestId, PendingModelBoundStrip>,
) {
    if pending.len() <= MAX_PENDING_MODEL_BOUND_STRIPS {
        return;
    }
    // Retain in-flight writes and timed-out requests (their terminal still owes a persist);
    // drop fresh entries until the bound holds.
    let excess = pending.len() - MAX_PENDING_MODEL_BOUND_STRIPS;
    let mut dropped = 0;
    pending.retain(|_, strip| {
        strip.applying
            || strip.timed_out
            || {
                if dropped < excess {
                    dropped += 1;
                    false
                } else {
                    true
                }
            }
    });
    if pending.len() > MAX_PENDING_MODEL_BOUND_STRIPS {
        tracing::warn!(
            maximum = MAX_PENDING_MODEL_BOUND_STRIPS,
            "pending model-bound strip bound exceeded; only timed-out or in-flight entries retained"
        );
    }
}

impl SessionActor {
    /// Drop abandoned model-bound strips at a turn boundary while retaining timed-out
    /// requests whose terminal event still owes one request-scoped side effect.
    pub(crate) fn retain_timed_out_model_bound_strips_for_new_turn(&self) {
        let ownership = self.turn_stream_drained.lock();
        let mut pending = self.pending_model_bound_strip.lock();
        pending.retain(|request_id, strip| match ownership.get(request_id) {
            Some(o) if o.waiter.is_none() => {
                strip.timed_out = true;
                true
            }
            Some(_) => false,
            None => strip.timed_out || strip.applying,
        });
        for request_id in ownership
            .iter()
            .filter_map(|(request_id, o)| o.waiter.is_none().then_some(request_id))
        {
            pending.entry(request_id.clone()).or_insert_with(|| PendingModelBoundStrip {
                timed_out: true,
                applying: false,
            });
            enforce_pending_model_bound_strip_bound(&mut pending);
        }
    }

    /// Invalidate queued model-bound strip work synchronously when rewind claims history.
    pub(crate) fn cancel_pending_model_bound_strips_for_rewind(&self) {
        self.pending_model_bound_strip.lock().clear();
    }

    /// Handle `SamplingEvent::ModelBoundStateStripped`: buffer a persistable strip for
    /// [`Self::apply_pending_model_bound_strip`].
    pub(crate) async fn handle_model_bound_stripped(
        &self,
        request_id: RequestId,
        stripped: usize,
    ) {
        {
            let mut pending = self.pending_model_bound_strip.lock();
            let timed_out = pending
                .get(&request_id)
                .is_some_and(|strip| strip.timed_out);
            pending.insert(
                request_id.clone(),
                PendingModelBoundStrip {
                    timed_out,
                    applying: false,
                },
            );
            enforce_pending_model_bound_strip_bound(&mut pending);
        }
        xai_grok_telemetry::unified_log::warn(
            "shell.turn.model_bound_stripped",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "sampler_request_id": request_id.as_str(),
                "stripped": stripped,
                "persist_deferred": true,
            })),
        );
    }

    /// Persist a buffered model-bound strip once the stripped retry terminals
    /// (`Completed` or `Failed`).
    pub(crate) async fn apply_pending_model_bound_strip(&self, request_id: &RequestId) {
        // Acquire rewrite ownership before claiming the entry:
        // rewind either clears queued work first, or waits until this proven strip finishes.
        let _rewrite_guard = self.image_strip_rewrite_barrier.lock_strip().await;
        {
            let mut pending = self.pending_model_bound_strip.lock();
            let Some(strip) = pending.get_mut(request_id) else {
                return;
            };
            if strip.applying {
                return;
            }
            strip.applying = true;
        }
        let outcome = self.chat_state_handle.strip_model_bound_history().await;
        let still_owned = self
            .pending_model_bound_strip
            .lock()
            .remove(request_id)
            .is_some_and(|strip| strip.applying);
        if !still_owned {
            return;
        }
        let (outcome_label, persisted) = match outcome {
            StripOutcome::Applied { stripped } => ("applied", stripped),
            StripOutcome::NoMatch => ("no_match", 0),
            StripOutcome::WriteFailed { .. } => ("write_failed", 0),
            StripOutcome::ActorUnavailable => ("actor_unavailable", 0),
        };
        xai_grok_telemetry::unified_log::warn(
            "shell.turn.model_bound_strip_persisted",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "sampler_request_id": request_id.as_str(),
                "outcome": outcome_label,
                "persisted": persisted,
            })),
        );
    }
}
