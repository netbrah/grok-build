//! Item 0 of the session-insights breakdown: the `x.ai/insights` protocol triple's construction path.
//!
//! The shell-side job hands its one result back through `emit_insights_result`: the additive
//! `SessionUpdate::SessionInsights` notification carrying the report path plus the at-a-glance
//! lines. These tests lock that seam: the result is persisted as an xAI update with exactly
//! those fields, so the pager (and a replaying client) can pick it up.

use super::support::*;
use super::*;
use xai_grok_sampling_types::ConversationItem;

/// Drain the persistence channel and return the `SessionInsights` fields if one was emitted.
fn drained_session_insights(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> Option<(String, Vec<String>)> {
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Xai(n)) = msg
            && matches!(
                n.update,
                crate::extensions::notification::SessionUpdate::SessionInsights { .. }
            )
        {
            let crate::extensions::notification::SessionUpdate::SessionInsights {
                report_path,
                at_a_glance,
            } = n.update
            else {
                unreachable!("matched on SessionInsights above")
            };
            return Some((report_path, at_a_glance));
        }
    }
    None
}

/// The reply seam persists the additive `SessionInsights` update with the report path and the
/// at-a-glance lines, the same way `SessionRecap` rides the persistence channel.
#[tokio::test(flavor = "current_thread")]
async fn emit_insights_result_persists_session_insights_update() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor
                .emit_insights_result(
                    "/tmp/grok-home/insights/2026-10-02T00-00-00Z/report.html".to_string(),
                    vec![
                        "12 sessions over 30 days".to_string(),
                        "$4.20 total, 82% cache hit".to_string(),
                    ],
                )
                .await;

            let (report_path, at_a_glance) = drained_session_insights(&mut persistence_rx)
                .expect("emit_insights_result must persist a SessionInsights update");
            assert_eq!(
                report_path,
                "/tmp/grok-home/insights/2026-10-02T00-00-00Z/report.html"
            );
            assert_eq!(at_a_glance.len(), 2);
            assert_eq!(at_a_glance[0], "12 sessions over 30 days");
            assert_eq!(at_a_glance[1], "$4.20 total, 82% cache hit");
        })
        .await;
}

/// The seam is display-only: it sends a notification and never mutates the model conversation.
#[tokio::test(flavor = "current_thread")]
async fn emit_insights_result_never_mutates_the_conversation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;
            assert_eq!(before.len(), 3, "seed must be applied before the reply");

            actor
                .emit_insights_result(
                    "/tmp/grok-home/insights/2026-10-02T00-00-00Z/report.html".to_string(),
                    vec!["12 sessions over 30 days".to_string()],
                )
                .await;
            let after = actor.chat_state_handle.get_conversation().await;

            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "the insights reply must never mutate the model conversation"
            );
            assert!(
                drained_session_insights(&mut persistence_rx).is_some(),
                "the update must still be persisted"
            );
        })
        .await;
}
