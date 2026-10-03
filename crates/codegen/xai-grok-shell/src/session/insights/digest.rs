//! The pure half of `/insights`: transcript → [`SessionDigest`] (SDD §5, §13 item 1).
//!
//! Everything in this module is a function over in-memory inputs — no file I/O,
//! no config reads, no clock reads. The reader side (item 4's `scope.rs`) fills a
//! [`DigestInput`] from the on-disk store and supplies the facts only a reader can
//! know: [`ReaderFacts::time_degraded`] (was `updates.jsonl` missing/truncated?)
//! and [`ReaderFacts::unreadable_lines`] (how many lines were skipped). The digest
//! itself derives [`DegradedTier::NoTimeAxis`] from the *empty axis it is handed* —
//! the two agree on every well-formed store, and T-1 pins that agreement on all
//! eight fixtures (single-derivation rule: `time_degraded` is a reader-set input
//! fact, `NoTimeAxis` is the digest's own derivation).
//!
//! `projection_chars` is likewise caller-supplied: the §10 `project()` (item 4)
//! computes `len(project(transcript))`, and the digest stores the LENGTH only —
//! the projection string is never cached (SDD §5).
//!
//! Deliberately absent from this module: `project()` and the tag neutraliser
//! (item 4, SDD §13), aggregation (item 2), and orchestration (`handle_insights`,
//! item 5).

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol as acp;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use xai_grok_sampling_types::ConversationItem;

use crate::session::persistence::Summary;
use crate::session::storage::ReplayedUpdate;
use crate::session::usage_file::UsageSummary;

/// Cache-schema stamp for [`SessionDigest`]; a mismatch means recompute (SDD §5).
/// Adding or changing a digest field is a bump of this const.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Gap, in seconds, below which consecutive messages count as one continuous
/// activity stretch (SDD §5). A const, not a knob — the same treatment T-4 gives
/// `CONCURRENCY_WINDOW_SECS`.
pub const IDLE_GAP_SECS: i64 = 300;

/// One session, digested. Written to the cache; every field is computed, never
/// modelled (SDD §5).
///
/// No `PartialEq` is derived on purpose: T-1's golden asserts field-by-field, and
/// `transcript_sha256` / `cache_version` are compared separately (SDD §12 — the
/// two re-pin on every fixture edit and would make a derived `PartialEq` golden
/// churn on every fixture byte change).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SessionDigest {
    /// `CACHE_SCHEMA_VERSION`; mismatch => recompute.
    pub cache_version: u32,
    pub session_id: String,
    /// `Summary.info.cwd`
    pub cwd: String,
    pub git_root: Option<String>,
    /// `Summary.head_branch` — CC has no branch data.
    pub head_branch: Option<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// updated − started; NOT activity.
    pub wall_minutes: i64,
    /// Sum of inter-message gaps < [`IDLE_GAP_SECS`] (= 300), in whole minutes.
    pub activity_minutes: i64,
    pub user_turns: usize,
    pub assistant_turns: usize,
    pub tool_counts: BTreeMap<String, u32>,
    pub languages: BTreeMap<String, u32>,
    pub files_modified: u32,
    /// Transcript-derived (SDD §5's two arithmetic rules, pinned by T-5): a
    /// rewritten line counts as one removed AND one added, and a final line with
    /// no trailing newline is still one line. By design this includes rejected
    /// edits and disagrees with the live `workspace_ops.rs` counters.
    pub lines_added: u64,
    pub lines_removed: u64,
    /// `len(project(transcript))`. The digest stores the LENGTH only; the
    /// projection string is never cached — item 4's `label.rs` re-projects through
    /// the same pure `project()` in this file (SDD §13).
    pub projection_chars: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Ours, from `UsageSummary`.
    pub reasoning_tokens: u64,
    /// Ours: cache-hit rate is a real insight CC cannot have.
    pub cached_read_tokens: u64,
    /// Ours; `None` when `cost_is_partial`.
    pub cost_usd_ticks: Option<i64>,
    pub cost_is_partial: bool,
    pub tool_errors: u32,
    pub interruptions: u32,
    /// Last user chunk → first agent chunk, seconds; kept iff `2 < gap < 3600`.
    /// Needs `updates.jsonl` (chunk-granular — not request → first token).
    pub response_gaps_secs: Vec<u32>,
    /// UTC hour-of-day per timestamped chunk, in order.
    pub message_hours: Vec<u32>,
    /// From `updates.jsonl`, not `chat_history`.
    pub user_message_timestamps: Vec<DateTime<Utc>>,
    /// Reader-set input fact (SDD §5): `updates.jsonl` missing/truncated ⇒ every
    /// time-derived field above is empty.
    pub time_degraded: bool,
    pub model_ids: Vec<String>,
    pub uses_mcp: bool,
    pub uses_subagent: bool,
    /// `None` unless the `--show-prompts` rung was passed (or no user turn
    /// exists), then truncated (SDD §5). Absence of the key is structural: T-10(b)
    /// asserts the default rung carries no key, not `Some("[redacted]")`.
    pub first_prompt: Option<String>,
    /// Empty = digested at full strength. A SET — several can apply at once; push
    /// order is the fixed §5 order so T-1's golden is order-stable.
    pub degraded: Vec<DegradedTier>,
    /// Lines skipped in either input; the count is surfaced in the report footer.
    pub unreadable_lines: u32,
    /// SHA-256 of the raw `chat_history.jsonl` bytes; the invalidation key.
    pub transcript_sha256: String,
}

/// Why a session's digest cannot be trusted at full strength. A **set**, not a
/// single tier (SDD §5): the T-0 fixture is a `chat_format_version: 0` session
/// with **no** `updates.jsonl`, i.e. `LegacyFormat` *and* `NoTimeAxis` at once,
/// which a single-valued field cannot express. Empty = digestible at full
/// strength. Push order is fixed — `LegacyFormat`, `NoTimeAxis`,
/// `InputTruncated`, `ChunkCapHit`, `UnreadableLines`.
///
/// Non-empty does not suppress the digest — it is still written, and whether the
/// session is rendered or excluded is decided by SDD §9 rule 3, not by this
/// field's cardinality.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DegradedTier {
    /// `chat_format_version: 0` — `synthetic_reason`/`model_id` absent,
    /// `user_turns` unreliable.
    LegacyFormat,
    /// `updates.jsonl` missing or truncated — every time-derived field is empty.
    NoTimeAxis,
    /// The §10 projection exceeded `chunk_summarize.max_input_chars`: text was cut.
    InputTruncated,
    /// The projection needed more chunks than `max_chunk_calls_per_session`, so
    /// only the first N summaries reached the aggregate (SDD §6) — a different
    /// truncation from `InputTruncated`, with a different cause and footer line.
    ChunkCapHit,
    /// ≥1 line skipped in either input; the count lives in
    /// [`SessionDigest::unreadable_lines`].
    UnreadableLines,
}

/// The session's on-disk identity, as plain facts (SDD §5: the `Summary` fields).
#[derive(Debug, Clone)]
pub struct SessionFacts {
    pub session_id: String,
    /// `Summary.info.cwd`
    pub cwd: String,
    pub git_root: Option<String>,
    pub head_branch: Option<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `0` = legacy `ChatRequestMessage` rows, `1` = `ConversationItem` rows.
    /// Drives [`DegradedTier::LegacyFormat`].
    pub chat_format_version: u8,
}

impl SessionFacts {
    /// The plain facts out of a persisted [`Summary`] — item 4's `scope.rs` calls
    /// this on every enumerated session.
    #[must_use]
    pub fn from_summary(summary: &Summary) -> Self {
        Self {
            session_id: summary.info.id.0.to_string(),
            cwd: summary.info.cwd.clone(),
            git_root: summary.git_root_dir.clone(),
            head_branch: summary.head_branch.clone(),
            started_at: summary.created_at,
            updated_at: summary.updated_at,
            chat_format_version: summary.chat_format_version,
        }
    }
}

/// The parsed transcript content (SDD §5: the `chat_history.jsonl` input).
#[derive(Debug, Clone)]
pub struct TranscriptContent {
    /// Parsed items in file order; unreadable lines are absent (they are counted
    /// in [`ReaderFacts::unreadable_lines`], never quarantined).
    pub items: Vec<ConversationItem>,
    /// The raw `chat_history.jsonl` bytes — `transcript_sha256` digests exactly
    /// these.
    pub raw_bytes: Vec<u8>,
    /// `project(items).chars().count()`, computed by the caller (item 4
    /// re-projects through the same pure `project()` at label time; the digest
    /// stores the length only, never the string — SDD §5).
    pub projection_chars: usize,
}

/// Facts only the reader can know (SDD §5: the tolerant-reads rule).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReaderFacts {
    /// `true` when `updates.jsonl` was missing, truncated, or wedged past the
    /// skip cap. Reader-set INPUT: the digest itself cannot distinguish
    /// "file absent" from "file present but empty of timestamped chunks" — it
    /// derives [`DegradedTier::NoTimeAxis`] from the axis it was handed, and T-1
    /// asserts the two agree on every fixture.
    pub time_degraded: bool,
    /// Lines skipped (unparseable) in either input. Never fatal — skip and count;
    /// never quarantine (SDD §5).
    pub unreadable_lines: u32,
}

/// The SDD §6 `chunk_summarize` knobs. The two `max_input_chars`/`chunk_chars`
/// tiers drive the digest's projection; `threshold_chars` is the third knob in
/// the same `chunk_summarize` group — the projection length above which
/// map-reduce kicks in, read by the LABELLING stage (SOT §6:724). The digest
/// never reads it: a `--no-model` run reports what WOULD have been chunked
/// without reading the knob, so the carrier lives here and the labeller takes
/// it from here.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionCaps {
    /// `insights.chunk_summarize.max_input_chars` (shipped default 400 000).
    pub max_input_chars: usize,
    /// `insights.chunk_summarize.chunk_chars` (shipped default 25 000) —
    /// characters, not bytes.
    pub chunk_chars: usize,
    /// `insights.max_chunk_calls_per_session` (shipped default 4).
    pub max_chunk_calls: usize,
    /// `insights.chunk_summarize.threshold_chars` (shipped default 30 000) —
    /// the projection length above which map-reduce kicks in, read by the
    /// labelling stage (SOT §6:724). The digest never reads it.
    pub threshold_chars: usize,
}

impl Default for ProjectionCaps {
    /// The shipped §6 defaults.
    fn default() -> Self {
        Self {
            max_input_chars: 400_000,
            chunk_chars: 25_000,
            max_chunk_calls: 4,
            threshold_chars: 30_000,
        }
    }
}

/// Everything [`digest_one`] needs. Plain structs — the reader fills it; nothing
/// here does I/O.
#[derive(Debug)]
pub struct DigestInput {
    pub session: SessionFacts,
    pub content: TranscriptContent,
    /// The time axis: the user/agent message chunks the replay stream yielded, in
    /// file order. Empty when `updates.jsonl` is missing or yielded nothing.
    pub time_axis: Vec<ReplayedUpdate>,
    /// `usage.json` totals; [`UsageSummary::default()`] when the file is absent.
    pub usage: UsageSummary,
    pub reader: ReaderFacts,
    pub caps: ProjectionCaps,
    /// `--show-prompts` rung input (SDD §5): `None` unless the caller is on the
    /// `Prompts` rung or above, then the truncated first user turn.
    pub first_prompt: Option<String>,
}

/// The ONE user-turn oracle (SDD §5): a real user message is a `User` item with
/// no `synthetic_reason` and non-empty content. Used by the counter here,
/// item 4's projector, and the facet input alike — one definition, three call
/// sites. CC's three divergent predicates are deliberately not copied: our
/// numbers may differ from CC's, and we say so rather than tune to match.
#[must_use]
pub fn is_user_turn(item: &ConversationItem) -> bool {
    matches!(
        item,
        ConversationItem::User(user)
            if user.synthetic_reason.is_none() && !user.content.is_empty()
    )
}

/// The pure core (SDD §5, §13 item 1): transcript → digest.
///
/// Returns `None` — the session is **excluded** from the report, not badged
/// (SDD §9 rule 3) — when the content input yielded no readable transcript at
/// all. A maximally degraded transcript (every [`DegradedTier`] at once) still
/// returns `Some`: truncation degrades, it never excludes.
#[must_use]
pub fn digest_one(input: &DigestInput) -> Option<SessionDigest> {
    let session = &input.session;
    let content = &input.content;
    let caps = &input.caps;

    // SDD §9 rule 3: no readable transcript at all is excluded (None), not
    // badged. A maximally degraded one still renders.
    if content.items.is_empty() {
        return None;
    }

    // The §5 tier set, pushed in its fixed declaration order.
    let mut degraded: Vec<DegradedTier> = Vec::new();
    if session.chat_format_version == 0 {
        degraded.push(DegradedTier::LegacyFormat);
    }
    if input.time_axis.is_empty() {
        degraded.push(DegradedTier::NoTimeAxis);
    }
    if content.projection_chars > caps.max_input_chars {
        degraded.push(DegradedTier::InputTruncated);
    }
    if caps.chunk_chars > 0
        && content.projection_chars.div_ceil(caps.chunk_chars) > caps.max_chunk_calls
    {
        degraded.push(DegradedTier::ChunkCapHit);
    }
    if input.reader.unreadable_lines > 0 {
        degraded.push(DegradedTier::UnreadableLines);
    }

    let mut user_turns = 0usize;
    let mut assistant_turns = 0usize;
    let mut tool_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut tool_errors = 0u32;
    let mut interruptions = 0u32;
    let mut model_ids: Vec<String> = Vec::new();
    let mut uses_mcp = false;
    let mut uses_subagent = false;
    let mut modified_files: BTreeSet<String> = BTreeSet::new();
    let mut lines_added = 0u64;
    let mut lines_removed = 0u64;

    for item in &content.items {
        match item {
            ConversationItem::User(user) => {
                if is_user_turn(item) {
                    user_turns += 1;
                }
                if user.prior_turn_interrupt.is_some() {
                    interruptions += 1;
                }
            }
            ConversationItem::Assistant(asst) => {
                assistant_turns += 1;
                if let Some(model) = &asst.model_id
                    && !model_ids.contains(model)
                {
                    model_ids.push(model.clone());
                }
                for call in &asst.tool_calls {
                    *tool_counts.entry(call.name.clone()).or_insert(0) += 1;
                    if call.name.starts_with("mcp__") {
                        uses_mcp = true;
                    }
                    match call.name.as_str() {
                        "task" | "Task" | "spawn_subagent" => uses_subagent = true,
                        _ => {}
                    }
                    let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
                        continue;
                    };
                    let Some(file_path) = args.get("file_path").and_then(Value::as_str) else {
                        continue;
                    };
                    // Only mutating tools name a file the session changed;
                    // reads and everything else count in `tool_counts` alone.
                    match call.name.as_str() {
                        "search_replace" => {
                            modified_files.insert(file_path.to_string());
                            if let Some(old) = args.get("old_string").and_then(Value::as_str) {
                                lines_removed += old.lines().count() as u64;
                            }
                            if let Some(new) = args.get("new_string").and_then(Value::as_str) {
                                lines_added += new.lines().count() as u64;
                            }
                        }
                        "write" => {
                            modified_files.insert(file_path.to_string());
                            if let Some(body) = args.get("content").and_then(Value::as_str) {
                                lines_added += body.lines().count() as u64;
                            }
                        }
                        // The wire args carry anchors, not old/new strings:
                        // the file counts, the lines do not.
                        "hashline_edit" => {
                            modified_files.insert(file_path.to_string());
                        }
                        _ => {}
                    }
                }
            }
            ConversationItem::ToolResult(result) => {
                if result.is_error {
                    tool_errors += 1;
                }
            }
            _ => {}
        }
    }

    let mut languages: BTreeMap<String, u32> = BTreeMap::new();
    for path in &modified_files {
        if let Some(language) = language_for_path(path) {
            *languages.entry(language.to_string()).or_insert(0) += 1;
        }
    }

    // The time axis: only timestamped user/agent message chunks carry weight;
    // a user-chunk run gaps to the FIRST agent chunk after it (SDD §5).
    let mut response_gaps_secs: Vec<u32> = Vec::new();
    let mut message_hours: Vec<u32> = Vec::new();
    let mut user_message_timestamps: Vec<DateTime<Utc>> = Vec::new();
    let mut user_run_last_ms: Option<i64> = None;
    let mut last_chunk_ms: Option<i64> = None;
    let mut activity_secs = 0i64;

    for update in &input.time_axis {
        let ReplayedUpdate::Acp(acp_update, meta) = update else {
            continue;
        };
        let is_user = matches!(acp_update, acp::SessionUpdate::UserMessageChunk(_));
        let is_agent = matches!(acp_update, acp::SessionUpdate::AgentMessageChunk(_));
        if !is_user && !is_agent {
            continue;
        }
        let Some(timestamp) = meta
            .as_ref()
            .and_then(|m| m.get("agentTimestampMs"))
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_millis)
        else {
            continue;
        };
        let ms = timestamp.timestamp_millis();
        if is_user {
            user_message_timestamps.push(timestamp);
            user_run_last_ms = Some(ms);
        } else if let Some(user_ms) = user_run_last_ms.take() {
            let gap = (ms - user_ms) / 1000;
            if (2..3600).contains(&gap) {
                response_gaps_secs.push(gap as u32);
            }
        }
        message_hours.push(timestamp.hour());
        if let Some(last) = last_chunk_ms {
            let gap_secs = (ms - last) / 1000;
            if (0..IDLE_GAP_SECS).contains(&gap_secs) {
                activity_secs += gap_secs;
            }
        }
        last_chunk_ms = Some(ms);
    }

    Some(SessionDigest {
        cache_version: CACHE_SCHEMA_VERSION,
        session_id: session.session_id.clone(),
        cwd: session.cwd.clone(),
        git_root: session.git_root.clone(),
        head_branch: session.head_branch.clone(),
        started_at: session.started_at,
        updated_at: session.updated_at,
        wall_minutes: session
            .updated_at
            .signed_duration_since(session.started_at)
            .num_minutes(),
        activity_minutes: activity_secs / 60,
        user_turns,
        assistant_turns,
        tool_counts,
        languages,
        files_modified: modified_files.len() as u32,
        lines_added,
        lines_removed,
        projection_chars: content.projection_chars,
        input_tokens: input.usage.input_tokens,
        output_tokens: input.usage.output_tokens,
        reasoning_tokens: input.usage.reasoning_tokens,
        cached_read_tokens: input.usage.cached_read_tokens,
        cost_usd_ticks: input.usage.cost_usd_ticks,
        cost_is_partial: input.usage.cost_is_partial,
        tool_errors,
        interruptions,
        response_gaps_secs,
        message_hours,
        user_message_timestamps,
        time_degraded: input.reader.time_degraded,
        model_ids,
        uses_mcp,
        uses_subagent,
        first_prompt: input.first_prompt.clone(),
        degraded,
        unreadable_lines: input.reader.unreadable_lines,
        transcript_sha256: format!("{:x}", Sha256::digest(&content.raw_bytes)),
    })
}

/// The §5 language label for a modified file, by extension — the closed table
/// the report vocabulary is drawn from. An unknown (or absent) extension
/// contributes no label; the file still counts in `files_modified`.
fn language_for_path(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "Rust",
        "py" => "Python",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" => "JavaScript",
        "go" => "Go",
        "c" | "h" => "C",
        "cpp" | "cc" | "hpp" => "C++",
        "java" => "Java",
        "rb" => "Ruby",
        "md" => "Markdown",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "sh" => "Shell",
        "sql" => "SQL",
        _ => return None,
    })
}

// ============================================================================
// Fixtures + tests (T-1 base golden, T-2, T-3 gaps arm, T-5,
// rendered-vs-excluded, push order)
// ============================================================================

#[cfg(test)]
pub mod shared_fixtures {
    //! The eight T-1 fixture transcripts as on-disk session layouts (SDD §12
    //! T-1; S-11: the base golden digest is item 1's, T-0's rendering arms are
    //! item 4's). The five arm-named builders — (a), (b), (c), (d), (g) — exist
    //! so item 4 reuses these exact bytes for its T-0 arms; (s1), (s2), (s3) are
    //! item 1's own.
    //!
    //! Layout per fixture:
    //! ```text
    //! <root>/sessions/<encode_cwd_dirname(cwd)>/<session_id>/
    //!     summary.json          (hand-written, per SDD §12 — no JsonlStorageAdapter)
    //!     chat_history.jsonl    (hand-written lines; (b) carries one corrupt line)
    //!     updates.jsonl         (absent for (a))
    //!     usage.json            ((s1) only)
    //! ```
    //!
    //! [`read`] plays the reader role item 4 owns in production: parse
    //! `chat_history.jsonl` per `chat_format_version` (skip and count unreadable
    //! lines — never quarantine), stream the time axis through the real
    //! `stream_replay_updates_at_hinted` (the root is a PARAMETER — never the
    //! process-global `grok_home()` `OnceLock`, SDD §12).

    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, Duration, NaiveDate, Utc};
    use xai_grok_config::encode_cwd_dirname;
    use xai_grok_sampling_types::{
        AssistantItem, ChatContentBlock, ChatRequestMessage, ContentPart, ConversationItem,
        MessageContent, PriorTurnInterrupt, Role, SyntheticReason, SystemItem, ToolCall,
        ToolResultItem, UserItem,
    };

    use super::{DigestInput, ProjectionCaps, ReaderFacts, SessionFacts, TranscriptContent};
    use crate::session::persistence::Summary;
    use crate::session::storage::{
        ReplayEmission, ReplayLookupFallback, ReplayPathHint, ReplayedUpdate,
        stream_replay_updates_at_hinted,
    };
    use crate::session::usage_file::{SessionUsageFile, UsageSummary};

    /// The fixture cwd. The encoded dir comes from `xai_grok_config::encode_cwd_dirname`
    /// — the same function the replay resolver uses (`storage/replay.rs`), so the
    /// writer and reader sides agree by construction.
    pub const CWD: &str = "/home/user/project";

    pub struct Fixture {
        pub name: &'static str,
        pub root: tempfile::TempDir,
        pub session_id: String,
        pub cwd: String,
        /// The fixture's `projection_chars` claim — an input the digest stores.
        /// The actual projection length is item 4's `project()` to decide (T-1(c)).
        pub projection_chars: usize,
        /// The caps the tier arithmetic is pinned against.
        pub caps: ProjectionCaps,
        pub first_prompt: Option<String>,
    }

    impl Fixture {
        /// Assemble the [`DigestInput`] from the fixture's on-disk state.
        #[must_use]
        pub fn input(&self, read: ReadTranscript) -> DigestInput {
            DigestInput {
                session: read.session,
                content: TranscriptContent {
                    items: read.items,
                    raw_bytes: read.raw_bytes,
                    projection_chars: self.projection_chars,
                },
                time_axis: read.time_axis,
                usage: read.usage,
                reader: ReaderFacts {
                    time_degraded: read.time_degraded,
                    unreadable_lines: read.unreadable_lines,
                },
                caps: self.caps,
                first_prompt: self.first_prompt.clone(),
            }
        }

        /// Play the reader role over this fixture's on-disk state.
        pub fn read(&self) -> ReadTranscript {
            read_transcript(self.root.path(), &self.session_id, &self.cwd)
                .unwrap_or_else(|e| panic!("fixture ({}) reads: {e}", self.name))
        }
    }

    /// What the reader-side pass over one session produced.
    pub struct ReadTranscript {
        pub session: SessionFacts,
        pub raw_bytes: Vec<u8>,
        pub items: Vec<ConversationItem>,
        pub unreadable_lines: u32,
        pub time_axis: Vec<ReplayedUpdate>,
        pub usage: UsageSummary,
        /// Reader-set fact (SDD §5): `updates.jsonl` missing, or the stream
        /// emitted nothing at all.
        pub time_degraded: bool,
    }

    /// The reader pass item 4 owns in production: tolerant parse of
    /// `chat_history.jsonl` per `chat_format_version` (skip + count, never
    /// quarantine), the real replay stream for the time axis, and `usage.json`
    /// if present.
    pub fn read_transcript(
        root: &Path,
        session_id: &str,
        cwd: &str,
    ) -> std::io::Result<ReadTranscript> {
        let dir = root
            .join("sessions")
            .join(encode_cwd_dirname(cwd))
            .join(session_id);
        let raw_bytes = fs::read(dir.join("chat_history.jsonl"))?;
        let summary: Summary = serde_json::from_str(&fs::read_to_string(dir.join("summary.json"))?)
            .expect("fixture summary parses");
        let session = SessionFacts::from_summary(&summary);

        let mut unreadable_lines = 0u32;
        let mut items: Vec<ConversationItem> = Vec::new();
        for line in raw_bytes.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let item: Option<ConversationItem> = if summary.chat_format_version == 0 {
                serde_json::from_slice::<ChatRequestMessage>(line)
                    .ok()
                    .map(v0_to_item)
            } else {
                serde_json::from_slice(line).ok()
            };
            match item {
                Some(it) => items.push(it),
                None => unreadable_lines += 1,
            }
        }

        let mut time_axis: Vec<ReplayedUpdate> = Vec::new();
        let emission = stream_replay_updates_at_hinted(
            session_id,
            root,
            ReplayPathHint {
                parent_cwd: Some(Path::new(cwd)),
                child_cwd: None,
                fallback: ReplayLookupFallback::HintedOnly,
            },
            |u| time_axis.push(u),
        )?;

        let usage = if let Ok(bytes) = fs::read(dir.join("usage.json")) {
            let file: SessionUsageFile =
                serde_json::from_slice(&bytes).expect("fixture usage parses");
            file.session
        } else {
            UsageSummary::default()
        };

        Ok(ReadTranscript {
            session,
            raw_bytes,
            items,
            unreadable_lines,
            time_axis,
            usage,
            time_degraded: emission == ReplayEmission::Empty,
        })
    }

    /// The minimal v0 → v1 item mapping the fixtures need (item 4's reader owns
    /// the production mapping).
    fn v0_to_item(m: ChatRequestMessage) -> ConversationItem {
        match m.role {
            Role::System => ConversationItem::System(SystemItem {
                content: text_of(&m.content).into(),
            }),
            Role::User => ConversationItem::User(UserItem {
                content: vec![ContentPart::Text {
                    text: text_of(&m.content).into(),
                }],
                ..Default::default()
            }),
            Role::Assistant => ConversationItem::Assistant(AssistantItem {
                content: text_of(&m.content).into(),
                tool_calls: m
                    .tool_calls
                    .iter()
                    .map(|t| ToolCall {
                        id: t.id.clone().unwrap_or_default().into(),
                        name: t.function.name.clone(),
                        arguments: t.function.arguments.clone().into(),
                    })
                    .collect(),
                model_id: m.model_id,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            Role::Tool => ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: m.tool_call_id.unwrap_or_default(),
                content: text_of(&m.content).into(),
                images: Vec::new(),
                is_error: false,
            }),
        }
    }

    fn text_of(content: &MessageContent) -> String {
        match content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ChatContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    // --- scaffolding -------------------------------------------------------

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|nd| nd.and_hms_opt(h, mi, s))
            .map(|ndt| ndt.and_utc())
            .expect("fixture timestamp")
    }

    fn base_fixture(session_id: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("fixture tempdir");
        let dir = root
            .path()
            .join("sessions")
            .join(encode_cwd_dirname(CWD))
            .join(session_id);
        fs::create_dir_all(&dir).expect("fixture session dir");
        (root, dir)
    }

    fn write_summary(dir: &Path, session_id: &str, version: u8, created: &str, updated: &str) {
        let v = serde_json::json!({
            "info": { "id": session_id, "cwd": CWD },
            "session_summary": "fixture",
            "created_at": created,
            "updated_at": updated,
            "num_messages": 0,
            "current_model_id": "fixture-model",
            "chat_format_version": version,
        });
        let summary: Summary = serde_json::from_value(v).expect("fixture summary value");
        fs::write(
            dir.join("summary.json"),
            serde_json::to_string_pretty(&summary).expect("serialize summary"),
        )
        .expect("write summary.json");
    }

    fn append_raw(dir: &Path, line: &str) {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("chat_history.jsonl"))
            .expect("open chat_history.jsonl")
            .write_all(format!("{line}\n").as_bytes())
            .expect("append line");
    }

    fn append_item(dir: &Path, item: &ConversationItem) {
        append_raw(dir, &serde_json::to_string(item).expect("serialize item"));
    }

    fn v0_line(dir: &Path, m: &ChatRequestMessage) {
        append_raw(
            dir,
            &serde_json::to_string(m).expect("serialize legacy line"),
        );
    }

    /// One `updates.jsonl` envelope line with `_meta.agentTimestampMs` — the only
    /// timestamp the digest builds on (SDD §5).
    fn updates_line(session_id: &str, user: bool, text: &str, at: DateTime<Utc>) -> String {
        serde_json::json!({
            "timestamp": at.timestamp(),
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": if user { "user_message_chunk" } else { "agent_message_chunk" },
                    "content": { "type": "text", "text": text },
                },
                "_meta": { "agentTimestampMs": at.timestamp_millis() },
            },
        })
        .to_string()
    }

    fn write_updates(dir: &Path, session_id: &str, lines: &[String]) {
        fs::write(
            dir.join("updates.jsonl"),
            lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
        )
        .expect("write updates.jsonl");
    }

    // --- item constructors (shared by the file fixtures and the in-memory tests) ---

    pub fn user_item(text: &str) -> ConversationItem {
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text { text: text.into() }],
            ..Default::default()
        })
    }

    pub fn user_item_synthetic(text: &str, reason: SyntheticReason) -> ConversationItem {
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text { text: text.into() }],
            synthetic_reason: Some(reason),
            ..Default::default()
        })
    }

    pub fn user_item_interrupted(text: &str) -> ConversationItem {
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text { text: text.into() }],
            prior_turn_interrupt: Some(PriorTurnInterrupt::MidTurnAbort),
            ..Default::default()
        })
    }

    pub fn assistant_item(text: &str, model: &str, calls: Vec<ToolCall>) -> ConversationItem {
        ConversationItem::Assistant(AssistantItem {
            content: text.into(),
            tool_calls: calls,
            model_id: Some(model.into()),
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }

    pub fn tool_result(id: &str, content: &str, is_error: bool) -> ConversationItem {
        ConversationItem::ToolResult(ToolResultItem {
            tool_call_id: id.into(),
            content: content.into(),
            images: Vec::new(),
            is_error,
        })
    }

    pub fn tool_call(name: &str, args: &serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("call-{name}").into(),
            name: name.into(),
            arguments: args.to_string().into(),
        }
    }

    /// Deterministic padding: `prefix-NNNN` then `x` to exactly `total` chars.
    fn padded(prefix: &str, i: usize, total: usize) -> String {
        let head = format!("{prefix}-{i:04}");
        let pad = total - head.len();
        let mut s = head;
        s.push_str(&"x".repeat(pad));
        s
    }

    /// `n_user` user items of `user_chars` chars, then `n_asst` assistant items
    /// of `asst_chars` chars — the transcript bodies behind the cap fixtures.
    fn bulk(
        n_user: usize,
        user_chars: usize,
        n_asst: usize,
        asst_chars: usize,
    ) -> Vec<ConversationItem> {
        let mut items = Vec::with_capacity(n_user + n_asst);
        for i in 0..n_user {
            items.push(user_item(&padded("u", i, user_chars)));
        }
        for i in 0..n_asst {
            items.push(assistant_item(
                &padded("a", i, asst_chars),
                "grok-4",
                Vec::new(),
            ));
        }
        items
    }

    fn two_chunk_updates(dir: &Path, session_id: &str, t0: DateTime<Utc>) {
        write_updates(
            dir,
            session_id,
            &[
                updates_line(session_id, true, "user", t0),
                updates_line(session_id, false, "agent", t0 + Duration::seconds(30)),
            ],
        );
    }

    // --- the eight fixtures ------------------------------------------------

    /// (a) — T-0(a): `chat_format_version: 0` with **no** `updates.jsonl` →
    /// `[LegacyFormat, NoTimeAxis]` in the declared §5 order, `time_degraded` set.
    pub fn legacy_no_updates() -> Fixture {
        let (root, dir) = base_fixture("sfx-a");
        write_summary(
            &dir,
            "sfx-a",
            0,
            "2026-09-01T09:00:00Z",
            "2026-09-01T09:30:00Z",
        );
        v0_line(&dir, &ChatRequestMessage::system("You are a fixture."));
        v0_line(&dir, &ChatRequestMessage::user("legacy question"));
        v0_line(
            &dir,
            &ChatRequestMessage {
                role: Role::Assistant,
                content: MessageContent::Text("legacy answer".into()),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
                model_id: None,
                reasoning_content: None,
            },
        );
        v0_line(
            &dir,
            &ChatRequestMessage {
                role: Role::Tool,
                content: MessageContent::Text("legacy tool out".into()),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call-1".into()),
                model_id: None,
                reasoning_content: None,
            },
        );
        Fixture {
            name: "a",
            root,
            session_id: "sfx-a".into(),
            cwd: CWD.into(),
            projection_chars: 40,
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    /// (b) — T-0(b): v1 with one corrupt line mid-file → `[UnreadableLines]`
    /// exactly, `unreadable_lines == 1`. (The filesystem half of T-0(b) — no
    /// `*.jsonl.corrupt` anywhere — is item 4's; the reader here only skips and
    /// counts.)
    pub fn corrupt_line() -> Fixture {
        let (root, dir) = base_fixture("sfx-b");
        write_summary(
            &dir,
            "sfx-b",
            1,
            "2026-09-02T10:00:00Z",
            "2026-09-02T10:31:00Z",
        );
        append_item(&dir, &user_item("fix the flake"));
        append_item(&dir, &assistant_item("done", "grok-4", Vec::new()));
        append_raw(&dir, "{not json");
        append_item(&dir, &tool_result("c1", "ok", false));
        let t0 = ts(2026, 9, 2, 10, 0, 0);
        two_chunk_updates(&dir, "sfx-b", t0);
        Fixture {
            name: "b",
            root,
            session_id: "sfx-b".into(),
            cwd: CWD.into(),
            projection_chars: 200,
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    /// (c) — T-0(c): with `max_input_chars` explicitly 60 000 a projection of
    /// 90 000 chars → `[InputTruncated]` exactly. The explicit knob is
    /// load-bearing: at shipped defaults a projection past 400 000 would also
    /// trip the 4-chunk cap and `ChunkCapHit` would co-fire.
    pub fn input_truncated() -> Fixture {
        let (root, dir) = base_fixture("sfx-c");
        write_summary(
            &dir,
            "sfx-c",
            1,
            "2026-09-03T08:00:00Z",
            "2026-09-03T09:00:00Z",
        );
        for item in bulk(150, 500, 50, 300) {
            append_item(&dir, &item);
        }
        let t0 = ts(2026, 9, 3, 8, 0, 0);
        two_chunk_updates(&dir, "sfx-c", t0);
        Fixture {
            name: "c",
            root,
            session_id: "sfx-c".into(),
            cwd: CWD.into(),
            projection_chars: 90_000,
            caps: ProjectionCaps {
                max_input_chars: 60_000,
                ..ProjectionCaps::default()
            },
            first_prompt: None,
        }
    }

    /// (d) — T-0(d): at shipped defaults a projection of 110 000 chars needs
    /// `ceil(110 000 / 25 000)` = 5 chunks > the 4-chunk cap → `[ChunkCapHit]`
    /// exactly (110 000 ≤ 400 000, so `InputTruncated` does not co-fire).
    pub fn chunk_cap_hit() -> Fixture {
        let (root, dir) = base_fixture("sfx-d");
        write_summary(
            &dir,
            "sfx-d",
            1,
            "2026-09-04T09:00:00Z",
            "2026-09-04T10:00:00Z",
        );
        for item in bulk(200, 500, 25, 400) {
            append_item(&dir, &item);
        }
        let t0 = ts(2026, 9, 4, 9, 0, 0);
        two_chunk_updates(&dir, "sfx-d", t0);
        Fixture {
            name: "d",
            root,
            session_id: "sfx-d".into(),
            cwd: CWD.into(),
            projection_chars: 110_000,
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    /// (g) — T-0(g): a projection of 50 000 chars, between 30 000 and 100 000 →
    /// `degraded == []`. The arm that kills the rival `InputTruncated`
    /// predicate: (c) sits above 400 000 at defaults and so would pass under
    /// both readings.
    pub fn clean_band() -> Fixture {
        let (root, dir) = base_fixture("sfx-g");
        write_summary(
            &dir,
            "sfx-g",
            1,
            "2026-09-05T11:00:00Z",
            "2026-09-05T12:00:00Z",
        );
        for item in bulk(90, 500, 20, 250) {
            append_item(&dir, &item);
        }
        let t0 = ts(2026, 9, 5, 11, 0, 0);
        two_chunk_updates(&dir, "sfx-g", t0);
        Fixture {
            name: "g",
            root,
            session_id: "sfx-g".into(),
            cwd: CWD.into(),
            projection_chars: 50_000,
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    /// (s1) — item 1's kitchen sink: pins every numeric field at once —
    /// turns, tool counts, errors, interruptions, model dedup, mcp/subagent
    /// flags, files/lines/languages, the full time axis, usage, and the
    /// `--show-prompts` rung.
    pub fn kitchen_sink() -> Fixture {
        let (root, dir) = base_fixture("sfx-s1");
        write_summary(
            &dir,
            "sfx-s1",
            1,
            "2026-09-06T13:59:00Z",
            "2026-09-06T15:01:00Z",
        );
        append_item(
            &dir,
            &ConversationItem::System(SystemItem {
                content: "You are a fixture.".into(),
            }),
        );
        append_item(&dir, &user_item("fix retry.rs"));
        append_item(
            &dir,
            &assistant_item(
                "Investigating.",
                "grok-4",
                vec![tool_call(
                    "search_replace",
                    &serde_json::json!({
                        "file_path": "src/retry.rs",
                        "old_string": "fn old() {}",
                        "new_string": "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}",
                    }),
                )],
            ),
        );
        append_item(&dir, &tool_result("c1", "replaced", false));
        append_item(&dir, &user_item_interrupted("and the config"));
        append_item(
            &dir,
            &assistant_item(
                "Checking deps.",
                "grok-4-latest",
                vec![
                    tool_call(
                        "read_file",
                        &serde_json::json!({ "file_path": "src/retry.rs" }),
                    ),
                    tool_call(
                        "mcp__github__create_issue",
                        &serde_json::json!({ "title": "flake" }),
                    ),
                ],
            ),
        );
        append_item(&dir, &tool_result("c2", "fn old() {}", false));
        append_item(&dir, &tool_result("c3", "rate limited", true));
        append_item(
            &dir,
            &assistant_item(
                "Spawning review.",
                "grok-4",
                vec![
                    tool_call(
                        "spawn_subagent",
                        &serde_json::json!({ "task": "review retry.rs" }),
                    ),
                    tool_call(
                        "search_replace",
                        &serde_json::json!({
                            "file_path": "config.toml",
                            "old_string": "a = 1",
                            "new_string": "b = 2\nc = 3",
                        }),
                    ),
                ],
            ),
        );
        append_item(&dir, &tool_result("c4", "review complete", false));

        // Time axis: gaps of 5 s and 3601 s → exactly one kept response gap.
        let t0 = ts(2026, 9, 6, 14, 0, 0);
        write_updates(
            &dir,
            "sfx-s1",
            &[
                updates_line("sfx-s1", true, "fix retry.rs", t0),
                updates_line("sfx-s1", false, "Investigating.", t0 + Duration::seconds(5)),
                updates_line("sfx-s1", true, "and the config", t0 + Duration::seconds(35)),
                updates_line(
                    "sfx-s1",
                    false,
                    "Spawning review.",
                    t0 + Duration::seconds(3636),
                ),
            ],
        );

        // `SessionUsageFile` wraps the totals under `"session"` (the reader
        // parses the envelope, not a bare summary) — the wire keys are
        // camelCase (the two ephemeral fields carry explicit snake renames and
        // are unused here).
        fs::write(
            dir.join("usage.json"),
            serde_json::json!({
                "sessionId": "sfx-s1",
                "updatedAt": "2026-09-06T15:01:00Z",
                "session": {
                    "inputTokens": 1000,
                    "outputTokens": 200,
                    "cachedReadTokens": 500,
                    "reasoningTokens": 50,
                    "costUsdTicks": 1234,
                    "costIsPartial": false,
                },
            })
            .to_string(),
        )
        .expect("write usage.json");

        Fixture {
            name: "s1",
            root,
            session_id: "sfx-s1".into(),
            cwd: CWD.into(),
            projection_chars: 12_000,
            caps: ProjectionCaps::default(),
            first_prompt: Some("fix retry.rs".into()),
        }
    }

    /// (s2) — legacy WITH a time axis: `[LegacyFormat]` exactly (updates present
    /// ⇒ no `NoTimeAxis`), `model_ids` empty (legacy rows carry none).
    pub fn legacy_with_updates() -> Fixture {
        let (root, dir) = base_fixture("sfx-s2");
        write_summary(
            &dir,
            "sfx-s2",
            0,
            "2026-09-07T10:00:00Z",
            "2026-09-07T10:31:00Z",
        );
        v0_line(&dir, &ChatRequestMessage::system("You are a fixture."));
        v0_line(&dir, &ChatRequestMessage::user("old q1"));
        v0_line(
            &dir,
            &ChatRequestMessage {
                role: Role::Assistant,
                content: MessageContent::Text("old a1".into()),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
                model_id: None,
                reasoning_content: None,
            },
        );
        v0_line(&dir, &ChatRequestMessage::user("old q2"));
        let t0 = ts(2026, 9, 7, 10, 0, 0);
        two_chunk_updates(&dir, "sfx-s2", t0);
        Fixture {
            name: "s2",
            root,
            session_id: "sfx-s2".into(),
            cwd: CWD.into(),
            projection_chars: 60,
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    /// (s3) — the strict-boundary arm: `max_input_chars` = 100 000 with a
    /// projection of EXACTLY 100 000 chars (not `>`) and exactly 4 chunks (not
    /// `> 4`) → `degraded == []`. Both predicates are strict by SDD §6; this
    /// fixture pins both boundaries from the passing side.
    pub fn caps_boundary() -> Fixture {
        let (root, dir) = base_fixture("sfx-s3");
        write_summary(
            &dir,
            "sfx-s3",
            1,
            "2026-09-08T12:00:00Z",
            "2026-09-08T13:00:00Z",
        );
        for item in bulk(198, 500, 5, 200) {
            append_item(&dir, &item);
        }
        let t0 = ts(2026, 9, 8, 12, 0, 0);
        two_chunk_updates(&dir, "sfx-s3", t0);
        Fixture {
            name: "s3",
            root,
            session_id: "sfx-s3".into(),
            cwd: CWD.into(),
            projection_chars: 100_000,
            caps: ProjectionCaps {
                max_input_chars: 100_000,
                ..ProjectionCaps::default()
            },
            first_prompt: None,
        }
    }

    /// All eight, in SDD T-1 order.
    pub fn all() -> Vec<Fixture> {
        vec![
            legacy_no_updates(),
            corrupt_line(),
            input_truncated(),
            chunk_cap_hit(),
            clean_band(),
            kitchen_sink(),
            legacy_with_updates(),
            caps_boundary(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};
    use sha2::{Digest, Sha256};
    use xai_grok_sampling_types::SyntheticReason;

    use super::shared_fixtures as fx;
    use super::{
        CACHE_SCHEMA_VERSION, DegradedTier, DigestInput, ProjectionCaps, ReaderFacts, SessionFacts,
        TranscriptContent, digest_one, is_user_turn,
    };
    use crate::session::storage::ReplayedUpdate;
    use crate::session::usage_file::UsageSummary;

    fn check<T: std::fmt::Debug + PartialEq>(name: &str, got: &T, want: &T) {
        assert_eq!(got, want, "field `{name}`");
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("fixture instant")
            .with_timezone(&Utc)
    }

    fn empty_map() -> BTreeMap<String, u32> {
        BTreeMap::new()
    }

    fn expected_tool_counts() -> BTreeMap<String, u32> {
        let mut m = BTreeMap::new();
        m.insert("mcp__github__create_issue".into(), 1);
        m.insert("read_file".into(), 1);
        m.insert("search_replace".into(), 2);
        m.insert("spawn_subagent".into(), 1);
        m
    }

    fn expected_languages() -> BTreeMap<String, u32> {
        let mut m = BTreeMap::new();
        m.insert("Rust".into(), 1);
        m.insert("TOML".into(), 1);
        m
    }

    fn sha_of(raw: &[u8]) -> String {
        format!("{:x}", Sha256::digest(raw))
    }

    // --- T-1: the base golden digest over the eight fixture transcripts ---

    #[test]
    fn t1_a_legacy_without_updates_is_double_degraded() {
        let f = fx::legacy_no_updates();
        let d =
            digest_one(&f.input(f.read())).expect("(a) renders: legacy is badged, never excluded");
        check("session_id", &d.session_id, &"sfx-a".to_string());
        check("cwd", &d.cwd, &fx::CWD.to_string());
        check("git_root", &d.git_root, &None);
        check("head_branch", &d.head_branch, &None);
        check("started_at", &d.started_at, &at("2026-09-01T09:00:00Z"));
        check("updated_at", &d.updated_at, &at("2026-09-01T09:30:00Z"));
        check("wall_minutes", &d.wall_minutes, &30);
        check("activity_minutes", &d.activity_minutes, &0);
        check("user_turns", &d.user_turns, &1usize);
        check("assistant_turns", &d.assistant_turns, &1usize);
        check("tool_counts", &d.tool_counts, &empty_map());
        check("languages", &d.languages, &empty_map());
        check("files_modified", &d.files_modified, &0u32);
        check("lines_added", &d.lines_added, &0u64);
        check("lines_removed", &d.lines_removed, &0u64);
        check("projection_chars", &d.projection_chars, &40usize);
        check("input_tokens", &d.input_tokens, &0u64);
        check("output_tokens", &d.output_tokens, &0u64);
        check("reasoning_tokens", &d.reasoning_tokens, &0u64);
        check("cached_read_tokens", &d.cached_read_tokens, &0u64);
        check("cost_usd_ticks", &d.cost_usd_ticks, &None);
        check("cost_is_partial", &d.cost_is_partial, &false);
        check("tool_errors", &d.tool_errors, &0u32);
        check("interruptions", &d.interruptions, &0u32);
        check(
            "response_gaps_secs",
            &d.response_gaps_secs,
            &Vec::<u32>::new(),
        );
        check("message_hours", &d.message_hours, &Vec::<u32>::new());
        check(
            "user_message_timestamps",
            &d.user_message_timestamps,
            &Vec::<DateTime<Utc>>::new(),
        );
        // The single-derivation agreement: reader fact and digest derivation match.
        check("time_degraded", &d.time_degraded, &true);
        check("model_ids", &d.model_ids, &Vec::<String>::new());
        check("uses_mcp", &d.uses_mcp, &false);
        check("uses_subagent", &d.uses_subagent, &false);
        check("first_prompt", &d.first_prompt, &None);
        check(
            "degraded",
            &d.degraded,
            &vec![DegradedTier::LegacyFormat, DegradedTier::NoTimeAxis],
        );
        check("unreadable_lines", &d.unreadable_lines, &0u32);
    }

    #[test]
    fn t1_b_corrupt_line_is_unreadable_tier_only() {
        let f = fx::corrupt_line();
        let d = digest_one(&f.input(f.read())).expect("(b) renders");
        check("user_turns", &d.user_turns, &1usize);
        check("assistant_turns", &d.assistant_turns, &1usize);
        check("wall_minutes", &d.wall_minutes, &31);
        check("activity_minutes", &d.activity_minutes, &0);
        check("response_gaps_secs", &d.response_gaps_secs, &vec![30u32]);
        check("message_hours", &d.message_hours, &vec![10u32, 10u32]);
        check(
            "user_message_timestamps",
            &d.user_message_timestamps,
            &vec![at("2026-09-02T10:00:00Z")],
        );
        check("time_degraded", &d.time_degraded, &false);
        check("model_ids", &d.model_ids, &vec!["grok-4".to_string()]);
        check(
            "degraded",
            &d.degraded,
            &vec![DegradedTier::UnreadableLines],
        );
        check("unreadable_lines", &d.unreadable_lines, &1u32);
        check("projection_chars", &d.projection_chars, &200usize);
        check("first_prompt", &d.first_prompt, &None);
    }

    #[test]
    fn t1_c_input_truncated_with_explicit_cap() {
        let f = fx::input_truncated();
        let d = digest_one(&f.input(f.read())).expect("(c) renders");
        check("user_turns", &d.user_turns, &150usize);
        check("assistant_turns", &d.assistant_turns, &50usize);
        check("wall_minutes", &d.wall_minutes, &60);
        check("activity_minutes", &d.activity_minutes, &0);
        check("response_gaps_secs", &d.response_gaps_secs, &vec![30u32]);
        check("message_hours", &d.message_hours, &vec![8u32, 8u32]);
        check("time_degraded", &d.time_degraded, &false);
        check("model_ids", &d.model_ids, &vec!["grok-4".to_string()]);
        check("degraded", &d.degraded, &vec![DegradedTier::InputTruncated]);
        check("projection_chars", &d.projection_chars, &90_000usize);
        check("unreadable_lines", &d.unreadable_lines, &0u32);
    }

    #[test]
    fn t1_d_chunk_cap_hit_at_defaults() {
        let f = fx::chunk_cap_hit();
        let d = digest_one(&f.input(f.read())).expect("(d) renders");
        check("user_turns", &d.user_turns, &200usize);
        check("assistant_turns", &d.assistant_turns, &25usize);
        check("wall_minutes", &d.wall_minutes, &60);
        check("response_gaps_secs", &d.response_gaps_secs, &vec![30u32]);
        check("message_hours", &d.message_hours, &vec![9u32, 9u32]);
        check("time_degraded", &d.time_degraded, &false);
        check("model_ids", &d.model_ids, &vec!["grok-4".to_string()]);
        check("degraded", &d.degraded, &vec![DegradedTier::ChunkCapHit]);
        check("projection_chars", &d.projection_chars, &110_000usize);
    }

    #[test]
    fn t1_g_clean_band_between_30k_and_100k() {
        let f = fx::clean_band();
        let d = digest_one(&f.input(f.read())).expect("(g) renders");
        check("user_turns", &d.user_turns, &90usize);
        check("assistant_turns", &d.assistant_turns, &20usize);
        check("wall_minutes", &d.wall_minutes, &60);
        check("response_gaps_secs", &d.response_gaps_secs, &vec![30u32]);
        check("message_hours", &d.message_hours, &vec![11u32, 11u32]);
        check("time_degraded", &d.time_degraded, &false);
        check("degraded", &d.degraded, &Vec::<DegradedTier>::new());
        check("projection_chars", &d.projection_chars, &50_000usize);
    }

    #[test]
    fn t1_s1_kitchen_sink_pins_every_numeric_field() {
        let f = fx::kitchen_sink();
        let d = digest_one(&f.input(f.read())).expect("(s1) renders");
        check("session_id", &d.session_id, &"sfx-s1".to_string());
        check("started_at", &d.started_at, &at("2026-09-06T13:59:00Z"));
        check("updated_at", &d.updated_at, &at("2026-09-06T15:01:00Z"));
        check("wall_minutes", &d.wall_minutes, &62);
        // Gaps 5 s + 30 s = 35 s of activity → 0 whole minutes.
        check("activity_minutes", &d.activity_minutes, &0);
        check("user_turns", &d.user_turns, &2usize);
        check("assistant_turns", &d.assistant_turns, &3usize);
        check("tool_counts", &d.tool_counts, &expected_tool_counts());
        check("languages", &d.languages, &expected_languages());
        check("files_modified", &d.files_modified, &2u32);
        // (s1)'s edits: 1→4 lines and 1→2 lines.
        check("lines_added", &d.lines_added, &6u64);
        check("lines_removed", &d.lines_removed, &2u64);
        check("projection_chars", &d.projection_chars, &12_000usize);
        check("input_tokens", &d.input_tokens, &1000u64);
        check("output_tokens", &d.output_tokens, &200u64);
        check("reasoning_tokens", &d.reasoning_tokens, &50u64);
        check("cached_read_tokens", &d.cached_read_tokens, &500u64);
        check("cost_usd_ticks", &d.cost_usd_ticks, &Some(1234));
        check("cost_is_partial", &d.cost_is_partial, &false);
        check("tool_errors", &d.tool_errors, &1u32);
        check("interruptions", &d.interruptions, &1u32);
        // 5 s kept; the 3601 s gap is dropped by the < 3600 bound.
        check("response_gaps_secs", &d.response_gaps_secs, &vec![5u32]);
        check(
            "message_hours",
            &d.message_hours,
            &vec![14u32, 14u32, 14u32, 15u32],
        );
        check(
            "user_message_timestamps",
            &d.user_message_timestamps,
            &vec![at("2026-09-06T14:00:00Z"), at("2026-09-06T14:00:35Z")],
        );
        check("time_degraded", &d.time_degraded, &false);
        // First-seen dedup: grok-4, grok-4-latest, (grok-4 dropped).
        check(
            "model_ids",
            &d.model_ids,
            &vec!["grok-4".to_string(), "grok-4-latest".to_string()],
        );
        check("uses_mcp", &d.uses_mcp, &true);
        check("uses_subagent", &d.uses_subagent, &true);
        check(
            "first_prompt",
            &d.first_prompt,
            &Some("fix retry.rs".to_string()),
        );
        check("degraded", &d.degraded, &Vec::<DegradedTier>::new());
        check("unreadable_lines", &d.unreadable_lines, &0u32);
    }

    #[test]
    fn t1_s2_legacy_with_updates_is_single_tier() {
        let f = fx::legacy_with_updates();
        let d = digest_one(&f.input(f.read())).expect("(s2) renders");
        check("user_turns", &d.user_turns, &2usize);
        check("assistant_turns", &d.assistant_turns, &1usize);
        check("wall_minutes", &d.wall_minutes, &31);
        check("response_gaps_secs", &d.response_gaps_secs, &vec![30u32]);
        check("message_hours", &d.message_hours, &vec![10u32, 10u32]);
        check(
            "user_message_timestamps",
            &d.user_message_timestamps,
            &vec![at("2026-09-07T10:00:00Z")],
        );
        check("time_degraded", &d.time_degraded, &false);
        // Legacy rows carry no `model_id` — empty, not a guess.
        check("model_ids", &d.model_ids, &Vec::<String>::new());
        check("degraded", &d.degraded, &vec![DegradedTier::LegacyFormat]);
        check("unreadable_lines", &d.unreadable_lines, &0u32);
    }

    #[test]
    fn t1_s3_caps_boundaries_are_strict() {
        let f = fx::caps_boundary();
        let d = digest_one(&f.input(f.read())).expect("(s3) renders");
        check("user_turns", &d.user_turns, &198usize);
        check("assistant_turns", &d.assistant_turns, &5usize);
        // 100 000 is not > 100 000; ceil(100 000 / 25 000) = 4 is not > 4.
        check("degraded", &d.degraded, &Vec::<DegradedTier>::new());
        check("projection_chars", &d.projection_chars, &100_000usize);
    }

    /// SDD §12: `transcript_sha256` and `cache_version` are excluded from the
    /// golden — compared directly instead of pinned.
    #[test]
    fn t1_sha_and_cache_version_compared_directly_on_all_eight() {
        for f in fx::all() {
            let read = f.read();
            let raw = read.raw_bytes.clone();
            let d = digest_one(&f.input(read))
                .unwrap_or_else(|| panic!("fixture ({}) digests", f.name));
            check(
                &format!("({}) transcript_sha256", f.name),
                &d.transcript_sha256,
                &sha_of(&raw),
            );
            check(
                &format!("({}) cache_version", f.name),
                &d.cache_version,
                &CACHE_SCHEMA_VERSION,
            );
        }
    }

    /// The single-derivation rule, pinned on all eight fixtures: the reader-set
    /// `time_degraded` fact and the digest-derived `NoTimeAxis` tier agree.
    #[test]
    fn t1_time_degraded_agrees_with_no_time_axis_on_all_eight() {
        for f in fx::all() {
            let d = digest_one(&f.input(f.read()))
                .unwrap_or_else(|| panic!("fixture ({}) digests", f.name));
            let no_time_axis = d.degraded.contains(&DegradedTier::NoTimeAxis);
            check(
                &format!("({}) time_degraded == NoTimeAxis", f.name),
                &d.time_degraded,
                &no_time_axis,
            );
        }
    }

    /// `NoTimeAxis` is the digest's derivation from the axis it is handed — NOT
    /// a re-statement of the reader's `time_degraded` fact. Pinned both ways
    /// with in-memory inputs: a present axis suppresses the tier even when the
    /// reader says degraded; an empty axis sets it even when the reader says
    /// fine. (`time_degraded` the field always carries the reader fact.)
    #[test]
    fn no_time_axis_tier_comes_from_the_axis_not_the_reader_fact() {
        let t0 = 1_750_000_000_000u64;
        let mut input = mem_input(
            vec![fx::user_item("q")],
            vec![chunk(true, t0), chunk(false, t0 + 30_000)],
            1,
        );
        input.reader.time_degraded = true;
        let d = digest_one(&input).expect("present axis digests");
        check(
            "time_degraded (field carries the reader fact)",
            &d.time_degraded,
            &true,
        );
        check(
            "degraded (axis present: no NoTimeAxis)",
            &d.degraded,
            &Vec::<DegradedTier>::new(),
        );

        let mut input = mem_input(vec![fx::user_item("q")], Vec::new(), 1);
        input.reader.time_degraded = false;
        let d = digest_one(&input).expect("empty axis digests");
        check(
            "time_degraded (field carries the reader fact)",
            &d.time_degraded,
            &false,
        );
        check(
            "degraded (axis empty: NoTimeAxis)",
            &d.degraded,
            &vec![DegradedTier::NoTimeAxis],
        );
    }

    // --- T-2: the single is_user_turn oracle ------------------------------

    #[test]
    fn t2_is_user_turn_counts_real_users_only() {
        let body = "fn main() { println!(\"same file in three places\"); }";
        let u1 = fx::user_item(&format!("look at this file:\n{body}"));
        let u2 = fx::user_item_synthetic(body, SyntheticReason::SystemReminder); // attachment
        let u3 = fx::user_item_synthetic(body, SyntheticReason::Interjection); // queued command
        let r1 = fx::tool_result("c9", body, false); // tool_result
        let a1 = fx::assistant_item("ok", "grok-4", Vec::new());

        // The one predicate, asserted item-by-item — the oracle.
        assert!(is_user_turn(&u1), "genuine user message is a user turn");
        assert!(
            !is_user_turn(&u2),
            "synthetic attachment is not a user turn"
        );
        assert!(
            !is_user_turn(&u3),
            "synthetic queued command is not a user turn"
        );
        assert!(!is_user_turn(&r1), "a tool_result is not a user turn");
        assert!(!is_user_turn(&a1), "an assistant item is not a user turn");

        let d = digest_one(&mem_input(vec![u1, u2, u3, r1, a1], Vec::new(), 1))
            .expect("T-2 transcript digests");
        check("user_turns", &d.user_turns, &1usize);
        check("assistant_turns", &d.assistant_turns, &1usize);
    }

    // --- T-3: the response-gap bucketing arm (the median arm is item 2's) ---

    fn chunk(user: bool, at_ms: u64) -> ReplayedUpdate {
        let update = if user {
            agent_client_protocol::SessionUpdate::UserMessageChunk(
                agent_client_protocol::ContentChunk::new(
                    agent_client_protocol::ContentBlock::Text(
                        agent_client_protocol::TextContent::new("x"),
                    ),
                ),
            )
        } else {
            agent_client_protocol::SessionUpdate::AgentMessageChunk(
                agent_client_protocol::ContentChunk::new(
                    agent_client_protocol::ContentBlock::Text(
                        agent_client_protocol::TextContent::new("x"),
                    ),
                ),
            )
        };
        let mut meta = agent_client_protocol::Meta::new();
        meta.insert("agentTimestampMs".into(), serde_json::json!(at_ms));
        ReplayedUpdate::Acp(update, Some(meta))
    }

    #[test]
    fn t3_response_gap_bucketing_keeps_both_strict_bounds() {
        // Four exchanges with gaps of 1 s, 3 s, 3599 s, 3601 s — the guards
        // `2 < gap < 3600` keep exactly the middle two.
        let t0 = 1_750_000_000_000u64; // an arbitrary fixed ms base
        let axis = vec![
            chunk(true, t0),
            chunk(false, t0 + 1_000), // 1 s — dropped
            chunk(true, t0 + 2_000),
            chunk(false, t0 + 5_000), // 3 s — kept
            chunk(true, t0 + 10_000),
            chunk(false, t0 + 3_609_000), // 3599 s — kept
            chunk(true, t0 + 4_000_000),
            chunk(false, t0 + 7_601_000), // 3601 s — dropped
        ];
        let d = digest_one(&mem_input(
            vec![
                fx::user_item("q"),
                fx::assistant_item("a", "grok-4", Vec::new()),
            ],
            axis,
            1,
        ))
        .expect("T-3 transcript digests");
        check(
            "response_gaps_secs",
            &d.response_gaps_secs,
            &vec![3u32, 3599u32],
        );
    }

    // --- T-5: transcript-derived line-diff accounting ----------------------

    #[test]
    fn t5_line_diff_accounting_pins_both_arithmetic_rules() {
        let ten: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        let body = ten.join("\n"); // 10 lines, no trailing newline
        // Rule (1): a rewritten line counts as one removed AND one added —
        // identical old and new text still counts both ways, it is not a no-op.
        let a1 = fx::assistant_item(
            "rewriting ten lines",
            "grok-4",
            vec![fx::tool_call(
                "search_replace",
                &serde_json::json!({
                    "file_path": "f.rs",
                    "old_string": body,
                    "new_string": body,
                }),
            )],
        );
        // Rule (2): a final line with no trailing newline is still one line.
        let a2 = fx::assistant_item(
            "writing three lines",
            "grok-4",
            vec![fx::tool_call(
                "write",
                &serde_json::json!({
                    "file_path": "g.toml",
                    "content": "a\nb\nc",
                }),
            )],
        );
        let d =
            digest_one(&mem_input(vec![a1, a2], Vec::new(), 1)).expect("T-5 transcript digests");
        check("lines_removed", &d.lines_removed, &10u64);
        check("lines_added", &d.lines_added, &13u64);
        check("files_modified", &d.files_modified, &2u32);
        check("languages", &d.languages, &expected_languages());
    }

    // --- rendered-vs-excluded + the fixed tier push order -------------------

    #[test]
    fn rendered_vs_excluded_and_fixed_push_order() {
        // Excluded: no readable transcript → None (not a degraded Some).
        let excluded = digest_one(&mem_input(Vec::new(), Vec::new(), 0));
        assert!(
            excluded.is_none(),
            "no readable transcript is excluded, not badged"
        );

        // Maximally degraded: every tier at once, in the fixed §5 push order —
        // and STILL rendered (Some).
        let items = vec![fx::user_item("old")];
        let d = digest_one(&mem_input_degraded(items)).expect("maximally degraded still renders");
        check(
            "degraded order",
            &d.degraded,
            &vec![
                DegradedTier::LegacyFormat,
                DegradedTier::NoTimeAxis,
                DegradedTier::InputTruncated,
                DegradedTier::ChunkCapHit,
                DegradedTier::UnreadableLines,
            ],
        );
        check("unreadable_lines", &d.unreadable_lines, &2u32);
        check("time_degraded", &d.time_degraded, &true);
    }

    // --- in-memory input helpers -------------------------------------------

    fn mem_items_bytes(items: &[xai_grok_sampling_types::ConversationItem]) -> Vec<u8> {
        items
            .iter()
            .map(|i| serde_json::to_string(i).expect("serialize in-memory item"))
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    }

    fn mem_input(
        items: Vec<xai_grok_sampling_types::ConversationItem>,
        time_axis: Vec<ReplayedUpdate>,
        version: u8,
    ) -> DigestInput {
        let base = mem_items_bytes(&items);
        DigestInput {
            session: SessionFacts {
                session_id: "mem".into(),
                cwd: "/mem".into(),
                git_root: None,
                head_branch: None,
                started_at: DateTime::from_timestamp(0, 0).expect("epoch"),
                updated_at: DateTime::from_timestamp(0, 0).expect("epoch"),
                chat_format_version: version,
            },
            content: TranscriptContent {
                items,
                raw_bytes: base,
                projection_chars: 1_000,
            },
            time_axis,
            usage: UsageSummary::default(),
            reader: ReaderFacts::default(),
            caps: ProjectionCaps::default(),
            first_prompt: None,
        }
    }

    fn mem_input_degraded(items: Vec<xai_grok_sampling_types::ConversationItem>) -> DigestInput {
        let base = mem_items_bytes(&items);
        DigestInput {
            session: SessionFacts {
                session_id: "mem-degraded".into(),
                cwd: "/mem".into(),
                git_root: None,
                head_branch: None,
                started_at: DateTime::from_timestamp(0, 0).expect("epoch"),
                updated_at: DateTime::from_timestamp(0, 0).expect("epoch"),
                chat_format_version: 0, // → LegacyFormat
            },
            content: TranscriptContent {
                items,
                raw_bytes: base,
                // 1000 > 10 → InputTruncated; div_ceil(1000 / 200) = 5 > 4 → ChunkCapHit.
                projection_chars: 1_000,
            },
            time_axis: Vec::new(), // → NoTimeAxis
            usage: UsageSummary::default(),
            reader: ReaderFacts {
                time_degraded: true,
                unreadable_lines: 2, // → UnreadableLines
            },
            caps: ProjectionCaps {
                max_input_chars: 10,
                chunk_chars: 200,
                max_chunk_calls: 4,
                threshold_chars: 10,
            },
            first_prompt: None,
        }
    }
}
