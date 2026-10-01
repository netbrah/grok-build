let meta = #{
    name: "event-plumbing-review-r11",
    description: "apex-23h: re-seat one clean three-lens review round over the round-9 bytes, against a write-once freeze whose pins were regenerated from the tree at convening time",
    when_to_use: "Round 8's review round 2 voided itself: all three seats found the live tree disagreeing with the freeze record the brief pointed them at, because the in-loop fixer moved the bytes and re-froze under a different filename while the script kept quoting the gate's stale record. The code items it was chasing all landed and were verified by the coordinator. This run convenes seats against the correct authority and carries the fixer's re-freeze forward as the next round's authority.",
    phases: [
        #{ title: "Review round 1", detail: "three fresh read-only lenses over the frozen bytes; hashing first, refusing to grade a moved tree" },
        #{ title: "Fix review round 1", detail: "only if a round returns blocker/major: comment-or-assertion edits, then a re-freeze into a NEW bank dir that becomes round 2's authority" },
        #{ title: "Review round 2", detail: "three fresh seats over the re-frozen bytes" },
    ],
};

// `join_lines` is NOT a Rhai builtin in this host; define before use (rounds 1 and 2 of this
// campaign both died on exactly this).
fn join_lines(items, sep) {
    let out = "";
    for i in 0..items.len() {
        if i > 0 { out = out + sep; }
        out = out + items[i].to_string();
    }
    out
}

let WT = "/Users/palanisd/Projects/upstream/wt/apex-ayl-hosted-tool-search";
let EVENTS = "crates/codegen/xai-grok-sampler/src/events.rs";
let TD = "crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_dispatch.rs";
let SC = "crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampling_events.rs";
let ST = "crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampling_events_tests.rs";
let REQTASK = "crates/codegen/xai-grok-sampler/src/actor/request_task.rs";
let SAMPLIB = "crates/codegen/xai-grok-sampler/src/lib.rs";
let SIX = EVENTS + ", " + SAMPLIB + ", " + REQTASK + ", " + SC + ", " + ST + ", " + TD;

// Pins regenerated from the tree at convening time (coordinator, 2026-09-30T18:35:03Z), not
// transcribed from an earlier round's brief. Round 7 and round 8 both died on the older habit.
let PINS = "7dde722982f1056cb0b6c1fb13bd6dfa7c90cdc069f9a3eabfbf70777d1ca22a  events.rs\n"
    + "626b95407634b6080f0b8e987a073a735af1f2a54977ccc8d13db2b907847281  sampler src/lib.rs\n"
    + "732307dc507df841f1b5b89a7f1970adb3c42be35ff7aff9336d312dd74cca20  request_task.rs\n"
    + "0818554fb4d8a2525ac32536eeabf6705f6e877157da757dbf0c5e55f317c450  sampling_events.rs\n"
    + "a98c85b491613b691b7d65c8e9f57e911fccaa22b7bdfce6ab9b6569b6f7fa5b  sampling_events_tests.rs\n"
    + "80c466ac5e2095fbd754347d5b161584082e0336237fc841a4c25512cf3a4400  tool_dispatch.rs\n";
let AUTH1 = "/tmp/bank/apex23h-r11/FREEZE.txt";

let STATE = "TREE STATE, measured by the coordinator at 2026-09-30T18:35:03Z.\n"
    + "- Rounds 1-9 are landed and UNCOMMITTED; six paths only; diffstat vs HEAD +1009/-6. HEAD `ff2351c584c658112cc36224f737a70461b8426e`, branch apex-ayl-hosted-tool-search.\n"
    + "- Authority: " + AUTH1 + " (coordinator-written, write-once: the bank dir is chmod a-w). Its live-tree rows are:\n" + PINS
    + "- It re-measures the bytes the round-9 fixer froze in /tmp/bank/apex23h-r9/FREEZE.txt (17:39:28Z, with a copy-equality gate and a positive control inside the record). Round 8's bytes are preserved in /tmp/bank/apex23h-r9/pre_r9/.\n"
    + "- Attribution of the +1009/-6, from the round-9 record: +992/-3 is rounds 1-8 of this cut; +10/-4 in " + ST + " and +4/-3 in " + TD + " are this round's two comment-only edits; +3/-3 in " + REQTASK + " is NOT this cut -- another lane rewrote three `id: \"...\"` test fixtures to `id: Some(\"...\")` at ~17:27Z inside `mod tests` and `mod enc_boundary_mf6_tests`. This cut's own hunk in that file is still exactly one hunk: the `output_observed` verdict comment at live 901-928.\n"
    + "- FOREIGN BUILD BREAK, and the reason no shell-suite verdict exists at these bytes: `cargo test -p xai-grok-shell --lib --features xai-grok-shell/test-support --no-run` fails with 32 errors in 9 files (coordinator's own runs at 18:29Z and 18:33Z, log at /tmp/bank/apex23h-r10/G20b_shell_test_compile_full.log). The cause is a sibling refactor in xai-grok-sampling-types (conversation.rs / conversation/responses.rs) plus the removal of `rs::WebSearchToolCallStatus` and `Option`-ing of `WebSearchToolCallAction`/`id` with a new required `queries` field. Six of the 32 errors name " + TD + " at live lines 488/491/492/506/513, which map to HEAD lines 412/415/416/430/437 -- the pre-existing `web_search_payload` helper, i.e. code this cut never added (this cut's hunks in that file are live 6, 130-203, 485 and 524-742). Open those lines yourself before accepting or rejecting this attribution.\n"
    + "- Sampler suite DOES have a verdict at these bytes: 438 passed / 4 failed at 17:48Z. The four names: the standing exception stream::responses::tests::response_completed_cache_write_tokens_lands_in_token_usage, plus three new sibling-attributed names (client::tests::ingress77_actionless_web_search_strict_parse_fatals_missing_action, client::tests::ingress77_actionless_web_search_done_ingests_with_sentinel_action, stream::responses::tests::tool_search_call_frame_survives_the_stream_deserialiser).\n"
    + "- Round 8's review round 1 returned 0 blocker / 5 major and every one of the five was ACCEPTED by the fixer and re-verified first-hand by the coordinator: F1/F4 were a bank artifact (the six-path diff filed as evidence was a mutant-window capture of M_C, quarantined with an erratum in /tmp/bank/apex23h-r8/logs/README.txt and a fresh capture at /tmp/bank/apex23h-r9/logs/V14_sixpath.diff); F2 rewrote the `unknown_terminal_writes_no_status_word` doc to name the filter blindness (no `tool_search` filter matches that test name, so a scoped `--lib tool_search` battery never selects it); F3 deleted the control-actor assertion from `owned_tool_search_call_stamps_turn_first_token` and replaced the substitute-coverage claim that was false as written; F5 recorded an erratum against round 7's freeze record (/tmp/bank/apex23h-r7/FREEZE-ERRATUM.txt).\n\n";

let RULES = "HARD RULES for every agent in this run.\n"
    + "1. NO WILDCARD ARMS anywhere in this cut's added lines; every match arm carries a stated verdict.\n"
    + "2. Scope is the six paths and nothing else: " + SIX + ". Never edit " + "xai-grok-sampling-types/src/conversation/tool_search.rs" + ", projection.rs, projection_tests.rs, conversation.rs, stream/responses.rs, model_switch.rs, handle.rs, actor/mod.rs, actor/mutations.rs, conversation/chat_completions.rs or conversation/messages.rs -- those belong to sibling lanes. Review seats edit NOTHING.\n"
    + "3. Review seats run NO cargo commands at all (shared target lock, and the shell test target is broken by a sibling anyway). Hashes and `git` read-only commands are fine.\n"
    + "4. Verify every claim against files you opened yourself, at these bytes. Report only what you observed, with file:line. An empty findings list is a valid answer.\n"
    + "5. Never restore/checkout/stash a tracked file; sibling dirt is its lane's state, and is recorded, not reverted.\n"
    + "6. Nothing is committed by this run. The worker announces a bank point; the controller cuts it.\n\n";

let REPORT_SCHEMA = #{
    "type": "object",
    "properties": #{
        "status": #{ "type": "string", "enum": ["done", "blocked"] },
        "files_touched": #{ "type": "array", "items": #{ "type": "string" } },
        "items_applied": #{ "type": "array", "items": #{ "type": "string" }, "description": "one entry per finding: ACCEPTED/LANDED or REJECTED + the file:line proof" },
        "commands": #{ "type": "array", "items": #{ "type": "string" } },
        "summary_lines": #{ "type": "array", "items": #{ "type": "string" } },
        "freeze_path": #{ "type": "string", "description": "absolute path of the NEW FREEZE.txt written after the fix pass" },
        "freeze_lines": #{ "type": "array", "items": #{ "type": "string" }, "description": "FULL sha256 + relative path, one per scope path, measured after the last edit" },
        "preexisting_failures": #{ "type": "array", "items": #{ "type": "string" } },
        "handoffs": #{ "type": "array", "items": #{ "type": "string" } },
        "notes": #{ "type": "string" }
    },
    "required": ["status", "files_touched", "items_applied", "commands", "summary_lines", "freeze_path", "freeze_lines", "handoffs", "notes"]
};

let REVIEW_SCHEMA = #{
    "type": "object",
    "properties": #{
        "graded_hashes_match": #{ "type": "boolean", "description": "false => the tree disagrees with the authority record: report that as a blocker and grade nothing else" },
        "measured_hashes": #{ "type": "array", "items": #{ "type": "string" }, "description": "the six values you measured, full sha256 + path" },
        "findings": #{ "type": "array", "items": #{ "type": "object", "properties": #{
            "severity": #{ "type": "string", "enum": ["blocker", "major", "minor", "note"] },
            "where": #{ "type": "string" },
            "claim": #{ "type": "string" },
            "evidence": #{ "type": "string" }
        }, "required": ["severity", "where", "claim", "evidence"], "additionalProperties": false } },
        "verdict": #{ "type": "string", "enum": ["clean", "changes-requested"] }
    },
    "required": ["graded_hashes_match", "measured_hashes", "findings", "verdict"]
};

let LENSES = [
    #{ name: "falsification-power", brief: "For every assertion this cut added, name a mutation of the production code that makes THAT assertion the one which fails. Round 6b and round 7 both produced assertions with no kill path (an assertion preceded by a stronger one can never be the failing assertion, because a test stops at the first panic), and round 8 produced a doc comment that claimed coverage a neighbouring test did not provide. Focus on the two round-9 comment edits: (a) in " + ST + ", the block replacing the deleted control-actor assertion claims that hoisting the ttft stamp above the ownership gate changes nothing observable, and that the gap opens only if the generation lookup ever falls back to the live generation -- open " + SC + " and check the ownership gate, `turn_generation` and the `None` arm for yourself; (b) in " + TD + ", `unknown_terminal_writes_no_status_word` claims that restoring the `Unknown` status word dies on its own first assertion -- read the test and say whether that assertion can actually be the one that fails." },
    #{ name: "cite-and-attribution-truth", brief: "Every cite and every attribution in this cut's added lines, re-opened against THESE bytes. Highest risk first: the `output_observed` verdict comment in " + REQTASK + " (live ~901-928) characterises which response frames `responses_event_may_have_output` and `responses_event_has_meaningful_content` accept, and it cites a sibling file (stream/responses.rs, lane apex-waj.5) that is being edited live -- re-open it and check each named group and each `=> true` claim, and flag any cite that no longer resolves. Then the F2 block in " + TD + " (the IR corpus figures: 235 parsed documents, zero `unknown`, zero `status: null`, and the claim about which filter selects the test), then the F3 block in " + ST + " (the named neighbour tests and what each observes). Report a cite as broken only from the file and line you opened." },
    #{ name: "scope-freeze-and-record", brief: "First action: read " + AUTH1 + " FROM DISK and hash the six live paths; any disagreement with its live-tree block => graded_hashes_match=false, print your values, grade nothing else. Then three things. (1) Attribution completeness: is every line of the +1009/-6 accounted for, and is the coordinator's claim right that the three `id: Some(...)` fixture lines in " + REQTASK + " are foreign while this cut's own hunk there is a single comment block? (2) Ownership: zero wildcard arms in added lines; no file outside the six paths carrying this cut's work; the pre-existing `backend_tool_call_status` still byte-identical to HEAD. (3) Record integrity for a bank point: could a stranger pair every claim in /tmp/bank/apex23h-r8, -r9 and -r11 with the bytes it was measured on? Check the round-8 errata actually land where a stranger would look, and that the coordinator's foreign-build attribution (32 errors, 6 of them on pre-existing HEAD lines of " + TD + ") holds by mapping the live error lines onto HEAD yourself with `git show HEAD:<path>`." }
];

let rounds_log = [];
let clean = false;
let round = 0;
let authority = AUTH1;
let authority_lines = PINS;

while round < 2 && !clean {
    round = round + 1;
    phase("Review round " + round.to_string());
    let jobs = [];
    for lens in LENSES {
        jobs.push(#{
            prompt: "You are a READ-ONLY review seat (" + lens.name + ", round 11." + round.to_string() + ") on bead apex-23h, lane event-plumbing. Edit no file. No git write commands. No cargo (shared target lock; the shell lib test target is broken by a sibling lane anyway).\n\n"
                + "AUTHORITY FOR THIS ROUND: " + authority + " -- read it from disk FIRST, then run `shasum -a 256` over the six live paths in " + WT + " and compare against that file's live-tree block. If any path disagrees: graded_hashes_match=false, print the six values you measured in measured_hashes, and grade NOTHING else (sibling lanes are live and can move the tree; a moved tree is not this cut's defect but it cannot be graded). This path supersedes any bank path named in your lens text. Cross-check against these convening-time pins too:\n" + authority_lines
                + "\nAlso recompute the non-scope dirty set (`git status --porcelain` minus the six paths) and say whether it moved since the record -- it is expected to drift, that is sibling work, report it as a note, never as a blocker.\n\n"
                + "YOUR LENS: " + lens.brief + "\n\n"
                + "Scope of this cut: the UNCOMMITTED diff of these six paths only -- " + SIX + ". Roughly 75-79 other files are dirty from sibling lanes, including xai-grok-sampling-types/src/conversation/tool_search.rs and stream/responses.rs; if you cannot scope to the six paths, say so rather than auditing another lane.\n"
                + STATE + RULES
                + "Severity discipline: blocker/major is for something that makes this cut wrong or unbankable (a false claim in a comment this cut wrote, an assertion with no kill path, a broken cite, an unattributed line, an arm that swallows a variant). Minor/note is for wording and record hygiene. Do not invent findings and do not pad; if the cut is right, return an empty findings list and verdict=clean.\n",
            label: "rev11-r" + round.to_string() + "-" + lens.name,
            capability_mode: "read-only",
            output_schema: REVIEW_SCHEMA,
            phase: "Review round " + round.to_string()
        });
    }
    let results = parallel(jobs);
    let blocking = [];
    let major = [];
    let minor = [];
    let ok = 0;
    let idx = 0;
    for r in results {
        idx = idx + 1;
        if r == () || r.success != true || r.output.findings == () {
            continue;
        }
        ok = ok + 1;
        if r.output.graded_hashes_match != true {
            blocking.push("BLOCKER: seat " + idx.to_string() + " (" + LENSES[idx - 1].name + ") refused to grade -- the tree disagrees with " + authority);
            continue;
        }
        for f in r.output.findings {
            let line = f.severity + ": " + f.where + " | " + f.claim + " | evidence: " + f.evidence;
            if f.severity == "blocker" {
                blocking.push(line);
            } else if f.severity == "major" {
                major.push(line);
            } else {
                minor.push(line);
            }
        }
    }
    if ok == 0 {
        pause("verification", "No reviewer returned a structured result in round 11." + round.to_string() + "; the round cannot count as clean.");
    }
    rounds_log.push("review 11." + round.to_string() + ": " + blocking.len().to_string() + " blocker, "
        + major.len().to_string() + " major, " + minor.len().to_string() + " minor, reviewers=" + ok.to_string());
    log(join_lines(rounds_log, "\n"));
    if ok > 0 && blocking.len() == 0 && major.len() == 0 {
        clean = true;
    } else if round < 2 {
        phase("Fix review round " + round.to_string());
        let newbank = "/tmp/bank/apex23h-r1" + (11 + round).to_string();
        let fixer = agent(
            "You are the review-fix driver for bead apex-23h, round 11." + round.to_string() + ". Apply these findings from independent read-only seats; if one is factually wrong, REJECT it with file:line proof -- do not silently soften a claim you cannot support. Keep this behaviour-free: comment and assertion edits inside the six paths only.\n\n<findings>\n" + join_lines(blocking, "\n") + "\n" + join_lines(major, "\n") + "\n</findings>\n\n"
            + STATE + RULES
            + "Cargo is permitted for you alone (check `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave|nextest|zsh|lspmux'` first and wait; never kill a sibling build). The shell lib test target does NOT compile at these bytes because of a sibling refactor -- that is not yours to fix and not yours to hide: if your edit is in the shell crate, the best verdict you can produce is a scoped compile attempt plus a statement that no verdict is available, and you must say which.\n"
            + "THEN FREEZE FOR THE NEXT ROUND, into a NEW directory that does not exist yet: " + newbank + " (never reuse an older bank dir -- round 8 lost its review because its frozen copies were rewritten in place by the next round). Copy the six paths there, write " + newbank + "/FREEZE.txt containing the utc timestamp, `git rev-parse HEAD`, the branch, FULL sha256 of the six live paths, FULL sha256 of the six copies, the copy-equality gate's own output INSIDE the record plus one positive control, the six-path diffstat, and a hashed fingerprint of the non-scope dirty set saved as its own file named with the UTC stamp. Spell every path list out inline -- zsh does not word-split an unquoted variable and that bug has produced four false-clean results in this cut. Finish with `chmod -R a-w " + newbank + "` so the record cannot be overwritten. Report that path in freeze_path and the six FULL-sha256-plus-path lines in freeze_lines.\n",
            #{ label: "rev11fix-r" + round.to_string(), output_schema: REPORT_SCHEMA, phase: "Fix review round " + round.to_string() }
        );
        if fixer == () || fixer.success != true {
            pause("verification", "Review fixer failed in round 11." + round.to_string() + "; audit its bytes, then relaunch to continue.");
        }
        if fixer.output != () && type_of(fixer.output.freeze_lines) == "Array" && fixer.output.freeze_lines.len() == 6
            && fixer.output.freeze_path != "" {
            authority = fixer.output.freeze_path;
            authority_lines = join_lines(fixer.output.freeze_lines, "\n");
        } else {
            pause("verification", "The fixer did not return a six-line re-freeze, so round 11." + (round + 1).to_string() + " would convene against a stale authority. Re-freeze and relaunch.");
        }
    }
}

let summary = "apex-23h review round 11 finished.\n" + join_lines(rounds_log, "\n")
    + "\nConverged clean: " + clean.to_string()
    + "\nFinal authority: " + authority + "\n" + authority_lines
    + "\nNothing committed by this workflow; the worker announces a bank point and the controller cuts it.";
log(summary);
