let meta = #{
    name: "hts-evidlint",
    description: "Two generator fixes that each cost a lane today: (1) a seat-retry helper so a provider error mid-phase does not stop a run, restricted to read-only and measure seats; (2) make assemble.zsh's record-under-/tmp lint test the PROPERTY instead of eight spellings its author enumerated.",
    when_to_use: "Board row ow-031. hts-evidroot lost a 4.2M-token implement seat to `empty response from model (reasoning_only)` with the change already on disk, because nothing re-spawns a dead seat. The coordinator then falsified the new lint by injecting one real shape (`let OUT_BAD = \"/tmp/hts-wf/\" + slug + \"/REPORT-injected.md\";`) which assembled rc=0 while the lint's selftest reported 8/8. Two items, one at a time, review loop after each. Does not commit.",
    phases: [
        #{ title: "Item1-Retry", detail: "add seat_once()/seat_retry() to scaffold-helpers, adopted only where re-running cannot double-apply an edit" },
        #{ title: "Review1", detail: "two fresh read-only lenses; loop to zero Blocking / zero Major" },
        #{ title: "Item2-Lint", detail: "generalise record_scan; the coordinator's injection becomes a permanent selftest case" },
        #{ title: "Review2", detail: "two fresh read-only lenses; loop to zero Blocking / zero Major" },
    ],
};

let PLANS = "/Users/palanisd/Projects/upstream/grok/plans";
let SC = PLANS + "/harness/workflow-design/scaffold";
let WF = PLANS + "/harness/workflow-design";
let OUT = WF + "/runs/evidlint";
let HOME_WF = "/Users/palanisd/.grok/workflows";
let SLUGS = ["6c6", "pwr", "waj2", "x2n", "waj40", "waj33"];

fn as_text(v) {
    if type_of(v) == "string" {
        return v;
    }
    return v.to_string();
}

fn payload(res) {
    if type_of(res) == "map" && res.contains("output") {
        return res.output;
    }
    return res;
}

fn field(obj, name) {
    if type_of(obj) != "map" || !obj.contains(name) {
        return "";
    }
    return as_text(obj[name]);
}

fn int_field(obj, name) {
    if type_of(obj) != "map" || !obj.contains(name) {
        return 0;
    }
    if type_of(obj[name]) == "int" {
        return obj[name];
    }
    return 0;
}

let COMMON = "REPOS. Generator: " + SC + " (snippets cat together by assemble.zsh into six drivers in " + HOME_WF + "). The generated drivers write lane records under " + WF + "/runs/<slug>/ and keep scratch in /tmp, deliberately (DECISIONS D-33). Plans is a git repo with NO remote; the code worktree is a separate dirty repo you must not touch.\n\n"
    + "RULES. Never `git add -A`, never commit, never tag or push. Do not edit " + SC + "/scaffold-phases.snippet.rhai — assemble.zsh does not cat it, so an edit there looks applied and changes nothing. Re-run `zsh " + SC + "/assemble.zsh` after editing any snippet, because the installed drivers are what the next lane actually runs. This Rhai host has no ternary operator, no `to_lowercase`, and its `trim()` / `make_upper()` mutate in place and return unit, so neither may appear in expression position; a top-level `let` bound to a map or array literal needs a trailing `;`; and a function body CANNOT see workflow-level `let` bindings, so thread every path in as a parameter. Capture exit statuses without a pipe between the command and `$?`.\n"
    + "BUDGET. Your read surface is bounded: open only the files this task names, and quote the lines you rely on instead of surveying the tree.\n\n";

// One review round: two independent read-only lenses, then one fix pass, up to
// max_rounds. Everything it needs arrives as a parameter because a function body
// cannot see workflow-level lets.
fn review_loop(tag, phase_name, item_desc, authorised, findings_file, max_rounds, fix_extra, common) {
    let rounds = 0;
    let clean = false;
    let summary = "no rounds run";
    let carry = "";
    while rounds < max_rounds && !clean {
        rounds += 1;
        let preface = "";
        if carry != "" {
            preface = "ROUND-OVER-ROUND. The previous round's findings and the fixes claimed against them are in " + findings_file + ". Re-open the bytes and check each fix actually landed; a fix you cannot see in the tree is the same finding again at the same severity.\n" + carry + "\n\n";
        }
        let intro = "You are a fresh, independent, READ-ONLY review lens (" + tag + " round " + rounds.to_string() + ") over a change to a Rhai driver generator. You did not write it and you cannot edit it. Grade only what is in the bytes; every finding needs a file:line and the quoted line. Zero findings is a valid and preferred answer over an invented one.\n\n"
            + preface + item_desc + "\n\n" + common
            + "AUTHORISED FILES (a change outside these is Blocking): " + authorised + "\n\n";
        let lensA = intro + "LENS A — DOES IT DO THE THING, AND ONLY THE THING. Follow the mechanism end to end in the REASSEMBLED driver, not the snippet: does the behaviour actually appear where a real lane will hit it, and is the claim in the comment or the log line true of the code? Then look for scope creep: edits to another item's logic, a guard quietly weakened, prose that now overstates what runs.\n";
        let lensB = intro + "LENS B — BREAKAGE AND FALSE GREENS. What does this change make possible that should not be possible? Concretely hunt: an assertion that cannot fail (its own negative case does not exist), a selftest that passes because it never ran, a predicate that matches only the inputs its author listed, a retry that re-runs a WRITE and so can apply an edit twice, a retry that loops a phase whose seat died for a deterministic reason rather than a provider reason, and a defect in the generator's own host idioms (ternary, unit-returning trim, a let the cat order defines after use). Name the input that would break it.\n";
        let schema = #{
            type: "object",
            properties: #{
                findings: #{ type: "array", items: #{ type: "string" }, description: "one per finding: SEVERITY | file:line | quoted line | why it matters | the command that would settle it" },
                blocking: #{ type: "integer" },
                major: #{ type: "integer" },
                ship: #{ type: "string", description: "SHIP or DO NOT SHIP" },
                notes: #{ type: "string" },
            },
            required: ["findings", "blocking", "major", "ship"],
        };
        let lenses = parallel([
            #{ label: tag + "-lens-a-r" + rounds.to_string(), phase: phase_name, capability_mode: "read-only", prompt: lensA, output_schema: schema },
            #{ label: tag + "-lens-b-r" + rounds.to_string(), phase: phase_name, capability_mode: "read-only", prompt: lensB, output_schema: schema },
        ]);
        let all = [];
        let blockers = 0;
        let majors = 0;
        let i = 0;
        while i < len(lenses) {
            let lp = payload(lenses[i]);
            let fl = lp["findings"];
            let j = 0;
            while j < len(fl) {
                all.push(as_text(fl[j]));
                j += 1;
            }
            blockers += int_field(lp, "blocking");
            majors += int_field(lp, "major");
            i += 1;
        }
        summary = "round=" + rounds.to_string() + " blocking=" + blockers.to_string() + " major=" + majors.to_string() + " findings=" + len(all).to_string();
        log(tag + " " + summary);
        if blockers == 0 && majors == 0 {
            clean = true;
        } else {
            let fixp = "Fix pass, " + tag + " round " + rounds.to_string() + ", on one item. Independent read-only lenses returned the findings below; the coordinator has not adjudicated them, so if one is wrong say so IN THE REPORT with the quoted line that proves it — never skip silently, never widen scope.\n\nFINDINGS:\n" + as_text(all) + "\n\n" + common
                + "AUTHORISED FILES, nothing else: " + authorised + "\n" + fix_extra + "\nAppend what you actually changed, per finding, to " + findings_file + ".\n";
            let fixer = agent(fixp, #{
                label: tag + "-fix-r" + rounds.to_string(),
                phase: phase_name,
                capability_mode: "all",
                output_schema: #{
                    type: "object",
                    properties: #{
                        applied: #{ type: "array", items: #{ type: "string" } },
                        rejected: #{ type: "array", items: #{ type: "string" } },
                        reassembled: #{ type: "string" },
                        selftest_rc: #{ type: "string" },
                    },
                    required: ["applied", "reassembled"],
                },
            });
            let fx = payload(fixer);
            carry = "Claimed fixes: " + as_text(field(fx, "applied")) + " | rejected: " + as_text(field(fx, "rejected")) + " | reassembled: " + field(fx, "reassembled") + "\n";
        }
    }
    return clean;
}

// ---------------------------------------------------------------- Item 1
phase("Item1-Retry");
let item1_desc = "THE ITEM: a driver loses its whole lane when one seat dies, and this campaign's dominant seat-death cause is a provider error (`empty response from model (reasoning_only)`) that has nothing to do with the work. hts-evidroot lost an implement seat at 4 229 637 tokens with the change already on disk; the run had no way to ask for the same seat again.\n\n"
    + "WHAT TO BUILD, deliberately narrow:\n"
    + "  1. A pure predicate `fn seat_unusable(res)` in " + SC + "/scaffold-helpers.snippet.rhai. It returns true when a seat returned NOTHING USABLE — the empty value, a map whose success is not true, a map with no `output`, or an `output` that is not a map. It must NOT try to interpret a verdict: a seat that came back and said `FAIL` is usable, and retrying it would loop a deterministic block forever.\n"
    + "  2. A retry LOOP at the call sites, written inline. Do NOT wrap `agent()` in a function — this host's ability to call the workflow host's `agent()` from inside a user-defined function has never been executed here, and an unproven form is not going into every driver. Inline means: call, test `seat_unusable`, call once more, then fall through to whatever pause the site already has.\n"
    + "  3. Exactly ONE retry per site, and only at sites whose seat does not write product or corpus files. A seat that edits and dies half-way must never be re-run blind, because the second copy would apply its edit again to its own output. Sites that run a long cargo battery may retry — one wasted build is cheaper than a lost lane — but the log line must say `RETRY after unusable seat reply (attempt N/2)` so a reader can tell a provider retry from a re-measure.\n"
    + "  4. The existing pause messages stay reachable: if the second attempt is also unusable, the site pauses exactly as it does today.\n\n"
    + "FALSIFY YOUR OWN PREDICATE, in " + OUT + "/item1-selftest.md. `seat_unusable` must be shown true for: the empty value; a map with success=false; a map whose output is missing; a map whose output is a string; AND one shape you did not design it for (a map whose output is a map but empty). Show the driver or snippet fragment you ran, not a description. A predicate with no negative case is the unable-to-fail class this campaign keeps meeting (BP-55).\n";
let item1_prompt = "One item, in one pass, in a generator of Rhai workflow drivers. A previous lane may have ALREADY landed part or all of this item and then died: grep for `fn seat_unusable` and for its call sites first. If they exist, do NOT re-author them — verify each against the spec below, correct what is wrong, delete what is redundant, and say in your report which sites you found pre-existing and whether each was already correct. Read " + OUT + "/falsifier.md first if it exists; then make the change; then reassemble.\n\n" + item1_desc + "\n" + COMMON
    + "AUTHORISED FILES: " + SC + "/scaffold-helpers.snippet.rhai, " + SC + "/scaffold-phases1.snippet.rhai, " + SC + "/scaffold-gate2.snippet.rhai, " + SC + "/scaffold-loop.snippet.rhai, and the bead blocks in " + SC + "/beadblock-*.rhai if and only if a call site lives there.\n"
    + "When you are done: run `zsh " + SC + "/assemble.zsh` (it rewrites all six drivers), then report per-file line counts and quote every retry site you added as file:line. Write the record to " + OUT + "/item1.md.\n";
let i1 = agent(item1_prompt, #{
    label: "evidlint-item1",
    phase: "Item1-Retry",
    capability_mode: "all",
    output_schema: #{
        type: "object",
        properties: #{
            predicate_added: #{ type: "string", description: "file:line of fn seat_unusable, quoted" },
            sites_adopted: #{ type: "array", items: #{ type: "string" }, description: "file:line per retry site, one line each, naming the phase it guards" },
            sites_declined: #{ type: "array", items: #{ type: "string" }, description: "write-capable sites deliberately left without retry, and why" },
            selftest_shapes: #{ type: "array", items: #{ type: "string" }, description: "each input and the observed true/false" },
            reassembled: #{ type: "string" },
            blockers: #{ type: "array", items: #{ type: "string" } },
        },
        required: ["predicate_added", "sites_adopted", "selftest_shapes", "reassembled"],
    },
});
let i1p = payload(i1);
if field(i1p, "predicate_added") == "" {
    pause("infra", "Item 1's seat returned nothing usable, so no retry predicate exists yet. Nothing downstream depends on it; relaunch this driver — Item 1 is idempotent because it reads the tree before editing.");
    return "NO VERDICT: item 1 seat died before reporting";
}
let c1 = review_loop("evidlint-item1", "Review1", "Round over ITEM 1: a `seat_unusable` predicate plus inline one-shot retry at non-writing call sites in the driver generator. In scope: the predicate, the retry loops, their log lines, and the selftest record. Out of scope: the record/lint work, the guard logic, the review scoring, the gates.",
    SC + "/scaffold-helpers.snippet.rhai, " + SC + "/scaffold-phases1.snippet.rhai, " + SC + "/scaffold-gate2.snippet.rhai, " + SC + "/scaffold-loop.snippet.rhai, " + SC + "/beadblock-*.rhai",
    OUT + "/FINDINGS-item1.md", 3,
        "Remember the two hard constraints: `agent()` is never called from inside a user-defined function on this host, and no write-capable seat may be auto-retried. If a finding asks for either, reject it with this sentence.\n",
    COMMON);

// ---------------------------------------------------------------- Item 2
phase("Item2-Lint");
let item2_desc = "THE ITEM: " + SC + "/assemble.zsh grew a lint that is supposed to fail assembly when a record-class file (report.md, PROCESS.md, artifact.md, findings, a verification record) would be written under /tmp. It has a selftest of eight canned shapes and reports 8/8. The coordinator then injected ONE REAL SHAPE into a snippet:\n"
    + "      let OUT_BAD = \"/tmp/hts-wf/\" + slug + \"/REPORT-injected.md\";\n"
    + "and assembly exited 0. The eight shapes pass because the predicate matches the eight spellings its author enumerated, not the property. That is the unable-to-fail assertion class this campaign has been cutting for ten rounds (BP-55).\n\n"
    + "WHAT TO BUILD.\n"
    + "  1. Rewrite `record_scan`'s test around the property: a line whose string literal contains a `/tmp` path AND a record-class filename, whether the filename is spelled inline, produced by concatenation whose tail is a record name, or assigned to a variable whose name suggests scratch (the last of these is the coordinator's injected case and it MUST be caught).\n"
    + "  2. Keep the legitimate scratch cases LEGAL and keep them proven: the cargo lock path, the OUT_LOCAL scratch parent, the `cmd > /tmp/out.txt 2>&1` redirection advice, the prohibition sentence that names /tmp/hts-wf/PROCESS.md in order to forbid it, and a comment. These must not start failing assembly or the next build is bricked.\n"
    + "  3. Selftest: keep the existing shapes and add the injected shape plus one more of your own devising that you expect the old predicate to miss. The selftest must FAIL (non-zero) if any expected catch is missed or any expected pass is rejected — verify that by temporarily breaking the predicate and showing the selftest notice you.\n"
    + "  4. Do not weaken any existing check (banned methods, ternary ban, the trim class) while doing this; if one of them has to move, quote it before and after.\n";
let item2_prompt = "One item, in one pass, in the generator's own syntax gate. Read " + OUT + "/item1.md and " + OUT + "/FINDINGS-item1.md first so you do not undo Item 1.\n\n" + item2_desc + "\n" + COMMON
    + "AUTHORISED FILES: " + SC + "/assemble.zsh only, plus new fixture files under " + SC + "/ if a shape cannot be expressed inline.\n"
    + "Prove, quoting real output: (a) the injected shape is now caught and assembly exits non-zero with the offending file:line named; (b) after removing it, assembly exits 0; (c) the selftest catches all shapes including yours; (d) the six drivers still reassemble. Write the record to " + OUT + "/item2.md.\n";
let i2 = agent(item2_prompt, #{
    label: "evidlint-item2",
    phase: "Item2-Lint",
    capability_mode: "all",
    output_schema: #{
        type: "object",
        properties: #{
            predicate_now: #{ type: "string", description: "the property as now implemented, quoted from assemble.zsh" },
            injection_caught: #{ type: "string", description: "YES/NO plus the observed exit status and the message the lint printed" },
            scratch_still_legal: #{ type: "string", description: "the four pass-cases and the status showing they were not rejected" },
            selftest_extended: #{ type: "array", items: #{ type: "string" } },
            selftest_can_fail: #{ type: "string", description: "the demonstration that the selftest itself is not a no-op" },
            reassembled: #{ type: "string" },
            blockers: #{ type: "array", items: #{ type: "string" } },
        },
        required: ["predicate_now", "injection_caught", "scratch_still_legal", "selftest_extended", "reassembled"],
    },
});
let c2 = review_loop("evidlint-item2", "Review2", "Round over ITEM 2: the record-under-/tmp lint in assemble.zsh must test the property, catch the coordinator's injected shape, still allow the four legitimate scratch spellings, and its selftest must be able to fail. Out of scope: Item 1's retry, the drivers' guard logic, the gates.",
    SC + "/assemble.zsh and new fixtures under " + SC + "/. A change to any .snippet.rhai file is itself a finding.",
    OUT + "/FINDINGS-item2.md", 3,
        "The four legitimate scratch spellings are contractual (cargo lock, OUT_LOCAL, the redirection advice, the prohibition sentence). A finding that asks to reject any of them is wrong; reject it with the line that shows it is one of the four.\n",
    COMMON);

log("item1 clean=" + c1.to_string() + " item2 clean=" + c2.to_string());
if !c1 || !c2 {
    pause("no_progress", "The review loops did not converge (item1 clean=" + c1.to_string() + ", item2 clean=" + c2.to_string() + "). The generator is mid-edit and the six installed drivers were reassembled from it, so no hts-* driver may be launched off it until the coordinator runs validate_only by script_path on all six and reads " + OUT + "/FINDINGS-item1.md and " + OUT + "/FINDINGS-item2.md.");
    return "NOT CLEAN item1=" + c1.to_string() + " item2=" + c2.to_string() + " — coordinator: validate_only all six by script_path, adjudicate the two FINDINGS files, do not commit.";
}
return "CLEAN both items | retry predicate + inline one-shot retry at non-writing sites; record lint now tests the property and catches the injected shape | coordinator MUST: validate_only all six by script_path, then commit harness/workflow-design pathspec-limited (no remote on this repo, so this is a checkpoint not an exfil).";
