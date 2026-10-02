//! `x.ai/insights` session-side handling — the protocol triple's construction path (item 0).
//!
//! The insights job (scope → digest → cache → label → aggregate → narrate → render → write)
//! lands in later items under `session/insights/`; item 0 owns the reply seam the finished job
//! hands its one result through: the report path plus the at-a-glance lines, emitted as the
//! additive `SessionUpdate::SessionInsights` notification on the `x.ai/session_notification`
//! rail. An older leader that does not know the variant decodes it as `Unknown` and ignores it;
//! the version-skew fallback (a shell-side line an older leader still renders) lands with item
//! 6's progress-line work — there is no shell-reachable progress channel for it in item 0.

use super::*;

impl SessionActor {
    /// Entry point for the `x.ai/insights` job, spawned from the command loop like `handle_recap`.
    ///
    /// Item 0 lands the protocol triple; the job body lands in a later item and hands its
    /// written report to [`Self::emit_insights_result`].
    pub(super) async fn handle_insights(&self) {
        tracing::info!(
            session_id = %self.session_info.id.0,
            "insights job accepted; the job body lands in a later item"
        );
    }

    /// Hand the finished insights job back to the pager: the additive
    /// [`crate::extensions::notification::SessionUpdate::SessionInsights`] notification
    /// carrying the report path plus the at-a-glance lines, on the `x.ai/session_notification`
    /// rail. An older leader that does not know the variant decodes it as `Unknown` and
    /// ignores it; the version-skew fallback (a line an older leader still renders) lands with
    /// item 6's progress-line work. Like every x.ai one-shot notification this path closes the
    /// cancel/rewind window (`closes_cancel_rewind_window`); the job-body item decides whether
    /// display-only `SessionInsights` belongs on its keep-open list.
    pub(crate) async fn emit_insights_result(&self, report_path: String, at_a_glance: Vec<String>) {
        tracing::info!(
            session_id = %self.session_info.id.0,
            report_path = %report_path,
            lines = at_a_glance.len(),
            "insights job finished; report written"
        );
        self.send_xai_notification(
            crate::extensions::notification::SessionUpdate::SessionInsights {
                report_path,
                at_a_glance,
            },
        )
        .await;
    }
}
