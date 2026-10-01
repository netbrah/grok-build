let meta = #{
    name: "hts-waj2-verify",
    description: "Close the one verification gap the interrupted round-3 fixer left on apex-waj.2: re-measure the lane's two scoped gate commands on the CURRENT bytes under the verification-record contract and re-fingerprint the tree, so the paused hts-waj2-resume-2 run has a tree it may legally gate. Writes records only; never edits product code.",
    when_to_use: "hts-waj2-resume-2 halted at Baseline because the two allowlisted files were rewritten at 12:43 local by a fixer pass that died mid-flight: the tree on disk (0d73590de460 / d3f008b35891) is not the tree Baseline/Red/Green/Mutant/Gate were measured against (45c6fb298cf6 / f6606cf67af5), and no green run exists for the current bytes. Run this instead of re-launching the whole resume driver, which would re-derive Red/Green/mutants that are already evidenced.",
    phases: [
        #{
            title: "Measure",
            detail: "One execute seat: re-fingerprint the tree, re-check the disk floor and the cargo lock, run the lane's two scoped gate commands on the current bytes with true exit statuses read back from files, attribute every failing name against the lane's foreign baseline, and write the re-verification record.",
        },
        #{
            title: "Reproduce",
            detail: "A fresh seat that may not read the record until it has its own numbers: repeats the two decisive commands into its own artifacts and diffs the name sets, classifying any discrepancy as TREE MOVED / BASELINE MOVED / ONLY-IN-RECORD-2 / BOTH-RECORDS.",
        },
        #{
            title: "Report",
            detail: "Coordinator-facing verdict only: LANDED-CURRENT-BYTES, LANE-RED, or NO VERDICT, plus whether the D1 mutant battery still has to be re-run.",
        },
    ],
};

// ---- lane constants (read-only work, shared dirty worktree) ----
let LANE = "/Users/palanisd/Projects/upstream/wt/apex-ayl-hosted-tool-search";
let WF = "/tmp/hts-wf";
let PROJ = "crates/codegen/xai-grok-sampling-types/src/conversation/projection.rs";
let CONV = "crates/codegen/xai-grok-sampling-types/src/conversation.rs";
let REC_DIR = "/Users/palanisd/Projects/upstream/grok/plans/harness/workflow-design/runs/waj2";
let DISK_FLOOR = 16000;

// The two commands are copied verbatim from the lane's own guarded sequence
// (/tmp/hts-wf/r3-seq2.zsh lines 43 and 68). Do not "improve" them: the review
// rounds graded the lane against exactly these.
let CMD1 = "cargo test --release -p xai-grok-sampling-types --lib conversation::projection_tests";
let CMD2 = "cargo test --release -p xai-grok-sampling-types --no-fail-fast";

let COMMON = "Working directory: " + LANE + " — a SHARED, already-dirty worktree owned by several lanes. "
    + "Hard rules: make NO edit to any file under crates/, Cargo.toml or Cargo.lock; run NO git write command "
    + "(no add/commit/stash/checkout/restore/clean); touch NO file another lane owns. "
    + "You may write only under " + WF + "/ and " + REC_DIR + "/. "
    + "Before every build run `df -m /Users/palanisd | tail -1` and require at least " + DISK_FLOOR + " MB free; "
    + "below the floor, poll every 20 s up to 40 polls, and if it never clears report SPACE_WAIT_TIMED_OUT rather than building. "
    + "Before every build run `pgrep -fl 'cargo|rustc' | grep -vE 'sem|weave-mcp'`; if a sibling build is live, poll-wait — NEVER kill a sibling cargo or rustc. "
    + "Serialise your own builds with `/usr/bin/lockf -k -s -t 5400 /tmp/hts-cargo.lock <cmd>` (advisory; use exactly that lockfile path). "
    + "Capture every exit status as `cmd > log 2>&1; echo \"REAL_EXIT=$?\" > tag.rc` and read the truth back from the .rc file — "
    + "never from the tool's own status and never from a pipeline's tail. "
    + "A run that dies in `ld`, with `linking with \\`cc\\` failed`, with SIGSEGV, or with `No space left on device` is a shared-target link artefact, not a test result: retry it up to 3 times and say so in the record. "
    + "CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_MIN_STACK=67108864 on every cargo invocation.";

fn rec_schema() {
    #{
        "type": "object",
        "properties": #{
            "cmd1_rc": #{ "type": "string", "description": "REAL_EXIT read back from the .rc file, or SPACE_WAIT_TIMED_OUT / LINK_ARTIFACT_ABANDONED" },
            "cmd2_rc": #{ "type": "string" },
            "fp_before": #{ "type": "string", "description": "sha256:12 of projection.rs and conversation.rs as found, plus the full porcelain dirty count" },
            "fp_after": #{ "type": "string", "description": "same two sha256:12 values re-measured after the last build" },
            "free_mb_start": #{ "type": "string" },
            "free_mb_end": #{ "type": "string" },
            "failing_names": #{ "type": "array", "items": #{ "type": "string" }, "description": "every failing test name observed, verbatim" },
            "attributed": #{ "type": "array", "items": #{ "type": "string" }, "description": "each failing name plus its attribution: lane / foreign-with-owner / unattributed" },
            "verdict": #{ "type": "string", "enum": ["lane-clean", "lane-red", "no-verdict"] },
            "record_path": #{ "type": "string", "description": "absolute path of the verification record just written" },
            "notes": #{ "type": "string" },
        },
        "required": ["cmd1_rc", "cmd2_rc", "fp_after", "verdict", "record_path"],
    }
}

phase("Measure");
log("Re-measuring apex-waj.2's two scoped gates on the bytes the interrupted fixer left on disk.");

let measure_prompt = "You are the VERIFY seat for apex-waj.2 (projector discovery arm). The lane's round-3 fixer was killed mid-flight and left code on disk with no green run against it. "
    + "Your only job is to supply the missing verification link so the paused gate run can proceed — you are not here to fix anything.\n\n"
    + COMMON + "\n\n"
    + "Do this, in order:\n"
    + "1. Read " + WF + "/r3-record-body.md (the (a)/(b) adjudication sections and its BEFORE/AFTER fingerprint blocks) and "
    + WF + "/r3-fingerprint-before.txt and " + WF + "/r3-fingerprint-after.txt, plus REPORT-waj2-gates.md in the same lane tree if present. "
    + "Record what BEFORE claims (45c6fb298cf6 projection / f6606cf67af5 conversation) and what is actually on disk now.\n"
    + "2. Re-fingerprint NOW: sha256 of " + PROJ + " and " + CONV + " (keep the 12-hex prefix) and the full `git status --porcelain` line count, into " + WF + "/rv-fingerprint-before.txt.\n"
    + "3. Run " + CMD1 + " as tag rv-cmd1, then " + CMD2 + " as tag rv-cmd2, under the disk floor, the sibling check and the lockf above. Logs and .rc files go under " + WF + "/.\n"
    + "4. Re-fingerprint AFTER the last build into " + WF + "/rv-fingerprint-after.txt. If the two file hashes moved during your own run, say so — that means a sibling is editing the lane and the verdict must be no-verdict.\n"
    + "5. Collect EVERY failing test name from both logs. Attribute each one: lane (lives in the two allowlisted files or in conversation::projection*), foreign-with-owner (name it and the owner you verified), or unattributed. "
    + "Use the lane's recorded foreign baseline as a HYPOTHESIS TO RE-MEASURE, never as an answer to copy.\n"
    + "6. Write " + REC_DIR + "/reverify-2026-09-30.md with: (a) every command line plus its REAL_EXIT read from the .rc file, (b) the before/after fingerprints and free-MB at both ends, (c) the full failing-name set with attribution for each, (d) the verdict computed as new-failures-minus-baseline: lane-clean only if both gates are REAL_EXIT=0 or every failure is attributed foreign; lane-red if any failure is the lane's; otherwise no-verdict. "
    + "A baseline you could not measure, or any unattributed name, forces no-verdict — never report clean to be helpful.\n"
    + "7. Note explicitly, as a one-line recommendation, whether the sealed mutant deaths (MUT-D1b..D3a) still stand given the `{ord}` change in the current bytes, or whether the D1 battery must be re-run before exfil.\n\n"
    + "Return the structured result. Do not paste the whole record into your reply — the path is enough.";

let m = agent(measure_prompt, #{
    label: "waj2v-measure",
    phase: "Measure",
    capability_mode: "execute",
    output_schema: rec_schema(),
});

let m_ok = m != () && m["verdict"] != ();
if !m_ok {
    pause("verification", "The Measure seat returned nothing usable. The lane tree is still unfingerprinted against the current bytes — inspect " + WF + "/rv-*.rc and relaunch, do not resume the gate run.");
}

let cmd1_rc = m["cmd1_rc"];
let cmd2_rc = m["cmd2_rc"];
log("Measure: cmd1=" + cmd1_rc + " cmd2=" + cmd2_rc + " verdict=" + m["verdict"]);

phase("Reproduce");

let repro_prompt = "You are the independent REPRODUCE seat for apex-waj.2. A prior seat wrote a verification record at " + m["record_path"] + ".\n"
    + "Protocol: run the two decisive commands FIRST with your own artifact tags rv2-cmd1 and rv2-cmd2, and only open that record after both .rc files exist. "
    + "It must not influence your numbers.\n\n"
    + COMMON + "\n\n"
    + "Commands, verbatim:\n  " + CMD1 + "\n  " + CMD2 + "\n\n"
    + "Then diff your failing-name set against the record's. Classify the discrepancy as exactly one of: "
    + "TREE MOVED (the two file hashes differ between the record's AFTER and your BEFORE), "
    + "BASELINE MOVED (the foreign set differs), "
    + "ONLY-IN-RECORD-2 (a name you saw that the record missed — usually an unserialised sibling build), "
    + "or BOTH-RECORDS (same names both sides). "
    + "Write " + REC_DIR + "/reverify-2026-09-30-repro.md with your commands, REAL_EXITs, fingerprints and that classification. "
    + "Return the structured result with verdict = agreement / tree_moved / baseline_moved / only_in_record_2 / both_records.";

let r = agent(repro_prompt, #{
    label: "waj2v-repro",
    phase: "Reproduce",
    capability_mode: "execute",
    output_schema: #{
        "type": "object",
        "properties": #{
            "cmd1_rc": #{ "type": "string" },
            "cmd2_rc": #{ "type": "string" },
            "fp_before": #{ "type": "string" },
            "classification": #{ "type": "string", "enum": ["agreement", "tree_moved", "baseline_moved", "only_in_record_2", "both_records"] },
            "record_path": #{ "type": "string" },
            "notes": #{ "type": "string" },
        },
        "required": ["classification", "cmd1_rc", "cmd2_rc"],
    },
});

let r_ok = r != () && r["classification"] != ();
if !r_ok {
    pause("verification", "The Reproduce seat returned nothing usable. The verification record at " + m["record_path"] + " is single-witness: the gate run must not be resumed on it.");
}

phase("Report");

let clean = m["verdict"] == "lane-clean" && (r["classification"] == "agreement" || r["classification"] == "both_records");

let out = "apex-waj.2 — VERIFICATION GAP CLOSED?\n";
out = out + "ON-DISK NOW (Measure BEFORE): " + m["fp_before"] + "\n";
out = out + "AFTER MEASURE BUILD: " + m["fp_after"] + "\n";
out = out + "GATES: cmd1 REAL_EXIT=" + cmd1_rc + "  cmd2 REAL_EXIT=" + cmd2_rc
    + "   |  repro cmd1=" + r["cmd1_rc"] + " cmd2=" + r["cmd2_rc"] + "\n";
out = out + "FREE MB: start " + m["free_mb_start"] + "  end " + m["free_mb_end"] + " (floor " + DISK_FLOOR + ")\n";
out = out + "MEASURE VERDICT: " + m["verdict"] + "   REPRO CLASSIFICATION: " + r["classification"] + "\n";
out = out + "RECORDS: " + m["record_path"] + "  " + r["record_path"] + "\n";
if m["notes"] != () {
    out = out + "MEASURE NOTES: " + m["notes"] + "\n";
}
if clean {
    out = out + "VERDICT: LANDED-CURRENT-BYTES — resume the paused hts-waj2-resume-2 run; its Baseline halt is satisfied by these two records.\n";
    out = out + "STILL OWED BEFORE EXFIL: the coordinator's decision on the D1 mutant battery (see MEASURE NOTES) and the full 6-package GATE.\n";
} else {
    out = out + "VERDICT: NOT CLEAN — do NOT resume the paused gate run. ";
    if m["verdict"] == "lane-red" {
        out = out + "The lane's own code fails its gates on these bytes: this needs a fresh fix round, not a gate.\n";
    } else {
        out = out + "The gap is verification/baseline, not code: re-measure per the classification above before any gate is run.\n";
    }
}
out;
