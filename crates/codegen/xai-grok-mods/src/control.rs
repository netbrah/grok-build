// crates/codegen/xai-grok-mods/src/control.rs
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use xai_grok_tools::implementations::grok_build::task::coordinator::{
    ActiveMessageAdmission, ChildControl, LocalBoxFuture, SendBoxFuture, SubagentProgress,
};
use xai_grok_tools::implementations::grok_build::task::types::{
    ActiveAgentMessageDelivery, ActiveAgentMessageOperation, AgentMailboxMessage,
};

use crate::protocol::{HostFrame, MessageMode};

#[derive(Clone)]
pub struct ModChildControl {
    tx: mpsc::UnboundedSender<HostFrame>,
    pending: Arc<Mutex<HashSet<String>>>,
    progress: Arc<Mutex<SubagentProgress>>,
    live_steer: bool,
    cancel: CancellationToken,
    closed: Arc<AtomicBool>,
}

impl ModChildControl {
    pub fn new(
        tx: mpsc::UnboundedSender<HostFrame>,
        pending: Arc<Mutex<HashSet<String>>>,
        progress: Arc<Mutex<SubagentProgress>>,
        live_steer: bool,
        cancel: CancellationToken,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            pending,
            progress,
            live_steer,
            cancel,
            closed,
        }
    }

    fn followup(&self, m: &AgentMailboxMessage) -> bool {
        if !self.accepts_native_message(m) || self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.pending.lock().insert(m.message_id.clone());
        let ok = self
            .tx
            .send(HostFrame::Message {
                message_id: m.message_id.clone(),
                mode: MessageMode::Followup,
                from: m.from_agent_id.clone(),
                text: m.body.clone(),
            })
            .is_ok();
        if !ok {
            self.pending.lock().remove(&m.message_id);
        }
        ok
    }
}

impl ChildControl for ModChildControl {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;

    fn progress(&self) -> Self::ProgressFuture {
        let p = self.progress.lock().clone();
        Box::pin(std::future::ready(p))
    }

    fn send_active_message(
        &self,
        d: ActiveAgentMessageDelivery,
    ) -> SendBoxFuture<ActiveMessageAdmission> {
        if !self.live_steer {
            return Box::pin(std::future::ready(ActiveMessageAdmission::Unsupported));
        }
        if self.closed.load(Ordering::Acquire) || self.tx.is_closed() {
            return Box::pin(std::future::ready(ActiveMessageAdmission::ChannelClosed));
        }
        let m = d.message();
        let frame = HostFrame::Message {
            message_id: m.message_id.clone(),
            mode: match d.operation() {
                ActiveAgentMessageOperation::Steer => MessageMode::Steer,
                ActiveAgentMessageOperation::Queue => MessageMode::Queue,
            },
            from: m.sender_session_id.clone(),
            text: m.text.to_string(),
        };
        let id = m.message_id.clone();
        let (tx, pending) = (self.tx.clone(), self.pending.clone());
        let admitted = d.commit_admission(|| {
            pending.lock().insert(id.clone());
            tx.send(frame).is_ok()
        });
        let out = match admitted {
            Some(true) => ActiveMessageAdmission::Admitted,
            None => ActiveMessageAdmission::Rejected,
            Some(false) => {
                pending.lock().remove(&id);
                ActiveMessageAdmission::ChannelClosed
            }
        };
        Box::pin(std::future::ready(out))
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }

    fn interrupt(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && self.tx.send(HostFrame::Interrupt).is_ok()
    }

    fn accepts_native_message(&self, m: &AgentMailboxMessage) -> bool {
        !m.native.as_ref().is_some_and(|n| n.encrypted)
    }

    fn deliver_initial_message(&self, m: &AgentMailboxMessage) -> bool {
        self.followup(m)
    }

    fn deliver_followup(&self, m: &AgentMailboxMessage) -> bool {
        self.followup(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_tools::implementations::grok_build::task::types::{
        AgentMailboxMessageKind, NativeAgentMessage,
    };

    fn sample_mailbox_msg(encrypted: bool) -> AgentMailboxMessage {
        AgentMailboxMessage {
            message_id: "m-1".into(),
            team_scope_id: "scope-1".into(),
            from_agent_id: "/root".into(),
            to_agent_id: "/root/worker".into(),
            kind: AgentMailboxMessageKind::NativeMessage,
            body: "check line 42".into(),
            created_at_ms: 1,
            native: Some(NativeAgentMessage {
                author: "/root".into(),
                recipient: "/root/worker".into(),
                encrypted,
                trigger_prompt_id: None,
            }),
        }
    }

    #[test]
    fn t3_followup_and_encrypted_rejection() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress = Arc::new(Mutex::new(SubagentProgress::default()));
        let cancel = CancellationToken::new();
        let closed = Arc::new(AtomicBool::new(false));
        let ctrl = ModChildControl::new(tx, pending.clone(), progress, true, cancel, closed);

        // Encrypted message rejected
        assert!(!ctrl.accepts_native_message(&sample_mailbox_msg(true)));
        assert!(!ctrl.deliver_followup(&sample_mailbox_msg(true)));
        assert!(pending.lock().is_empty());

        // Plaintext message delivered as Followup and tracked in pending
        assert!(ctrl.deliver_followup(&sample_mailbox_msg(false)));
        assert!(pending.lock().contains("m-1"));
        let frame = rx.try_recv().expect("frame");
        assert_eq!(
            frame,
            HostFrame::Message {
                message_id: "m-1".into(),
                mode: MessageMode::Followup,
                from: "/root".into(),
                text: "check line 42".into(),
            }
        );
    }
}
