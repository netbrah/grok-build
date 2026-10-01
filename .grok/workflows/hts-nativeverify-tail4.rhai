let meta = #{
    name: "hts-nativeverify-tail4",
    description: "Put the six workflow-design native drafts (3 132 lines, written 2026-09-29 against a pinned HEAD) through independent verification: one fresh read-only seat per draft re-checks its load-bearing claims against the CURRENT harness source and user guide, a fixer applies only the findings the coordinator can uphold first-hand, and a FRESH round re-verifies until a full round has zero Blocking and zero Major. Never touches product code.",
    when_to_use: "The drafts/native-*.md and designs/OVERWATCH-NATIVE.md set was authored in one day against `wt/grok-build-responses` at HEAD 6f520df3 and has never been re-checked. Before any of it becomes a design decision, a D-nn ruling, or an installed skill, each citation and each 'the code does X' claim needs an independent seat reading today's source. Also use after the harness moves (an upstream pull) and the corpus may have rotted.",
    phases: [
        #{
            title: "Round",
            detail: "Six fresh read-only seats in parallel, one per document. Each resolves the document's own pointers against today's source and user guide, re-reads the code behind its load-bearing claims, and returns findings with severity plus the evidence it actually saw — never a summary of the document.",
        },
        #{
            title: "Fix",
            detail: "One seat rewrites ONLY the passages the coordinator upheld, in place in the draft, with a dated correction note per change, and re-runs the citation-resolution audit on the touched files.",
        },
        #{
            title: "Report",
            detail: "Convergence state per document, the upheld-and-fixed list, the rejected list with reasons, and what remains owed before any of this becomes a decision or an installed skill.",
        },
    ],
};

// ---- corpus under verification -------------------------------------------------
// SRC is the alias every draft declares in its own preamble; the drafts cite the
// harness through it, so a verifier must resolve there and nowhere else.
let WD = "/Users/palanisd/Projects/upstream/grok/plans/harness/workflow-design";
let SRC = "/Users/palanisd/Projects/upstream/wt/grok-build-responses/crates/codegen";
let RUNS = WD + "/runs/native-verify";
let DOCS = "/Users/palanisd/.grok/docs/user-guide";
let MAX_ROUNDS = 3;

// Round 3 coverage is split, deliberately: hts-nativeverify-3 verified
// native-config-hooks-rules.md and native-skills-plugins.md before 4 of its 6 seats died on a
// proxy stream error (board ow-051). Those two seats' outputs are banked at
// runs/native-verify/round3-partial-2of6.md against the SAME corrected bytes, so this run carries
// only the four documents that have no seat yet. Re-verifying the two that already reported would
// re-spend ~8 M tokens for no new coverage.
let DOCS_LIST = [
    WD + "/drafts/native-seat-routing.md",
    WD + "/drafts/native-observability.md",
    WD + "/drafts/native-beads-vs-board.md",
    WD + "/designs/OVERWATCH-NATIVE.md",
];

// Labels are declared, not sliced out of the paths: this host's string surface was
// measured narrow (probe-*/probe-case runs on 2026-09-30) and a missing method aborts
// the whole script at runtime, invisible to validate_only (BEST-PRACTICES 57).
let DOC_TAGS = ["hooks-rules", "skills-plugins", "seat-routing", "observability", "beads-board", "overwatch"];

// Verbatim source pins the documents themselves declare. A verifier that cannot
// reproduce these is reading a different tree than the document argues from.
let PINS = "The drafts pin their evidence to worktree /Users/palanisd/Projects/upstream/wt/grok-build-responses, "
    + "branch feat/first-class-responses-catalog, HEAD 6f520df3, with `SRC/` aliased to "
    + SRC + ". First action, before reading any claim: run `git -C /Users/palanisd/Projects/upstream/wt/grok-build-responses "
    + "log --oneline -3` and `git -C … status --porcelain | wc -l`, and state in your findings whether the tree has moved "
    + "since the draft was written. Where it has moved, a citation that no longer resolves is only a finding if the CLAIM "
    + "it supports is now false — drift in a line number is a Minor, a claim the code contradicts is Blocking.";

let RULES = "You are READ-ONLY. Create nothing, edit nothing, delete nothing, run no git write command, no cargo, no build. "
    + "Do not quote key material: before quoting any log or config run `grep -rEil 'sk-[A0-9a-z]{16,}'` over what you are about to quote and expect no hits; if a hit appears, describe the file instead of quoting it. "
    + "Report ONLY what you verified with your own eyes in this session, each finding carrying path:line and the one-line quote that decides it. "
    + "If you could not check something, say UNVERIFIED rather than guessing — an invented finding costs more than a missed one. "
    + "Severity: Blocking = the document argues for something the source contradicts, or a load-bearing citation resolves to nothing; "
    + "Major = a claim is supportable only with a qualification the document does not state, or a proposed mechanism has a real defect; "
    + "Minor = stale line number, dead relative path, stale HEAD pin, wording. "
    + "Cap yourself at your 8 most consequential findings; a long list of Minors buries the one Blocking.";

// agent() yields a wrapper; a seat counts as usable only if it succeeded AND carried a
// structured map under `output`. Kept as a function so every gate in this driver asks the
// same question the same way.
// `for x in ()` is a runtime abort on this host, and a schema-valid-but-empty seat payload
// is exactly what the canned host returns, so normalise before iterating. The dry run caught
// this at line 168; the real round-1 run caught the mirror image of it.
fn arr(v) {
    if type_of(v) == "array" { v; }
    else { []; }
}

fn usable(res) {
    if res == () { false; }
    else if type_of(res) != "map" { false; }
    else if res["success"] != true { false; }
    else { res["output"] != () && type_of(res["output"]) == "map"; }
}

fn findings_schema() {
    #{
        "type": "object",
        "properties": #{
            "doc": #{ "type": "string", "description": "absolute path of the document you verified" },
            "tree_state": #{ "type": "string", "description": "HEAD + dirty count you measured, and whether it moved vs the draft's pin" },
            "claims_checked": #{ "type": "integer", "description": "how many load-bearing claims you actually re-read source for" },
            "findings": #{
                "type": "array",
                "items": #{
                    "type": "object",
                    "properties": #{
                        "severity": #{ "type": "string", "enum": ["blocking", "major", "minor", "unverified"] },
                        "claim": #{ "type": "string", "description": "the document's claim, in a short quote plus its line" },
                        "evidence": #{ "type": "string", "description": "path:line you read plus the one-line quote that decides it" },
                        "fix": #{ "type": "string", "description": "the concrete edit the fixer should make" },
                    },
                    "required": ["severity", "claim", "evidence", "fix"],
                },
            },
            "still_sound": #{ "type": "array", "items": #{ "type": "string" }, "description": "the load-bearing claims you confirmed TRUE, with the path:line that confirms each" },
        },
        "required": ["doc", "tree_state", "findings", "still_sound"],
        // findings/still_sound are iterated through arr(); required keeps a seat from returning an empty map.
    }
}

// `fn` bodies cannot see workflow-level `let` bindings in this host, so every constant
// a prompt needs is threaded in as a parameter (BEST-PRACTICES 57; the dry-run host
// catches this class, validate_only's stub path does not always).
fn verify_prompt(doc, round_no, pins, docs_dir, wd, runs, rules) {
    // Round 2+ must not re-derive round 1: the corrections are themselves claims, so the
    // highest-value check is whether each stamped edit is true against source.
    let focus = "";
    if round_no > 1 {
        focus = "ROUND " + round_no + " FOCUS: this corpus was already corrected once. Grep the file for "
            + "`[CORRECTED 2026-09-30` and treat EVERY stamp as a claim to falsify — re-open the source it "
            + "cites and confirm the replacement text is right; a wrong correction is a fresh Blocking, worse "
            + "than the original error because it now reads like verified material. Then continue with "
            + "load-bearing claims you have not yet seen, and report Minors only if they would mislead a "
            + "reader into acting wrongly.\n\n";
    }
    return "Verify one design document against the harness as it exists TODAY. Round " + round_no + ".\n\n"
        + focus
        + "Document: " + doc + "\n"
        + "Its evidence base: " + pins + "\n"
        + "The user guide is at " + docs_dir + " (05-configuration, 08-skills, 09-plugins, 10-hooks, 12-project-rules, 14-headless-mode, 16-subagents, 17-sessions, 18-sandbox are the ones these drafts lean on).\n"
        + "A prior coordinator pass left a citation-resolution measurement at " + runs + "/2026-09-30-resolution.txt — read YOUR document's rows and treat them as hints to check, not as verdicts: an unresolved pointer is often a bare continuation filename the drafts use legitimately, and a resolved one proves nothing about the claim.\n\n"
        + "Method: (1) list the document's load-bearing claims — the ones its recommendations actually depend on, not its background prose; "
        + "(2) for each, go to the source or the user guide and read the code, then record whether today's source still says it; "
        + "(3) for any claim about what a mechanism DOES, prefer the strongest available evidence: the code path, then a test that pins it, then the docs — a doc that contradicts the code is itself a finding; "
        + "(4) where the document proposes something that does not exist yet, check the claim about the HOOK point it would attach to, and say plainly if the seam it needs is absent.\n\n"
        + rules;
}

fn fix_schema() {
    #{
        "type": "object",
        "properties": #{
            "applied": #{ "type": "array", "items": #{ "type": "string" }, "description": "each upheld finding you edited, as 'doc:line — what you changed'" },
            "skipped": #{ "type": "array", "items": #{ "type": "string" }, "description": "findings you did NOT apply and why (refused / already true / needs the coordinator)" },
            "audit_after": #{ "type": "string", "description": "the citation-audit TOTAL row re-run over the touched documents" },
            "lines_changed": #{ "type": "string" },
        },
        "required": ["applied", "skipped"],
    }
}

let round_log = "";
let last_blocking = 0;
let last_major = 0;
let round_no = 1;
let any_fix = "";

while round_no <= MAX_ROUNDS {
    phase("Round " + round_no);
    log("Round " + round_no + ": six fresh read-only seats, one per native draft.");

    let seats = [];
    let i = 0;
    while i < DOCS_LIST.len() {
        seats.push(agent(verify_prompt(DOCS_LIST[i], round_no, PINS, DOCS, WD, RUNS, RULES), #{
            label: "rv" + round_no + "-" + DOC_TAGS[i],
            phase: "Round " + round_no,
            capability_mode: "read-only",
            output_schema: findings_schema(),
        }));
        i = i + 1;
    }

    // agent() returns a WRAPPER — {agent_id, success, output, cancelled, tokens_used,
    // duration_ms} — and the schema payload lives under `output`. Reading res["findings"]
    // off the wrapper silently yields () for every seat, so a fully successful round scores
    // as empty and the short-round guard pauses paid work. (hts-housefix.rhai:71-99 already
    // had this right; re-deriving it wrong here is the lesson, not the helper.)
    let got = [];
    for s in seats {
        if usable(s) {
            got.push(s["output"]);
        }
    }
    if got.len() < DOCS_LIST.len() {
        pause("verification", "Round " + round_no + " returned " + got.len() + " of " + DOCS_LIST.len() + " seats. A short round is not a clean round: the missing documents are unverified. Inspect and relaunch rather than advancing.");
    }

    let blocking = [];
    let major = [];
    let minor = [];
    for s in got {
        for f in arr(s["findings"]) {
            let tagged = s["doc"] + " :: " + f["severity"] + " :: " + f["claim"] + " || EVIDENCE " + f["evidence"] + " || FIX " + f["fix"];
            if f["severity"] == "blocking" {
                blocking.push(tagged);
            } else if f["severity"] == "major" {
                major.push(tagged);
            } else {
                minor.push(tagged);
            }
        }
    }
    last_blocking = blocking.len();
    last_major = major.len();

    let rtext = "## Round " + round_no + " — " + got.len() + " seats, blocking=" + last_blocking + " major=" + last_major + " minor=" + minor.len() + "\n\n";
    for b in blocking {
        rtext = rtext + "- BLOCKING " + b + "\n";
    }
    for m in major {
        rtext = rtext + "- MAJOR " + m + "\n";
    }
    for m in minor {
        rtext = rtext + "- minor " + m + "\n";
    }
    for s in got {
        rtext = rtext + "\n### Confirmed sound in " + s["doc"] + " (tree: " + s["tree_state"] + ")\n";
        let sounds = arr(s["still_sound"]);
        let k = 0;
        while k < sounds.len() && k < 12 {
            rtext = rtext + "- " + sounds[k] + "\n";
            k = k + 1;
        }
    }
    round_log = round_log + rtext;
    log("Round " + round_no + " verdict: blocking=" + last_blocking + " major=" + last_major + " minor=" + minor.len());

    if last_blocking == 0 && last_major == 0 {
        break;
    }
    if round_no == MAX_ROUNDS {
        break;
    }

    phase("Fix " + round_no);
    let fix_prompt = "Apply the coordinator's upheld findings to the native design drafts. You may edit ONLY these files, and only the passages named:\n"
        + WD + "/drafts/native-seat-routing.md\n" + WD + "/drafts/native-observability.md\n"
        + WD + "/drafts/native-beads-vs-board.md\n" + WD + "/designs/OVERWATCH-NATIVE.md\n\n"
        + "Findings, in severity order (fix every BLOCKING and MAJOR; take the MINORs where the edit is one line):\n"
        + round_log + "\n"
        + "Rules: these are planning documents, so in-place correction is legitimate — but never silently. Precede each corrected passage with a dated note in the form "
        + "`**[CORRECTED 2026-09-30, round N: <what was wrong and what the source now says>]**` so a reader can see the corpus converge, and update the document's own state pin if the tree moved. "
        + "Do NOT soften a claim into vagueness to make a finding go away, and do NOT delete a section to dodge it — fix the claim or mark it **[UNVERIFIED]** with the reason. "
        + "You may read source and run the audit script `/tmp/draft_pointer_audit2.py` (read-only) but you may not touch anything outside the six files above; no git commands at all — the coordinator commits.\n"
        + "After editing, re-run the audit over the touched documents and paste its TOTAL row.";
    let fx = agent(fix_prompt, #{
        label: "rv" + round_no + "-fix",
        phase: "Fix " + round_no,
        capability_mode: "read-write",
        output_schema: fix_schema(),
    });
    if usable(fx) {
        any_fix = any_fix + "\n### Fixes after round " + round_no + "\n";
        let fxo = fx["output"];
        if fxo["applied"] != () {
            for a in arr(fxo["applied"]) {
                any_fix = any_fix + "- " + a + "\n";
            }
        }
        if fxo["skipped"] != () {
            for sk in arr(fxo["skipped"]) {
                any_fix = any_fix + "- SKIPPED " + sk + "\n";
            }
        }
        if fxo["audit_after"] != () {
            any_fix = any_fix + "- audit after: " + fxo["audit_after"] + "\n";
        }
    }
    round_no = round_no + 1;
}

phase("Report");
let verdict_word = "NOT CONVERGED";
if last_blocking == 0 && last_major == 0 {
    verdict_word = "CONVERGED";
}
let out = "Native-design verification — " + verdict_word + " after " + round_no + " round(s)\n";
out = out + "Corpus: 4 documents this round (2 already covered in round 3), " + WD + "/{drafts/native-*.md, designs/OVERWATCH-NATIVE.md}\n";
out = out + "Final round tallies: blocking=" + last_blocking + " major=" + last_major + "\n\n";
out = out + round_log + any_fix + "\n";
out = out + "OWED AFTER THIS RUN (coordinator, not this driver):\n";
out = out + "- Adjudicate each finding first-hand before it is treated as upheld — this driver COLLECTS findings, it does not rule on them.\n";
out = out + "- Pathspec-limited commit of the corrected drafts (plans has no remote, so this is a checkpoint) + a REGISTER row for this verification pass.\n";
out = out + "- The headless instrumented-run arm: record which hooks fire and in what order, which no static read can establish. That needs its own driver against the live harness, not more document reading.\n";
out = out + "- Only then may DECISIONS.md D-nn rulings and any installed skill cite these drafts.\n";
out;
