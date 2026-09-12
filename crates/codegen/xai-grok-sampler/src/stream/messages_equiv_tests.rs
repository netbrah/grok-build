//! stream ≡ non-stream golden infrastructure (MW-3 R4) — the hce-05 port.
//!
//! Re-expressed (D1) from xli@3d4a08271e `codex-api/tests/stream_equiv.rs`
//! (HCE-05) + its `tests/fixtures/stream_equiv/` tree (20 fixture dirs,
//! byte-exact here, MANIFEST.sha256-verified). Grok runs the DISCLOSED
//! 19-SUPERSET: xli registers 17/20 (gemini eq-17/18 spokes-skipped there,
//! eq-19 unregistered); eq-15 is responses-only (NO messages_sse.json) and
//! is non-replayable on the messages route — its expected-side role is
//! documented below, not replayed.
//!
//! Contract (spec R4): each replayable fixture is fed through the SAME
//! decode path the production messages client uses (`eventsource()` →
//! `[DONE]` clean-end → `try_parse_stream_error` → strict serde →
//! `stream_messages`), projected by the adapter below — the ONLY new code
//! in the golden path — and compared to a checked-in golden. Goldens were
//! re-captured ONCE after the adapter landed (MW-1 D6 A7-last flow; first
//! capture = the golden; re-baseline only via the documented env var when
//! a pipeline stage changes).
//!
//! D5 (tamper guard): MANIFEST.sha256 is verified before the suite runs;
//! a fixture mismatch ABORTS the suite (not a test failure) — fresh grok
//! infrastructure, xli's runner has no manifest check.

use super::*;
use crate::events::SamplingEvent;
use eventsource_stream::Eventsource;
use futures_util::stream;
use sha2::{Digest, Sha256};
use xai_grok_sampling_types::messages::MessageStreamEvent;
use xai_grok_sampling_types::{ConversationResponse, SamplingError};

const FIXTURE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/stream_equiv");
const GOLDEN_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/stream_equiv_goldens");

/// The 19 replayable fixtures (xli's 17-registered superset + eq-19; minus
/// the non-replayable eq-15). eq-17/eq-18 (gemini) are registered with the
/// ROUTING-SEMANTIC "reference-only" flag — DECISION-2: the flag lives in
/// test registration ONLY (never in the D5-immutable metadata.json); no
/// assertion about gemini routing is made (item 11 consumes the evidence).
const REPLAY_FIXTURES: &[&str] = &[
    "eq-01-text-normal",
    "eq-02-text-tool-normal",
    "eq-03-text-thinking-normal",
    "eq-04-thinking-tool-normal",
    "eq-05-mcp-namespaced",
    "eq-06-parallel-tools",
    "eq-07-tool-with-preamble",
    "eq-08-text-ping",
    "eq-09-tool-ping",
    "eq-14-redacted-thinking",
    "eq-16-thinking-tool-response",
    "eq-17-gemini-text-normal",
    "eq-18-gemini-text-tool",
];
/// eq-17/eq-18: routing-semantic reference-only flag (registration only).
const GEMINI_REFERENCE_ONLY: &[&str] = &["eq-17-gemini-text-normal", "eq-18-gemini-text-tool"];
/// Policy-assert fixtures: typed terminal + grok-world failure class, no
/// golden (xli policy_assertions re-expressed — see each test).
const POLICY_FIXTURES: &[&str] = &[
    "eq-10-tool-truncated-invalid-json",
    "eq-11-tool-no-stop",
    "eq-12-parallel-one-bad",
    "eq-13-provider-partial",
    "eq-19-thinking-truncated",
    "eq-20-tool-truncated-mid",
];

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Parse `MANIFEST.sha256` in `root` and verify every listed file.
/// Detection half (returns a typed error so the tamper test can assert
/// without aborting the process).
fn verify_manifest(root: &str) -> Result<(), String> {
    let manifest = std::fs::read_to_string(format!("{root}/MANIFEST.sha256"))
        .map_err(|e| format!("MANIFEST.sha256 unreadable in {root}: {e}"))?;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (expected, rel) = line
            .split_once("  ")
            .ok_or_else(|| format!("malformed MANIFEST line: {line:?}"))?;
        let bytes = std::fs::read(format!("{root}/{rel}"))
            .map_err(|e| format!("fixture missing: {rel}: {e}"))?;
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(format!(
                "fixture tampered: {rel}: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

/// D5 suite abort: the main fixture tree verifies ONCE per test process;
/// any mismatch kills the suite (a tampered fixture invalidates every
/// golden, so continuing would paper over it).
static MAIN_TREE_VERIFIED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

fn require_main_tree_verified() {
    MAIN_TREE_VERIFIED.get_or_init(|| {
        if let Err(e) = verify_manifest(FIXTURE_ROOT) {
            eprintln!("stream_equiv MANIFEST verification failed: {e}");
            std::process::abort();
        }
    });
}

/// Read the fixture's `messages_sse.json` (a JSON array of physical SSE
/// lines) and re-emit it as an SSE byte stream, exactly as the wire would
/// deliver it.
fn fixture_sse_bytes(dir: &str) -> String {
    let raw = std::fs::read(format!("{FIXTURE_ROOT}/{dir}/messages_sse.json"))
        .unwrap_or_else(|e| panic!("fixture unreadable: {dir}: {e}"));
    let lines: Vec<String> = serde_json::from_slice(&raw)
        .unwrap_or_else(|e| panic!("fixture lines not a string array: {dir}: {e}"));
    let mut out = String::new();
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Replay one fixture through the production decode mirror
/// (client.rs messages stream: `eventsource()` → `[DONE]` clean end →
/// `try_parse_stream_error` → strict serde → `SamplingError::Serialization`)
/// into `stream_messages`, collecting every yielded event.
async fn replay_fixture(dir: &str) -> Vec<SamplingEvent> {
    require_main_tree_verified();
    let sse = fixture_sse_bytes(dir);
    let byte_stream = stream::iter(vec![Ok::<_, std::io::Error>(axum::body::Bytes::from(sse))]);
    let mut decoded: Vec<Result<MessageStreamEvent, SamplingError>> = Vec::new();
    let mut events = byte_stream.eventsource();
    use futures_util::StreamExt;
    while let Some(result) = events.next().await {
        let Ok(sse_event) = result else { break };
        let data = sse_event.data;
        if data == "[DONE]" {
            break;
        }
        let item = match xai_grok_sampling_types::error::try_parse_stream_error(&data) {
            Some(err) => Err(err),
            None => serde_json::from_str::<MessageStreamEvent>(&data)
                .map_err(SamplingError::Serialization),
        };
        decoded.push(item);
    }
    let raw = stream::iter(decoded).boxed();
    collect(stream_messages(
        raw,
        None,
        RequestId::from("stream-equiv"),
        Duration::from_secs(60),
    ))
    .await
}

async fn collect(s: impl futures_util::Stream<Item = SamplingEvent>) -> Vec<SamplingEvent> {
    let mut out = Vec::new();
    let mut s = std::pin::pin!(s);
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

/// Terminal helper: assert `events` ends in `Completed` and return the
/// response (the no_throw invariant — a typed terminal, never a panic).
fn take_completed<'a>(dir: &str, events: &'a [SamplingEvent]) -> &'a ConversationResponse {
    match events
        .last()
        .unwrap_or_else(|| panic!("{dir}: no terminal event"))
    {
        SamplingEvent::Completed { response, .. } => response.as_ref(),
        SamplingEvent::Failed { error, .. } => {
            panic!("{dir}: expected Completed, got Failed({})", error.message)
        }
        other => panic!("{dir}: expected a terminal event, got {other:?}"),
    }
}

/// Assert `events` ends in `Failed` with the given stream-error type and
/// message phrase (the grok-world policy re-expression).
fn take_failed(dir: &str, events: &[SamplingEvent], error_type: &str, phrase: &str) {
    match events
        .last()
        .unwrap_or_else(|| panic!("{dir}: no terminal event"))
    {
        SamplingEvent::Failed { error, .. } => {
            assert!(error.is_retryable, "{dir}: stream errors stay retryable");
            assert!(
                error
                    .message
                    .contains(&format!("stream error ({error_type})")),
                "{dir}: expected error_type {error_type}, got {}",
                error.message
            );
            assert!(
                error.message.contains(phrase),
                "{dir}: expected {phrase:?} in {}",
                error.message
            );
        }
        other => panic!("{dir}: expected Failed({error_type}), got {other:?}"),
    }
}

/// The projection adapter — the ONLY new code in the golden path.
/// Normalizes a grok `ConversationResponse` to the comparator's canonical
/// shape: stop_reason (post-transform-override), text (blocks already
/// joined with "\n" by the transform), reasoning (the trailing reasoning
/// sibling), tool_calls (arguments as parsed JSON when valid, else the raw
/// string), usage sums, message_id, model.
fn project_response(response: &ConversationResponse) -> serde_json::Value {
    let assistant = response.assistant();
    let tool_calls: Vec<serde_json::Value> = assistant
        .map(|a| {
            a.tool_calls
                .iter()
                .map(|tc| {
                    let arguments = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .unwrap_or(serde_json::Value::String(tc.arguments.to_string()));
                    serde_json::json!({
                        "id": tc.id.to_string(),
                        "name": tc.name,
                        "arguments": arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let reasoning = response.reasoning_items().next().map(|r| {
        let text = r
            .summary
            .iter()
            .filter_map(|p| match p {
                xai_grok_sampling_types::rs::SummaryPart::SummaryText(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::json!({
            "text": text,
            "encrypted_content": r.encrypted_content,
        })
    });
    let usage = response.usage.as_ref().map(|u| {
        serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
            "reasoning_tokens": u.reasoning_tokens,
            "cached_prompt_tokens": u.cached_prompt_tokens,
            "cache_creation_prompt_tokens": u.cache_creation_prompt_tokens,
        })
    });
    serde_json::json!({
        "message_id": response.message_id,
        "model": assistant.and_then(|a| a.model_id.clone()),
        "stop_reason": response.stop_reason.as_ref().map(|s| s.as_ref().to_string()),
        "text": assistant.map(|a| a.content.to_string()).unwrap_or_default(),
        "reasoning": reasoning,
        "tool_calls": tool_calls,
        "usage": usage,
    })
}

/// Compare `events` (a comparison fixture) against the checked-in golden.
/// First-capture mode (`GROK_STREAM_EQUIV_GOLDEN=1`) writes the golden —
/// the ONE documented re-baseline path (a pipeline stage changed).
async fn assert_stream_equiv(dir: &str) {
    let events = replay_fixture(dir).await;
    let response = take_completed(dir, &events);
    let projection = project_response(response);
    let golden_path = format!("{GOLDEN_ROOT}/{dir}/projection.json");
    if std::env::var_os("GROK_STREAM_EQUIV_GOLDEN").is_some() {
        if let Some(parent) = std::path::Path::new(&golden_path).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            &golden_path,
            serde_json::to_string_pretty(&projection).unwrap() + "\n",
        )
        .unwrap();
        return;
    }
    let golden = match std::fs::read_to_string(&golden_path) {
        Ok(g) => g,
        Err(_) => panic!(
            "{dir}: golden missing — run once with GROK_STREAM_EQUIV_GOLDEN=1 (first capture)"
        ),
    };
    let golden_value: serde_json::Value =
        serde_json::from_str(&golden).unwrap_or_else(|e| panic!("{dir}: golden invalid: {e}"));
    assert_eq!(
        projection, golden_value,
        "{dir}: stream projection diverged from the non-stream golden"
    );
}

// ── D5 MANIFEST tests ──────────────────────────────────────────────────────

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/MANIFEST.sha256 (infrastructure; the manifest check itself is fresh grok infra per spec D5 — xli's runner has none)
/// 20/20 on-disk: every fixture dir exists and every MANIFEST-listed file
/// verifies byte-exact (82 files). Suite-verified: a mismatch would abort
/// via require_main_tree_verified before any replay.
#[test]
fn manifest_verifies_all_twenty_fixtures_on_disk() {
    require_main_tree_verified();
    let entries: Vec<String> = std::fs::read_dir(FIXTURE_ROOT)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries.len(),
        20,
        "20 fixture dirs on disk, got {entries:?}"
    );
    for i in 1..=20usize {
        let prefix = format!("eq-{i:02}-");
        assert!(
            entries.iter().any(|n| n.starts_with(&prefix)),
            "missing {prefix}* dir"
        );
    }
    // 20/20: each dir's messages-side entry set is present per its role
    let sse_count = entries
        .iter()
        .filter(|d| {
            std::path::Path::new(FIXTURE_ROOT)
                .join(d)
                .join("messages_sse.json")
                .is_file()
        })
        .count();
    assert_eq!(
        sse_count, 19,
        "19/20 carry messages_sse.json (eq-15 is responses-only)"
    );
}

/// Provenance: fresh grok infra (spec D5 — xli's runner has no manifest check): a single-byte fixture mutation must be DETECTED; the replay path turns detection into a suite abort (require_main_tree_verified).
#[test]
fn manifest_tamper_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_string_lossy().into_owned();
    let fixture = format!("{FIXTURE_ROOT}/eq-01-text-normal");
    // Copy the fixture dir + a minimal manifest for it.
    let dst = format!("{root}/eq-01-text-normal");
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&fixture).unwrap().filter_map(|e| e.ok()) {
        let from = entry.path();
        if from.is_file() {
            std::fs::copy(
                &from,
                format!("{dst}/{}", from.file_name().unwrap().to_string_lossy()),
            )
            .unwrap();
        }
    }
    // The temp manifest lists ONLY the eq-01 files (same relative names;
    // sha256 is path-independent).
    let lines: Vec<String> = std::fs::read_to_string(format!("{FIXTURE_ROOT}/MANIFEST.sha256"))
        .unwrap()
        .lines()
        .filter(|l| l.contains("eq-01-text-normal/"))
        .map(|l| l.to_owned())
        .collect();
    assert_eq!(lines.len(), 3, "eq-01 has three MANIFEST-listed files");
    std::fs::write(format!("{root}/MANIFEST.sha256"), lines.join("\n") + "\n").unwrap();
    assert!(
        verify_manifest(&root).is_ok(),
        "an unmodified copy must verify"
    );
    // Tamper one byte of messages_sse.json → detection.
    let target = format!("{dst}/messages_sse.json");
    let mut bytes = std::fs::read(&target).unwrap();
    let last = bytes.len() - 1;
    bytes[last] = if bytes[last] == b']' { b'[' } else { b']' };
    std::fs::write(&target, bytes).unwrap();
    let err = verify_manifest(&root).unwrap_err();
    assert!(
        err.contains("tampered") && err.contains("eq-01-text-normal/messages_sse.json"),
        "tamper must name the file: {err}"
    );
}

// ── 19/19 replays ──────────────────────────────────────────────────────────
// Comparison fixtures: replay → Completed → projection == golden (first
// capture = the golden, committed). Policy fixtures: replay → typed
// terminal (no_throw) with the grok-world failure class.

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-01-text-normal (re-expressed; replayed through the grok transform; golden first-captured post-adapter per spec R4)
#[tokio::test]
async fn eq_01_text_normal_stream_equiv() {
    assert_stream_equiv("eq-01-text-normal").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-02-text-tool-normal (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_02_text_tool_normal_stream_equiv() {
    assert_stream_equiv("eq-02-text-tool-normal").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-03-text-thinking-normal (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_03_text_thinking_normal_stream_equiv() {
    assert_stream_equiv("eq-03-text-thinking-normal").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-04-thinking-tool-normal (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_04_thinking_tool_normal_stream_equiv() {
    assert_stream_equiv("eq-04-thinking-tool-normal").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-05-mcp-namespaced (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_05_mcp_namespaced_stream_equiv() {
    assert_stream_equiv("eq-05-mcp-namespaced").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-06-parallel-tools (re-expressed; arrival/INDEX order per DECISION-1 — the golden pins the order; golden first-captured)
#[tokio::test]
async fn eq_06_parallel_tools_stream_equiv() {
    assert_stream_equiv("eq-06-parallel-tools").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-07-tool-with-preamble (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_07_tool_with_preamble_stream_equiv() {
    assert_stream_equiv("eq-07-tool-with-preamble").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-08-text-ping (re-expressed; ping liveness; golden first-captured)
#[tokio::test]
async fn eq_08_text_ping_stream_equiv() {
    assert_stream_equiv("eq-08-text-ping").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-09-tool-ping (re-expressed; ping liveness; golden first-captured)
#[tokio::test]
async fn eq_09_tool_ping_stream_equiv() {
    assert_stream_equiv("eq-09-tool-ping").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-14-redacted-thinking (re-expressed; the redacted block opens inert and its stop is a no-op — C2 recording keeps the replay green; golden first-captured)
#[tokio::test]
async fn eq_14_redacted_thinking_stream_equiv() {
    assert_stream_equiv("eq-14-redacted-thinking").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-16-thinking-tool-response (re-expressed; golden first-captured)
#[tokio::test]
async fn eq_16_thinking_tool_response_stream_equiv() {
    assert_stream_equiv("eq-16-thinking-tool-response").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-17-gemini-text-normal (ported + registered per DECISION-2; REFERENCE-ONLY routing flag in this registration only — real anthropic-shaped SSE that exercises the decoder; NO gemini routing assertion; item 11 consumes the evidence; golden first-captured)
#[tokio::test]
async fn eq_17_gemini_text_normal_stream_equiv_reference_only() {
    assert!(
        GEMINI_REFERENCE_ONLY.contains(&"eq-17-gemini-text-normal"),
        "routing flag must live in registration"
    );
    assert_stream_equiv("eq-17-gemini-text-normal").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-18-gemini-text-tool (ported + registered per DECISION-2; REFERENCE-ONLY routing flag in this registration only; NO gemini routing assertion; golden first-captured)
#[tokio::test]
async fn eq_18_gemini_text_tool_stream_equiv_reference_only() {
    assert!(
        GEMINI_REFERENCE_ONLY.contains(&"eq-18-gemini-text-tool"),
        "routing flag must live in registration"
    );
    assert_stream_equiv("eq-18-gemini-text-tool").await;
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-10-tool-truncated-invalid-json (re-expressed; xli policy no_throw + tool_args_state → grok-world typed terminal: Failed(invalid_tool_args), the R3 deterministic class for provider-asserted tool_use terminals whose args are not valid JSON)
#[tokio::test]
async fn eq_10_tool_truncated_invalid_json_policy() {
    let events = replay_fixture("eq-10-tool-truncated-invalid-json").await;
    take_failed("eq-10", &events, "invalid_tool_args", "not valid JSON");
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-11-tool-no-stop (re-expressed; xli policy no_throw → grok-world typed terminal: Failed(unclosed_blocks) — the tool block never closed before message_stop)
#[tokio::test]
async fn eq_11_tool_no_stop_policy() {
    let events = replay_fixture("eq-11-tool-no-stop").await;
    take_failed(
        "eq-11",
        &events,
        "unclosed_blocks",
        "before all blocks ended",
    );
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-12-parallel-one-bad (re-expressed; one bad of two parallel tool blocks → Failed(invalid_tool_args) — the good block's args do not excuse the bad one)
#[tokio::test]
async fn eq_12_parallel_one_bad_policy() {
    let events = replay_fixture("eq-12-parallel-one-bad").await;
    take_failed("eq-12", &events, "invalid_tool_args", "not valid JSON");
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-13-provider-partial (re-expressed; provider-partial JSON, same terminal shape as eq-10 → Failed(invalid_tool_args))
#[tokio::test]
async fn eq_13_provider_partial_policy() {
    let events = replay_fixture("eq-13-provider-partial").await;
    take_failed("eq-13", &events, "invalid_tool_args", "not valid JSON");
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-19-thinking-truncated (ported + registered — unregistered in xli; grok runs the disclosed 19-superset; thinking block open at message_stop → Failed(unclosed_blocks))
#[tokio::test]
async fn eq_19_thinking_truncated_policy() {
    let events = replay_fixture("eq-19-thinking-truncated").await;
    take_failed(
        "eq-19",
        &events,
        "unclosed_blocks",
        "before all blocks ended",
    );
}

/// Provenance: xli@3d4a08271e — codex-api/tests/fixtures/stream_equiv/eq-20-tool-truncated-mid (re-expressed; per-spoke truncation policy shape, terminal as eq-10 → Failed(invalid_tool_args))
#[tokio::test]
async fn eq_20_tool_truncated_mid_policy() {
    let events = replay_fixture("eq-20-tool-truncated-mid").await;
    take_failed("eq-20", &events, "invalid_tool_args", "not valid JSON");
}

/// eq-15 is responses-only (no messages_sse.json) — NON-REPLAYABLE on the
/// messages route. Its role (xli): the expected-side fixture for the
/// responses-wire projection comparator (non_stream_response.json +
/// responses_events.json). Documented here per spec R4 (G7); it IS verified
/// on-disk by the MANIFEST test (20/20) but has no replay by design.
#[test]
fn eq_15_responses_only_is_documented_non_replayable() {
    require_main_tree_verified();
    let dir = std::path::Path::new(FIXTURE_ROOT).join("eq-15-tool-response-only");
    assert!(
        !dir.join("messages_sse.json").is_file(),
        "eq-15 has no messages SSE"
    );
    assert!(dir.join("non_stream_response.json").is_file());
    assert!(dir.join("responses_events.json").is_file());
    assert!(
        !REPLAY_FIXTURES
            .iter()
            .chain(POLICY_FIXTURES)
            .any(|d| d.starts_with("eq-15")),
        "eq-15 must not be registered for replay"
    );
}
