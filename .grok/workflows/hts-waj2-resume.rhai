// hts-waj2-resume — continuation driver for the apex-waj.2 lane (projector discovery arm).
// Standalone on purpose: it inherits nothing from the scaffold generator, so none of its damage
// rides along. The lane's Baseline / Red / Green / Mutant / Gate phases are DONE and evidenced; it
// blocked at GateReproduce on "gates did not reproduce clean", and the non-clean is FOREIGN.
// This driver therefore starts at the gate, re-measures the foreign baseline, and goes on to the
// three-lens review. It never commits, tags or pushes.

let meta = #{
    name: "hts-waj2-resume",
    description: "Resume apex-waj.2 at the gate instead of re-running it: re-measure the foreign-failure baseline in the shared dirty worktree under the verification-record contract (commands + real exits + tree fingerprint + attributed foreign failures + verdict = new failures minus baseline), reproduce that verdict with an independent seat, then run the three-lens review to zero Blocking / zero Major. Reports NO VERDICT rather than a fake clean.",
    when_to_use: "The hts-waj2 run (wf_01a0ef6a856571a0be91a75699693aeb) is blocked at GateReproduce with 'Gates did not reproduce clean'. Its Green/Mutant/Gate evidence stands (crate green 840/0, REAL_EXIT=0). Do NOT resume that run and do NOT relaunch the full TDD driver — the residue is already in the working tree and a re-run would re-edit a shared dirty file.",
    phases: [
        #{ title: "Baseline", detail: "re-measure the tree (tip, dirty set, fingerprints of the two owned files, free disk, cargo lock); confirm Baseline/Red/Green/Mutant/Gate are landed and refuse to redo them; halt if the tree is not the one the evidence describes" },
        #{ title: "Gate", detail: "verification record: exact command lines + real exit statuses read from .rc files + tree fingerprint + FOREIGN-FAILURE BASELINE with per-name attribution + verdict = new failures minus baseline" },
        #{ title: "GateReproduce", detail: "one independent seat re-runs the decisive commands and is graded against the attributed baseline, never against 'clean' — the check the blocked run lacked" },
        #{ title: "Review", detail: "exactly three fresh read-only lenses (design-waj2 §7) plus one fixer, max 3 rounds; a round with fewer than 3 lenses reporting is not clean" },
        #{ title: "Report", detail: "verdict, open findings, foreign baseline, NOTHING COMMITTED" },
    ],
};

// ── paths and pins (all re-measured by the Baseline seat; the pins are what the prior evidence
//    was taken against, so a mismatch is a halt, not a licence to re-implement) ────────────────
let WT = "/Users/palanisd/Projects/upstream/wt/apex-ayl-hosted-tool-search";
let CR = WT + "/crates/codegen/xai-grok-sampling-types/src/conversation";
let PROJ = CR + "/projection.rs";
let TS = CR + "/projection_tests.rs";
let TIP = "ff2351c5";
let WF = "/tmp/hts-wf";
let GATE_REC = WF + "/VERIFICATION-RECORD-waj2-resume.md";
let DESIGN = "/Users/palanisd/Projects/upstream/grok/plans/harness/workflow-design/designs/design-waj2.md";
let REPORTS = WF + "/REPORT-waj2-green.md, " + WF + "/REPORT-waj2-mutants.md, " + WF + "/REPORT-waj2-gates.md";
let RAW = WF + "/w2-g*.log and " + WF + "/w2-g*.rc";
let MIN_DISK_MB = 16000;

// ── what is already done. Every phase prompt carries this; no seat may redo it. ───────────────
let DONE = "LANE STATE — apex-waj.2 (projector discovery arm), coordinator-measured 2026-09-30:\n"
    + "- Worktree " + WT + " at tip `" + TIP + "`, 65 dirty entries, several lanes sharing it. Free space was 51 GiB at measurement; the prior gate phase ran when it was 16 GiB.\n- OWNED, and the only files this lane may ever edit: `" + PROJ + "` and `" + TS + "` (design-waj2 §1 allowlist). Everything else in the dirty set is another lane's.\n- Baseline, Red, Green, Mutant and Gate are DONE and evidenced in " + REPORTS + " and in " + RAW + ". The residue is IN THE WORKING TREE, uncommitted: `project_discovery_rekey`, `strip_discovery_pair`, `tool_search_output_id`, `DropReason::DiscoveryDemoted`, and the six §6 test names.\n- Crate of record: `cargo test --release -p xai-grok-sampling-types --no-fail-fast` → REAL_EXIT=0, `test result: ok. 840 passed; 0 failed` (w2-g2b), with the same 840/0 inside the 6-package GATE itself (w2-g5c). All six §6 names pass; the five known-answer `tso_*` constants were re-derived from an independent python3 oracle, 5/5.\n- E0004 count is 0 tree-wide (`cargo check --workspace --all-targets --keep-going`, w2-g4c), so cmd-4's REAL_EXIT=101 is foreign compile errors in other crates, not this lane's. cmd-6 prints zero `_ =>` lines in the owned diff.\n- Every mutant MUT-D1b..MUT-D3a produced its named death and was reverted (§5 of the gates report).\n\nYOU ARE NOT ASKED TO: re-run Red, re-implement anything, re-run the mutation battery, or repair a foreign failure. If a phase below asks you to run a command, run it and report it — do not 'improve' the code while you are there.\n";

// ── the prior foreign-failure record. Re-measure it first-hand; correct and extend it; never
//    copy it forward as if it were proven. It was attributed, not baseline-proven. ────────────
let FOREIGN = "PRIOR FOREIGN-FAILURE RECORD (from " + WF + "/REPORT-waj2-gates.md §3, at " + TIP + ").\n"
    + "This is a HYPOTHESIS to re-measure, not a baseline to paste. 11 names fell in the 6-package GATE, none in xai-grok-sampling-types:\n- stream::responses::tests::response_completed_cache_write_tokens_lands_in_token_usage — standing adjudicated GATE exception.\n- agent::subagent::tests::wake::rejected_deferred_start_restores_prior_without_publication — known phantom red under parallel load (design §8).\n- agent::catalog_bake_tests::{bundled_catalog_parses_rich_rows, fallback_path_seams_run_on_all_bundled_rows, role_pins_come_from_the_baked_file} — sibling catalog lane: bundled catalog has 18 rows, the bake pins 12; catalog_bake_tests.rs is clean at HEAD; a projector cut cannot move a row count.\n- agent::mvp_agent::tests::spawn_seeds_root_conversation_group_in_turn_config — environment-dependent (ambient endpoint config with no API key for gpt-5.6-sol).\n- agent::subagent::tests::wake::started_wake_with_failed_metadata_write_preserves_prior_durable_artifacts, and session::acp_session::auto_wake_suppression_tests::cancel_barrier_rejects_task_completion_wake_without_reporting_it — the subagent/wake-admission lane (xai-grok-subagent-resolution + shell subagent files dirty) under parallel load.\n- session::unified_list::tests::parse_list_req_forces_kind_under_process_chat_mode_only — empty-array-vs-null deserialisation shift (xai-acp-lib / xai-grok-mcp dirty).\n- session::workflow::registry::tests::save_through_symlinked_session_root_stays_in_canonical_project — macOS TMPDIR symlink canonicalisation, environment red. a_provider_that_declines_the_headless_run_can_still_sign_the_user_in — non-hermetic: live HTTPS to the real IdP, HTTP 403.\n- conversation::tool_search::tests::a_reused_call_id_spanning_whole_turns_does_not_drag_the_cut_back — INTERMITTENT and, decisively, it lives in `conversation/tool_search.rs`, which is NOT on this lane's allowlist. Its observed history: absent at g1/g1b, PASSED at g2b, FAILED at g3, PASSED at g5b. Characterise it by repetition, never by one run.\n";

// ── rules every seat gets, verbatim ───────────────────────────────────────────────────────────
let RULES = "RULES (non-negotiable):\n"
    + "- The worktree is SHARED and DIRTY. Edit only the two allowlisted files, and only if your phase says edits are in scope. Never revert, reformat, reorder or 'helpfully fix' a line you did not author. `tool_search.rs`, `conversation.rs`, `conversation/responses.rs`, `xai-chat-state/src/actor/mutations.rs`, the shell's `storage/mod.rs` and the catalog bake files all belong to other lanes.\n- NO git write commands anywhere: no add, commit, tag, push, stash, checkout, restore, clean or reset. The coordinator exfs later with an explicit pathspec. `git status`, `git diff`, `git log`, `git show` are fine.\n- Before ANY cargo/rustc invocation: `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave-mcp'`. If a sibling build holds the target lock, WAIT (wrap the command in `lockf -k -s -t 5400 /tmp/hts-cargo.lock`). Never kill a sibling build; the lock makes a build slow and that is expected.\n- Before anything that builds, run `df -m " + WT + " | tail -1` and report the free MB. Below " + MIN_DISK_MB.to_string() + " MB: stop and say so — do not start a build that fills the disk.\n- Exit statuses are read FROM A FILE, never from a pipeline. Template:\n      cd " + WT + "\n      <command> > " + WF + "/r-<name>.log 2>&1\n      rc=$?; echo \"REAL_EXIT=$rc\" > " + WF + "/r-<name>.rc\n  Never pipe cargo into tail/head (you read the pipe's status, not cargo's). `grep -c` prints 0 and exits 1 — read the NUMBER it printed, not its status. A single `cargo check --workspace` run's error count is a FLOOR; widen to `--keep-going` if you need a total.\n- Warm target/: a cold dir fabricates sampler `request_builder bypass_*` reds. Re-run before calling one a regression.\n- Before quoting any log excerpt in your output: `grep -rEil 'sk-[A0-9a-z]{16,}' <the files you produced>` and report the hit count, which must be 0. Never quote key material.\n- Keep your own output small: point at the log path and quote the decisive lines, not the log.\n- An honest partial beats a confident full run. If you ran out of budget, say what you did not measure.\n";

// ── the verification-record contract. A record without (c) is NO VERDICT. ─────────────────────
let GATE_SPEC = "VERIFICATION RECORD CONTRACT. A whole-tree gate in a shared dirty worktree is NOT a per-lane verdict, so write " + GATE_REC + " containing exactly:\n"
    + "  (a) every command line you invoked, in order, each with its REAL exit status read back from its own .rc file (`cat` the .rc in your output — do not restate it from memory);\n  (b) the FINGERPRINT of the tree it ran against: `git rev-parse --short HEAD`, `shasum -a 256` of the two allowlisted files, the entry count of `git status --porcelain`, and `df -m` free MB — all captured immediately before the first build and again after the last;\n  (c) the FOREIGN-FAILURE BASELINE: every failing test name you observed anywhere in the tree, each with its attribution (which sibling lane, which environment dependency, which standing exception, or which repeated-run proof that it is intermittent). A name you cannot attribute is NEW, not foreign;\n  (d) the VERDICT, computed as (failing names) minus (attributed foreign baseline), which must be reported as one of:\n      lane-clean    — the new-failure set is empty and this lane's own crate is green at REAL_EXIT=0;\n      no-verdict    — you could not produce (c) for every foreign name, so the result is unfalsifiable. Report it as NO VERDICT: NOT clean, and NOT a failure of this lane;\n      lane-red      — a failing name is attributable to the two allowlisted files.\nDo not absorb a foreign failure into a green verdict, and do not launder an unattributable one into a red one.\n";

fn as_lines(v) {
    // A dry-run host hands back a scalar where the schema says array; keep it as one line.
    if v == () {
        return [];
    }
    if type_of(v) == "array" {
        return v;
    }
    return [v.to_string()];
}

fn join_lines(items, sep) {
    let out = "";
    let n = 0;
    for it in as_lines(items) {
        if n > 0 {
            out = out + sep;
        }
        out = out + it.to_string();
        n = n + 1;
    }
    return out;
}

fn is_empty(v) {
    return as_lines(v).len() == 0;
}

fn tagged(items, tag) {
    let out = [];
    for s in as_lines(items) {
        if s.to_string().contains(tag) {
            out.push(s.to_string());
        }
    }
    return out;
}

fn tail_if(cond, s) {
    if cond {
        return s;
    }
    return "";
}

// ── schemas: flat only (strings, integers, arrays of strings) ─────────────────────────────────
fn state_schema() {
    #{
        "type": "object",
        "properties": #{
            "status": #{ "type": "string", "enum": ["ok", "blocked"] }, "verdict": #{ "type": "string", "enum": ["continue", "halt"] }, "tree_fingerprint": #{ "type": "string", "description": "HEAD short sha | sha256:12 of projection.rs | sha256:12 of projection_tests.rs | dirty-entry count | df free MB" }, "cargo_lock": #{ "type": "string", "description": "pgrep summarised: idle, or which sibling holds the target lock" }, "disk_free_mb": #{ "type": "integer" }, "done_confirmed": #{ "type": "array", "items": #{ "type": "string" }, "description": "one per lane-state claim checked first-hand, with the evidence line" }, "discrepancies": #{ "type": "array", "items": #{ "type": "string" }, "description": "where the tree contradicts the reports; empty if it does not" }, "halt_reason": #{ "type": "string", "description": "empty unless verdict is halt" },
            "notes": #{ "type": "string" }
        },
        "required": ["status", "verdict", "tree_fingerprint", "cargo_lock", "disk_free_mb", "done_confirmed", "discrepancies", "halt_reason", "notes"]
    }
}

fn gate_schema() {
    #{
        "type": "object",
        "properties": #{
            "status": #{ "type": "string", "enum": ["ran", "blocked"] }, "verdict": #{ "type": "string", "enum": ["lane-clean", "no-verdict", "lane-red"] }, "commands_and_exits": #{ "type": "array", "items": #{ "type": "string" }, "description": "record (a): one per command — the full command line, then REAL_EXIT as read from its .rc" }, "tree_fingerprint": #{ "type": "string", "description": "record (b), measured before the first build" }, "tree_fingerprint_after": #{ "type": "string", "description": "record (b) again after the last build; equal means nothing moved under the run" }, "foreign_baseline": #{ "type": "array", "items": #{ "type": "string" }, "description": "record (c): one per foreign failing name — name | attribution | the evidence for the attribution" }, "new_failures": #{ "type": "array", "items": #{ "type": "string" }, "description": "names not attributable to another lane or a declared environment dependency; empty is the only lane-clean shape" }, "intermittent_evidence": #{ "type": "array", "items": #{ "type": "string" }, "description": "the repeated runs of the tool_search cut-back test, each with its REAL_EXIT and pass/fail" }, "lane_crate_summary": #{ "type": "string", "description": "the verbatim `test result:` line for xai-grok-sampling-types" }, "raw_key_hits": #{ "type": "integer" }, "record_path": #{ "type": "string" },
            "notes": #{ "type": "string" }
        },
        "required": ["status", "verdict", "commands_and_exits", "tree_fingerprint", "tree_fingerprint_after", "foreign_baseline", "new_failures", "intermittent_evidence", "lane_crate_summary", "raw_key_hits", "record_path", "notes"]
    }
}

fn findings_schema() {
    #{
        "type": "object",
        "properties": #{
            "findings": #{ "type": "array", "maxItems": 12, "items": #{ "type": "string" }, "description": "flat, one per finding, EXACTLY this shape: [Blocking] claim … || evidence: file:line + the artifact line that shows it || fix: …   (severity tag first, one of Blocking / Major / Minor / Nit)" }, "lens": #{ "type": "string", "description": "which lens you ran" }, "verdict": #{ "type": "string", "enum": ["ship", "fix-first"] },
            "notes": #{ "type": "string", "description": "what you checked that is sound, and what you could not check without a shell" }
        },
        "required": ["findings", "lens", "verdict", "notes"]
    }
}

fn fix_schema() {
    #{
        "type": "object",
        "properties": #{
            "status": #{ "type": "string", "enum": ["green", "red", "blocked"] }, "items_landed": #{ "type": "array", "items": #{ "type": "string" } }, "items_rejected": #{ "type": "array", "items": #{ "type": "string" }, "description": "finding | why it is factually wrong, with file:line" }, "items_not_landed": #{ "type": "array", "items": #{ "type": "string" } }, "tree_fingerprint": #{ "type": "string", "description": "re-measured after your edits — the next round reads this, not the old one" }, "recheck": #{ "type": "array", "items": #{ "type": "string" }, "description": "cmd-1 / cmd-2 / cmd-6 re-run after the fix, each with its REAL_EXIT read from its .rc" }, "handoffs": #{ "type": "array", "items": #{ "type": "string" }, "description": "anything owed outside the two allowlisted files" },
            "notes": #{ "type": "string" }
        },
        "required": ["status", "items_landed", "items_rejected", "items_not_landed", "tree_fingerprint", "recheck", "handoffs", "notes"]
    }
}

// ── Phase 1: re-establish the ground, and refuse to redo the four finished phases ─────────────
phase("Baseline");
let base = agent(
    "You re-establish ground for a RESUMED lane. Nothing is implemented here and nothing is re-run: the lane's Baseline, Red, Green and Mutant phases are closed.\n\n" + DONE + "\n"
    + "Do, with shell commands only (no edits, no cargo build):\n1. `git -C " + WT + " rev-parse --short HEAD`; `shasum -a 256` of the two allowlisted files; `git -C " + WT + " status --porcelain | wc -l`; `df -m " + WT + " | tail -1`; `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave-mcp'`.\n2. Confirm first-hand, one line each, that the lane state above is true of THIS tree: the four symbols and the six §6 test names are present in the two allowlisted files (`grep -n` each); the `test result: ok. 840 passed; 0 failed` line is really in " + WF + "/w2-g2b.log; every " + WF + "/w2-g*.rc file's REAL_EXIT matches what the gates report claims; and the interim keep arm plus its rationale comment are still in `projection.rs`.\n3. Report as a discrepancy anything the tree contradicts: a moved arm, a missing §6 name, a report line not in its log, a new dirty file among the two allowlisted ones that the reports do not mention, or a HEAD that is not " + TIP + ".\nverdict=halt ONLY IF the tree is not the tree the evidence was taken against, or free disk is under " + MIN_DISK_MB.to_string() + " MB, or the cargo lock is held by something that is not a sibling cargo build. A dirty shared tree, foreign failures elsewhere and a moving HEAD are the expected condition — do NOT halt for them. Do not run cargo in this phase; the gate seat owns the builds.\n\n" + RULES,
    #{ label: "waj2r-baseline", capability_mode: "execute", output_schema: state_schema(), phase: "Baseline" });
if base.success != true {
    pause("infra", "BASELINE phase — the state seat died before reporting the tree fingerprint. Re-measure by hand: git rev-parse HEAD, shasum -a 256 of projection.rs and projection_tests.rs, df -m, and the REAL_EXIT lines in " + WF + "/w2-g*.rc; then relaunch.");
    return;
}
let st = base.output;
if st.verdict == "halt" {
    pause("user", "BASELINE phase HALT — the tree is not the one the lane's evidence was taken against: " + st.halt_reason + "  Fingerprint: " + st.tree_fingerprint + "  Read " + REPORTS + " against the current tree, decide whether the residue is intact, then relaunch.");
    return;
}
if st.disk_free_mb < MIN_DISK_MB {
    pause("infra", "BASELINE phase — free disk is " + st.disk_free_mb.to_string() + " MB, under the " + MIN_DISK_MB.to_string() + " MB floor the prior gate phase was already squeezed by. Free space (or point the lane at a larger volume) before any build: a cargo build that fills the disk corrupts every lane in this worktree.");
    return;
}
let FP = st.tree_fingerprint;

// ── Phase 2: the verification-record gate. Re-measure the foreign baseline; do not trust the
//    prior seat's list, and never report a bare clean or a bare failure. ───────────────────────
phase("Gate");
let p_gate = "You are the GATE seat for a resumed apex-waj.2. The implementation is closed and you may not edit any file. Your deliverable is a VERIFICATION RECORD, not a green tick.\n\n"
    + DONE + "\n" + FOREIGN + "\n" + GATE_SPEC + "\n"
    + "Baseline fingerprint measured before you started: " + FP + "\n\nRun, from " + WT + ", the design's gate commands (design-waj2 §5) in this order, each through `lockf -k -s -t 5400 /tmp/hts-cargo.lock`, each writing its own log and .rc under " + WF + ":\n  cmd-1  cargo test --release -p xai-grok-sampling-types --lib conversation::projection_tests\n  cmd-2  cargo test --release -p xai-grok-sampling-types --no-fail-fast\n  cmd-4  cargo check --workspace --all-targets --keep-going        (report the `^error\\[E0004` COUNT as a number)\n  cmd-5  the 6-package GATE exactly as design-waj2 §5 cmd-5 spells it\n  cmd-6  git diff -U0 -- the two allowlisted files, then grep -n '^+.*_ =>'\nThen, and this is the whole reason the lane stalled, do the two things the blocked run did not:\n  A. Re-derive the foreign baseline from THIS run's own log, name by name — never copy the list above. For every failing name anywhere in the tree, attribute it or declare it NEW. Check the owning file's dirty state (`git status --porcelain <file>`) and `git log -1 --format=%h -- <file>` before blaming a sibling lane; a name in a file this lane owns is lane-red, full stop.\n  B. Characterise `conversation::tool_search::tests::a_reused_call_id_spanning_whole_turns_does_not_drag_the_cut_back` by repetition: run it alone, then 4 more times (warm), and report each REAL_EXIT and verdict. It was absent at g1/g1b, passed at g2b, failed at g3, passed at g5b. If it reproduces deterministically, it is no longer intermittent — say so and check whether the two allowlisted files can reach it at all (they do not call it; `strip_discovery_pair` is unwired, which is design-mandated).\nThen write " + GATE_REC + " with sections (a) (b) (c) (d) exactly as specified, and report its path.\n\n" + RULES;
let gate = agent(p_gate, #{ label: "waj2r-gate", capability_mode: "execute", output_schema: gate_schema(), phase: "Gate" });
if gate.success != true {
    pause("verification", "GATE phase — the gate seat died mid-run, so there is no verification record. Check " + WF + " for the logs and .rc files it did write, and relaunch: a partial record is worse than none because it looks like a verdict.");
    return;
}
let g = gate.output;
let log_lines = [];
log_lines.push("BASELINE: " + st.verdict + " | " + FP + " | disk " + st.disk_free_mb.to_string() + " MB | lock " + st.cargo_lock
    + tail_if(!is_empty(st.discrepancies), " | DISCREPANCIES " + join_lines(st.discrepancies, " ; ")));
log_lines.push("GATE: verdict=" + g.verdict + " | lane crate: " + g.lane_crate_summary + " | foreign=" + join_lines(g.foreign_baseline, " ; ")
    + tail_if(!is_empty(g.new_failures), " | NEW " + join_lines(g.new_failures, " ; ")) + " | record " + g.record_path);
if g.verdict == "lane-red" || !is_empty(g.new_failures) {
    pause("verification", "GATE phase — an unattributed failing name: " + join_lines(g.new_failures, " ; ") + ". The record is at " + g.record_path + ". The coordinator adjudicates every name first-hand before a fixer is pointed at anything; do not relaunch this driver until each name above is either attributed to a lane or reproduced inside the two allowlisted files.");
    return;
}
let no_verdict = false;
if g.verdict == "no-verdict" {
    no_verdict = true;
}

// ── Phase 3: an independent second pass, graded against the attributed baseline. The blocked run
//    failed because it compared a whole-tree gate to "clean"; this one compares name SETS. ──────
phase("GateReproduce");
let rep = agent(
    "You are an INDEPENDENT reproduce seat for apex-waj.2. Do not open " + GATE_REC + " until after you have your own numbers — your value is that you did not read it.\n\n"
    + DONE + "\n" + GATE_SPEC + "\n"
    + "Re-run cmd-1, cmd-2 and cmd-5 from design-waj2 §5 into NEW log and .rc files (suffix -r2). Then compute the failing-name set and compare the failing-name set against §3 of \"" + WF + "/REPORT-waj2-gates.md\" (the prior run's record; the fresh record written minutes ago is NOT your reference — yours must stand on its own).\nA name that differs from §3 is NOT automatically this lane's failure: decide, per name, whether a sibling lane's file moved under the run (re-run `git status --porcelain` and compare mtimes to your own start time), whether it is one of the declared environment dependencies, or whether it is the intermittent tool_search cut-back test. Report the intermittent test's repeated-run results again, independently.\nverdict: lane-clean if your new-failure set is empty; no-verdict if any foreign name is unattributable; lane-red only for a name reachable from the two allowlisted files. Append your pass to " + GATE_REC + " as a clearly-labelled reproduce section.\n\n" + RULES,
    #{ label: "waj2r-repro", capability_mode: "execute", output_schema: gate_schema(), phase: "GateReproduce" });
if rep.success != true {
    pause("verification", "GATEREPRODUCE phase — the independent seat died, so the gate is single-attested. The prior run's whole-tree gate was also single-attested; re-run this phase before review.");
    return;
}
let rp = rep.output;
log_lines.push("REPRO: verdict=" + rp.verdict + " | lane crate: " + rp.lane_crate_summary + tail_if(!is_empty(rp.new_failures), " | NEW " + join_lines(rp.new_failures, " ; ")) + " | intermittent " + join_lines(rp.intermittent_evidence, " ; "));
if rp.verdict == "lane-red" || !is_empty(rp.new_failures) {
    pause("verification", "GATEREPRODUCE phase — the second pass disagrees with the first: " + join_lines(rp.new_failures, " ; ") + " appeared and is not attributed. This is exactly the discrepancy that blocked the previous run; adjudicate it by hand (whose file, whose edit, did the tree move mid-run) before review grades it.");
    return;
}
if rp.verdict == "no-verdict" {
    no_verdict = true;
}
FP = rp.tree_fingerprint;

// ── Phase 4: three fresh read-only lenses plus one fixer, max 3 rounds. A read-only seat has no
//    shell, so it grades source and artifacts — it is never told to run a gate. ────────────────
let REV_HEAD = "You are a FRESH review lens on bead apex-waj.2 (projector discovery arm) in " + WT + ". You inherited nothing and trust no receipt you have not read in the file itself. You have NO shell: read the sources and the artifacts named below, and say in `notes` what you could not check without running a command.\n\n"
    + "TARGET: the working tree at fingerprint " + FP + " — the two allowlisted files `projection.rs` and `projection_tests.rs` in crates/codegen/xai-grok-sampling-types/src/conversation/, uncommitted. Read " + DESIGN + " (§1 allowlist, §4 mutants, §6 done bar, §8 known-bad) and " + WF + "/REPORT-waj2-gates.md first.\nSEVERITY: Blocking = a model-visible wire item can be wrong or a handle's bytes are rewritten, the lane reached outside its two files, a claimed mutant death is not real, or the gate record is unfalsifiable. Major = an invariant the cut claims to pin that is unpinned or pinned for the wrong reason, a vacuous or self-referential assertion, a design clause silently not met, a foreign failure absorbed into a clean verdict. Minor/Nit = otherwise. Cite file:line in the CURRENT file plus the artifact line that shows it; an observation with no reproduction is a note, not a finding.\nReturn findings FLAT, one string each, shaped exactly: [Blocking] claim … || evidence: … || fix: …   If nothing rises to Blocking or Major, return an empty findings array and say so in notes — do not manufacture findings to look thorough. Do not edit anything and do not run git write commands.\n\nYOUR LENS:\n";
let LENSES = [
    "L1 BYTE FIDELITY. Open `projection.rs` at `tool_search_output_id` and at the interim unconditional-keep arm. Does any path rewrite, reorder, add or delete a key inside `ToolSearchItem::raw()` other than the output's own `id`, and is the rewrite done on a cloned `Value` via `as_object_mut` rather than by rebuilding the item from typed accessors? Is `strip_discovery_pair` still unreachable from `project_switch_history` (it must stay `pub(crate)` and called by nothing but tests — an unreachable strip is design-mandated here, not dead code to delete)? Does the mint gate on the source output carrying an `id` so an absent one stays absent?",
    "L2 ID DOMAIN AND ORACLE. Is the minted id `tso_` plus the same truncation as `xw_reasoning_id_values`, keyed on (cell, ord, call_id, tools) with `ord` counting discovery outputs only, and is `call_id` provably never rewritten anywhere in the diff? Are the expected constants derived from an independent oracle rather than printed out of the implementation — check the python3 recomputation in the gates report against the literals actually in `projection_tests.rs`, and check the MUT-D1f witness id is asserted ABSENT (absent `tools` must canonicalise as `[]`, never `null`). A test whose expected value is computed by the same helper that produced the actual value is a Major.",
    "L3 LAYER, COLLISION AND GATE HONESTY. Does the diff touch `tools[]`, `messages.rs`, `responses.rs`, `mutations.rs`, `tool_search.rs` or `project_switch_history`'s signature at all (it must not: those are apex-waj.35 / .48 / .5 / .11 / .37)? Do any `DropReason` doc-comments still assert that no Discovery drop reason exists, now that PA-29 added one? Then audit " + WF + "/REPORT-waj2-gates.md and " + GATE_REC + ": does the record contain all four parts (a)-(d), does every foreign name carry an attribution, and is the intermittent tool_search test characterised by repetition rather than a single run? A gate record missing part (c), or reporting NO VERDICT as clean, is Blocking on the record (not on the code) — and a NEW failing name blamed on a sibling without naming the sibling's file is a Major.",
];
let rounds = [];
let open = [];
let clean = false;
let round = 0;
while round < 3 && !clean {
    round = round + 1;
    phase("Review round " + round.to_string());
    let jobs = [];
    for lens in LENSES {
        jobs.push(#{ prompt: REV_HEAD + lens, label: "waj2r-rev-r" + round.to_string() + "-" + jobs.len().to_string(), capability_mode: "read-only", output_schema: findings_schema(), phase: "Review" });
    }
    let res = parallel(jobs);
    let dead = [];
    let i = 0;
    for r in res {
        if r == () || r.success != true || r.output == () {
            dead.push(i);
        }
        i = i + 1;
    }
    if dead.len() > 0 {
        let again = [];
        for d in dead {
            again.push(#{ prompt: REV_HEAD + LENSES[d], label: "waj2r-rev-r" + round.to_string() + "-l" + d.to_string() + "-retry", capability_mode: "read-only", output_schema: findings_schema(), phase: "Review" });
        }
        let redone = parallel(again);
        let m = 0;
        for d in dead {
            res[d] = redone[m];
            m = m + 1;
        }
    }
    let blocking = [];
    let major = [];
    let minor = [];
    let seats = 0;
    let disputed = false;
    for r in res {
        if r == () || r.success != true || r.output == () {
            continue;
        }
        seats = seats + 1;
        if r.output.verdict == "fix-first" {
            disputed = true;
        }
        blocking = blocking + tagged(r.output.findings, "[Blocking]");
        major = major + tagged(r.output.findings, "[Major]");
        minor = minor + tagged(r.output.findings, "[Minor]") + tagged(r.output.findings, "[Nit]");
    }
    // A lens that came back dead is not a clean round, however quiet the other two were.
    if seats < LENSES.len() {
        rounds.push("review round " + round.to_string() + ": NOT CLEAN — only " + seats.to_string() + " of 3 lenses reported (each dead lens already retried once)");
        pause("infra", "REVIEW round " + round.to_string() + " — only " + seats.to_string() + " of 3 lenses reported after one retry each. A quiet round from two seats is not a clean round from three; relaunch this driver (the gate record is already on disk at " + GATE_REC + ", so nothing measured is lost).");
        return;
    }
    open = blocking + major + minor;
    clean = is_empty(blocking) && is_empty(major) && !disputed;
    rounds.push("review round " + round.to_string() + " @ " + FP + ": " + blocking.len().to_string() + " Blocking, " + major.len().to_string() + " Major, " + minor.len().to_string() + " Minor/Nit, seats=" + seats.to_string() + tail_if(disputed && clean, " (all lenses graded fix-first despite no Blocking/Major)") + tail_if(clean, " CLEAN"));
    if !clean && round < 3 {
        phase("Fix round " + round.to_string());
        let fx = agent(
            "You are the FIXER for apex-waj.2, working alone: every review seat is finished, so nothing races you. Tree is at " + FP + ". Edit ONLY `projection.rs` and `projection_tests.rs`; a finding that needs any other file goes to handoffs with its evidence.\n\n" + DONE + "\n" + GATE_SPEC + "\n"
            + "For each finding below: re-read the cited code first. If it is factually wrong, REJECT it with file:line proof and the command or artifact line — do not apply a wrong fix to be agreeable. If you accept it: get it red (build the mutant it implies), fix, get it green, then re-run cmd-1, cmd-2 and cmd-6 with their own logs and .rc files and report the REAL_EXIT values. Re-measure the tree fingerprint when you finish — the next round must grade the build you actually produced, and append what changed to " + GATE_REC + ".\n\n<FINDINGS>\n" + join_lines(open, "\n") + "\n</FINDINGS>\n\n" + RULES,
            #{ label: "waj2r-fix-r" + round.to_string(), capability_mode: "all", output_schema: fix_schema(), phase: "Fix" });
        if fx.success != true {
            pause("verification", "FIX round " + round.to_string() + " — the fixer died mid-flight and the tree may sit between two findings. Diff " + PROJ + " and " + TS + " against the fingerprint " + FP + " recorded above, decide whether to keep or unwind its partial edit, then relaunch.");
            return;
        }
        rounds.push("fix round " + round.to_string() + ": landed " + join_lines(fx.output.items_landed, " ; ") + tail_if(!is_empty(fx.output.items_rejected), " | rejected " + join_lines(fx.output.items_rejected, " ; ")) + tail_if(!is_empty(fx.output.items_not_landed), " | NOT landed " + join_lines(fx.output.items_not_landed, " ; ")) + " -> " + fx.output.tree_fingerprint + " | recheck " + join_lines(fx.output.recheck, " ; "));
        FP = fx.output.tree_fingerprint;
    }
}

// ── Phase 5: print the verdict. A script has no filesystem, so the durable record is the gate
//    seat's; this is the run's own summary, and it never claims a clean it did not measure. ─────
phase("Report");
let verdict = "lane-clean (new failures minus the attributed foreign baseline = empty, lane crate green at REAL_EXIT=0)";
if no_verdict {
    verdict = "NO VERDICT — a foreign failing name could not be attributed, so the gate result is unfalsifiable. That is NOT clean and NOT a failure of this lane.";
}
let report = "apex-waj.2 (projector discovery arm) — RESUMED AT THE GATE by hts-waj2-resume\n"
    + "VERDICT: " + verdict + "\n" + join_lines(log_lines, "\n") + "\n"
    + "GATE / REPRODUCE / REVIEW:\n" + join_lines(rounds, "\n") + "\n"
    + "VERIFICATION RECORD: " + GATE_REC + "  — (a) command lines + REAL_EXIT from each .rc, (b) tree fingerprint before and after, (c) foreign-failure baseline with per-name attribution, (d) verdict = new failures minus baseline.\n"
    + "REVIEW: converged=" + clean.to_string() + " after " + round.to_string() + " round(s); exactly three read-only lenses per round, a round with fewer than three reporting lenses is never treated as clean, one fixer per round, 3-round cap.\n"
    + "OPEN FINDINGS (" + open.len().to_string() + "):\n" + join_lines(open, "\n") + tail_if(is_empty(open), "  (none open at the last round that reported)\n") + "\n"
    + "STILL OWED, NOT BY THIS LANE: the live-wire arm (the D1 re-key and the D3 drop are proven as code shape only — apex-waj.35 owns the route tuple that would ever call them), and `tool_search.rs:6561`'s now-false 'DropReason has no Discovery variant' note.\n"
    + "NOTHING COMMITTED\n";
log(report);
report;
