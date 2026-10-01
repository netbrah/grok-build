use xai_grok_sampling_types::ConversationItem;

/// Select a recent window of the conversation history for the flush model.
/// Starts with the last `recent_message_count` messages, then expands backward to the nearest `User` message.
/// The window therefore always starts on a user boundary and may be larger than `recent_message_count`.
pub fn select_flush_window(
    messages: Vec<ConversationItem>,
    recent_message_count: usize,
) -> Vec<ConversationItem> {
    let messages: Vec<_> = messages
        .into_iter()
        .filter(|item| !matches!(item, ConversationItem::System(_)))
        .collect();

    let total = messages.len();
    // `start` is used as an INDEX below, so it has to name a real row: with
    // `recent_message_count == 0` (a configured zero, or any caller that means "no
    // window") `saturating_sub` leaves it at `total` and the backward walk's
    // `messages[start]` panics with index-out-of-bounds. Clamping to the last row keeps
    // the documented contract instead — the walk then expands that row back to its
    // `User` boundary. Found by the apex-waj.21 F-4 cut-site gate, which drives every
    // `recent_message_count` from 0 up.
    let mut start = total
        .saturating_sub(recent_message_count)
        .min(total.saturating_sub(1));
    // Two contracts, both kept: the window starts on a `User` row, and it never starts
    // between the halves of a discovery pair. The backward walk stops ON a `User` row,
    // and a `User` row can sit between the halves (the provider mints `call_id: null`
    // rows with no join key, and synthetic injections land anywhere), so the snap and
    // the walk run against each other to a fixed point. Both only move DOWN, so each
    // pass strictly decreases `start` until the two agree (apex-waj.21 review F-4;
    // ruling apex-waj.18 A-26: a bare `tool_search_output` is the provider desync).
    loop {
        while start > 0 && !matches!(messages[start], ConversationItem::User(_)) {
            start -= 1;
        }
        let snapped = xai_grok_sampling_types::conversation::tool_search::snap_index_over_discovery_pairs(
            &messages, start,
        );
        if snapped == start {
            break;
        }
        start = snapped;
    }
    messages.into_iter().skip(start).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_flush_window_expands_to_user_boundary() {
        let mut messages = vec![ConversationItem::user("early question")];
        for i in 0..20 {
            messages.push(ConversationItem::assistant(format!("response {i}")));
        }

        let window = select_flush_window(messages, 20);

        assert_eq!(window.len(), 21);
        assert!(matches!(window[0], ConversationItem::User(_)));
    }

    #[test]
    fn test_select_flush_window_filters_system_messages() {
        let messages = vec![
            ConversationItem::system("you are helpful"),
            ConversationItem::user("hi"),
            ConversationItem::assistant("hello"),
        ];

        let window = select_flush_window(messages, 20);

        assert!(
            window
                .iter()
                .all(|item| !matches!(item, ConversationItem::System(_)))
        );
        assert_eq!(window.len(), 2);
    }

    #[test]
    fn test_select_flush_window_short_conversation() {
        let messages = vec![
            ConversationItem::user("hi"),
            ConversationItem::assistant("hello"),
        ];

        let window = select_flush_window(messages, 20);

        assert_eq!(window.len(), 2);
        assert!(matches!(window[0], ConversationItem::User(_)));
    }

    #[test]
    fn flush_window_preserves_agent_provenance_until_request_projection() {
        let human = ConversationItem::user("human request");
        let agent = ConversationItem::agent_message("agent context");
        let window = select_flush_window(vec![human.clone(), agent], 20);
        let projected =
            xai_chat_state::compaction_utils::ModelRequestHistory::from_raw(window).into_items();

        assert_eq!(projected[0].text_content(), human.text_content());
        assert_eq!(
            projected[1].text_content(),
            format!(
                "{}\nagent context",
                xai_chat_state::compaction_utils::AGENT_MESSAGE_MODEL_LABEL
            )
        );
    }

    /// apex-waj.21 review F-4: the backward walk stops ON a `User` row, and that row
    /// can sit between the halves of a discovery pair. The flush model then gets a bare
    /// `tool_search_output` — an answer whose call it never saw — which is the provider
    /// desync the pair exists to prevent. The start is re-snapped after the walk.
    #[test]
    fn flush_window_start_never_lands_inside_a_discovery_pair() {
        use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
        let discovery = |raw: serde_json::Value| ConversationItem::Discovery {
            item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
        };
        let messages = vec![
            ConversationItem::user("find the crm tools"),
            ConversationItem::assistant("searching"),
            discovery(serde_json::json!({
                "type": "tool_search_call",
                "id": "tsc_flush_1",
                "call_id": "call_flush_1",
                "status": "completed",
                "execution": "client",
                "arguments": { "query": "crm", "limit": 1 }
            })),
            ConversationItem::user("still there?"),
            discovery(serde_json::json!({
                "type": "tool_search_output",
                "id": "tso_flush_1",
                "call_id": "call_flush_1",
                "status": "completed",
                "execution": "client",
                "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
            })),
            ConversationItem::assistant("found them"),
        ];
        // recent_message_count = 2 → start 4, which is not a User; the walk back stops
        // on the User at 3, i.e. between the call (2) and its output (4).
        let window = select_flush_window(messages, 2);
        let kept = window
            .iter()
            .filter(|item| item.discovery().is_some())
            .count();
        assert_eq!(
            kept, 2,
            "the flush window must widen over the pair, never send half of it: {window:?}"
        );
        assert!(
            matches!(window.first(), Some(ConversationItem::User(_))),
            "the user-boundary contract of the window still holds: {window:?}"
        );
    }

    /// The F-4 cut-site gate drives every `recent_message_count` from 0 upward and
    /// found a latent panic that pre-dated this cut: `start` was used as an index into
    /// the filtered history, and `total.saturating_sub(0)` == `total`, so the backward
    /// walk indexed one past the last row (`index out of bounds: the len is 6 but the
    /// index is 6`). A configured zero must select a window, not abort the flush model.
    #[test]
    fn zero_recent_message_count_selects_a_window_instead_of_indexing_past_the_end() {
        let messages = vec![
            ConversationItem::user("the question"),
            ConversationItem::assistant("the answer"),
            ConversationItem::user("the next question"),
            ConversationItem::assistant("the next answer"),
        ];
        let window = select_flush_window(messages.clone(), 0);
        assert!(
            matches!(window.first(), Some(ConversationItem::User(_))),
            "a zero window still starts on a user boundary: {window:?}"
        );
        // The two degenerate histories the walk has to survive as well.
        assert!(select_flush_window(vec![], 0).is_empty());
        let one_row = vec![ConversationItem::assistant("no user row at all")];
        assert_eq!(select_flush_window(one_row, 0).len(), 1);
    }
}
