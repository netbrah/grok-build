//! STABLE-REMINDER-1 (apex-ayl.110) — builder-seam pair tests (SDD §4(b)).
//!
//! Drives `patch_responses_request` on the recorded t21 fixture bodies
//! (`smoke/redteam/fixtures/parity/110/` — order-faithful request bodies from
//! the full-sweep t21 redteam capture; META.json carries the source sha256s)
//! and asserts the SDD §2.3 byte-identity invariant in its element-wise form:
//! between two consecutive main-loop requests with unchanged reminder state,
//! every item of the previous request serializes identically in the next one
//! — the previous request's input is an element-wise prefix of the next one
//! (STRICT `kept == prev_bytes`, no tolerance).
//!
//! The four pairs are the documented pre-cut RED pairs (the offline gate
//! `smoke/redteam/test_parity_stablereminder.py --gate` findings of record):
//!   4->6  kept 27,280 of 80,998  (D@5 vs D@10 — D<R, re-anchored mid-prefix)
//!   6->7  kept 34,734 of 84,988  (one-time D<->R order flip to the tail)
//!   7->8  kept 45,495 of 93,318  (steady-state D strip + re-insert at new tail)
//!   8->9  kept 46,403 of 94,229  (same steady-state class)
//!
//! Pre-cut these pairs FAIL (the per-request retain+insert re-places D);
//! post M-append (anchor-pinned D) they must PASS. The test harness owns the
//! per-session anchor state (SDD §2.1 anchor-state home; round-3 R2B-4):
//! the first drive of a pair sets it (first injection at the TAIL), the
//! second consumes it (pin while the raw prefix is byte-identical).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{
    DAnchorState, EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT,
    EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED, PROACTIVE_MULTI_AGENT_MODE_TEXT,
    PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED, patch_responses_request,
};
use xai_grok_sampling_types::ReasoningEffort;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("smoke/redteam/fixtures/parity/110")
        .canonicalize()
        .expect("fixture dir smoke/redteam/fixtures/parity/110 must exist (coordinator-lane artifact)")
}

fn fixture_input(n: u32) -> Vec<Value> {
    let path = fixture_dir().join(format!("req-{:03}_body.json", n));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {}", path.display(), e));
    let body: Value = serde_json::from_str(&text).expect("fixture body is valid JSON");
    body["input"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{} has no input array", path.display()))
}

/// Defensive strip of the developer D item. The fixture bodies are PRE-CUT
/// wire bodies with D already injected by the pre-cut seam at its pre-cut
/// position; the raw conversation input is the body's items minus D, and the
/// seam re-injects D on every drive.
fn raw_input(n: u32) -> Vec<Value> {
    fixture_input(n)
        .into_iter()
        .filter(|item| item.get("role").and_then(Value::as_str) != Some("developer"))
        .collect()
}

/// One seam drive on the sol/codex family at medium effort (the t21 capture
/// shape): serialize, patch, return the patched body. The harness-owned
/// `d_anchor` carries the per-session `<multi_agent_mode>` anchor across
/// drives (round-3 R2B-4 — caller-side state, not part of the pure patch).
fn drive(input: Vec<Value>, d_anchor: &mut DAnchorState) -> Value {
    let mut body = json!({ "input": input });
    patch_responses_request(
        &mut body,
        Some("codex"),
        Some(ReasoningEffort::Medium),
        false,
        None,
        d_anchor,
    );
    body
}

/// Element-wise common prefix of the two patched inputs: item count plus the
/// byte accounting for the RED receipt (compact serde_json per item).
fn common_prefix(prev: &Value, cur: &Value) -> (usize, usize, usize) {
    let pi = prev["input"].as_array().expect("patched body has an input array");
    let ci = cur["input"].as_array().expect("patched body has an input array");
    let k = pi
        .iter()
        .zip(ci.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let kept: usize = pi[..k]
        .iter()
        .map(|item| serde_json::to_string(item).expect("item serializes").len())
        .sum();
    let prev_bytes = serde_json::to_string(pi).expect("input array serializes").len();
    (k, kept, prev_bytes)
}

fn assert_pair_stable(prev_n: u32, cur_n: u32) {
    // The test harness owns the per-session anchor state (SDD §2.1,
    // round-3 R2B-4): the first drive sets it (first injection at the TAIL —
    // the single uniform anchor rule), the second consumes it (pin while the
    // raw prefix is byte-identical).
    let mut d_anchor = DAnchorState::Anchor(None);
    let prev = drive(raw_input(prev_n), &mut d_anchor);
    let cur = drive(raw_input(cur_n), &mut d_anchor);
    let (k, kept, prev_bytes) = common_prefix(&prev, &cur);
    let pi = prev["input"].as_array().unwrap().len();
    let ci = cur["input"].as_array().unwrap().len();
    assert!(
        ci >= pi,
        "pair {}->{}: next request input ({} items) is shorter than the previous ({} items)",
        prev_n,
        cur_n,
        ci,
        pi
    );
    assert_eq!(
        k,
        pi,
        "pair {}->{}: element-wise common prefix covers only {} of {} previous items \
         (kept {} of {} input bytes) — SDD §2.3 strict kept==prev_bytes violated",
        prev_n,
        cur_n,
        k,
        pi,
        kept,
        prev_bytes
    );
}

#[test]
fn pair_4_to_6_stable_prefix() {
    assert_pair_stable(4, 6);
}

#[test]
fn pair_6_to_7_stable_prefix() {
    assert_pair_stable(6, 7);
}

#[test]
fn pair_7_to_8_stable_prefix() {
    assert_pair_stable(7, 8);
}

#[test]
fn pair_8_to_9_stable_prefix() {
    assert_pair_stable(8, 9);
}

// ---------------------------------------------------------------------------
// Synthetic anchor-behavior tests (no fixtures).
// ---------------------------------------------------------------------------

fn item(role: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{ "type": "input_text", "text": text }],
    })
}

fn find_d_index(body: &Value) -> usize {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .position(|i| i.get("role").and_then(Value::as_str) == Some("developer"))
        .expect("exactly one developer (D) item expected")
}

#[test]
fn anchor_pins_d_across_appends() {
    let base = vec![item("user", "u1"), item("assistant", "a1"), item("user", "u2")];
    let mut d_anchor = DAnchorState::Anchor(None);
    let first = drive(base.clone(), &mut d_anchor);
    assert_eq!(
        find_d_index(&first),
        3,
        "first injection places D at the TAIL (single uniform anchor rule)"
    );

    // M-append: items append AFTER D (SDD §4(b) shape — tool-only turn then
    // the next user message).
    let mut extended = base.clone();
    extended.extend([
        item("assistant", "a3"),
        item("assistant", "function_call"),
        item("assistant", "function_call_output"),
        item("user", "u4"),
    ]);
    let second = drive(extended, &mut d_anchor);
    assert_eq!(
        find_d_index(&second),
        3,
        "D stays pinned at its anchored index across the append"
    );
    // The first patched body is an element-wise prefix of the second.
    let fi: Vec<&Value> = first["input"].as_array().unwrap().iter().collect();
    let si: Vec<&Value> = second["input"].as_array().unwrap().iter().collect();
    assert!(si.len() >= fi.len(), "M-append never shrinks the body");
    for (a, b) in fi.iter().zip(si.iter()) {
        assert_eq!(a, b, "M-append: previous body is an element-wise prefix of the next");
    }
}

#[test]
fn anchor_reanchors_at_tail_after_prefix_divergence() {
    let base = vec![item("user", "u1"), item("assistant", "a1"), item("user", "u2")];
    let mut d_anchor = DAnchorState::Anchor(None);
    let _first = drive(base, &mut d_anchor);
    // A declared reset (SDD §6 taxonomy) diverged the raw prefix: an early
    // item is rewritten in place. The stored fingerprint no longer matches,
    // so D re-anchors at the TAIL (the single uniform anchor rule).
    let diverged = vec![
        item("user", "u1-rewritten"),
        item("assistant", "a1"),
        item("user", "u2"),
        item("assistant", "a3"),
    ];
    let second = drive(diverged, &mut d_anchor);
    assert_eq!(
        find_d_index(&second),
        4,
        "stale fingerprint re-anchors D at the TAIL"
    );
    match &d_anchor {
        DAnchorState::Anchor(Some(anchor)) => assert_eq!(
            anchor.position, 4,
            "the re-anchor re-stores the anchor at the new tail placement"
        ),
        other => panic!("anchor must be re-stored after re-anchor: {other:?}"),
    }
}

#[test]
fn in_place_d_content_update_keeps_anchor_position() {
    let input = vec![item("user", "u1"), item("assistant", "a1"), item("user", "u2")];
    let mut d_anchor = DAnchorState::Anchor(None);
    // Sub-ultra: D carries the expanded explicit_request_only sentence.
    let before = drive(input.clone(), &mut d_anchor);
    let d1 = find_d_index(&before);
    let text_before = before["input"][d1]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        text_before.contains("explicit_request_only"),
        "medium-effort codex D text: {text_before:?}"
    );

    // Same input, ultra: the mode text flips to the bare proactive keyword.
    // The change lands IN PLACE at the anchor — same index, fresh bytes
    // (SDD §6 taxonomy item 2) — never a move.
    let mut body = json!({ "input": input });
    patch_responses_request(
        &mut body,
        Some("codex"),
        Some(ReasoningEffort::Ultra),
        false,
        None,
        &mut d_anchor,
    );
    let d2 = find_d_index(&body);
    let text_after = body["input"][d2]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        d2, d1,
        "the D-content reset is a sanctioned one-time IN-PLACE update — the position never moves"
    );
    assert!(text_after.contains("proactive"), "ultra codex D text: {text_after:?}");
    assert_ne!(text_after, text_before, "the mode text actually changed");
    let before_items: Vec<&Value> = before["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != d1)
        .map(|(_, v)| v)
        .collect();
    let after_items: Vec<&Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != d2)
        .map(|(_, v)| v)
        .collect();
    assert_eq!(before_items, after_items, "only D's bytes changed");
}

#[test]
fn legacy_placement_matches_pre_cut_rule() {
    // T2 shape: [u,a,u,a] -> D at index 4 (tail — the input ends non-user).
    let mut d_anchor = DAnchorState::Legacy;
    let mut body = json!({ "input": [
        item("user", "u1"),
        item("assistant", "a1"),
        item("user", "u2"),
        item("assistant", "a2"),
    ]});
    patch_responses_request(
        &mut body,
        Some("codex"),
        Some(ReasoningEffort::Medium),
        false,
        None,
        &mut d_anchor,
    );
    assert_eq!(
        find_d_index(&body),
        4,
        "Legacy: tail when the input ends on a non-user item (T2)"
    );

    // [u,a,u] -> D before the last user message (index 2).
    let mut body = json!({ "input": [
        item("user", "u1"),
        item("assistant", "a1"),
        item("user", "u2"),
    ]});
    patch_responses_request(
        &mut body,
        Some("codex"),
        Some(ReasoningEffort::Medium),
        false,
        None,
        &mut d_anchor,
    );
    assert_eq!(
        find_d_index(&body),
        2,
        "Legacy: before the last user message (T2)"
    );

    // Legacy never stores an anchor (pre-cut byte-identical special case).
    assert!(matches!(d_anchor, DAnchorState::Legacy));
}

// ---------------------------------------------------------------------------
// Forbidden freeze/stale phrasing (SDD §4(b), round-1 R1C-8).
// ---------------------------------------------------------------------------

const FORBIDDEN_PHRASING: &[&str] = &["stale", "frozen", "no longer current"];

/// If `bytes[i]` (an apostrophe) starts a CHAR LITERAL (`'x'`, `'\x'`),
/// return the index just past it; otherwise return `i + 1` (lifetimes like
/// `'static` and stray apostrophes are not literals). The literal content is
/// bounded: a single UTF-8 scalar (≤ 4 bytes) or a backslash escape
/// (`'\''`, `'\\'`, `'\x{…}'` — at most 9 more bytes). Without this, a
/// `"` inside a char literal (e.g. `replace('"', "'")`) desyncs every
/// string/brace tracker that follows.
fn past_char_literal(bytes: &[u8], i: usize) -> usize {
    let n = bytes.len();
    if i + 1 >= n {
        return i + 1;
    }
    if bytes[i + 1] == b'\\' {
        let mut j = i + 2;
        while j < n && j <= i + 10 {
            if bytes[j] == b'\'' {
                return j + 1;
            }
            j += 1;
        }
        return i + 1;
    }
    let mut j = i + 1;
    while j < n && j <= i + 4 {
        if bytes[j] == b'\'' {
            return j + 1;
        }
        j += 1;
    }
    i + 1
}

fn assert_no_forbidden_phrasing_in_text(label: &str, text: &str) {
    let lower = text.to_lowercase();
    for token in FORBIDDEN_PHRASING {
        assert!(
            !lower.contains(token),
            "{label}: forbidden freeze/stale phrasing {token:?} in {text:?} (R1C-8)"
        );
    }
}

fn assert_no_forbidden_phrasing_in_source(label: &str, src: &str) {
    for literal in string_literals(&strip_comments(src)) {
        assert_no_forbidden_phrasing_in_text(label, &literal);
    }
}

/// Strip `//` and `/* */` comments (string-aware) so quoted prose in docs —
/// e.g. the "FROZEN arm" comments — cannot false-positive the literal scan.
fn strip_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_str = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            b'\'' => {
                let past = past_char_literal(bytes, i);
                for k in i..past {
                    out.push(bytes[k]);
                }
                i = past;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                out.push(b' ');
                out.push(b' ');
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("source is valid UTF-8")
}

/// Extract the `"..."` string-literal payloads (escape-aware).
fn string_literals(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            i += 1;
            loop {
                if i >= bytes.len() {
                    break;
                }
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    out.push(src[start..i].to_string());
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else {
            if bytes[i] == b'\'' {
                i = past_char_literal(bytes, i);
                continue;
            }
            i += 1;
        }
    }
    out
}

/// The balanced-brace body of `fn <fn_name>(...)` (string- and comment-aware).
fn fn_body<'a>(src: &'a str, fn_name: &str) -> &'a str {
    let needle = format!("fn {fn_name}(");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("fn {fn_name} not found"));
    let open = src[start..].find('{').expect("fn body brace");
    let abs = start + open;
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut i = abs;
    let mut in_str = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'\'' => {
                i = past_char_literal(bytes, i);
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                in_line_comment = true;
                i += 2;
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                in_block_comment = true;
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[abs..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced braces in fn {fn_name}")
}

#[test]
fn forbidden_phrasing_scan_r1c_8() {
    // The D-mode-text constants (SDD §4(b) R1C-8) — 4 at the post-.86 landing
    // (the two bare keywords + both expanded twins, AXIS 2).
    assert_no_forbidden_phrasing_in_text(
        "PROACTIVE_MULTI_AGENT_MODE_TEXT",
        PROACTIVE_MULTI_AGENT_MODE_TEXT,
    );
    assert_no_forbidden_phrasing_in_text(
        "PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED",
        PROACTIVE_MULTI_AGENT_MODE_TEXT_EXPANDED,
    );
    assert_no_forbidden_phrasing_in_text(
        "EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT",
        EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT,
    );
    assert_no_forbidden_phrasing_in_text(
        "EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED",
        EXPLICIT_REQUEST_ONLY_MULTI_AGENT_MODE_TEXT_EXPANDED,
    );

    // The reminder emission templates (R1C-8) — STRING LITERALS ONLY, scoped
    // to the named template fns: whole-file scans false-positive on
    // unrelated "stale"/"frozen" prose (log messages, docs).
    let search_tool = include_str!("../../xai-grok-tools/src/implementations/search_tool/mod.rs");
    assert_no_forbidden_phrasing_in_source(
        "search_tool/mod.rs::build_server_reminder",
        fn_body(search_tool, "build_server_reminder"),
    );
    assert_no_forbidden_phrasing_in_source(
        "search_tool/mod.rs::build_delta_reminder",
        fn_body(search_tool, "build_delta_reminder"),
    );
    let mcp_failed =
        include_str!("../../xai-grok-shell/src/session/acp_session_impl/mcp_failed_reminder.rs");
    assert_no_forbidden_phrasing_in_source(
        "mcp_failed_reminder.rs::render_failed_section",
        fn_body(mcp_failed, "render_failed_section"),
    );
    let mcp = include_str!("../../xai-grok-shell/src/session/acp_session_impl/mcp.rs");
    assert_no_forbidden_phrasing_in_source(
        "mcp.rs::format_mcp_connecting_reminder",
        fn_body(mcp, "format_mcp_connecting_reminder"),
    );
    let reminders =
        include_str!("../../xai-grok-shell/src/session/acp_session_impl/reminders.rs");
    assert_no_forbidden_phrasing_in_source(
        "reminders.rs::wrap_in_reminder_tag",
        fn_body(reminders, "wrap_in_reminder_tag"),
    );
}
