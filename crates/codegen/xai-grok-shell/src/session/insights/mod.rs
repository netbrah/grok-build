//! Session insights: per-session digest → aggregate → labelled report (SDD §4).
//!
//! Module map:
//! - `digest.rs` — transcript → [`SessionDigest`] (pure) — item 1
//! - `aggregate.rs` — digests → `Aggregate` (pure) — item 2
//! - `report.rs` — HTML render (pure) — item 3
//! - `scope.rs` — session selection, limits and the storage reads — item 4
//! - `cache.rs` — read-through cache — item 4
//! - `label.rs` — facet calls + validation — item 7
//! - `narrate.rs` — section fan-out + at-a-glance — item 8
//!
//! Item 1 ships only the pure half: the §5 types, the §5 consts, `digest_one`
//! and `is_user_turn`. No orchestration here — `handle_insights` lands with the
//! command entry (item 5), and `project()` / the tag neutraliser land in item 4
//! (SDD §13). No file I/O in this tree yet.

pub mod digest;

pub use digest::{
    CACHE_SCHEMA_VERSION, DegradedTier, DigestInput, IDLE_GAP_SECS, ProjectionCaps, ReaderFacts,
    SessionDigest, SessionFacts, TranscriptContent, digest_one, is_user_turn,
};
