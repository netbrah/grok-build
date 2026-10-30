use std::io;

use tokio::sync::{mpsc, oneshot};
use xai_chat_state::{ChatPersistence, StrictAppendAck, StrictAppendError};
use xai_grok_sampling_types::ConversationItem;

use super::persistence::PersistenceMsg;

/// Production `ChatPersistence` that sends to the existing session persistence channel.
pub(crate) struct ChannelChatPersistence {
    tx: mpsc::UnboundedSender<PersistenceMsg>,
}

impl ChannelChatPersistence {
    pub(crate) fn new(tx: mpsc::UnboundedSender<PersistenceMsg>) -> Self {
        Self { tx }
    }
}

impl ChatPersistence for ChannelChatPersistence {
    fn persist_native_checkpoint_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
        pending: xai_chat_state::persistence::native::NativePendingProtection,
    ) -> oneshot::Receiver<
        Result<
            xai_chat_state::persistence::native::NativeAppendAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (respond_to, receiver) = oneshot::channel();
        let _ = self.tx.send(PersistenceMsg::PersistNativeCheckpointAndAck {
            write,
            pending,
            respond_to,
        });
        receiver
    }

    fn persist_native_projection_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeProjectionWrite,
    ) -> oneshot::Receiver<
        Result<
            xai_grok_sampling_types::conversation::native_search_history::NativeCompoundAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (respond_to, receiver) = oneshot::channel();
        let _ = self
            .tx
            .send(PersistenceMsg::PersistNativeProjectionAndAck { write, respond_to });
        receiver
    }

    fn read_native_history(
        &mut self,
    ) -> oneshot::Receiver<
        Result<
            xai_chat_state::persistence::native::NativeRead,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (respond_to, receiver) = oneshot::channel();
        let _ = self
            .tx
            .send(PersistenceMsg::ReadNativeHistory { respond_to });
        receiver
    }
    fn persist_native_generation_and_ack(
        &mut self,
        write: xai_chat_state::persistence::native::NativeWrite,
    ) -> oneshot::Receiver<
        Result<
            xai_chat_state::persistence::native::NativeAppendAck,
            xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable,
        >,
    > {
        let (respond_to, receiver) = oneshot::channel();
        let _ = self
            .tx
            .send(PersistenceMsg::PersistNativeGenerationAndAck { write, respond_to });
        receiver
    }
    fn persist_message(&mut self, item: &ConversationItem) {
        let _ = self.tx.send(PersistenceMsg::Chat(item.clone()));
    }

    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .tx
            .send(PersistenceMsg::AppendCwdSwitchAndAck {
                item: item.clone(),
                respond_to: reply,
            })
            .is_err()
        {
            let (reply, receiver) = oneshot::channel();
            let _ = reply.send(Err(StrictAppendError::Indeterminate(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "session persistence actor unavailable; retry by generation",
            ))));
            return receiver;
        }
        receiver
    }

    fn replace_history(&mut self, items: &[ConversationItem]) {
        let _ = self
            .tx
            .send(PersistenceMsg::ReplaceChatHistory(items.to_vec()));
    }

    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>> {
        let (respond_to, receiver) = oneshot::channel();
        if self
            .tx
            .send(PersistenceMsg::ReplaceChatHistoryForStripAndAck {
                messages: items.to_vec(),
                respond_to,
            })
            .is_err()
        {
            let (reply, receiver) = oneshot::channel();
            let _ = reply.send(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "session persistence actor unavailable for strip rewrite",
            )));
            return receiver;
        }
        receiver
    }

    fn flush(&mut self) {
        let _ = self.tx.send(PersistenceMsg::Flush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channel_persistence_sends_chat_messages() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut persistence = ChannelChatPersistence::new(tx);
        let item = ConversationItem::user("test");
        persistence.persist_message(&item);
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, PersistenceMsg::Chat(_)));
    }

    #[tokio::test]
    async fn channel_persistence_sends_acknowledged_chat_append() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut persistence = ChannelChatPersistence::new(tx);
        let item = ConversationItem::working_directory_switch("moved", 1);
        let ack = persistence.persist_working_directory_switch_and_ack(&item);
        let msg = rx.recv().await.unwrap();
        let PersistenceMsg::AppendCwdSwitchAndAck {
            item: persisted,
            respond_to,
        } = msg
        else {
            panic!("expected acknowledged chat append");
        };
        assert_eq!(
            serde_json::to_vec(&persisted).unwrap(),
            serde_json::to_vec(&item).unwrap()
        );
        respond_to.send(Ok(StrictAppendAck::Appended)).unwrap();
        assert!(matches!(
            ack.await.unwrap().unwrap(),
            StrictAppendAck::Appended
        ));
    }

    #[tokio::test]
    async fn channel_persistence_sends_replace_history() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut persistence = ChannelChatPersistence::new(tx);
        persistence.replace_history(&[ConversationItem::system("compacted")]);
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, PersistenceMsg::ReplaceChatHistory(_)));
    }

    #[tokio::test]
    async fn channel_persistence_sends_acked_strip_rewrite() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut persistence = ChannelChatPersistence::new(tx);
        let ack = persistence.replace_history_for_strip_and_ack(&[ConversationItem::system("s")]);
        let msg = rx.recv().await.unwrap();
        let PersistenceMsg::ReplaceChatHistoryForStripAndAck { respond_to, .. } = msg else {
            panic!("expected acked strip rewrite, got {msg:?}");
        };
        respond_to.send(Ok(())).unwrap();
        assert!(ack.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn channel_persistence_acks_strip_error_when_actor_gone() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let mut persistence = ChannelChatPersistence::new(tx);
        let ack = persistence.replace_history_for_strip_and_ack(&[ConversationItem::system("s")]);
        assert!(
            ack.await.unwrap().is_err(),
            "dead persistence actor must ack an error, not hang or succeed"
        );
    }

    #[tokio::test]
    async fn channel_persistence_sends_flush() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut persistence = ChannelChatPersistence::new(tx);
        persistence.flush();
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, PersistenceMsg::Flush));
    }
}
