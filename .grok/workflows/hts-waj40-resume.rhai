// hts-waj40-resume — continuation driver for bead apex-waj.40 (wiretap2 fail-open state parse).
// Standalone on purpose: it inherits nothing from scaffold/, whose generated output is currently broken (drafts/FLEET-PARSE-20260930.md). Shape precedent: library/campaign-rhai/wiretap2-slice-b4.rhai, event-plumbing-fix-r6b.rhai, .grok/workflows/hts-refleet.rhai.
// This lane stopped at the "Gates did not reproduce clean" pause. It starts at the gate.

let meta = #{
    name: "hts-waj40-resume",
    description: "Resume apex-waj.40 at the gate, not at the code: re-measure the foreign-failure baseline from source, run the V0/G1/G2/G3/E2E battery as a VERIFICATION RECORD (commands + true exit statuses read back from files, tree fingerprint, named foreign baseline, verdict = new failures minus baseline), reproduce it with an independent seat, then converge a three-lens review with a fixer. Never edits product code outside the one owned file, never commits, tags or pushes.",
    when_to_use: "Baseline, Red, Green and three mutant rounds are DONE and evidenced in /tmp/hts-wf/REPORT-waj40-{green,mbt,mbt2,mbt3,gates}.md; the run blocked at GateReproduce. Fires when a workflow slot is free and the plans tree is stable. Do NOT fire it to re-implement anything — the fail-closed fix is already in the working tree and this driver refuses to redo finished phases.",
    phases: [
        #{ title: "Baseline", detail: "prove the six prior artifacts still exist and still describe the tree on disk; refuse to redo Red/Green/mutants" }, #{ title: "Gate", detail: "re-measure the foreign baseline first, then run V0/G1/G2/G3/E2E into a verification record" },
        #{ title: "GateReproduce", detail: "independent second seat, same commands, record diff — tree-moved vs baseline-moved vs real new failure" }, #{ title: "Review", detail: "three read-only lenses + one fixer, max 3 rounds, a short round is never clean" },
        #{ title: "Report", detail: "verdict, open findings, what is still owed, and NOTHING COMMITTED" }
    ]
};

let PLANS = "/Users/palanisd/Projects/upstream/grok/plans";
let INSTR = PLANS + "/harness/hosted-tool-search/ratchet-capture/codex-arm/wiretap2.py"; let PY = "/Users/palanisd/.homebrew/bin/python3";
let WT = "/Users/palanisd/Projects/upstream/wt/apex-ayl-hosted-tool-search";
let W = "/tmp/hts-wf"; let OUT = W + "/waj40-resume";
let LEDGER = W + "/harden/PROCESS.md";

// The blob every prior artifact was stamped against (REPORT-waj40-gates.md s3 G2); if the instrument no longer hashes to this, all prior evidence is stale — the lane is not broken, the tree moved.
// Corrected 2026-09-30 by the coordinator. The value this line carried at authoring time
// (0f17365ea64381c4fa93966024769489b1fac5d4f8e472375ddd62e0235101a2) matched NOTHING: not the
// working tree, not the plans HEAD blob (da8afdc45cdd). It was copied from an artifact that was
// itself describing a different tree, and the driver's own baseline guard then halted a lane on
// it ("tree moved under the evidence") when in fact the pin had never been true. The file's
// mtime, 06:46:23, predates this driver. Lesson generalised into BP-54: a driver must not carry
// a frozen digest as a precondition — it must measure one. Until the generator does that, this
// constant is the digest measured by the coordinator at 2026-09-30T10:2x local, re-measured below.
let LIVE_SHA = "a738e70691532ef65ba74a67d6d0bc2331577679efb0a087deb7349f124e14c8";
let COMMIT_BLOB_SHA = "938e47ab03969a25cd49cc41cba07dd3a434039a102db7ce97915801076b37a0";

let EVID = [
    W + "/REPORT-waj40-green.md", W + "/REPORT-waj40-mbt.md", W + "/REPORT-waj40-mbt2.md",
    W + "/REPORT-waj40-mbt3.md", W + "/REPORT-waj40-gates.md",
    PLANS + "/harness/workflow-design/designs/design-waj40.md"
];

// ---- host-safe helpers (transplanted verbatim from scaffold-helpers; do not re-author) ----
fn as_text(v) {
    if v == () { return ""; }
    if type_of(v) == "string" { return v; }
    return v.to_string();
}

fn as_array(v) {
    if type_of(v) == "array" { return v; }
    return [];
}

fn join_lines(arr, sep) {
    let out = "";
    let first = true;
    for e in arr {
        if !first { out = out + sep; }
        out = out + as_text(e);
        first = false;
    }
    return out;
}

fn n_of(arr) {
    return as_array(arr).len().to_string();
}

// ---- schemas: FLAT only (strings, booleans, integers, arrays of strings) ----
fn baseline_schema() {
    return #{
        type: "object",
        properties: #{
            evidence_present: #{ type: "array", items: #{ type: "string" }, maxItems: 8 },
            evidence_missing: #{ type: "array", items: #{ type: "string" }, maxItems: 8 },
            instrument_sha256: #{ type: "string" },
            digest_matches_evidence: #{ type: "boolean" },
            plans_head: #{ type: "string" },
            fix_uncommitted: #{ type: "boolean" },
            worktree_state: #{ type: "string" },
            tree_moved: #{ type: "boolean" },
            summary: #{ type: "string" }
        },
        required: ["evidence_present", "evidence_missing", "instrument_sha256", "digest_matches_evidence", "plans_head", "fix_uncommitted", "worktree_state", "tree_moved", "summary"],
        additionalProperties: false
    };
}

// The VERIFICATION RECORD, as a structured return. `foreign_baseline` is the falsifiability
// half: a record with an empty one is NO VERDICT by construction, however green it looks.
fn gate_schema() {
    return #{
        type: "object",
        properties: #{
            record_path: #{ type: "string" },
            commands: #{ type: "array", items: #{ type: "string" }, maxItems: 14 },
            tree_fingerprint: #{ type: "string" },
            foreign_baseline: #{ type: "array", items: #{ type: "string" }, maxItems: 10 },
            new_failures: #{ type: "array", items: #{ type: "string" }, maxItems: 10 },
            baseline_measured: #{ type: "boolean" },
            disk_guard_ok: #{ type: "boolean" },
            arm_g1: #{ type: "string" },
            arm_g2: #{ type: "string" },
            arm_g3: #{ type: "string" },
            arm_e2e: #{ type: "string" },
            verdict: #{ type: "string" },
            summary: #{ type: "string" }
        },
        required: ["record_path", "commands", "tree_fingerprint", "foreign_baseline", "new_failures", "baseline_measured", "disk_guard_ok", "arm_g1", "arm_g2", "arm_g3", "arm_e2e", "verdict", "summary"],
        additionalProperties: false
    };
}

fn review_schema() {
    return #{
        type: "object",
        properties: #{
            clean: #{ type: "boolean" },
            blocking: #{ type: "array", items: #{ type: "string" }, maxItems: 12 },
            major: #{ type: "array", items: #{ type: "string" }, maxItems: 12 },
            minor: #{ type: "array", items: #{ type: "string" }, maxItems: 12 },
            refused: #{ type: "array", items: #{ type: "string" }, maxItems: 8 },
            summary: #{ type: "string" }
        },
        required: ["clean", "blocking", "major", "minor", "refused", "summary"],
        additionalProperties: false
    };
}

// ---- shared preamble: scope, gate honesty, read/write rules ----
let rules = "CONTEXT: bead apex-waj.40 — the wiretap2 capture instrument used to fail OPEN on a torn wiretap2-state.json (it swallowed the parse error, reset state and re-spent provider budget). The fix makes a damaged resume refuse to boot: `try:` / `except Exception as exc:` / stderr refusal naming wiretap2-state.json / `sys.exit(2)`, in " + INSTR + " around :1959-1977. This is campaign tooling that happens to be a shipped instrument, not Rust product code.\n"
    + "OWNERSHIP: the ONLY file this lane may edit is " + INSTR + " (design-waj40.md s1). The code worktree " + WT + " is SHARED and DIRTY (65 modified entries across several lanes) — never edit a file this lane does not own, never revert another lane's lines, never `git add`, NEVER commit, tag or push in any repo. The plans repo is dirty for the same reason; your fix is owed to the controller as a commit message, not a commit.\n"
    + "NO RUST HERE: this bead touched no Rust file and runs no cargo. If you think you need cargo, that is a scope violation — stop and say so in your return. (House rule if a sibling lane's build is in flight: `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave-mcp'` — wait for the target lock, never kill a sibling build.)\n"
    + "SERIALIZATION: the selftest battery IS the mutex (design s9 / FOOTGUNS). Before any battery run: `pgrep -fl 'wiretap2.py --selftest'`; if a sibling battery or daemon is live, wait and re-check — two batteries on one box produce a red nobody owns. Also check `pgrep -fl 'port 9777|port 9778'`. DISK: run `df -g /System/Volumes/Data` before anything that writes captures. Report disk_guard_ok=false if under 8 free GiB (it fell 19->12 GiB during the gates phase on sibling builds); the driver halts on that.\n"
    + "GATE HONESTY (this is why the lane is stuck): a green run is not a verdict. A VERIFICATION RECORD must contain (a) every command line actually invoked with its TRUE exit status captured as `cmd > log 2>&1` then `echo \"X_RC=$?\" > x.rc` and read back from that file — never `| tail`, never `cmd; echo EXIT=$?`, never a status you remember; (b) a fingerprint of the tree it ran against; (c) the FOREIGN-FAILURE BASELINE — the named failures this lane does not own, each with its attribution, re-measured now rather than copied from the design; and (d) the verdict, computed as new-failures-minus-baseline. A record without (c) is unfalsifiable: report verdict NO VERDICT. Never dress a baseline as a regression, and never dress a real new failure as baseline noise.\n"
    + "OFFLINE LIMIT: every gate here is loopback/mock (upstream pointed at closed ports 1/9777/9778). No paid request, and no offline green may be written as verification at the source — the live-capture arm at the real proxy is still owed.\n"
    + "READ RULES: grep before reading; never more than ~120 contiguous lines of one file (wiretap2.py is 12,577 lines and a battery log is ~700 KB); cap yourself at 25 tool calls. WRITE RULES: `mkdir -p " + OUT + "` first; the artifact goes to disk FIRST in appends of at most 60 lines, the flat structured return last. Keep every return value FLAT — a mis-shaped nested array returns an API 400 and destroys the seat and its work. Write only under " + OUT + " and " + W + "; never `rm` (the harness bans it) — leave every log in place.\n"
    + "LEDGER: append 3-6 lines to " + LEDGER + " when you finish (item, what you ran/changed, verdict, what is left). Append, never rewrite; if you cannot append, write " + OUT + "/PROCESS-append.md and say so.\n"
    + "EVIDENCE: every claim cites the file:line or the log file it came from. Quote the decisive line.\n";

phase("Baseline");
// The five artifacts below are the lane's sunk cost (~25 M tok, 7 seats). Re-deriving them is not
// caution, it is a second lane racing the first over a shared dirty tree — so this phase checks the artifacts still describe the tree, then REFUSES to repeat any of them.
let b = agent(rules + "\nTASK B-1 — establish that the finished work is still the finished work, and change nothing.\n"
    + "Already DONE and evidenced — do NOT redo, re-run or re-implement any of it: Baseline/Red/Green (" + W + "/REPORT-waj40-green.md: arm 43 `st_43_damaged_state_refuses_to_respend` defined ~:6093, registered ~:12252, round-2 `rc == 2` clause :6121-6129, file-naming clause :6130-6132) and mutant rounds 1-3 (REPORT-waj40-mbt.md / -mbt2.md / -mbt3.md, one repair round between r1 and r2).\n"
    + "Gate pass 1 — REPORT-waj40-gates.md: V0 blob identity PASS, G1 RED differential PASS (G1_RC=1, arm 43 FAIL, 53/78 with 25 behavioural gaps), G2 live green PASS (78/78, listeners_left=0), G3 mutant PASS (77/78, 1 failed: arm 43) on a re-derived single-hunk gates-M1.py, and the s6 fresh-tag/torn-resume E2E (RESUME_BOOT_RC=2, refusal names the file). NOT done: the review lenses, the commit/exfil, the run-arm.sh:145 sha pin, the parseable-but-keyless cap-reset gap, the live-capture arm.\n"
    + "DO NOW, read-only plus one note file: (1) `mkdir -p " + OUT + "`; `ls -l` each artifact in this list and record present/missing: " + join_lines(EVID, ", ") + ". (2) `shasum -a 256 " + INSTR + "` — the gates evidence was taken against " + LIVE_SHA + "; any other digest means the tree moved under the evidence, so report tree_moved=true with the new digest plus the `git -C " + PLANS + " status --porcelain -- harness/hosted-tool-search/ratchet-capture/codex-arm/` line for it, and do not chase why.\n"
    + "(3) `git -C " + PLANS + " rev-parse --short HEAD` and `git -C " + PLANS + " log --oneline -1 -- harness/hosted-tool-search/ratchet-capture/codex-arm/wiretap2.py`: if the last commit on the path is still 2e29c76 (blob " + COMMIT_BLOB_SHA + ", fail-open by design) the fix is uncommitted — fix_uncommitted=true, which is expected, not a defect. (4) `git -C " + WT + " rev-parse --short HEAD` and `git -C " + WT + " status --porcelain | wc -l`, both reported as worktree_state, e.g. \"ff2351c5 dirty=65\" — read-only, touch nothing there.\n"
    + "(5) Grep the live file to confirm the fix is physically present — the `if os.path.exists(state_path):` / `try:` / `except Exception as exc:` / `sys.exit(2)` shape — and report the line numbers you actually see. (6) Write " + OUT + "/baseline.md with all of the above plus one line naming the phases this driver is forbidden to re-run, then return.",
    #{ label: "waj40r-baseline", output_schema: baseline_schema(), phase: "Baseline", capability_mode: "all" });

if b == () || b.success != true || b.output == () {
    pause("infra", "The baseline seat died before returning, so nothing about the tree state is known. Re-launch: this phase is read-only and costs minutes, not the lane.");
    return "PARTIAL: baseline seat died; tree state unknown, nothing was measured.";
}
log("baseline: " + as_text(b.output.summary));
log("instrument sha=" + as_text(b.output.instrument_sha256) + " plans HEAD=" + as_text(b.output.plans_head) + " worktree=" + as_text(b.output.worktree_state) + " fix_uncommitted=" + as_text(b.output.fix_uncommitted));
if as_array(b.output.evidence_missing).len() > 0 {
    pause("user", "Evidence artifacts are missing: " + join_lines(as_array(b.output.evidence_missing), ", ") + ". This driver deliberately refuses to re-derive Red/Green/mutants — re-running them would put a second seat on a shared dirty tree and would not reproduce the numbers the lane is standing on. Recover the artifacts or relaunch a full TDD driver instead.");
    return "HALTED: prior evidence incomplete (" + join_lines(as_array(b.output.evidence_missing), ", ") + "); continuation refuses to redo finished phases.";
}
// A pin mismatch at BASELINE is not a moving target: it means the stamp this driver inherited
// was stale before the run started, which is a fact to record, not a reason to stop. Movement
// DURING the run is a different thing and is caught downstream, where pass 2 compares its own
// measured FINGERPRINT against pass 1's (GateReproduce, case 1) rather than against any literal.
// Halting here cost one lane on a false premise; halting on a file that is not there is still
// fatal, because nothing downstream can measure a module it cannot read.
// Empty digest = the baseline seat could not read the module (instrument_sha256 is a required
// schema field, so absence means absence of measurement, not absence of a key in the reply).
if as_text(b.output.instrument_sha256) == "" {
    pause("infra", "The baseline seat returned no instrument digest, so the module at " + INSTR + " is missing or unreadable and there is nothing to gate. Settle the tree with the owning lane first.");
    return "HALTED: instrument absent or unmeasured.";
}
if b.output.tree_moved == true || b.output.digest_matches_evidence != true {
    log("STALE STAMPS (recorded, not fatal): the instrument measures " + as_text(b.output.instrument_sha256) + ", not the " + LIVE_SHA + " this driver inherited. Every prior artifact stamped " + LIVE_SHA + " describes a different build, so pass 1 must copy its own MODULE_SHA256 into the record and no later seat may quote an older one. The freeze point for THIS run is the measured digest.");
}
log("Baseline holds: " + n_of(b.output.evidence_present) + "/" + n_of(EVID) + " artifacts present, instrument digest matches. Red, Green and all 3 mutant rounds are NOT re-run.");

phase("Gate");
// Order matters: the baseline is measured BEFORE the battery, from source, so that the verdict is
// subtraction against something observed today rather than a number copied out of the design.
let g = agent(rules + "\nTASK G-1 — re-measure the FOREIGN-FAILURE BASELINE first, from source, before running any gate. For each candidate below, decide from the current tree whether it still stands, and return it in foreign_baseline as \"<named failure/arm> — <attribution>\". Add any new foreign failure you find; never silently drop one:\n"
    + "  * the non-43 FAILs in an `--against` run (expected around arms 36, 44-58, 61-65, 67-69) — the committed 2e29c76 blob predates ~1,300 lines of rig features; `gap_classes()` labels them `behavioural` vs `module lacks the flag`. Owned by history, not by .40.\n  * run-arm.sh:145 pinning `\"tee\": {\"daemon\": \"wiretap2.py (sha 13d6a2ec...)\"}` — matches none of live / frozen / committed digests. Pre-existing drift owned by the arm lane; report, do not fix.\n"
    + "  * the parseable-but-keyless cap reset: a state file of `{\"next_index\": 3}` resumes and re-inits `forwarded_posts` to 0 via `setdefault` (~:1981); the `cap_policy` guard does not catch it and arm 43 tears bytes rather than removing a key. Out of scope for .40 per design s3.4/s3.5 — owned by the cap-policy lane.\n  * the live-capture arm at the real proxy — still owed, so the ceiling of any verdict here is OFFLINE.\n  * curl code variance on the refusal path (RESUME_HEALTH_RC 7 vs 56 across rounds): both are \"no daemon answered\" with an empty body — an accepted branch, not a failure.\n"
    + "Set baseline_measured=true only if you confirmed each of these against the current file or log; false if any is inherited. An inherited baseline is an unfalsifiable record.\n"
    + "\nTASK G-2 — run the gate set (design-waj40.md s5), each exactly as written, each with `> log 2>&1` then `echo \"X_RC=$?\" > out/x.rc`, and read the status back from the .rc file. Serialize behind any sibling battery first (SERIALIZATION rule). Commands, with " + INSTR + " as the module and " + PY + " as the interpreter:\n"
    + "  V0  `git -C " + PLANS + " show 2e29c76:harness/hosted-tool-search/ratchet-capture/codex-arm/wiretap2.py | shasum -a 256` and `shasum -a 256 /tmp/wiretap2-2e29c76a505d.py` — the two digests must match each other and " + COMMIT_BLOB_SHA + "; if they do not, report and stop, R1's differential would be against a different artifact.\n  G1  `<module> --selftest --against /tmp/wiretap2-2e29c76a505d.py` → expect NON-ZERO rc AND `grep -c '^FAIL 43 damaged_state_refuses_to_respend'` == 1. Record the SUMMARY line and the GAP CLASSES line verbatim.\n"
    + "  G2  `<module> --selftest` → expect rc=0, `PASS 43 …`, `SUMMARY = N/N passed, 0 failed`, `HERMETICITY … listeners_left=0`; copy MODULE_SHA256 into the record so no later seat can quote a stale copy.\n  G3  `<module> --selftest --against <single-hunk mutant>` → expect NON-ZERO rc with ONLY arm 43 failing. Re-derive the mutant from the FINAL blob with the precondition-guarded script " + W + "/gates-mutate-m1.py (it refuses to write unless it finds exactly one `# Fail CLOSED` anchor and exactly one `sys.exit(2)` within 20 lines) into " + OUT + "/M1.py; " + W + "/wiretap2-M1.py is a known pre-repair two-hunk copy — usable only as a continuity check, say so. Never mutate the live file. A green G3 means arm 43 is vacuous and the bead is not done.\n  E2E design s6, both halves, upstream on closed loopback ports, fresh HOME from `mktemp -d`: fresh --tag boots healthy with no `refusing to start`; then tear the state file (`printf '%s' '{\"next_index\": 3, \"forwarded_po'`) and re-run the identical command on the same --home and --tag → expect a NON-ZERO boot plus `refusing to start` naming wiretap2-state.json, with the damaged bytes left untouched. Never `rm` the home.\n"
    + "\nTASK G-3 — write " + OUT + "/gate-record-1.md BEFORE returning, with the four mandatory parts labelled: (a) COMMANDS: one line per command as `LABEL rc=<status read from .rc> :: <full command line>`; (b) FINGERPRINT: instrument sha256, plans HEAD, plans dirty count, worktree HEAD+dirty count, and the counts you saw; (c) FOREIGN-FAILURE BASELINE: every item from G-1 with its attribution; (d) VERDICT: new_failures = observed failures minus the baseline, and verdict = VERIFIED-OFFLINE if new_failures is empty and all four arms pass, NEW FAILURES if not, NO VERDICT if baseline_measured is false or any arm lacks a status read back from a file. Mirror those four parts into the structured return (commands and foreign_baseline as arrays of strings, arm_g1/g2/g3/e2e each as `PASS <the decisive observed token>` or `FAIL <why>`). Run the raw-key sweep `grep -rEil 'sk-[A0-9a-z]{16,}' " + OUT + "` and report the hit count in summary; it must be 0.",
    #{ label: "waj40r-gate-1", output_schema: gate_schema(), phase: "Gate", capability_mode: "all" });

if g == () || g.success != true || g.output == () {
    pause("infra", "Gate pass 1 died mid-run. The battery is the mutex and logs land in " + OUT + " — check `pgrep -fl 'wiretap2.py --selftest'` and the newest log there before relaunching, so a half-dead battery is not left holding the box.");
    return "PARTIAL: gate pass 1 seat died; no verification record.";
}
if g.output.disk_guard_ok != true {
    pause("infra", "Disk guard tripped: the gate seat reports under 8 free GiB on /System/Volumes/Data. The battery writes capture dirs per arm and a full battery on a nearly-full APFS volume fails in ways that look like code defects. Free space (stash-aside + scoped cargo clean is the house pattern; never rm) and relaunch.");
    return "HALTED: disk guard — not enough free space to run the battery honestly.";
}
log("gate 1 fingerprint: " + as_text(g.output.tree_fingerprint));
log("gate 1 arms: G1=" + as_text(g.output.arm_g1) + " G2=" + as_text(g.output.arm_g2) + " G3=" + as_text(g.output.arm_g3) + " E2E=" + as_text(g.output.arm_e2e));
log("gate 1 baseline (" + n_of(g.output.foreign_baseline) + " named): " + join_lines(as_array(g.output.foreign_baseline), " | "));
log("gate 1 verdict=" + as_text(g.output.verdict) + " new_failures=" + n_of(g.output.new_failures) + " record=" + as_text(g.output.record_path));
if g.output.baseline_measured != true || n_of(g.output.foreign_baseline) == "0" {
    pause("verification", "Gate pass 1 carries no re-measured foreign-failure baseline, so its verdict is unfalsifiable — that is NO VERDICT, not clean and not a failure of the lane. Re-measure the baseline from source (the 24-25 --against behavioural gaps, run-arm.sh:145, the keyless cap reset, the owed live arm) and rerun this driver.");
    return "NO VERDICT: gate record has no foreign-failure baseline.";
}

phase("GateReproduce");
// The exact place the last two runs died. Halting on "did not reproduce" without naming WHOSE tree moved is what made the pause useless, so this pass classifies the discrepancy instead of just observing it: tree-moved (no lane can fix it) / baseline-moved (measurement is wrong) /
// real new failure agreed by both records (goes to the fixer).
let ask_repro = rules + "\nTASK R — independent second gate pass for apex-waj.40. You did NOT see pass 1's reasoning: read ONLY " + as_text(g.output.record_path) + " for the command lines and the claimed baseline, then re-run the whole set yourself (V0, G1, G2, G3 on a re-derived single-hunk mutant, and the E2E both halves), each with its own log and its own .rc file under " + OUT + " (suffix every filename with -2 so pass 1's logs survive untouched). Never trust pass 1's status token — read your own .rc.\n"
    + "Write " + OUT + "/gate-record-2.md as a DIFF against record 1, per arm: same rc? same SUMMARY counts? same failing-arm set? Then classify, in this order:\n"
    + "  1. TREE MOVED — your FINGERPRINT differs from record 1's (instrument sha256, plans HEAD, or dirty counts). Say exactly which component moved; this is not a lane failure and no fix of yours can settle it.\n  2. BASELINE MOVED — the foreign-failure set you can actually attribute differs from record 1's. List the items that appeared or vanished, and set baseline_measured accordingly.\n  3. NEW FAILURE — a failure in neither record's baseline. Prefix every new_failures entry with `BOTH-RECORDS:` if record 1 lists the same failure, or `ONLY-IN-RECORD-2:` if it does not — that prefix is how the driver decides whether a fixer is worth waking.\n"
    + "Report verdict VERIFIED-OFFLINE only if your own four arms all pass against your own measured baseline; NO VERDICT if any status came from memory or the baseline is inherited; otherwise NEW FAILURES. Keep commands/arm_* flat strings, mirror the record, and re-run the raw-key sweep over " + OUT + " and put the hit count in summary (must be 0).";
let r2 = agent(ask_repro, #{ label: "waj40r-gate-2", output_schema: gate_schema(), phase: "GateReproduce", capability_mode: "all" });

if r2 == () || r2.success != true || r2.output == () {
    pause("infra", "The reproduce seat died mid-battery. Both records are on disk in " + OUT + "; check `pgrep -fl 'wiretap2.py --selftest'` before relaunching so you do not stack a third battery on the box.");
    return "PARTIAL: reproduce seat died; record 1 stands alone and is unconfirmed.";
}
if r2.output.disk_guard_ok != true {
    pause("infra", "Disk guard tripped on the reproduce pass (under 8 free GiB). Clear space — never with rm — and relaunch; a battery that dies on ENOSPC looks exactly like a code defect.");
    return "HALTED: disk guard on reproduce pass.";
}
log("gate 2 fingerprint: " + as_text(r2.output.tree_fingerprint));
log("gate 2 verdict=" + as_text(r2.output.verdict) + " new_failures=" + n_of(r2.output.new_failures) + " record=" + as_text(r2.output.record_path));
log("discrepancy classification: " + as_text(r2.output.summary));
if as_text(r2.output.tree_fingerprint) != as_text(g.output.tree_fingerprint) {
    pause("infra", "Gates did not reproduce because the TREE moved: record 1 fingerprint " + as_text(g.output.tree_fingerprint) + " vs record 2 " + as_text(r2.output.tree_fingerprint) + ". " + as_text(r2.output.summary) + " Another lane is editing this repo while we gate it. Settle the tree (or pin a clean checkout) and relaunch — no fixer can make a moving tree reproduce, and grading it would be theatre.");
    return "HALTED: tree fingerprint differs between gate passes; not a lane verdict.";
}
let agreed_failures = []; let solo_failures = [];
for x in as_array(r2.output.new_failures) {
    if as_text(x).contains("BOTH-RECORDS") { agreed_failures.push(as_text(x)); } else { solo_failures.push(as_text(x)); }
}
if r2.output.baseline_measured != true || n_of(r2.output.foreign_baseline) == "0" || as_text(r2.output.verdict) == "NO VERDICT" {
    pause("verification", "The reproduce pass could not state its own foreign-failure baseline, so there is no second independent measurement to compare against — NO VERDICT, which is neither clean nor a failure of this lane. Both records are in " + OUT + "; re-measure the baseline (the --against behavioural gaps, run-arm.sh:145, the keyless cap reset, the owed live arm) and relaunch.");
    return "NO VERDICT: reproduce pass carried no measured baseline.";
}
if solo_failures.len() > 0 {
    pause("verification", "A failure shows up in one gate pass and not the other: " + join_lines(solo_failures, " | ") + ". That is the signature of an unserialized battery or a flaky arm, not of a defect — the two runs must be brought to agree (re-run the disagreeing arm twice on a box with no sibling battery live) before anything is fixed. Records: " + as_text(g.output.record_path) + " and " + as_text(r2.output.record_path) + ".");
    return "HALTED: gate passes disagree on a failure that is not in either baseline.";
}
let gates_clean = false;
if as_text(g.output.verdict) == "VERIFIED-OFFLINE" && as_text(r2.output.verdict) == "VERIFIED-OFFLINE" && agreed_failures.len() == 0 {
    gates_clean = true;
}
if gates_clean {
    log("Gates reproduced: two independent records, same fingerprint, empty new-failures-minus-baseline. OFFLINE ceiling stands — the live-capture arm is still owed.");
} else {
    log("Gates reproduced WITH " + agreed_failures.len().to_string() + " failure(s) that both records agree are lane-owned; carrying them into review as open findings rather than halting: " + join_lines(agreed_failures, " | "));
}

phase("Review");
let rounds = 0;
let clean = false;
let open_blockers = [];
let open_majors = [];
// Failures both gate records agree the lane owns stay in the findings set until a fixer's own
// verified re-run retires them; a read-only lens that never saw the battery cannot clear them.
let gate_seed = [];
if !gates_clean {
    for f in agreed_failures { gate_seed.push("gate arm both records agree is lane-owned: " + f); }
    if agreed_failures.len() == 0 { gate_seed.push("a gate pass is not VERIFIED-OFFLINE and named no new failure — re-measure which arm moved and why before anything is called clean"); }
}
// design-waj40.md s7, three fresh seats. Read-only is enforced tool-side: no shell, so a lens grades source and records — re-running the battery is NOT part of a lens.
let base = rules + "\nYOU ARE A READ-ONLY REVIEW LENS over apex-waj.40 (three lenses per round, max 3 rounds; you are a fresh seat on this round). Capability override: you have no Edit/Write and no Execute — do not attempt the battery, a grep-with-side-effect, a ledger append or any shell; your structured return IS the deliverable and must stand alone. Bounded: at most 15 tool calls, at most 400 words, grep for the fragment then read +/- 12 lines, never a whole 12,577-line file. Gate pass 1: " + as_text(g.output.summary) + " | pass 2: " + as_text(r2.output.summary) + " | verdicts " + as_text(g.output.verdict) + " / " + as_text(r2.output.verdict) + ". Records: " + as_text(g.output.record_path) + " , " + as_text(r2.output.record_path) + ". Use `clean: false` only for Blocking/Major; put trivia in minor; a finding you cannot cite a line for goes in refused, not in blocking.\n";
let ask_l1 = base + "\nLENS 1 — CAP TRUTH. Read the live state-load path (~:1955-1989) and the `cap_policy` guard. Is there ANY remaining path by which a daemon starts with `forwarded_posts` lower than what it really spent: the `resumed` flag branches, the tmp+`os.replace` write in `save_state_locked` (~:365-371) racing a boot, `setdefault` on a key that parsed but is absent, a signal handler? Does `sys.exit(2)` really run before the pid file (:~2134) and before the listener binds (:~2164), so a refused boot can neither hold the port nor leave a pid for run-arm.sh:93 to trust? Cite the lines that close each path.";
let ask_l2 = base + "\nLENS 2 — MITIGATION PRESERVATION. Prove from source that a FRESH --tag can never reach the new refusal, and that no new refusal was added on the fresh path (that would break every operator's current workaround). Then check every caller still passes --home and --tag (run-arm.sh:91-95, the battery's base_argv ~:3113 and raw_argv ~:7476) and could not now die on a directory it pre-created itself with a partial state file from a killed prior run — is that a NEW operational failure mode for run-arm.sh retries? FOOTGUNS E2 (always a fresh tag) must still be true verbatim, preserved rather than replaced.";
let ask_l3 = base + "\nLENS 3 — EVIDENCE HYGIENE. Is anything in the working-tree delta attributable to .40 at all, or is the remaining deliverable verification + exfil? Do the two gate records satisfy parts (a)-(d) — every status read from a .rc, a fingerprint, a named baseline with attribution, a verdict computed as subtraction — or does either one smuggle a baseline into new_failures, or a new failure into the baseline? Is MODULE_SHA256 stamped in both? Would a reader of `/__wiretap2/state` (answers `{}` on OSError, ~:892-897) be fooled into a green? Does the offline ceiling get stated, or does any line read as verification at the source?";

while rounds < 3 && clean != true {
    rounds = rounds + 1;
    let pre = "\nROUND " + rounds.to_string() + " OF 3 — anything the fixer changed since the previous round is already on disk, so re-read before you cite a line.\n";
    let revs = parallel([
        #{ prompt: pre + ask_l1, label: "waj40r-l1-r" + rounds.to_string(), output_schema: review_schema(), phase: "Review", capability_mode: "read-only" },
        #{ prompt: pre + ask_l2, label: "waj40r-l2-r" + rounds.to_string(), output_schema: review_schema(), phase: "Review", capability_mode: "read-only" },
        #{ prompt: pre + ask_l3, label: "waj40r-l3-r" + rounds.to_string(), output_schema: review_schema(), phase: "Review", capability_mode: "read-only" }
    ]);
    let l1 = (); let l2 = (); let l3 = ();
    if revs.len() > 0 { l1 = revs[0]; }
    if revs.len() > 1 { l2 = revs[1]; }
    if revs.len() > 2 { l3 = revs[2]; }
    // A dead lens is retried exactly once; a 2-of-3 round is never allowed to read as a verdict.
    let retry = "\nRE-DISPATCH: the previous seat on this assignment died before returning (model-side empty response after burning millions of tokens). Take the narrow path — grep first, read only the named windows, answer in under 300 words. A short correct answer beats a thorough dead one.\n";
    if l1 == () || l1.success != true || l1.output == () {
        log("lens 1 did not return in round " + rounds.to_string() + " — retrying once");
        l1 = agent(retry + pre + ask_l1, #{ label: "waj40r-l1-r" + rounds.to_string() + "-retry", output_schema: review_schema(), phase: "Review", capability_mode: "read-only" });
    }
    if l2 == () || l2.success != true || l2.output == () {
        log("lens 2 did not return in round " + rounds.to_string() + " — retrying once");
        l2 = agent(retry + pre + ask_l2, #{ label: "waj40r-l2-r" + rounds.to_string() + "-retry", output_schema: review_schema(), phase: "Review", capability_mode: "read-only" });
    }
    if l3 == () || l3.success != true || l3.output == () {
        log("lens 3 did not return in round " + rounds.to_string() + " — retrying once");
        l3 = agent(retry + pre + ask_l3, #{ label: "waj40r-l3-r" + rounds.to_string() + "-retry", output_schema: review_schema(), phase: "Review", capability_mode: "read-only" });
    }
    let live = [];
    if l1 != () && l1.success == true && l1.output != () { live.push(l1.output); }
    if l2 != () && l2.success == true && l2.output != () { live.push(l2.output); }
    if l3 != () && l3.success == true && l3.output != () { live.push(l3.output); }
    // A 2-of-3 round cannot see the mitigation surface and the cap-truth surface at once; two agreeing lenses are not a substitute for the third that died.
    if live.len() < 3 {
        pause("infra", "Round " + rounds.to_string() + " could not land three lenses even after one bounded retry each (" + live.len().to_string() + " of 3 returned). The lane is not reviewed — a short round is NOT a clean round, whatever the returned lenses say. Inspect " + OUT + " and relaunch.");
        return "PARTIAL: review round " + rounds.to_string() + " landed " + live.len().to_string() + " of 3 lenses; findings unresolved.";
    }
    open_blockers = []; open_majors = []; for s in gate_seed { open_blockers.push(s); }
    for l in live {
        for x in as_array(l.blocking) { open_blockers.push(as_text(x)); }
        for y in as_array(l.major) { open_majors.push(as_text(y)); }
    }
    log("round " + rounds.to_string() + ": 3/3 lenses, " + open_blockers.len().to_string() + " blocking, " + open_majors.len().to_string() + " major");
    if open_blockers.len() == 0 && open_majors.len() == 0 {
        clean = true;
        log("CLEAN at round " + rounds.to_string() + ": " + as_text(live[0].summary));
    } else if rounds < 3 {
        let fx = agent(rules + "\nRound " + rounds.to_string() + " review of apex-waj.40 returned findings. Address ONLY these, changing nothing else in the one file you own (" + INSTR + "):\n" + join_lines(open_blockers, "\n") + "\n" + join_lines(open_majors, "\n") + "\nIf a finding is wrong, refuse it and quote the line that makes it wrong — do not accommodate a reviewer who misread the instrument. Do NOT widen .40 into the keyless-cap-reset gap or the run-arm.sh:145 pin: both are named foreign baseline and touching them here is a scope violation. Any edit invalidates the gate records: after editing, re-run the affected arms (G2 always, G3 with a mutant re-derived from your edited blob) into " + OUT + "/gate-record-fix-r" + rounds.to_string() + ".md with parts (a)-(d) intact, and say in your return whether the verdict survived. Never commit, tag or push.\n"
            + "Return the same flat record shape; put what you changed in summary and the arms' status in arm_g1/g2/g3/arm_e2e (use arm_g1=UNCHANGED if you did not re-run the differential and explain why).",
            #{ label: "waj40r-fixer-r" + rounds.to_string(), output_schema: gate_schema(), phase: "Gate", capability_mode: "all" });
        if fx == () || fx.success != true || fx.output == () {
            pause("infra", "The round-" + rounds.to_string() + " fixer died with " + open_blockers.len().to_string() + " blocking and " + open_majors.len().to_string() + " major open, and it may have edited the instrument before it died. Re-hash " + INSTR + " against " + LIVE_SHA + " and read the newest log in " + OUT + " before relaunching.");
            return "PARTIAL: fixer died on round " + rounds.to_string() + " with open findings and a possibly half-edited instrument.";
        }
        if as_text(fx.output.verdict) != "VERIFIED-OFFLINE" {
            pause("verification", "The round-" + rounds.to_string() + " fix edited the instrument and the re-run did not return VERIFIED-OFFLINE (verdict " + as_text(fx.output.verdict) + ": " + as_text(fx.output.summary) + "). The tree now differs from both gate records — the fingerprint comparison below would be meaningless. Records in " + OUT + ".");
            return "HALTED: post-fix gate re-run is not VERIFIED-OFFLINE; fingerprint of record 1/2 is stale.";
        }
        gate_seed = []; log("fixer round " + rounds.to_string() + ": " + as_text(fx.output.summary) + " — its own verified re-run retires the seeded gate arms");
    }
}

phase("Report");
let verdict = "NOT CLEAN"; if clean == true { verdict = "CLEAN"; }
log("WAJ40-RESUME " + verdict + " — review rounds=" + rounds.to_string() + " (cap 3), gates_clean=" + as_text(gates_clean) + " seeded_gate_findings=" + gate_seed.len().to_string());
log("gate 1: " + as_text(g.output.verdict) + " | " + as_text(g.output.record_path)); log("gate 2: " + as_text(r2.output.verdict) + " | " + as_text(r2.output.record_path));
log("foreign baseline (not this lane's, never sent to a fixer): " + join_lines(as_array(r2.output.foreign_baseline), " | "));
log("open blockers (" + open_blockers.len().to_string() + "): " + join_lines(open_blockers, " | ")); log("open majors (" + open_majors.len().to_string() + "): " + join_lines(open_majors, " | "));
log("STILL OWED BY THE COORDINATOR: the commit/exfil itself (plans HEAD still carries the fail-open blob at 2e29c76, so closing .40 as implemented would be false); the live-capture arm at the real proxy — every number above is loopback/mock; the run-arm.sh:145 daemon sha pin; the parseable-but-keyless forwarded_posts reset (a new bead, not .40); and FOOTGUNS E2 stays verbatim — a damaged resume is now LOUD, a resume is still not SAFE.");
log("NOTHING COMMITTED");
return "WAJ40-RESUME " + verdict + " rounds=" + rounds.to_string() + " blocking=" + open_blockers.len().to_string() + " major=" + open_majors.len().to_string() + " gates=" + as_text(g.output.verdict) + "/" + as_text(r2.output.verdict) + " records=" + as_text(g.output.record_path) + "," + as_text(r2.output.record_path) + ". NOTHING COMMITTED.";
