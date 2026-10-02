//! Tool dispatch helpers for `SessionActor`.
//! Covers `dispatch_tool` and its lock and display helpers, direct bash-mode execution, and tool argument parse-error formatting.

use super::*;
use std::path::PathBuf;
use xai_grok_sampling_types::conversation::tool_search::MAX_SUMMARY_QUERY_BYTES;

/// Number of output lines to show in final bash mode output summary
const BASH_MODE_FINAL_OUTPUT_LINES: usize = 10;
const BASH_MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Phase 2: dispatch a tool call through [`WorkspaceOps::call_tool`].
///
/// Agent sessions always use local workspace ops (in-process toolset).
pub(super) async fn dispatch_tool(
    workspace_ops: &xai_grok_workspace::WorkspaceOps,
    prepared: &PreparedToolCall,
    session_id: &str,
) -> Result<ToolRunResult, xai_tool_runtime::ToolError> {
    tracing::debug!(
        tool = %prepared.tool_name,
        call_id = %prepared.tool_call_id.0,
        session = %session_id,
        mode = "local",
        "dispatch_tool"
    );
    workspace_ops
        .call_tool(
            &prepared.tool_name,
            prepared.parsed_args.clone(),
            &prepared.tool_call_id.0,
            Some(session_id),
        )
        .await
}

/// First string-valued argument among `keys`, in priority order.
fn str_arg<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k)?.as_str())
}

/// Extract the workspace path that a tool call targets, to serialize concurrent same-file edits inside `execute_tool_calls`.
/// `file_path`: grok_build (`search_replace`), opencode (`EditTool`, `WriteTool`, `ReadTool`), codex (`read_file`).
/// `target_directory` is deliberately omitted: a directory listing isn't an edit and must not share a file lock.
pub(super) fn lock_path_for_args(args: &serde_json::Value, cwd: &Path) -> Option<String> {
    let input = Path::new(str_arg(args, &["file_path", "path", "target_file"])?);
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        cwd.join(input)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    let lock_path = canonicalize_existing_ancestor(&normalized).unwrap_or(normalized);
    Some(lock_path.to_string_lossy().into_owned())
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut canonical) = dunce::canonicalize(ancestor) {
            suffix.reverse();
            canonical.extend(suffix);
            return Some(canonical);
        }
        suffix.push(ancestor.file_name()?.to_owned());
        ancestor = ancestor.parent()?;
    }
}

/// Pull the path a read/list tool targets and classify it against the store.
/// Keys span harnesses: `read_file` uses `target_file`, grep uses `path`, `list_dir` uses `target_directory`.
/// The path grammar lives in `xai_compaction_transcript`.
pub(super) fn compaction_artifact_read(
    args: &serde_json::Value,
) -> Option<xai_compaction_transcript::CompactionArtifact> {
    let path = str_arg(
        args,
        &["target_file", "file_path", "path", "target_directory"],
    )?;
    xai_compaction_transcript::classify_compaction_path(path)
}

/// Map a backend-hosted tool name to a user-facing title, ACP ToolKind, and `raw_input` JSON for display in the pager's tool call UI.
/// The `raw_input` carries metadata that the pager's `tool_call_to_block()` uses to select the correct renderer.
pub(super) fn backend_tool_display(name: &str) -> (String, acp::ToolKind, serde_json::Value) {
    match name {
        "web_search" => (
            "Web search:".to_string(),
            acp::ToolKind::Search,
            serde_json::json!({"variant": "WebSearch", "backend": true}),
        ),
        "x_search" => (
            "X search:".to_string(),
            acp::ToolKind::Search,
            serde_json::json!({"variant": "XSearch", "backend": true}),
        ),
        n => (
            n.to_string(),
            acp::ToolKind::Other,
            serde_json::json!({"backend": true}),
        ),
    }
}

/// The backend reports each call's real success or failure in the payload's `status` field.
/// A `"failed"` status becomes [`acp::ToolCallStatus::Failed`]; any other or absent status stays `Completed`.
/// Consumers, notably the headless `streaming-messages-json` `web_search_tool_result_error` branch, see the real failure instead of `Completed`.
pub(super) fn backend_tool_call_status(result: Option<&serde_json::Value>) -> acp::ToolCallStatus {
    let failed = result
        .and_then(|r| r.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("failed");
    if failed {
        acp::ToolCallStatus::Failed
    } else {
        acp::ToolCallStatus::Completed
    }
}

/// Display form for a client-executed tool-discovery call: the query and limit are the whole call,
/// so `raw_input` is a JSON object holding them — `tool_search_call.arguments` is an object, and a
/// stringified argument blob would leave the pager nothing to render. `raw_input` carries the query
/// verbatim; only the title echo is bounded, the way the IR's own `text_summary` bounds it, because
/// a model-authored query of any length lands here.
pub(super) fn tool_search_display(
    query: &str,
    limit: u64,
) -> (String, acp::ToolKind, serde_json::Value) {
    let title = if query.len() > MAX_SUMMARY_QUERY_BYTES {
        format!(
            "Tool search: {:?}…",
            truncate_bytes(query, MAX_SUMMARY_QUERY_BYTES)
        )
    } else {
        format!("Tool search: {query:?}")
    };
    (
        title,
        acp::ToolKind::Search,
        serde_json::json!({"query": query, "limit": limit}),
    )
}

/// The search terminal is a total typed map onto the card's status.
/// `Error` is the item's terminal failure. `Unknown` — the IR's view state for a status spelling this
/// build does not know — maps to `Failed` too, and that is THIS CUT's decision, not the IR's: an
/// unmodelled terminal may not render as a success, and `Failed` is the only ACP status that says so
/// without claiming a terminal the wire never sent. The IR rules on a different question, and says so
/// itself — what a status outside its vocabulary means for PAIRING, answered on
/// `ToolSearchItem::has_unmodelled_status` in
/// `xai-grok-sampling-types/src/conversation/tool_search.rs`, where the view degrades to `Unknown`,
/// the bytes are untouched, and the pairing verdict routes the item away from both `Incomplete` and
/// `Paired`. Nothing there addresses a card's terminal status; this map borrows the STRUCTURE of that
/// refusal (an unresolvable status is never promoted to the good outcome), not its authority.
pub(super) fn tool_search_call_status(
    status: xai_grok_sampler::ToolSearchStatus,
) -> acp::ToolCallStatus {
    match status {
        xai_grok_sampler::ToolSearchStatus::InProgress => acp::ToolCallStatus::InProgress,
        xai_grok_sampler::ToolSearchStatus::Completed => acp::ToolCallStatus::Completed,
        xai_grok_sampler::ToolSearchStatus::Error | xai_grok_sampler::ToolSearchStatus::Unknown => {
            acp::ToolCallStatus::Failed
        }
    }
}

/// The word written into `raw_output.status`: the IR's own wire spelling, so the card never
/// invents a second vocabulary for a terminal the provider named.
///
/// `Unknown` has no word and returns `None`, and the consumer then OMITS the key. Stated verdict:
/// the IR's vocabulary is `in_progress` / `completed` / `error` and nothing else — the
/// `STATUSES_IN_VOCABULARY` array and its three `STATUS_*` consts in
/// `xai-grok-sampling-types/src/conversation/tool_search.rs` — and `ToolSearchStatus` deliberately
/// carries no `Serialize` precisely because a derived impl would emit `"unknown"`, "a status string
/// nothing in the corpus has ever carried", as counted over 235 parsed documents by that type's
/// no-`Serialize` corpus measurement block. That verdict is scoped to the corpus the IR measured,
/// not to every wire:
/// no captured `tool_search_*` item in it carries a `status` this enum cannot name, and
/// `"status": null` measures at 0 occurrences there, so `None` means omit the key — never null it, and
/// never invent a word for a terminal the corpus never showed. The decision is not lost: the ACP
/// status carries it, via [`tool_search_call_status`].
pub(super) fn tool_search_status_word(
    status: xai_grok_sampler::ToolSearchStatus,
) -> Option<&'static str> {
    match status {
        xai_grok_sampler::ToolSearchStatus::InProgress => Some("in_progress"),
        xai_grok_sampler::ToolSearchStatus::Completed => Some("completed"),
        xai_grok_sampler::ToolSearchStatus::Error => Some("error"),
        // No word for an unmodelled terminal — see the doc above; the key is omitted.
        xai_grok_sampler::ToolSearchStatus::Unknown => None,
    }
}

/// Expose the resolved model ID only when the backend actually routed elsewhere AND the catalog opted this model into checkpoint identity.
/// It checks the same `show_model_fingerprint` flag as the fingerprint itself, so one server-side setting governs both.
/// The client keeps no per-slug default.
pub(super) fn should_show_resolved_model(
    requested: &str,
    resolved: &str,
    show_checkpoint_identity: bool,
) -> bool {
    show_checkpoint_identity && requested != resolved
}

/// Resolve the shell name for the system prompt `Shell:` field.
/// Unix: basename of `$SHELL`.
/// Windows: name from the `detect_windows_shell` cascade (pwsh, then powershell.exe, then Git Bash, then cmd.exe), since `$SHELL` is absent.
pub(super) fn resolve_session_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL")
            .ok()
            .and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "bash".to_string())
    }

    #[cfg(not(unix))]
    {
        xai_grok_config::shell::detect_windows_shell()
            .name()
            .to_string()
    }
}

/// Key in `ToolError::details` that carries the HTTP status code.
/// Used by both error producers (image_gen, video_gen, test helpers) and the `is_auth_tool_error` classifier to avoid accidental key mismatch.
pub(crate) const HTTP_STATUS_DETAILS_KEY: &str = "status";

impl SessionActor {
    /// Extract the bash command from the prompt blocks if present in meta.
    /// Returns Some(command) if the prompt is a direct bash command, None otherwise.
    pub(super) fn extract_bash_command(prompt_blocks: &[acp::ContentBlock]) -> Option<String> {
        use crate::extensions::prompt_meta::PromptBlockMeta;
        for block in prompt_blocks {
            if let acp::ContentBlock::Text(text) = block
                && let Some(meta_val) = &text.meta
                && let Some(meta) = PromptBlockMeta::from_value(meta_val)
            {
                return meta.bash_command;
            }
        }
        None
    }

    /// Handle a direct bash command from bash mode.
    /// Runs the command with streaming output and sends updates to the TUI.
    pub(super) async fn handle_direct_bash_command(
        &self,
        _prompt_id: &str,
        command: String,
        prompt_blocks: &[acp::ContentBlock],
    ) -> PromptTurnResult {
        tracing::info!("Handling direct bash command");

        // Send user message chunks to scrollback (so the user sees their command)
        let model_id = self.current_model_id().await;
        let user_chunk_meta = serde_json::json!({ "modelId": model_id })
            .as_object()
            .cloned();
        for block in prompt_blocks.iter() {
            let update = acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(block.clone()).meta(user_chunk_meta.clone()),
            );
            let notification_meta = self.build_notification_meta();
            let _ = self
                .notifications
                .persistence_tx
                .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
                    acp::SessionNotification::new(self.session_info.id.clone(), update)
                        .meta(notification_meta.as_object().cloned()),
                ))));
        }

        // Persist the user message for session history
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::ContentChunk(PersistenceContentChunk::new(
                prompt_blocks.to_vec(),
            )));
        // Bash turns bypass `handle_prompt`'s commit point; the command is now in the ordered persistence stream, so a send-now may cancel this turn
        self.mark_front_message_committed().await;

        // Run the bash command with streaming enabled
        let tool_call_id = acp::ToolCallId::from(format!("bash-mode-{}", uuid::Uuid::new_v4()));

        // Send initial ToolCall to register with TUI

        use xai_grok_tools::types::ToolInput;
        // Use the stripped command as the description so the pager shows the real command (not a generic label) while satisfying the required field
        let title_command = xai_grok_tools::util::strip_redundant_session_cd(
            &command,
            self.tool_context.cwd.as_path(),
        );
        let tool_input = ToolInput::Bash(BashToolInput {
            command: command.clone(),
            timeout: None,
            description: title_command.clone().into_owned(),
            is_background: false,
        });
        // Bash mode has no model-issued wire name; resolve the toolset's execute tool by kind so the x.ai/tool identity still stamps
        let bash_marker = serde_json::json!({"bash_mode": true}).as_object().cloned();
        let exec_wire = {
            let agent = self.agent.borrow();
            agent
                .tool_bridge()
                .toolset()
                .tool_name_for_kind(xai_grok_tools::types::tool::ToolKind::Execute)
        };
        let bash_meta = match exec_wire {
            Some(wire) => self.stamp_tool_meta(bash_marker.clone(), &wire, Some(&tool_input)),
            None => bash_marker,
        };
        self.send_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(tool_call_id.clone(), format!("Execute `{title_command}`"))
                    .kind(acp::ToolKind::Execute)
                    .status(acp::ToolCallStatus::InProgress)
                    .content(Vec::new())
                    .locations(Vec::new())
                    .raw_input(serde_json::to_value(&tool_input).ok())
                    .meta(bash_meta),
            ),
            None,
        )
        .await;

        let request = TerminalRunRequest {
            tool_call_id: tool_call_id.clone(),
            command: command.clone(),
            cwd: self.tool_context.cwd.clone(),
            env: self.tool_context.session_env.as_ref().clone(),
            timeout: BASH_MODE_TIMEOUT,
            output_byte_limit: 1_048_576, // 1 MiB
            stream: true,                 // Enable streaming for bash mode
            output_file: None,            // No file logging for interactive bash mode
        };

        let result = self.tool_context.terminal.run(request).await;

        // Format the output
        let (output, exit_code, timed_out, signal) = match result {
            Ok(res) => (
                res.combined_output,
                res.exit_code.unwrap_or(-1),
                res.timed_out,
                res.signal,
            ),
            Err(e) => (format!("Error running command: {}", e), -1, false, None),
        };

        // Full stdout for the TUI; prompt/history keep a last-N tail so dumps do not inflate the next turn
        let full_output = output.trim_end().to_string();
        let lines: Vec<&str> = full_output.lines().collect();
        let total_lines = lines.len();
        let history_output = if total_lines > BASH_MODE_FINAL_OUTPUT_LINES {
            let start = total_lines - BASH_MODE_FINAL_OUTPUT_LINES;
            let last_lines = lines[start..].join("\n");
            format!("... ({} lines)\n{}", total_lines, last_lines)
        } else {
            full_output.clone()
        };

        let is_backgrounded = signal.as_deref() == Some("backgrounded");

        // Send final tool call update
        // For backgrounded commands, don't mark as completed/failed; let the background task do that
        if !is_backgrounded {
            let final_status = if exit_code == 0 && signal.is_none() {
                acp::ToolCallStatus::Completed
            } else {
                acp::ToolCallStatus::Failed
            };
            let bash_output = BashOutput {
                output_for_prompt: BashOutput::make_output_for_prompt(&history_output),
                output: full_output.as_bytes().to_vec(),
                exit_code,
                command: command.clone(),
                truncated: false,
                signal: signal.clone(),
                timed_out,
                description: None,
                current_dir: self.tool_context.cwd.to_string(),
                output_file: String::new(),
                total_bytes: full_output.len(),
                output_delta: None,
                was_bare_echo: false,
            };
            self.send_update(
                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                    tool_call_id,
                    acp::ToolCallUpdateFields::new()
                        .status(Some(final_status))
                        .raw_output(serde_json::to_value(ToolsToolOutput::Bash(bash_output)).ok()),
                )),
                None,
            )
            .await;
        }

        // No AgentMessageChunk summary is sent here: the execute block already shows the full output, so an agent copy would duplicate scrollback
        // Old sessions that persisted one still replay fine

        // Build a single user message for chat history that includes command, output, and exit code
        let user_message = format!(
            "I executed a terminal command: `{}`\n\nOutput:\n```\n{}\n```\n\n[exit code: {}]",
            command, history_output, exit_code
        );

        // Add to chat history as a user message only
        self.chat_state_handle
            .push_user_message(ConversationItem::user(&user_message));

        self.chat_state_handle.flush();

        let flush_error = self.flush_to_disk().await.err();
        self.disk_full_acp_error(flush_error.as_ref())?;

        let total_tokens = self.chat_state_handle.get_total_tokens().await;
        ok_end_turn(total_tokens, None)
    }
}

// ── Tool argument error formatting ─────────────────────────────────────

// `truncate_bytes` is the UTF-8-safe truncation helper from xai-grok-sampling-types

/// Maximum bytes of `raw_arguments` echoed in a parse-error tool_result.
/// The model already holds the full arguments in context, so a prefix plus the JSON error position is enough; echoing more grows every later turn.
/// A syntax error position past this limit points into truncated text, but the model still has the full arguments in context.
pub(crate) const MAX_ARGS_IN_ERROR: usize = 2_000;

/// Build the user-facing error message shown when tool arguments cannot be parsed.
/// The message is stored as a `tool_result` in the conversation history, so the model sees it on the very next turn.
/// Without the echoed original, the model would only see that empty object and have to regenerate all its work from scratch.
pub(super) fn build_tool_parse_error_message(
    function_name: &str,
    err: &xai_tool_runtime::ToolError,
    raw_arguments: &str,
) -> String {
    let mut msg = format!("Failed to parse arguments for tool `{function_name}`: {err}");

    if raw_arguments.is_empty() {
        return msg;
    }

    // Append the original arguments (capped) so the model knows what it sent.
    // Use truncate_bytes to avoid panicking on a multi-byte UTF-8 boundary.
    msg.push_str("\n\nYour original arguments:\n");
    let prefix = truncate_bytes(raw_arguments, MAX_ARGS_IN_ERROR);
    msg.push_str(prefix);
    if prefix.len() < raw_arguments.len() {
        msg.push_str("\n... (truncated)");
    }

    // If the arguments string is not valid JSON, append the exact position of the syntax error so the model can fix it directly
    // Use `IgnoredAny`: we only need the error, not a DOM
    if let Err(json_err) = serde_json::from_str::<serde::de::IgnoredAny>(raw_arguments) {
        msg.push_str(&format!(
            "\n\nNote: the arguments above contain invalid JSON — {json_err}\n\
             Please fix the syntax and retry."
        ));
    }

    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_sampler::ToolSearchStatus;
    use xai_grok_sampling_types::rs;

    fn web_search_payload(status: rs::WebSearchCallStatus) -> serde_json::Value {
        // Hand-built, then serialized through the same `serde_json::to_value(ws)` the sampler
        // calls on `BackendToolCallCompleted` (`xai-grok-sampler/src/stream/responses.rs`).
        // The action carries 0.42.1's `queries`; the sampler's minted sentinel carries the
        // deprecated empty `query` instead (`conversation::sentinel_web_search_action_json`).
        // `query` must be named because `WebSearchActionSearch` derives no `Default`
        // (async-openai 0.42.1 response.rs:2160), and the suppression is scoped to this
        // literal — not to the whole fixture — so a later deprecated use here still warns.
        let action = {
            #[allow(deprecated)]
            rs::WebSearchToolCallAction::Search(rs::WebSearchActionSearch {
                query: None,
                queries: Some(vec!["rust async runtime".to_string()]),
                sources: None,
            })
        };
        serde_json::to_value(rs::WebSearchToolCall {
            action: Some(action),
            id: "ws1".to_string(),
            status,
        })
        .expect("serialize web_search_call payload")
    }

    /// A backend web-search failure must map to ACP `Failed` so the headless `web_search_tool_result_error` branch is reachable in production.
    /// A completed call or an absent payload stays `Completed`.
    /// Exercises the real payload shape, not a hand-built status.
    #[test]
    fn backend_failed_web_search_maps_to_failed_status() {
        let failed = web_search_payload(rs::WebSearchCallStatus::Failed);
        assert_eq!(failed["status"], "failed", "wire field name is `status`");
        assert_eq!(
            backend_tool_call_status(Some(&failed)),
            acp::ToolCallStatus::Failed
        );

        let completed = web_search_payload(rs::WebSearchCallStatus::Completed);
        assert_eq!(
            backend_tool_call_status(Some(&completed)),
            acp::ToolCallStatus::Completed
        );

        assert_eq!(
            backend_tool_call_status(None),
            acp::ToolCallStatus::Completed
        );
    }

    /// The word the wire spells for a terminal, and the card status that word must produce.
    /// The match is exhaustive on purpose: widen `rs::WebSearchCallStatus` (0.42.1 already
    /// added `Incomplete` over the fork's four) and this stops compiling until someone rules
    /// on the new terminal instead of letting it default onto a finished card.
    ///
    /// Pinned as the consumer behaves TODAY, not as an aspiration. Its only producer is
    /// `SamplingEvent::BackendToolCallCompleted` (`sampling_events.rs:569-577`) and it
    /// compares one string, so every word but `failed` lands on `Completed`: `in_progress`
    /// and `searching` are names that event does not currently carry, and `Incomplete` is
    /// the terminal 0.42.1 added that nobody has ruled on yet. Changing an arm is a product
    /// decision — see `tool_search_call_status` above, where this crate's other search
    /// surface refuses that outcome to its own unmodelled terminal.
    fn web_search_status_contract(
        status: &rs::WebSearchCallStatus,
    ) -> (&'static str, acp::ToolCallStatus) {
        use rs::WebSearchCallStatus as S;
        match status {
            S::Failed => ("failed", acp::ToolCallStatus::Failed),
            S::InProgress => ("in_progress", acp::ToolCallStatus::Completed),
            S::Searching => ("searching", acp::ToolCallStatus::Completed),
            S::Completed => ("completed", acp::ToolCallStatus::Completed),
            S::Incomplete => ("incomplete", acp::ToolCallStatus::Completed),
        }
    }

    /// Every terminal the pinned dependency can name must reach a decided card status
    /// through the real serialized payload, so an added or renamed terminal cannot arrive as
    /// a silently-completed card. The word is pinned too: `backend_tool_call_status` reads
    /// only `status` and only ever against `"failed"`, so a renamed word would turn every
    /// real failure into a success.
    #[test]
    fn every_web_search_status_maps_to_a_decided_card_status() {
        for status in [
            rs::WebSearchCallStatus::InProgress,
            rs::WebSearchCallStatus::Searching,
            rs::WebSearchCallStatus::Completed,
            rs::WebSearchCallStatus::Failed,
            rs::WebSearchCallStatus::Incomplete,
        ] {
            let (word, expected) = web_search_status_contract(&status);
            let payload = web_search_payload(status.clone());
            assert_eq!(
                payload["status"], word,
                "`{status:?}` must serialize the wire word `{word}` under `status`"
            );
            assert_eq!(
                backend_tool_call_status(Some(&payload)),
                expected,
                "`{status:?}` must map to a decided card status"
            );
        }
    }

    /// A discovery card renders as a search (the same kind web/x search use) and carries the
    /// model's real query and limit. `tool_search_call.arguments` is a JSON object, so `raw_input`
    /// must be one too — never a stringified argument blob.
    #[test]
    fn tool_search_display_carries_query_limit_and_search_kind() {
        let (title, kind, raw_input) = tool_search_display("deploy the canary", 5);
        assert_eq!(kind, acp::ToolKind::Search);
        assert_eq!(
            title, r#"Tool search: "deploy the canary""#,
            "the title must quote the query the way the IR summary does"
        );
        assert_eq!(
            raw_input,
            serde_json::json!({"query": "deploy the canary", "limit": 5})
        );
    }

    /// The title echo mirrors the IR's `text_summary` form exactly — `truncate_bytes` first, `{:?}`
    /// second, ellipsis last (the `ToolSearchKind::Call` truncating arm of `text_summary` in
    /// `xai-grok-sampling-types/src/conversation/tool_search.rs`) — escape-after-truncate included.
    /// Parity with the IR is the requirement, not a tighter bound. The predicate is
    /// `> MAX_SUMMARY_QUERY_BYTES`, not `>=`, so a query of exactly the cap rides the untruncated arm
    /// — the same boundary the IR pins in its own test
    /// `a_query_of_exactly_the_cap_is_echoed_whole_and_unmarked`. `raw_input` is data, not an echo, so
    /// it keeps the query whole.
    #[test]
    fn tool_search_display_title_boundary_matches_the_ir_predicate() {
        let exact = "y".repeat(MAX_SUMMARY_QUERY_BYTES);
        let (title, _, raw_input) = tool_search_display(&exact, 8);
        assert_eq!(
            title,
            format!(r#"Tool search: {exact:?}"#),
            "at exactly the cap the whole query is echoed, with no truncation marker"
        );
        assert_eq!(
            raw_input.get("query").and_then(serde_json::Value::as_str),
            Some(exact.as_str())
        );

        let over = format!("{exact}z");
        let (title, _, raw_input) = tool_search_display(&over, 8);
        assert!(
            !title.contains(&over),
            "the whole over-cap query must not reach the title"
        );
        assert_eq!(
            raw_input.get("query").and_then(serde_json::Value::as_str),
            Some(over.as_str()),
            "only the title echo is bounded; raw_input must carry the whole over-cap query"
        );
    }

    /// The ORDER the two docs above claim — `truncate_bytes` first, `{:?}` second, exactly like the
    /// IR's `text_summary` (its `ToolSearchKind::Call` truncating arm, in
    /// `xai-grok-sampling-types/src/conversation/tool_search.rs`) — gets its own falsifier here,
    /// because neither test above can see it: the exact-cap arm never enters the truncating branch,
    /// and the bound in the test below is an upper bound that a shorter string also satisfies. A query
    /// of exactly `MAX_SUMMARY_QUERY_BYTES` ordinary bytes followed by ONE control byte must be cut
    /// down to the pad BEFORE anything escapes it, so the escaped echo is the closed quoted run of
    /// exactly `MAX_SUMMARY_QUERY_BYTES` `y` characters. Escape first and the observable this test
    /// refuses is a payload of one `y` FEWER with its closing quote eaten, against the fully closed
    /// run of `MAX_SUMMARY_QUERY_BYTES` `y` the cut-first order renders. The byte budget is the cause,
    /// not the escape: escaping first spends the OPENING QUOTE inside the budget, so one `y` fewer
    /// fits and the closing quote falls beyond the cut. Computed at the cap's current value of 200 for
    /// a pad of 200 `y` plus one `\n` — escaped in full the string is 204 bytes and cut to 200 it is
    /// `"` plus 199 `y`; cut first and then escaped it is `"` plus 200 `y` plus `"`, 202 bytes. The
    /// `\n` expansion is not what moves the cut: it sits at indices 201-202 of the fully escaped
    /// string, beyond the 200-byte window.
    #[test]
    fn tool_search_display_truncates_the_title_before_escaping_it() {
        let pad = "y".repeat(MAX_SUMMARY_QUERY_BYTES);
        let query = format!("{pad}\n");

        let (title, _, _) = tool_search_display(&query, 6);
        assert_eq!(
            title,
            format!("Tool search: {pad:?}…"),
            "the cut must precede the escape: the echo has to be the whole pad, quoted"
        );
        // That whole-title equality is the whole falsifier, so nothing is derived from it here. It
        // refuses the escape-first shape directly: escaping first spends the OPENING QUOTE inside
        // the byte budget, so one `y` fewer fits and the cut eats the CLOSING quote — the echo would
        // be `"` plus 199 `y`, not the closed quoted run of `MAX_SUMMARY_QUERY_BYTES` `y` this
        // equality demands. Stripping the prefix, the ellipsis and the quotes in order to re-measure
        // the payload, or asserting that no control byte survived the cut, would each be entailed by
        // an equality that had already passed — and a test stops at its first panic, so such an
        // assertion can never be the one that is seen to fail.
    }

    /// The cap itself, proven against a bound that is actually true. `truncate_bytes` runs BEFORE
    /// `{:?}`, so a query of control bytes legitimately yields a title longer than
    /// `MAX_SUMMARY_QUERY_BYTES`: Debug escaping expands one control byte to at most 6 chars
    /// (`\u{1b}`), and the quotes and ellipsis are literal. The honest bound is therefore the prefix
    /// + `MAX * 6` escaped bytes + the two quotes + the ellipsis. Drop `truncate_bytes` and the
    /// escaped echo of this 20x-over-cap query blows straight through it.
    ///
    /// That query only exercises a 2-byte escape (`\n`), so it lands well inside the bound. The last
    /// case is the tight one: ESC is the worst escape Debug produces — `\u{1b}`, six bytes for one
    /// query byte — so a cap+1 ESC query must land EXACTLY on the bound. An upper bound that a
    /// shorter string also satisfies cannot catch a bound that drifted loose; this equality can.
    ///
    /// What neither query here can carry is a WHOLE-ECHO falsifier: `{:?}` escapes every one of their
    /// bytes, so the raw query can never appear in the title and a `contains` check would hold for any
    /// implementation that keeps the escape. That is a statement about `contains` alone, not about these
    /// queries' reach — the bound assertion below does falsify "truncation dropped, echo intact", and
    /// does so as a LENGTH failure: remove `truncate_bytes` and this 20x-over-cap `\n` query echoes 8018
    /// escaped bytes (4 000 newlines, two escaped bytes each, against the cap of 200), and the bound
    /// fires, reading "title echoed 8018 bytes, over the 1218-byte escape-aware bound".
    /// The whole-echo falsifier lives in `tool_search_display_title_boundary_matches_the_ir_predicate`,
    /// whose over-cap query is plain ASCII and so survives escaping whole. A plain needle over here
    /// would restate it, and would be the weaker of the two: widen the cap by one and this test's own
    /// bound equality still fires, while a 2x-over-cap plain query is cut to the widened cap and passes.
    #[test]
    fn tool_search_display_caps_an_escaping_query_within_the_derived_bound() {
        let query = "\n".repeat(MAX_SUMMARY_QUERY_BYTES * 20);
        let (title, _, _) = tool_search_display(&query, 4);
        let bound = "Tool search: ".len() + MAX_SUMMARY_QUERY_BYTES * 6 + "\"\"…".len();
        assert!(
            title.len() <= bound,
            "title echoed {} bytes, over the {bound}-byte escape-aware bound",
            title.len()
        );
        assert!(
            title.ends_with('…'),
            "an over-cap query must be marked truncated: {title}"
        );

        let esc = "\u{1b}".repeat(MAX_SUMMARY_QUERY_BYTES + 1);
        let (title, _, _) = tool_search_display(&esc, 4);
        assert_eq!(
            title.len(),
            bound,
            "the worst-case escape must land exactly on the derived bound, got {} bytes: {title}",
            title.len()
        );
    }

    /// The search status reaches ACP through a total typed map over the IR's whole vocabulary.
    /// Neither a failure nor a status this build cannot name may render as a completed card.
    #[test]
    fn tool_search_status_maps_onto_acp_tool_call_status() {
        assert_eq!(
            tool_search_call_status(ToolSearchStatus::InProgress),
            acp::ToolCallStatus::InProgress
        );
        assert_eq!(
            tool_search_call_status(ToolSearchStatus::Completed),
            acp::ToolCallStatus::Completed
        );
        assert_eq!(
            tool_search_call_status(ToolSearchStatus::Error),
            acp::ToolCallStatus::Failed
        );
        assert_eq!(
            tool_search_call_status(ToolSearchStatus::Unknown),
            acp::ToolCallStatus::Failed,
            "an unrecognised terminal may not read as success"
        );
    }

    /// Every word written into `raw_output.status` must be the IR's own spelling: parsing it back
    /// with `from_wire` has to return the same variant, so a word that drifts to a second vocabulary
    /// (`"failed"`) fails here rather than reaching a card.
    ///
    /// `Unknown` is deliberately excluded. It carries no word (it maps to `None`, pinned by
    /// [`unknown_terminal_writes_no_status_word`]), and a round-trip would be vacuous for it anyway:
    /// `from_wire` ends in `Some(_) => Self::Unknown` / `None => Self::Unknown` (the tail of
    /// `ToolSearchStatus::from_wire` in
    /// `xai-grok-sampling-types/src/conversation/tool_search.rs`), a lossy catch-all that
    /// returns `Unknown` for ANY string, so any invented word would appear to "round-trip". The three
    /// variants looped here each have an exact arm, so a drifted word dies.
    #[test]
    fn tool_search_status_words_round_trip_through_the_ir_parser() {
        for status in [
            ToolSearchStatus::InProgress,
            ToolSearchStatus::Completed,
            ToolSearchStatus::Error,
        ] {
            let word = tool_search_status_word(status)
                .expect("a modelled terminal always has the IR's word");
            assert_eq!(
                ToolSearchStatus::from_wire(Some(word)),
                status,
                "`{word}` is not the IR's spelling for {status:?}"
            );
        }
    }

    /// `Unknown` writes no status word at all. The IR measures `"unknown"` at zero occurrences across
    /// 235 parsed documents and refuses a derived `Serialize` for exactly that reason — the
    /// no-`Serialize` corpus measurement block in `ToolSearchStatus`'s doc, at
    /// `xai-grok-sampling-types/src/conversation/tool_search.rs`; `"status": null` measures
    /// at 0 there too. Scope is that corpus, not every wire: it says no captured `tool_search_*` item
    /// carries a `status` the IR cannot name, not that no provider could ever send one. Restoring
    /// `Unknown => Some("unknown")` dies here, on the `Unknown` assertion first below, and again
    /// end-to-end in `unknown_tool_search_completion_omits_the_status_key_and_fails_the_card`.
    /// A scoped battery has to ask for this test by something its name actually contains: no
    /// `tool_search` filter matches `unknown_terminal_writes_no_status_word`, so a
    /// `-lib tool_search` run never selects THIS unit test. It does not run clean either — the
    /// end-to-end test named above carries `tool_search` in its own name, so it is selected and
    /// fails — but the report names the consumer, not this map. Filter on `status_word` to
    /// localise the failure here.
    /// The three modelled words are asserted here as well, so a mutation that returned `None` for the
    /// whole vocabulary could not satisfy the `Unknown` pin by erasing the rest of it.
    #[test]
    fn unknown_terminal_writes_no_status_word() {
        assert_eq!(tool_search_status_word(ToolSearchStatus::Unknown), None);
        assert_eq!(
            tool_search_status_word(ToolSearchStatus::InProgress),
            Some("in_progress")
        );
        assert_eq!(
            tool_search_status_word(ToolSearchStatus::Completed),
            Some("completed")
        );
        assert_eq!(
            tool_search_status_word(ToolSearchStatus::Error),
            Some("error")
        );
    }
}
