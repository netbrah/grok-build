//! Item 2's pure aggregation: per-session digests → [`Aggregate`]
//! (SDD §4 module layout, §13 item 2).
//!
//! This is the pure half of item 2. It reads no storage, makes no model call, and
//! defines none of the six facet enums — the `Friction`/`Outcome` enums the
//! `friction`/`outcomes` maps are keyed by stay in `types` (item 1's types
//! cycle); this file only carries the maps and the per-session facts that feed
//! them. What it computes:
//! - [`Totals`] — §11's footer counters, with the two identity equations holding
//!   by construction (`scanned = analyzed + excluded + self_excluded`);
//! - [`Medians`] — §9 rule 6's upper median `sorted[floor(n/2)]`, applied by name;
//! - `max_concurrent_sessions` + `interval_readable_sessions` — §9 rule 8's
//!   two-pointer sweep over each session's `[started_at, updated_at]` interval,
//!   clipped to [`CONCURRENCY_WINDOW_SECS`]; `NoTimeAxis` sessions sit outside the
//!   denominator (T-4 pins the boundary).
//!
//! The per-session facts only the labeller knows (the `partial` badge, the goal
//! categories, the per-facet friction counts and the outcome) arrive on
//! [`AnalyzedSession`], not on the digest: §11 warns that
//! `partial` is "any badge rendered, whatever the cause" — six causes, four with
//! no [`DegradedTier`] variant — so it cannot be derived from the digest alone.
//! Item 8 supplies them; item 4 (the reader) supplies the excluded / self-excluded
//! counts. Item 3 (the renderer) reads [`Aggregate`].

use std::collections::BTreeMap;

use serde::Serialize;

use super::digest::{DegradedTier, SessionDigest};
use super::types::{CappedBy, Friction, Outcome};

/// §9 rule 8's window: a session counts toward the concurrency peak only within
/// this many seconds of its start. A named const, not a knob — the same treatment
/// this spec gives its own `IDLE_GAP_SECS` (SDD §12 T-4).
pub const CONCURRENCY_WINDOW_SECS: i64 = 1800;

/// The pure aggregation output the renderer (item 3) reads: the facet-free
/// counters plus the two chart-ordered facet maps; the `Friction`/`Outcome`
/// enums they are keyed by stay in `types` (item 1's types cycle).
#[derive(Debug, Clone, Serialize)]
pub struct Aggregate {
    /// §11's footer counters; its field names ARE the footer terms.
    pub totals: Totals,
    /// §9 rule 6's medians by name.
    pub medians: Medians,
    /// §9 rule 8's bar; T-4 pins the sweep and this field.
    pub max_concurrent_sessions: u32,
    /// Rule 8's DENOMINATOR: sessions the sweep could read an interval for.
    /// NoTimeAxis sessions are outside it.
    pub interval_readable_sessions: u32,
    /// UTC-labelled axis (timezone is v2).
    pub hour_histogram: BTreeMap<u8, u32>,
    /// Chart-ordered, not insertion-ordered.
    pub goal_categories: BTreeMap<String, u32>,
    /// Chart-ordered, not insertion-ordered (item 2b).
    pub friction: BTreeMap<Friction, u32>,
    /// Chart-ordered, not insertion-ordered (item 2b).
    pub outcomes: BTreeMap<Outcome, u32>,
}

/// §11's footer counters.
#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    /// Outer term of §11's first equation: `analyzed + excluded + self_excluded`.
    pub sessions_scanned: u32,
    /// = clean + partial; the footer's headline N.
    pub sessions_analyzed: u32,
    /// Any badge rendered, whatever the cause (§11).
    pub sessions_partial: u32,
    /// No readable transcript: never in analyzed.
    pub sessions_excluded: u32,
    /// Item 4's self-exclusion: readable, unbadged, in no bucket.
    pub sessions_self_excluded: u32,
    /// Carries LegacyFormat: a subset of partial.
    pub sessions_legacy: u32,
    /// Reported cost only; 0 where the provider reports none.
    pub cost_ticks: u64,
    /// This total hides unpriced calls (§6; declared U-4).
    pub cost_is_partial: bool,
    /// Sorted, deduped; empty == no ceiling bound the run. The §11
    /// `insights_generated` event mirrors this one source into the footer
    /// (item 3's renderer, T-12(i)) and the telemetry (item 8's guard).
    pub capped_by: Vec<CappedBy>,
    /// Sum of the rendered sessions' wall spans, in seconds.
    pub duration_secs_total: u64,
    /// added + removed, on §5's arithmetic (T-5).
    pub lines_changed: u64,
}

/// §9 rule 6's medians.
#[derive(Debug, Clone, Serialize)]
pub struct Medians {
    /// §9 rule 6's rule by name: sorted[floor(n/2)] over the response gaps.
    pub response_time_secs: u64,
    /// The same rule over the wall-time values, not over `analyzed`.
    pub wall_time_mins: u64,
}

/// One rendered session as the pure [`aggregate`] sees it: its digest plus the
/// four per-session facts only the labeller (item 8) knows. Bundled so the caller
/// cannot supply a badge for one session against another's digest.
#[derive(Debug)]
pub struct AnalyzedSession<'a> {
    /// The session's digest (SDD §5). Every numeric field is computed over these.
    pub digest: &'a SessionDigest,
    /// `true` iff the session carries any §9 rule 3 badge cause. A
    /// `DegradedTier::LegacyFormat` session is badged, so a conforming caller
    /// sets this for it.
    pub partial: bool,
    /// Goal categories (free strings, §5 — not the facet enums).
    pub goal_categories: &'a [String],
    /// The session's per-facet friction counts (an empty map when the session
    /// has no labelled friction or is badged; contributes nothing).
    pub friction_counts: &'a BTreeMap<Friction, u32>,
    /// The session's labelled outcome; `None` when the session was not labelled
    /// (badged / unlabelled), which contributes nothing to `outcomes`. A single
    /// primary outcome per session: §5's `SessionFacets.outcome` is a plain
    /// `Outcome`, but a badged session has no `SessionFacets` at all, hence
    /// `Option` here.
    pub outcome: Option<Outcome>,
}

/// Everything the pure [`aggregate`] needs. Plain data — no I/O, no model call
/// (SDD §13 item 2).
#[derive(Debug)]
pub struct AggregateInput<'a> {
    /// The rendered sessions (clean or badged). Excluded and self-excluded
    /// sessions contribute nothing to any field.
    pub analyzed: &'a [AnalyzedSession<'a>],
    /// No-readable-transcript sessions (SDD §9 rule 3's narrow exclusion).
    pub sessions_excluded: u32,
    /// The invoking session + any transcript whose mtime moved mid-run.
    pub sessions_self_excluded: u32,
    /// The run-level ceiling record — which of the four `CappedBy` ceilings
    /// bound the run (item 8's guard collects it; a cost stop records exactly
    /// one — `Cost`; the shipped-default chunk-cap tie records both; empty ==
    /// no ceiling bound the run). `aggregate()` normalizes to sorted, deduped.
    pub capped_by: &'a [CappedBy],
}

/// The pure aggregation over passed-in inputs. No storage read, no model call
/// (SDD §13 item 2).
pub fn aggregate(input: &AggregateInput<'_>) -> Aggregate {
    let analyzed = input.analyzed;
    let sessions_analyzed = analyzed.len() as u32;
    let sessions_partial = analyzed.iter().filter(|s| s.partial).count() as u32;
    // Carries LegacyFormat: a subset of partial (§11).
    let sessions_legacy = analyzed
        .iter()
        .filter(|s| s.digest.degraded.contains(&DegradedTier::LegacyFormat))
        .count() as u32;
    let sessions_excluded = input.sessions_excluded;
    let sessions_self_excluded = input.sessions_self_excluded;
    // §11's first identity equation, by construction.
    let sessions_scanned = sessions_analyzed + sessions_excluded + sessions_self_excluded;

    // Reported cost only; a `None` (no provider price) contributes 0.
    let cost_ticks = analyzed
        .iter()
        .map(|s| i64::max(0, s.digest.cost_usd_ticks.unwrap_or(0)))
        .sum::<i64>() as u64;
    let cost_is_partial = analyzed.iter().any(|s| s.digest.cost_is_partial);

    // §5 :659's invariant, enforced here so the footer always renders sorted+deduped:
    // the caller passes the raw ceiling record, the pure aggregate normalizes it.
    let capped_by: Vec<CappedBy> = {
        let mut v = input.capped_by.to_vec();
        v.sort();
        v.dedup();
        v
    };

    // Wall spans summed in seconds (a negative `wall_minutes` clamps to 0).
    let duration_secs_total = analyzed
        .iter()
        .map(|s| i64::max(0, s.digest.wall_minutes) as u64 * 60)
        .sum();
    // added + removed, on §5's arithmetic (T-5).
    let lines_changed = analyzed
        .iter()
        .map(|s| s.digest.lines_added + s.digest.lines_removed)
        .sum();

    let mut response_gaps: Vec<u64> = analyzed
        .iter()
        .flat_map(|s| s.digest.response_gaps_secs.iter().copied().map(u64::from))
        .collect();
    let response_time_secs = upper_median(&mut response_gaps);

    let mut wall_mins: Vec<u64> = analyzed
        .iter()
        .map(|s| i64::max(0, s.digest.wall_minutes) as u64)
        .collect();
    let wall_time_mins = upper_median(&mut wall_mins);

    let mut hour_histogram: BTreeMap<u8, u32> = BTreeMap::new();
    for s in analyzed {
        for &h in &s.digest.message_hours {
            *hour_histogram.entry(h as u8).or_insert(0) += 1;
        }
    }

    let mut goal_categories: BTreeMap<String, u32> = BTreeMap::new();
    for s in analyzed {
        for cat in s.goal_categories {
            *goal_categories.entry(cat.clone()).or_insert(0) += 1;
        }
    }

    let mut friction: BTreeMap<Friction, u32> = BTreeMap::new();
    for s in analyzed {
        for (f, c) in s.friction_counts {
            *friction.entry(*f).or_insert(0) += c;
        }
    }

    let mut outcomes: BTreeMap<Outcome, u32> = BTreeMap::new();
    for s in analyzed {
        if let Some(o) = s.outcome {
            *outcomes.entry(o).or_insert(0) += 1;
        }
    }

    let (max_concurrent_sessions, interval_readable_sessions) = concurrency_sweep(analyzed);

    Aggregate {
        totals: Totals {
            sessions_scanned,
            sessions_analyzed,
            sessions_partial,
            sessions_excluded,
            sessions_self_excluded,
            sessions_legacy,
            cost_ticks,
            cost_is_partial,
            capped_by,
            duration_secs_total,
            lines_changed,
        },
        medians: Medians {
            response_time_secs,
            wall_time_mins,
        },
        max_concurrent_sessions,
        interval_readable_sessions,
        hour_histogram,
        goal_categories,
        friction,
        outcomes,
    }
}

/// §9 rule 6's upper median: `sorted[floor(n/2)]`, not the midpoint average.
/// Empty input → 0 (the chart renders 0 explicitly, §9 rule 8).
fn upper_median(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

/// §9 rule 8's two-pointer sweep. Each readable session's interval is
/// `[started_at, min(updated_at, started_at + CONCURRENCY_WINDOW_SECS)]`
/// (half-open, clipped to the window); `NoTimeAxis` sessions sit outside both the
/// sweep and the denominator. Returns `(peak concurrency, denominator)`.
fn concurrency_sweep(analyzed: &[AnalyzedSession<'_>]) -> (u32, u32) {
    let mut starts: Vec<i64> = Vec::new();
    let mut ends: Vec<i64> = Vec::new();
    for s in analyzed {
        if s.digest.degraded.contains(&DegradedTier::NoTimeAxis) {
            continue;
        }
        let start = s.digest.started_at.timestamp();
        let end = s.digest.updated_at.timestamp().max(start);
        let clipped_end = end.min(start + CONCURRENCY_WINDOW_SECS);
        starts.push(start);
        ends.push(clipped_end);
    }
    let interval_readable_sessions = starts.len() as u32;
    if starts.is_empty() {
        return (0, interval_readable_sessions);
    }
    starts.sort_unstable();
    ends.sort_unstable();
    let mut max_active = 0u32;
    let mut j = 0usize;
    for (i, &start) in starts.iter().enumerate() {
        while j < ends.len() && ends[j] <= start {
            j += 1;
        }
        let active = (i + 1 - j) as u32;
        if active > max_active {
            max_active = active;
        }
    }
    (max_active, interval_readable_sessions)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Duration, Utc};

    use super::{AggregateInput, AnalyzedSession, CONCURRENCY_WINDOW_SECS, aggregate};
    use crate::session::insights::digest::{CACHE_SCHEMA_VERSION, DegradedTier, SessionDigest};
    use crate::session::insights::types::{CappedBy, Friction, Outcome};

    fn check<T: std::fmt::Debug + PartialEq>(name: &str, got: &T, want: &T) {
        assert_eq!(got, want, "field `{name}`");
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("fixture instant")
            .with_timezone(&Utc)
    }

    /// A [`SessionDigest`] with the fields under test set and the rest defaulted.
    fn digest(
        id: &str,
        started_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        wall_minutes: i64,
        lines_added: u64,
        lines_removed: u64,
        cost_usd_ticks: Option<i64>,
        cost_is_partial: bool,
        response_gaps: Vec<u32>,
        message_hours: Vec<u32>,
        degraded: Vec<DegradedTier>,
    ) -> SessionDigest {
        SessionDigest {
            cache_version: CACHE_SCHEMA_VERSION,
            session_id: id.to_string(),
            cwd: String::new(),
            git_root: None,
            head_branch: None,
            started_at,
            updated_at,
            wall_minutes,
            activity_minutes: 0,
            user_turns: 1,
            assistant_turns: 1,
            tool_counts: BTreeMap::new(),
            languages: BTreeMap::new(),
            files_modified: 0,
            lines_added,
            lines_removed,
            projection_chars: 0,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_read_tokens: 0,
            cost_usd_ticks,
            cost_is_partial,
            tool_errors: 0,
            interruptions: 0,
            response_gaps_secs: response_gaps,
            message_hours,
            user_message_timestamps: Vec::new(),
            time_degraded: degraded.contains(&DegradedTier::NoTimeAxis),
            model_ids: Vec::new(),
            uses_mcp: false,
            uses_subagent: false,
            first_prompt: None,
            degraded,
            unreadable_lines: 0,
            transcript_sha256: String::new(),
        }
    }

    /// The const is the one definition the bar's label rests on (SDD §12 T-4).
    const _: () = {
        assert!(CONCURRENCY_WINDOW_SECS == 1800);
    };

    // --- T-4: the parallel-session metric (SDD §12 T-4, §9 rule 8) ---
    //
    // Three sessions (A, B, C) overlap inside the window; one pair (D, E) is 1 s
    // outside it. D runs 3000 s (longer than the window); E starts 1 s after D's
    // window edge (D.start + CONCURRENCY_WINDOW_SECS + 1), so D's clipped interval
    // never reaches E. Without the clip, D would overlap both E and the A/B/C
    // group — inventing parallel work and raising the peak to 4. With the clip,
    // the peak is exactly the three overlapping sessions. F is NoTimeAxis and sits
    // outside the denominator.
    #[test]
    fn t4_peak_is_the_three_overlapping_and_the_1s_pair_does_not_count() {
        let base = at("2026-01-01T00:00:00Z");
        let t = |s: i64| base + Duration::seconds(s);

        // A, B, C: overlap on [2100, 2600] -> peak 3, all inside the window.
        let a = digest(
            "a",
            t(2000),
            t(2600),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let b = digest(
            "b",
            t(2050),
            t(2650),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let c = digest(
            "c",
            t(2100),
            t(2700),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        // D: runs 3000 s (> the window). Its clipped interval ends at D.start + 1800.
        let d = digest(
            "d",
            t(0),
            t(3000),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        // E: starts 1 s outside D's window edge (t(1801) == t(0) + 1800 + 1).
        let e = digest(
            "e",
            t(1801),
            t(1900),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        // F: NoTimeAxis -> outside the sweep and outside the denominator.
        let f = digest(
            "f",
            t(5000),
            t(6000),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            vec![DegradedTier::NoTimeAxis],
        );

        let sa = AnalyzedSession {
            digest: &a,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let sb = AnalyzedSession {
            digest: &b,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let sc = AnalyzedSession {
            digest: &c,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let sd = AnalyzedSession {
            digest: &d,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let se = AnalyzedSession {
            digest: &e,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let sf = AnalyzedSession {
            digest: &f,
            partial: true,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };

        let input = AggregateInput {
            analyzed: &[sa, sb, sc, sd, se, sf],
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &[],
        };
        let agg = aggregate(&input);

        // The three overlapping sessions are the peak; D's clipped interval never
        // reaches E (1 s outside) or the A/B/C group, and F is outside the sweep.
        check(
            "max_concurrent_sessions",
            &agg.max_concurrent_sessions,
            &3u32,
        );
        // Denominator = the sessions the sweep read an interval for: A, B, C, D, E.
        // F (NoTimeAxis) is outside it (SDD §9 rule 8).
        check(
            "interval_readable_sessions",
            &agg.interval_readable_sessions,
            &5u32,
        );
    }

    // --- Totals + Medians (SDD §11's identity equations, §9 rule 6's median) ---
    //
    // Item 2 APPLIES rule 6 (upper median sorted[floor(n/2)]) by computing the
    // expected value with that rule and asserting the aggregate matches. It does
    // NOT re-assert T-3's predicate (that the rule IS the upper median, not the
    // midpoint): the even-sized value sets below mean a midpoint average would
    // disagree (90 vs 75, 50 vs 40), so the application is load-bearing.
    #[test]
    fn totals_identity_and_medians_apply_the_upper_median_rule() {
        let base = at("2026-01-01T00:00:00Z");
        let t = |s: i64| base + Duration::seconds(s);

        // Four rendered sessions, non-overlapping (each in its own 600 s slot) so
        // the concurrency is 1 and the test's focus stays on totals/medians.
        let s1 = digest(
            "s1",
            t(0),
            t(600),
            10,
            5,
            3,
            Some(100),
            false,
            vec![30],
            vec![9],
            Vec::new(),
        );
        let s2 = digest(
            "s2",
            t(1000),
            t(1600),
            30,
            10,
            0,
            Some(200),
            false,
            vec![120],
            vec![10],
            vec![DegradedTier::UnreadableLines],
        );
        let s3 = digest(
            "s3",
            t(2000),
            t(2600),
            50,
            1,
            1,
            None,
            true,
            vec![60],
            vec![11],
            vec![DegradedTier::LegacyFormat],
        );
        let s4 = digest(
            "s4",
            t(3000),
            t(3600),
            70,
            0,
            4,
            Some(300),
            false,
            vec![90],
            vec![12],
            Vec::new(),
        );

        let goals_2 = vec!["debugging".to_string()];
        let goals_3 = vec!["debugging".to_string(), "refactoring".to_string()];
        let goals_4 = vec!["refactoring".to_string()];

        let a1 = AnalyzedSession {
            digest: &s1,
            partial: false,
            goal_categories: &[],
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let a2 = AnalyzedSession {
            digest: &s2,
            partial: true,
            goal_categories: &goals_2,
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let a3 = AnalyzedSession {
            digest: &s3,
            partial: true,
            goal_categories: &goals_3,
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };
        let a4 = AnalyzedSession {
            digest: &s4,
            partial: false,
            goal_categories: &goals_4,
            friction_counts: &BTreeMap::new(),
            outcome: None,
        };

        let input = AggregateInput {
            analyzed: &[a1, a2, a3, a4],
            sessions_excluded: 2,
            sessions_self_excluded: 1,
            capped_by: &[],
        };
        let agg = aggregate(&input);

        // §11's identity equations, on the stored counters:
        //   scanned = analyzed + excluded + self_excluded  ->  7 = 4 + 2 + 1
        //   analyzed = clean + partial  ->  clean = 4 - 2 = 2 (s1, s4)
        check("sessions_scanned", &agg.totals.sessions_scanned, &7u32);
        check("sessions_analyzed", &agg.totals.sessions_analyzed, &4u32);
        check("sessions_partial", &agg.totals.sessions_partial, &2u32);
        check("sessions_excluded", &agg.totals.sessions_excluded, &2u32);
        check(
            "sessions_self_excluded",
            &agg.totals.sessions_self_excluded,
            &1u32,
        );
        // LegacyFormat is a subset of partial: s3 is both.
        check("sessions_legacy", &agg.totals.sessions_legacy, &1u32);
        // Reported cost only; None (s3) contributes 0.
        check("cost_ticks", &agg.totals.cost_ticks, &600u64);
        check("cost_is_partial", &agg.totals.cost_is_partial, &true);
        // Wall spans summed in seconds: (10 + 30 + 50 + 70) * 60.
        check(
            "duration_secs_total",
            &agg.totals.duration_secs_total,
            &9600u64,
        );
        // added + removed: (5+3) + (10+0) + (1+1) + (0+4).
        check("lines_changed", &agg.totals.lines_changed, &24u64);

        // §9 rule 6 applied: upper median sorted[floor(n/2)].
        //   response gaps [30, 120, 60, 90] -> sorted [30, 60, 90, 120][2] = 90 (midpoint 75).
        //   wall minutes  [10, 30, 50, 70]   -> sorted [10, 30, 50, 70][2]   = 50 (midpoint 40).
        check(
            "response_time_secs",
            &agg.medians.response_time_secs,
            &90u64,
        );
        check("wall_time_mins", &agg.medians.wall_time_mins, &50u64);

        // UTC hour axis: one message per session, at hours 9, 10, 11, 12.
        let mut hours = BTreeMap::new();
        for h in [9u8, 10, 11, 12] {
            hours.insert(h, 1);
        }
        check("hour_histogram", &agg.hour_histogram, &hours);

        // Goal categories: debugging x2 (s2, s3), refactoring x2 (s3, s4).
        let mut goals = BTreeMap::new();
        goals.insert("debugging".to_string(), 2u32);
        goals.insert("refactoring".to_string(), 2u32);
        check("goal_categories", &agg.goal_categories, &goals);

        // Non-overlapping sessions: peak 1, all four readable.
        check(
            "max_concurrent_sessions",
            &agg.max_concurrent_sessions,
            &1u32,
        );
        check(
            "interval_readable_sessions",
            &agg.interval_readable_sessions,
            &4u32,
        );
    }

    // --- friction + outcomes (item 2b: the two chart-ordered facet maps) ---
    //
    // (a) Labelled A `{BuggyCode:1, ToolFailed:1}` and B `{BuggyCode:2}` overlap
    //     on `BuggyCode` -> `{BuggyCode:3, ToolFailed:1}`.
    // (b) A and B are `PartiallyAchieved`, C is `FullyAchieved`
    //     -> `{PartiallyAchieved:2, FullyAchieved:1}`.
    // (c) D is badged/unlabelled (empty `friction_counts`, `outcome: None`) and
    //     contributes nothing: the badged run's maps equal the unbadged run's —
    //     a `None` outcome counted as a phantom outcome, or an empty map
    //     contributing a key, would break the equality.
    #[test]
    fn friction_and_outcomes_aggregate_exactly_and_badged_sessions_contribute_nothing() {
        let base = at("2026-01-01T00:00:00Z");
        let t = |s: i64| base + Duration::seconds(s);

        let da = digest(
            "fa",
            t(0),
            t(100),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let db = digest(
            "fb",
            t(200),
            t(300),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let dc = digest(
            "fc",
            t(400),
            t(500),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let dd = digest(
            "fd",
            t(600),
            t(700),
            0,
            0,
            0,
            None,
            false,
            Vec::new(),
            Vec::new(),
            vec![DegradedTier::UnreadableLines],
        );

        let friction_a =
            BTreeMap::from([(Friction::BuggyCode, 1u32), (Friction::ToolFailed, 1u32)]);
        let friction_b = BTreeMap::from([(Friction::BuggyCode, 2u32)]);
        let friction_c: BTreeMap<Friction, u32> = BTreeMap::new();
        let friction_d: BTreeMap<Friction, u32> = BTreeMap::new();

        let sa = AnalyzedSession {
            digest: &da,
            partial: false,
            goal_categories: &[],
            friction_counts: &friction_a,
            outcome: Some(Outcome::PartiallyAchieved),
        };
        let sb = AnalyzedSession {
            digest: &db,
            partial: false,
            goal_categories: &[],
            friction_counts: &friction_b,
            outcome: Some(Outcome::PartiallyAchieved),
        };
        let sc = AnalyzedSession {
            digest: &dc,
            partial: false,
            goal_categories: &[],
            friction_counts: &friction_c,
            outcome: Some(Outcome::FullyAchieved),
        };
        let sd = AnalyzedSession {
            digest: &dd,
            partial: true,
            goal_categories: &[],
            friction_counts: &friction_d,
            outcome: None,
        };

        let mut want_friction: BTreeMap<Friction, u32> = BTreeMap::new();
        want_friction.insert(Friction::BuggyCode, 3);
        want_friction.insert(Friction::ToolFailed, 1);
        let mut want_outcomes: BTreeMap<Outcome, u32> = BTreeMap::new();
        want_outcomes.insert(Outcome::PartiallyAchieved, 2);
        want_outcomes.insert(Outcome::FullyAchieved, 1);

        // One array, sliced twice: `AnalyzedSession` is not `Copy`, so the
        // locals move exactly once.
        let sessions: [AnalyzedSession; 4] = [sa, sb, sc, sd];

        // (a) + (b): the three labelled sessions aggregate exactly.
        let agg3 = aggregate(&AggregateInput {
            analyzed: &sessions[..3],
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &[],
        });
        check("friction", &agg3.friction, &want_friction);
        check("outcomes", &agg3.outcomes, &want_outcomes);

        // (c): adding the badged/unlabelled session changes neither map.
        let agg4 = aggregate(&AggregateInput {
            analyzed: &sessions,
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &[],
        });
        check(
            "friction with badged session",
            &agg4.friction,
            &want_friction,
        );
        check(
            "outcomes with badged session",
            &agg4.outcomes,
            &want_outcomes,
        );
    }

    // --- capped_by (item 2c: §5's :659; T-12(i) reads `Totals::capped_by`) ---
    //
    // The run-level ceiling record the caller (item 8's guard) collects;
    // `aggregate()` normalizes it to the SOT invariant — sorted, deduped;
    // empty == no ceiling bound the run. The field is run-level, independent
    // of the per-session `analyzed` data, so `analyzed: &[]` isolates the
    // normalization from everything else the aggregate computes.
    #[test]
    fn capped_by_is_normalized_to_sorted_and_deduped() {
        // (a) No ceiling bound the run: empty in, empty out.
        let agg_a = aggregate(&AggregateInput {
            analyzed: &[],
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &[],
        });
        check(
            "capped_by (none)",
            &agg_a.totals.capped_by,
            &Vec::<CappedBy>::new(),
        );

        // (b) A cost stop records exactly one (the §9 :918 cost-stop case).
        let cost = [CappedBy::Cost];
        let agg_b = aggregate(&AggregateInput {
            analyzed: &[],
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &cost,
        });
        check(
            "capped_by (cost)",
            &agg_b.totals.capped_by,
            &vec![CappedBy::Cost],
        );

        // (c) The shipped-default chunk-cap tie with a duplicate: the two chunk
        //     caps sort by the enum's Ord (ChunkPerSession < ChunkPerRun) and the
        //     duplicate is dropped — T-12(i) "names both chunk caps".
        let tie = [
            CappedBy::ChunkPerRun,
            CappedBy::ChunkPerSession,
            CappedBy::ChunkPerRun,
        ];
        let agg_c = aggregate(&AggregateInput {
            analyzed: &[],
            sessions_excluded: 0,
            sessions_self_excluded: 0,
            capped_by: &tie,
        });
        check(
            "capped_by (tie + duplicate)",
            &agg_c.totals.capped_by,
            &vec![CappedBy::ChunkPerSession, CappedBy::ChunkPerRun],
        );
    }
}
