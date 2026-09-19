//! Switch-time projector — proactive per-item cross-wire projection (apex-ayl.71).
//!
//! Companion to `sdd-71-projector.md`: applies the T0-T3 fidelity ladder
//! (xwfix README) per item, at model-switch time, to the STORAGE form
//! (`ConversationItem`) — dialect-agnostic (the send pipeline stays
//! dialect-specific). No new storage: per-item provenance comes from the
//! per-turn `model_id` fields already persisted on assistant records
//! (sdd-71 §2).
//!
//! Ladder (per reasoning item; tool/user/assistant items follow the
//! §4 invariants):
//! - T0 — same model (= same boundary): verbatim, id + field markers kept
//!   (post-.75 same-boundary KEEP default).
//! - T1 — foreign origin: `xw_` re-key + `encrypted_content` stripped +
//!   summary kept. The one no-re-key row is AZ -> AZ (strict targets: the
//!   strict projector IS the strip — id+content removed pre-send, so the
//!   store keeps the original id; matrix §1.4, sdd-71 §3).
//! - T3 (vertex targets only) — the `/messages` wire has no client-wire
//!   site for non-carrier backend tool call items (the build degrades them
//!   to synthetic text and its D5 pairing drops any result that does not
//!   pair with an ASSISTANT call), so the pair-atomic remedy is drop both
//!   (xwfix invariant 3 / sdd-71 §4 invariant 1). `CodexRawInput` carriers
//!   are the exception: carrier survival (invariant 2/4) keeps them opaque
//!   on every tier.
//!
//! Invariants enforced (sdd-71 §4): pairing integrity · carrier survival ·
//! idempotence (`project(project(h)) == project(h)`) · no empty id · no
//! foreign `encrypted_content` · byte-identity of non-projected items.
//!
//! Dependency note (sdd-71 §9 step 7, the named G3 item): the T1 id grammar
//! needs SHA-256. `sha2` is a workspace dependency but NOT a direct
//! dependency of this crate, and adding one is outside this cut's file set
//! (any pathspec expansion is a coordinator ruling), so the digest is
//! implemented self-contained below and byte-pinned by the 12/12
//! known-answer goldens in `projection_tests::xw_proj_id_grammar_canonical`
//! plus the in-file KATs.

use std::collections::HashSet;

use serde_json::Value;

use super::{BackendToolCallItem, BackendToolKind, ConversationItem};
use crate::catalog_wire::{CatalogFamily, catalog_family};
use crate::messages_model::is_anthropic_model;
use crate::rs::ReasoningItem;

/// Target boundary regime for a switch projection (prep report decision D2 —
/// the three regimes the Table-2 rows need: sol/terra = AZ-strict rows,
/// qwen/glm = VL-lenient vLLM-shim rows, sonnet = VX-M vertex rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// AZ-strict rows (gpt-5.6 sol/terra/luna): the strict projector strips
    /// id+content pre-send — the schema form IS the strip; do NOT re-key.
    AzStrict,
    /// VL-lenient rows (vLLM-shim families, qwen/glm): T1 re-key for foreign
    /// reasoning, T0 for own.
    VLLenient,
    /// Vertex rows (VX-M, claude on `/messages`): the build has no
    /// responses-native site for backend tool call items; non-carrier calls
    /// are pair-atomically dropped with their results at projection time.
    Vertex,
}

/// Projected post-switch history (prep report decision D2 — public `items`
/// field; the L0 suite accesses it through the single `proj_items` choke
/// point).
#[derive(Debug, Clone)]
pub struct ProjectedHistory {
    pub items: Vec<ConversationItem>,
}

/// Project a persisted history for a cross-wire switch to
/// `target_model_id` on `boundary` (sdd-71 §9 — decision D1: the name and
/// signature are fixed by the spec and the Table-2 tests).
///
/// The `target_model_id` string doubles as the ID-grammar `{cell}` slot
/// (D3 / sdd-71 §5: at runtime it is the target row's model id; in L0 unit
/// tests the fixture cell name is passed explicitly so the 12/12 goldens
/// reproduce).
pub fn project_switch_history(
    items: &[ConversationItem],
    target_model_id: &str,
    boundary: Boundary,
) -> ProjectedHistory {
    // Vertex targets only — mirror the /messages build's D5 pairing
    // (`clean_orphaned_items`, conversation/messages.rs): a tool_result
    // survives on that wire only when an ASSISTANT tool_call pairs with it.
    // A result paired only with a backend call (or with nothing) is dropped
    // at build time, so the store drops it proactively to keep the storage
    // form equal to what the target wire can represent.
    let mut assistant_call_ids: HashSet<String> = HashSet::new();
    if boundary == Boundary::Vertex {
        for item in items {
            if let ConversationItem::Assistant(a) = item {
                for tc in &a.tool_calls {
                    assistant_call_ids.insert(tc.id.as_ref().to_string());
                }
            }
        }
    }

    let mut projected = Vec::with_capacity(items.len());
    let mut reasoning_ord = 0usize;
    for (idx, item) in items.iter().enumerate() {
        match item {
            ConversationItem::Reasoning(r) => {
                let owner = forward_owner_model(items, idx);
                projected.push(ConversationItem::Reasoning(project_reasoning(
                    r,
                    owner,
                    target_model_id,
                    boundary,
                    reasoning_ord,
                )));
                reasoning_ord += 1;
            }
            ConversationItem::BackendToolCall(b)
                if boundary == Boundary::Vertex && !is_carrier(b) =>
            {
                // T3: the vertex wire has no site for this class; its result
                // (if any) is co-dropped by the ToolResult arm below —
                // pair-atomic, portable transcript intact.
            }
            ConversationItem::ToolResult(t)
                if boundary == Boundary::Vertex
                    && !assistant_call_ids.contains(t.tool_call_id.as_str()) =>
            {
                // Pair-atomic co-drop (covers results paired only with a
                // dropped backend call and pre-existing orphans alike).
            }
            other => projected.push(other.clone()),
        }
    }

    ProjectedHistory { items: projected }
}

/// Forward attribution (sdd-71 §2.3): the owning model of a reasoning item
/// is the `model_id` of the next `Assistant` item after it that carries a
/// resolvable model. `None` when unresolvable (trailing run / no owner) —
/// fail-closed: treat as foreign, never KEEP.
fn forward_owner_model(items: &[ConversationItem], idx: usize) -> Option<&str> {
    items.iter().skip(idx + 1).find_map(|item| match item {
        ConversationItem::Assistant(a) => a.model_id.as_deref(),
        _ => None,
    })
}

/// One reasoning item through the ladder (sdd-71 §3 decision table).
fn project_reasoning(
    r: &ReasoningItem,
    owner: Option<&str>,
    target_model_id: &str,
    boundary: Boundary,
    ord: usize,
) -> ReasoningItem {
    // T0 — same model = same boundary (sdd-71 §2 rule (i); post-.75
    // same-boundary KEEP default): verbatim, id + field markers kept.
    if owner == Some(target_model_id) {
        return r.clone();
    }

    // Cross-boundary (or cross-deployment within one boundary): T1 —
    // encrypted_content stripped proactively (D-ENC's job, moved to switch
    // time) + re-key — EXCEPT the one no-re-key row, AZ -> AZ: on strict
    // targets the strict projector IS the strip (id+content removed
    // pre-send), so the store keeps the original id.
    let keep_original_id = boundary == Boundary::AzStrict
        && owner.map(model_boundary_class) == Some(Boundary::AzStrict);
    let mut id = if keep_original_id {
        r.id.clone()
    } else {
        xw_reasoning_id(target_model_id, ord, r)
    };

    // Invariant 4 (no empty id, the .69 class): a no-re-key id that is
    // empty is an anomaly (strict rows mint `encitem_` markers); repair
    // with the T1 synthesis so no projected reasoning carries `id:""`. The
    // strict send path strips the id pre-send either way.
    if id.is_empty() {
        id = xw_reasoning_id(target_model_id, ord, r);
    }

    ReasoningItem {
        id,
        summary: r.summary.clone(),
        content: r.content.clone(),
        encrypted_content: None,
        status: r.status.clone(),
    }
}

/// Classify a model row into its boundary regime for the re-key decision.
///
/// Slug-keyed (no row metadata is visible at this seam — the row-aware
/// config fields live in the sampler; the L0 projector classifies from the
/// model id, the same key the crate's slug fallbacks use):
/// - `gpt-*` / o-series slugs (the AZ-strict rows) -> [`Boundary::AzStrict`]
/// - `claude*` slugs (vertex rows, `/messages` wire) -> [`Boundary::Vertex`]
/// - everything else (vLLM-shim families, grok, unknown) ->
///   [`Boundary::VLLenient`] — the fail-closed lenient class: unknown
///   owners are never KEEP-eligible (sdd-71 §2.3) and foreign origins on
///   lenient targets re-key.
pub fn model_boundary_class(model_id: &str) -> Boundary {
    match catalog_family(model_id) {
        CatalogFamily::OpenAi => Boundary::AzStrict,
        _ if is_anthropic_model(model_id) => Boundary::Vertex,
        _ => Boundary::VLLenient,
    }
}

/// Whether a backend tool call item is a `CodexRawInput` carrier (the
/// compaction-carrier class that survives projection opaquely on every
/// tier — xwfix invariant 4 / sdd-71 §4 invariant 2).
fn is_carrier(b: &BackendToolCallItem) -> bool {
    matches!(b.kind, BackendToolKind::CodexRawInput(_))
}

/// T1 id synthesis (sdd-71 §5; xwfix README "Id synthesis (T1)"; shared
/// with .69 — one grammar, by construction):
///
/// ```text
/// id = "xw_" + sha256(
///     "{cell}|{ord}|{json.dumps(content,sort_keys=True)}|{json.dumps(summary,sort_keys=True)}"
/// ).hexdigest()[:24]
/// ```
///
/// - `{cell}` := the target row's model id (D3 slot binding; L0 tests pass
///   the fixture cell name).
/// - `{ord}`  := 0-based index of the reasoning item among the reasoning
///   records of the projected history.
/// - A record whose storage form has no `content` key canonicalizes to `[]`
///   (that is what made the 12/12 goldens reproduce).
/// - Canonicalization is Python `json.dumps` defaults, NOT serde_json's
///   compact form (`py_json_canonicalize` below).
fn xw_reasoning_id(cell: &str, ord: usize, r: &ReasoningItem) -> String {
    let content = match &r.content {
        None => Value::Array(Vec::new()),
        Some(parts) => serde_json::to_value(parts).expect("ReasoningTextContent must serialize"),
    };
    let summary = serde_json::to_value(&r.summary).expect("SummaryPart must serialize");

    let canonical_content = py_json_canonicalize(&content);
    let canonical_summary = py_json_canonicalize(&summary);
    let mut preimage = String::with_capacity(
        cell.len() + 32 + canonical_content.len() + canonical_summary.len(),
    );
    preimage.push_str(cell);
    preimage.push('|');
    preimage.push_str(&ord.to_string());
    preimage.push('|');
    preimage.push_str(&canonical_content);
    preimage.push('|');
    preimage.push_str(&canonical_summary);

    let digest = sha256(preimage.as_bytes());
    let hex = hex_digest(&digest);
    format!("xw_{}", &hex[..24])
}

/// Python-`json.dumps`-default canonicalization (sdd-71 §5
/// "canonicalization parity — implementation-critical"): sorted object
/// keys, `", "` / `": "` separators, `ensure_ascii` escaping. serde_json's
/// default (compact separators, unescaped UTF-8) does NOT match Python and
/// would break the 12/12 known-answer goldens.
fn py_json_canonicalize(value: &Value) -> String {
    let mut out = String::new();
    py_json_write(value, &mut out);
    out
}

fn py_json_write(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&py_number_repr(n)),
        Value::String(s) => py_json_write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_write(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Python sorts object keys by code point; Rust `String` order
            // (UTF-8 byte order) is the same order for valid UTF-8.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_write_string(key, out);
                out.push_str(": ");
                match map.get(*key) {
                    Some(v) => py_json_write(v, out),
                    None => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
}

fn py_json_write_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 || (ch as u32) > 0x7E => {
                // ensure_ascii: CPython's c_make_encoder rule
                // `c < 0x20 || c > 0x7e` — DEL (0x7F) and every non-ASCII
                // code point escape as `\uXXXX`, surrogate-paired above
                // 0xFFFF.
                let cp = ch as u32;
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xD800 + (v >> 10),
                        0xDC00 + (v & 0x3FF)
                    ));
                } else {
                    out.push_str(&format!("\\u{:04x}", cp));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Python-`float.__repr__`-compatible number formatting for the
/// canonicalizer. The T1 hash input is structurally strings-only
/// (`SummaryPart` / `ReasoningTextContent` carry no numbers), so this path
/// is defensive parity, not a golden-pinned surface.
fn py_number_repr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().expect("serde_json Number carries an int or an f64");
    if !f.is_finite() {
        return match f {
            f64::INFINITY => "Infinity".to_string(),
            f64::NEG_INFINITY => "-Infinity".to_string(),
            _ => "NaN".to_string(),
        };
    }
    if f == f.round() && f.abs() < 1e16 {
        // Python always keeps at least one fractional digit: 1.0 -> "1.0".
        return format!("{f:.1}");
    }
    let abs = f.abs();
    if abs >= 1e16 || (abs > 0.0 && abs < 1e-4) {
        // Python scientific: "1e+20", "1e-05" (signed, >= 2 exponent digits).
        let rendered = format!("{f:e}");
        let (mantissa, exp) = rendered
            .split_once('e')
            .unwrap_or((rendered.as_str(), "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp >= 0 { '+' } else { '-' };
        return format!("{mantissa}{sign}{:02}", exp.abs());
    }
    format!("{f}")
}

/// Self-contained SHA-256 (FIPS 180-4). See the module-level dependency
/// note for why the campaign `sha2` workspace dependency is not used here.
/// Byte-pinned by `digest_kat` below and, end-to-end, by the 12/12 xw_
/// goldens in the L0 suite.
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: append 0x80, zero-pad to 56 mod 64, then the 64-bit
    // big-endian bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (
            h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7],
        );
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}
