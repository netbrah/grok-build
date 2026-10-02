//! `x.ai/insights` extension handler.
//!
//! Triggers the shell-side insights job via [`SessionCommand::Insights`].
//! This is fire-and-forget: the handler returns as soon as the command is queued rather than
//! blocking on the job. The finished job later hands its one result back to the pager as a
//! [`SessionUpdate::SessionInsights`](crate::extensions::notification::SessionUpdate::SessionInsights)
//! notification: the report path plus the at-a-glance lines.

use agent_client_protocol as acp;

use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use crate::session::SessionCommand;

#[tracing::instrument(skip_all)]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InsightsRequest {
        session_id: String,
    }

    let req: InsightsRequest = parse_params(args)?;
    tracing::info!("handling /insights request");

    let sid: acp::SessionId = req.session_id.clone().into();
    // After a leader restart the reconnect may still be replaying `session/load`;
    // wait for that load to finish instead of failing with "session not found" (same as `x.ai/recap`)
    let Some(session) = agent.session_handle_waiting_for_load(&sid).await else {
        return Err(
            acp::Error::invalid_params().data(format!("session not found: {}", req.session_id))
        );
    };

    // Fire-and-forget: the result is emitted later as a SessionInsights notification
    // We only ack that the request was accepted
    let _ = session.cmd_tx.send(SessionCommand::Insights);

    to_ext_response(Ok(serde_json::json!({ "ok": true })))
}
