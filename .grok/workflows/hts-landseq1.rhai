let meta = #{
    name: "hts-landseq1",
    description: "Land LANDING-PLAN seq 1 (the async-openai 0.42.1 re-pin plus every typed-site repair it forces) as a BUILDABLE tree, proved by a real cargo run and then a three-lens review loop. Assembles synthetic blobs in a throwaway root from the frozen preimage, never by editing the shared dirty tree. No commit — the coordinator exfils.",
    when_to_use: "The landing plan exists at plans:harness/hosted-tool-search/map/LANDING-PLAN.md (content stamp f3ef08d0a4b1) and the coordinator has adjudicated three attack rounds first-hand (map/ADJUDICATION-landing-plan.md). What no read-only round could produce is the one fact seq 1 hinges on: does its tree compile. This run produces that fact, drives it to green, runs the six-package GATE against the adjudicated fail-set, and converges a review round. apex-waj.21's Discovery variant is seq 2 and is NOT in scope here.",
    phases: [
        #{ title: "Precheck", detail: "re-take every pin the plan depends on and refuse the run if any moved" },
        #{ title: "Assemble", detail: "build seq 1's synthetic blobs in a throwaway root from the frozen preimage, by pinned @@ key" },
        #{ title: "Green", detail: "cargo check the assembled tree in a scratch worktree, drive compile errors to fixpoint" },
        #{ title: "Gate", detail: "the six-package GATE on those bytes, true rc read back from disk, only NEW failures reported" },
        #{ title: "Review", detail: "three independent read-only lenses over the staged change and its evidence" },
        #{ title: "Fix", detail: "Blocking then Major, then a fresh review round, until a round is clean" },
        #{ title: "Report", detail: "the exfil packet: pathspec list, blob hashes, provenance body, what is still owed" },
    ],
};

// ---------------------------------------------------------------------------
// Constants. Every one is re-measured in Precheck; these are the expected values
// a seat must refuse a run over, not facts to trust.
// ---------------------------------------------------------------------------
let WT = "/Users/palanisd/Projects/upstream/wt/apex-ayl-hosted-tool-search";
let PLANS = "/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/map";
let PLAN = PLANS + "/LANDING-PLAN.md";
let ADJ = PLANS + "/ADJUDICATION-landing-plan.md";
let STAMP = "f3ef08d0a4b100917e32aba54501747dec0e2663d1df022dc7c31a37af2532b7";
let BASE = "2e1d6ced90f9d5defe513148deb223da36aa25eb";
let BRANCH = "apex-ayl-hosted-tool-search";
let BASE_SHORT = "2e1d6ced";
let STAMP_SHORT = "f3ef08d0a4b1";
let OUT = "/tmp/hts-wf/seq1";
let SCRATCH = "/tmp/hts-seq1-tree";
let LOCK = "/tmp/hts-cargo.lock";

fn as_text(v) {
    if type_of(v) == "string" { v; }
    else if v == () { ""; }
    else { try { to_string(v); } catch(e) { ""; } }
}

fn as_array(v) {
    if type_of(v) == "array" { v; } else { []; }
}

fn join_lines(items, sep) {
    let arr = as_array(items);
    let out = "";
    let first = true;
    for it in arr {
        if first { out = as_text(it); first = false; }
        else { out = out + sep + as_text(it); }
    }
    return out;
}

// The verdict rule this campaign had to invent twice. A completion, a log tail, or
// a seat's prose is not a verdict. A verdict exists iff the .rc file exists AND the
// log carries at least one '^test result' line. rc=101 with zero such lines is a
// VACUOUS GREEN (a build that produced no test binary), and an exit status beside a
// log with no 'test result' line is BLOCKED, never FAILED.
fn verdict_rule(cmd_name, out_dir) {
    return "VERDICT RULE for `" + cmd_name + "`: write the exit status to a file "
        + "(`" + cmd_name + "; echo $? > " + out_dir + "/" + cmd_name + ".rc`), then READ THE FILE BACK, "
        + "and separately report `grep -c '^test result' <log>`. Quote all three numbers. "
        + "If the count is 0 the run is BLOCKED (no test binary was produced), not FAILED and not "
        + "PASSED — reporting it either way is the false-green class this campaign already has two "
        + "written incidents about. Never report a verdict from a log tail or from a completion notice.";
}

// House rules every seat gets, because seats are fresh and the shared tree is dirty
// with five other lanes' work. Script-level: Rhai functions cannot see script
// variables, so anything interpolating a constant has to live at top level.
let HOUSE_RULES = "HARD RULES (violating one voids the run):\n"
        + "- The worktree at " + WT + " is SHARED and already dirty with sibling lanes' work "
        + "(85 files, ~+9,158/-562 vs " + BASE_SHORT + "). NEVER `git stash`, `git clean`, "
        + "`git checkout --`, `git restore`, or revert anything there. Never `git add`.\n"
        + "- You write NO product code in " + WT + ". Blobs are assembled in a throwaway root "
        + "(`mktemp -d`) per the plan's recipe. The only permitted writes inside " + WT + " are the "
        + "two new fixture paths named in your brief, and the scratch worktree lives outside it.\n"
        + "- No `rm -rf`. The harness blocks it; do not work around it.\n"
        + "- Before ANY cargo command: `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave-mcp'`. A sibling "
        + "lane may hold the target lock. Wait and poll; NEVER kill a cargo/rustc you did not start.\n"
        + "- Serialize every build behind the shared lock: `lockf -k -s -t 5400 " + LOCK + " <cmd>` "
        + "(macOS has lockf, not flock).\n"
        + "- Scoped shell checks on xai-grok-shell MUST carry `--features xai-grok-shell/test-support`, "
        + "or you get 6 phantom E0425/E0432/E0282.\n"
        + "- zsh does NOT word-split an unquoted `$var`; use arrays or `${=var}`. `exec zsh -c` replaces "
        + "the shell and silently drops the rest of the line. Quote `HEAD:path` forms.\n"
        + "- Before quoting ANY log, capture, or report excerpt: `grep -rEil 'sk-[A0-9a-z]{16,}' <paths>` "
        + "must return 0 hits. Never print a key value. `APEX_LLM_PROXY_KEY` / `CODEX_LLM_PROXY_KEY` may "
        + "be in your env: never `env | grep`, never echo them.\n"
        + "- A claim you did not measure is tagged as inherited. Do not upgrade a citation into a fact.\n";

fn schema_precheck() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["head_ok", "stamp_ok", "keys_ok", "removed_type_sites", "free_gib",
                   "cargo_idle", "refuse_reason", "measurements"],
        properties: #{
            head_ok: #{ type: "boolean" },
            stamp_ok: #{ type: "boolean" },
            keys_ok: #{ type: "boolean" },
            removed_type_sites: #{ type: "integer" },
            free_gib: #{ type: "number" },
            cargo_idle: #{ type: "boolean" },
            refuse_reason: #{ type: "string" },
            measurements: #{ type: "string" },
        },
    };
}

fn schema_assemble() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["assembled", "paths_staged", "apply_failures", "blob_manifest",
                   "authored_lines", "notes"],
        properties: #{
            assembled: #{ type: "boolean" },
            paths_staged: #{ type: "array", items: #{ type: "string" } },
            apply_failures: #{ type: "array", items: #{ type: "string" } },
            blob_manifest: #{ type: "string" },
            authored_lines: #{ type: "integer" },
            notes: #{ type: "string" },
        },
    };
}

fn schema_build() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["check_rc", "check_result_lines", "errors_remaining", "fix_rounds",
                   "evidence_files", "notes"],
        properties: #{
            check_rc: #{ type: "integer" },
            check_result_lines: #{ type: "integer" },
            errors_remaining: #{ type: "array", items: #{ type: "string" } },
            fix_rounds: #{ type: "integer" },
            evidence_files: #{ type: "array", items: #{ type: "string" } },
            notes: #{ type: "string" },
        },
    };
}

fn schema_gate() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["gate_rc", "test_result_lines", "passed", "failed_names",
                   "new_failures", "tree_fingerprint", "vacuous", "evidence_files", "notes"],
        properties: #{
            gate_rc: #{ type: "integer" },
            test_result_lines: #{ type: "integer" },
            passed: #{ type: "integer" },
            failed_names: #{ type: "array", items: #{ type: "string" } },
            new_failures: #{ type: "array", items: #{ type: "string" } },
            tree_fingerprint: #{ type: "string" },
            vacuous: #{ type: "boolean" },
            evidence_files: #{ type: "array", items: #{ type: "string" } },
            notes: #{ type: "string" },
        },
    };
}

fn schema_findings() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["lens_complete", "findings", "notes"],
        properties: #{
            lens_complete: #{ type: "boolean" },
            findings: #{
                type: "array",
                items: #{
                    type: "object",
                    additionalProperties: false,
                    required: ["severity", "title", "evidence", "fix"],
                    properties: #{
                        severity: #{ type: "string", enum: ["Blocking", "Major", "Minor"] },
                        title: #{ type: "string" },
                        evidence: #{ type: "string" },
                        fix: #{ type: "string" },
                    },
                },
            },
            notes: #{ type: "string" },
        },
    };
}

fn schema_report() {
    return #{
        type: "object",
        additionalProperties: false,
        required: ["packet_path", "pathspec", "provenance_body_path", "still_owed", "notes"],
        properties: #{
            packet_path: #{ type: "string" },
            pathspec: #{ type: "array", items: #{ type: "string" } },
            provenance_body_path: #{ type: "string" },
            still_owed: #{ type: "array", items: #{ type: "string" } },
            notes: #{ type: "string" },
        },
    };
}

// ---------------------------------------------------------------------------
// Phase 1 — Precheck. Every pin the plan rests on, re-taken. The plan itself says
// its numbers are timestamped measurements and not constants; round 3's own brief
// quoted a stale dirty-count. A run that assembles blobs against a moved preimage
// produces a tree that matches nothing.
// ---------------------------------------------------------------------------
phase("Precheck");

let pre_prompt = "You are the Precheck seat for LANDING-PLAN seq 1. Read-only over git; you write "
    + "only " + OUT + "/PRECHECK.md.\n\n"
    + "Context: branch " + BRANCH + " is checked out at " + WT + " and its base is " + BASE
    + ". The plan is " + PLAN + " and the coordinator's first-hand adjudication is " + ADJ + ".\n\n"
    + "Run these, quoting the actual output of each into " + OUT + "/PRECHECK.md:\n"
    + "1. `git -C " + WT + " rev-parse HEAD` — must equal " + BASE + ". If it moved, the plan's "
    + "positional hunk keys are void: set head_ok=false and put the new tip in refuse_reason.\n"
    + "2. `sed '/^<!-- CONTENT-STAMP -->/d' " + PLAN + " | shasum -a 256` — must equal " + STAMP
    + ". A copy that fails this is a stale round-2 artifact and every build-bearing sentence in it "
    + "is known to contradict the artifact on five points.\n"
    + "3. Re-take the dirty surface: `git -C " + WT + " diff HEAD --shortstat` and "
    + "`git -C " + WT + " status --porcelain | wc -l`. Report both; the plan's 85 / +9,158 / -562 is "
    + "a timestamp, not a constant.\n"
    + "4. Keys still resolve. For each path in seq 1's list (read the plan's section 4 'seq 1' heading "
    + "and the section 3 rows for those paths), confirm the pinned `@@` HEAD-side start lines the plan "
    + "names actually appear in `git diff " + BASE + " -- <path>`. Report any key that does not resolve. "
    + "keys_ok=false only if a seq-1 key is missing.\n"
    + "5. The type the re-pin deletes. `git -C " + WT + " grep -c 'WebSearchToolCallStatus' " + BASE
    + " -- crates` — this is the removed-type census (12 sites / 7 files when the coordinator measured "
    + "it). Then confirm the plan NEVER names that type: `grep -c WebSearchToolCallStatus " + PLAN
    + "`. Put the HEAD-side count in removed_type_sites. This number matters: seq 1 must drive it to "
    + "ZERO in its own tree, and the plan's hunk assignments cover it only incidentally.\n"
    + "6. Disk before any build: `df -g /Users | tail -1` -> free_gib. Below 40 means Assemble must "
    + "not share the target dir.\n"
    + "7. `pgrep -fl 'cargo|rustc' | grep -v -E 'sem|weave-mcp'` -> cargo_idle (true if empty).\n\n"
    + "Set refuse_reason to \"\" if nothing refuses. " + HOUSE_RULES;

let pre = agent(pre_prompt, #{ label: "seq1-precheck", output_schema: schema_precheck(),
                              phase: "Precheck", capability_mode: "execute" });
if pre.success != true || pre.output == () {
    pause("verification", "Precheck returned nothing. Nothing is assembled and no build runs — "
        + "every later phase depends on the preimage being where the plan thinks it is.");
    throw("PRECHECK_UNREADABLE");
}
let po = pre.output;
log("precheck: head_ok=" + as_text(po.head_ok) + " stamp_ok=" + as_text(po.stamp_ok)
    + " keys_ok=" + as_text(po.keys_ok) + " removed_type_sites=" + as_text(po.removed_type_sites)
    + " free_gib=" + as_text(po.free_gib) + " cargo_idle=" + as_text(po.cargo_idle));
if po.head_ok != true || po.stamp_ok != true || po.keys_ok != true {
    pause("verification", "Refusing to assemble seq 1: " + as_text(po.refuse_reason)
        + ". The plan's hunk keys are positional against " + BASE_SHORT
        + " and its build-bearing sentences are stamp-guarded; re-derive the plan against the new "
        + "preimage (or restore the stamp) and relaunch. Do not relaunch this run to find out again.");
    throw("PRECHECK_REFUSED");
}
if po.removed_type_sites == 0 {
    pause("verification", "The removed-type census came back ZERO on the preimage, which contradicts "
        + "both the plan and the board (ow-056: the whole 32-error GATE block is this bump). Either the "
        + "grep ran against the wrong ref or the preimage is not what this plan was written against. "
        + "Adjudicate before assembling.");
    throw("PRECHECK_TYPE_CENSUS_ZERO");
}
log(as_text(po.measurements));

// ---------------------------------------------------------------------------
// Phase 2 — Assemble. Synthetic blobs, throwaway root, pinned @@ keys.
// The plan's section 8 says the reference exfil driver is NOT sufficient as written:
// it can only stage whole working-tree files, and 18 paths need partial blobs.
// ---------------------------------------------------------------------------
phase("Assemble");

let asm_prompt = "You are the Assemble seat for LANDING-PLAN seq 1. Your job is to produce seq 1's "
    + "tree WITHOUT touching the shared worktree, then prove what you produced.\n\n"
    + "AUTHORITY. Read, in this order: " + ADJ + " (the coordinator's rulings, which override the plan "
    + "where they differ), then " + PLAN + " section 4 'seq 1' in full, then section 3 rows for seq 1's "
    + "paths, then section 8 (exfil mechanics — read the round-1 correction and the round-3 chain-head "
    + "rule before you touch git). Stamp verified: " + STAMP + ".\n\n"
    + "METHOD, exactly as section 8 states it. Per path:\n"
    + "  root=$(mktemp -d); mkdir -p \"$root/$(dirname \"$p\")\"\n"
    + "  git -C " + WT + " show " + BASE + ":\"$p\" > \"$root/$p\"\n"
    + "  git -C " + WT + " diff " + BASE + " -U3 -- \"$p\" > \"$root/all.patch\"\n"
    + "  # select ONLY seq 1's hunks into \"$root/own.patch\", matching by the pinned @@ HEAD-side START "
    + "LINE, never by ordinal position (ordinals shift the moment a commit lands)\n"
    + "  ( cd \"$root\" && git apply -C1 --recount own.patch )   # rc must be 0; no target-file operand "
    + "exists in git apply, so never pass a trailing path\n"
    + "Then, and ONLY then, delete withheld added lines line-addressed — and locate every withheld span "
    + "by CONTENT ANCHOR plus an asserted line count, logging the numbers actually cut. Working-tree "
    + "coordinates are valid only for a working-tree-derived blob and the chain-head rule forbids that "
    + "start for every path in this commit.\n\n"
    + "THREE THINGS THAT ARE NOT IN TODAY'S DIFF AND ARE EASY TO MISS (section 8 spells them out):\n"
    + "  a) the authored decode-seam arm in xai-grok-sampling-types/src/conversation/responses.rs at the "
    + "HEAD :129 gap (the non-streaming seam), written together with its replacement comment as ONE unit "
    + "(ruling R-2). It is red-first code: write the test that fails before it exists.\n"
    + "  b) ruling R-1 FORBIDS authoring anything at xai-grok-sampler/src/stream/responses.rs:644. That "
    + "arm set belongs to hunk 4 / apex-waj.5, which is P0 BLOCKED, and two arms in one match is "
    + "unreachable_patterns, which no gate in this house can see. Hunk 4 stays dirty.\n"
    + "  c) the cargo-vendor-config.toml edit (deleting the stale git-source mapping lines the plan names) "
    + "on a file that is otherwise clean.\n\n"
    + "TEST WITNESS. client.rs hunk 4 (tool_search_call_frames_do_not_kill_the_turn) currently reads two "
    + ".sse captures by ABSOLUTE PATH from a DIFFERENT repo (the plans repo, whose .gitignore excludes "
    + "**/captures/), so it cannot run on any other checkout. Do NOT bank 281 KB of streams — that is "
    + "operator decision X1 and it is open. Instead write the two captured `tool_search_call` event frames "
    + "VERBATIM, byte for byte, into two new tracked fixtures named in the plan's section 8 "
    + "(xai-grok-sampler/tests/fixtures/…), rewire the test to those, and put a doc comment on the test "
    + "stating exactly what was subset and that the frame bytes themselves are unmodified. A subset stream "
    + "is acceptable here because the assertion is about those frames surviving the deserializer; a "
    + "simplified frame is NOT. Raw-key sweep the fixtures before you finish.\n\n"
    + "REMOVED-TYPE SWEEP. After assembly: `git -C <root> grep -c 'WebSearchToolCallStatus' -- crates` "
    + "must be ZERO, and `--all-targets` style references to it are seq 1's problem, not a later commit's. "
    + "If any survives, your blob set is wrong — say so in apply_failures rather than moving on.\n\n"
    + "Scratch worktree for the build phase: create it OUTSIDE the shared tree at " + SCRATCH + " with "
    + "`git -C " + WT + " worktree add --detach " + SCRATCH + " " + BASE + "`, then materialise your blobs "
    + "into it at their repo-relative paths. Do not commit anything, anywhere.\n\n"
    + "DELIVERABLE: " + OUT + "/BLOBS.tsv — one row per path: path, mode, blob sha, byte size, hunk keys "
    + "applied, withheld spans cut. authored_lines = the count of lines YOU authored (arm + comment + "
    + "fixtures + vendor edit), which must be reported separately from lines copied from the diff. "
    + HOUSE_RULES;

let asm = agent(asm_prompt, #{ label: "seq1-assemble", output_schema: schema_assemble(),
                              phase: "Assemble", capability_mode: "execute" });
if asm.success != true || asm.output == () || asm.output.assembled != true {
    let why = "no structured result";
    if asm.output != () { why = as_text(asm.output.notes); }
    pause("verification", "Assemble did not produce seq 1's tree: " + why
        + ". No build and no review are meaningful. Check apply_failures in " + OUT
        + " — a refused hunk means a @@ key moved, and per ow-050 a resume re-executes NOTHING: relaunch "
        + "a fresh run after the keys are re-derived.");
    throw("ASSEMBLE_INCOMPLETE");
}
log("assembled " + as_array(asm.output.paths_staged).len().to_string() + " paths, "
    + as_text(asm.output.authored_lines) + " authored lines, "
    + as_array(asm.output.apply_failures).len().to_string() + " apply failures");

// ---------------------------------------------------------------------------
// Phase 3 — Green. This is the measurement the three read-only rounds could not
// produce. The plan states its own limit: git plumbing dry runs "prove the blobs are
// the right shapes and prove nothing about type-checking".
// ---------------------------------------------------------------------------
phase("Green");

let green_prompt = "You are the Green seat for LANDING-PLAN seq 1. The tree exists at " + SCRATCH
    + " (assembled by a prior seat; its manifest is " + OUT + "/BLOBS.tsv — read it first and confirm the "
    + "tree on disk matches it before you build, because if it does not, nothing you measure is seq 1).\n\n"
    + "GOAL: make seq 1's tree type-check, then make its own tests run, and drive compile errors to "
    + "fixpoint. Every fix is a real edit to " + SCRATCH + " — never to " + WT + ".\n\n"
    + "Order, with a true exit status captured per step:\n"
    + "1. `cargo check --release -p xai-grok-sampling-types --lib` — the variant-free re-pin surface. "
    + "This is where `rs::FunctionTool` field additions and the `Option<String>` reasoning-id widening "
    + "either hold or do not.\n"
    + "2. `cargo check --release -p xai-grok-sampler --lib --tests`\n"
    + "3. `cargo check --release -p xai-grok-shell --tests --features xai-grok-shell/test-support` "
    + "— WITHOUT that feature flag you get 6 phantom E0425/E0432/E0282 that are not defects. Do NOT use "
    + "--all-targets: benches/session_list.rs carries a committed duplicate-field defect (apex-waj.63, "
    + "2x E0062, clean at " + BASE_SHORT + ") that `cargo test` never builds and that will "
    + "appear on every run whatever this commit does. Do not fix that bench in this cut.\n"
    + "4. `cargo check --release -p xai-chat-state -p xai-grok-config-types -p xai-grok-agent --tests`\n\n"
    + "Wrap each in `lockf -k -s -t 5400 " + LOCK + " <cmd>`. Set CARGO_TARGET_DIR to the shared "
    + "target dir under " + WT + " so the scratch worktree reuses the existing cache instead of "
    + "rebuilding 144 GiB cold — and re-check `df -g /Users` first; if free space dropped below 40 GiB, "
    + "STOP and report rather than starting a cold second target directory.\n\n"
    + "THE TWO THINGS SEQ 1 IS ALSO SUPPOSED TO PROVE, which a green build alone does not:\n"
    + "  - apex-waj.54 (the repair-bypass): `deserialize_response_event_for_dialect` returns "
    + "`Ok(Some(event))` from its FIRST match arm, so the normalise/repair pass that owns "
    + "`fill_missing_web_search_action` runs only on the Err path. Under 0.42.1 `WebSearchToolCall.action` "
    + "became optional, action-less frames now PARSE, the early return fires, and the sentinel is never "
    + "filled. That is why these two tests are red on the re-pin and green on the fork: "
    + "`ingress77_actionless_web_search_strict_parse_fatals_missing_action` and "
    + "`…_ingests_with_sentinel_action` (named in the sampler client tests). The re-pin and this fix are "
    + "ONE commit by ruling; if they are still red, seq 1 is not landable.\n"
    + "  - the decode-direction change the coordinator confirmed: the fork's `ReasoningItem.id` is a "
    + "required `String` with no serde default, and 0.42.1's is `Option<String>`, so a reasoning frame "
    + "with an absent or null `id` goes from FATAL PARSE to DECODING AS NONE and landing in the `xw_` "
    + "empty-id synthesis. A hermetic test pinning that (both shapes) ships with this commit. If the test "
    + "does not exist yet, write it red first.\n\n"
    + "Do NOT weaken, `#[ignore]`, or delete a test to reach green, and do not add a "
    + "`#[allow(unreachable_patterns)]`. If a fix would touch a path seq 1 does not own per the plan, stop "
    + "and name it instead of editing it. " + verdict_rule("green-check", OUT) + " " + HOUSE_RULES;

let green = agent(green_prompt, #{ label: "seq1-green", output_schema: schema_build(),
                                  phase: "Green", capability_mode: "execute" });
let green_ok = green.success == true && green.output != ()
    && green.output.check_rc == 0 && as_array(green.output.errors_remaining).len() == 0;
if green.success != true || green.output == () {
    pause("verification", "The Green seat returned nothing. The build may still be running in "
        + SCRATCH + " — check `pgrep -fl cargo` before relaunching, and never kill a build you did not "
        + "start.");
    throw("GREEN_UNREADABLE");
}
log("green: check_rc=" + as_text(green.output.check_rc) + " result_lines="
    + as_text(green.output.check_result_lines) + " errors_remaining="
    + as_array(green.output.errors_remaining).len().to_string() + " fix_rounds="
    + as_text(green.output.fix_rounds));
if green_ok != true {
    let errs = join_lines(as_array(green.output.errors_remaining), "\n  - ");
    pause("verification", "seq 1's tree does not type-check: rc=" + as_text(green.output.check_rc)
        + ", errors left:\n  - " + errs + "\n\nThis is the finding this run exists to produce — the plan "
        + "predicted the shape ('if a cargo check on seq 2's tree fails, the failure name tells us which "
        + "hunk belongs one commit earlier') but could not run it. Adjudicate each error name against the "
        + "plan's section 3 hunk table, amend the sequence, and relaunch. Per ow-050 a resume retries "
        + "nothing: journal replay returns cached failures, so relaunch a fresh run after amending.");
    throw("SEQ1_DOES_NOT_BUILD");
}

// ---------------------------------------------------------------------------
// Phase 4 — Gate. The six-package GATE, verbatim, against the adjudicated fail-set.
// ---------------------------------------------------------------------------
phase("Gate");

let KNOWN_FAILING = "stream::responses::tests::response_completed_cache_write_tokens_lands_in_token_usage,"
    + "session::worktree::tests::create_worktree_for_resume_honors_git_ref,"
    + "wake::ordinary_spawn_disposes_worktree_when_only_remote_settings_enable_snapshot,"
    + "wake::ordinary_spawn_with_failed_metadata_write_persists_output_and_disposes_worktree,"
    + "a_provider_that_declines_the_headless_run_returns_declined_without_touching_the_run,"
    + "agent::mvp_agent::tests::spawn_seeds_resolution_from_the_agent_definition,"
    + "workflow::registry::tests::…symlinked_session_root…,"
    + "responses_stream_reasoning_text_part_missing_text_completes_turn,"
    + "wake::rejected_deferred_start_restores_prior_without_publication,"
    + "wake::started_wake_with_failed_metadata_write_preserves_prior_durable_artifacts,"
    + "auto_wake_suppression_tests::cancel_barrier_…,"
    + "unified_list::tests::parse_list_req_forces_kind_…";

let gate_prompt = "You are the Gate seat. Run the standing GATE on the tree at " + SCRATCH + " and "
    + "classify every failure. No edits.\n\n"
    + "TREE FINGERPRINT FIRST, before the build: a content-sensitive fingerprint of the staged tree "
    + "(sha256 over `git -C " + SCRATCH + " diff --binary` plus the blob manifest), recorded in "
    + OUT + "/gate-fingerprint.txt. A check without a fingerprint proves little, because the tree moves "
    + "under it — two lanes in this campaign reported green that no second seat could reproduce.\n\n"
    + "THE GATE, exactly as written (env vars, the feature flag, --no-fail-fast, both --skip flags). "
    + "Run it with " + SCRATCH + " as cwd and CARGO_TARGET_DIR pointed at the shared target dir under "
    + WT + ", behind `lockf -k -s -t 5400 " + LOCK + "`:\n\n"
    + "CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_MIN_STACK=67108864 cargo test --release "
    + "-p xai-grok-sampler -p xai-grok-shell -p xai-grok-sampling-types -p xai-chat-state "
    + "-p xai-grok-config-types -p xai-grok-agent --features xai-grok-shell/test-support "
    + "--no-fail-fast -- --skip image_strip_tests "
    + "--skip cancellation_uses_run_owned_cancel_event_without_parent_detach\n\n"
    + "Long build: run it as the main command of a long-lived session and poll; do not launch it in a "
    + "background subshell that dies with its shell. Tee the log into " + OUT + "/gate.log.\n\n"
    + "Then classify. For EACH failing test name: run it ALONE (`cargo test --release -p <crate> "
    + "--features xai-grok-shell/test-support -- --exact <name> --nocapture`, same lock) and label it "
    + "pass-alone (flake or cross-test coupling) or fail-alone (real) or blocked. Only the "
    + "fail-alone names are candidates for NEW failures.\n\n"
    + "This is the adjudicated fail-set from the coordinator's last content-fingerprinted GATE "
    + "(dir /tmp/hts-wf/gate-20261001T162202Z, fingerprint 3be93539c42e, 58 'test result' lines, "
    + "9,360 passed, 14 named failures): " + KNOWN_FAILING + ". Anything NOT in that list is a NEW "
    + "failure and AGENTS.md section 5 says STOP and report — you do not get to decide a new red is a "
    + "flake without running it alone.\n\n"
    + "TWO NAMES ARE EXPECTED TO GO GREEN HERE AND GREEN IS THE POINT: "
    + "ingress77_actionless_web_search_strict_parse_fatals_missing_action and "
    + "…_ingests_with_sentinel_action (apex-waj.54). If they are red, seq 1 landed the bump without the "
    + "repair and must not be exfilled — say that in new_failures. Conversely "
    + "responses_stream_reasoning_text_part_missing_text_completes_turn is a known content-type fallout "
    + "name: if it moved, say so rather than folding it into the known set.\n\n"
    + "vacuous=true if the 'test result' count is 0 whatever the exit status says. "
    + verdict_rule("gate", OUT) + " " + HOUSE_RULES;

let gate = agent(gate_prompt, #{ label: "seq1-gate", output_schema: schema_gate(),
                                phase: "Gate", capability_mode: "execute" });
if gate.success != true || gate.output == () {
    pause("verification", "The Gate seat returned nothing while the GATE may still be running. "
        + "Check for a cargo process you did not start before relaunching; never kill a sibling build.");
    throw("GATE_UNREADABLE");
}
let gate_out = gate.output;
log("gate: rc=" + as_text(gate_out.gate_rc) + " result_lines=" + as_text(gate_out.test_result_lines)
    + " passed=" + as_text(gate_out.passed) + " fingerprint=" + as_text(gate_out.tree_fingerprint));
log("new failures: " + join_lines(as_array(gate_out.new_failures), "\n  - "));
if gate_out.vacuous == true || as_text(gate_out.test_result_lines) == "0" {
    pause("verification", "VACUOUS GATE: rc=" + as_text(gate_out.gate_rc)
        + " with zero '^test result' lines. No test binary was produced, which is the exact shape this "
        + "campaign has been burned by twice — once where a fabricated completion called a still-"
        + "compiling run 'passed', and once where it called a passing run 'failed'. Zero is BLOCKED. "
        + "Find the build error before anything reads this as a verdict.");
    throw("GATE_VACUOUS");
}
let gate_clean = as_array(gate_out.new_failures).len() == 0;
if gate_clean != true {
    pause("verification", "The GATE names NEW failures on seq 1's bytes: "
        + join_lines(as_array(gate_out.new_failures), "; ")
        + ". AGENTS.md section 5 is a STOP. The review phase below is still dispatched — its cost is "
        + "already committed — but nothing is exfillable until each new name is adjudicated first-hand "
        + "against source, not classified by a seat.");
}

// ---------------------------------------------------------------------------
// Phase 5/6 — Review loop. Three lenses, then fix, then a FRESH round. Applied is
// not reviewed: the coordinator's own edits are unverified until a new round says so.
// ---------------------------------------------------------------------------
phase("Review");

// Script-level lens text: Rhai functions cannot see the constants above, so
// the briefs are assembled here and the loop only supplies the per-round context.
let LENS_PRE = "You are a READ-ONLY review lens over LANDING-PLAN seq 1 as it was actually assembled. "
        + "Read-only: no edits anywhere, in " + SCRATCH + " or in " + WT + ".\n\n";
let LENS_POST = "EVIDENCE UNDER REVIEW: the staged tree at " + SCRATCH + ", the blob manifest "
        + OUT + "/BLOBS.tsv, the build and gate records in " + OUT + " (read the .rc files, not a summary), "
        + "and the plan " + PLAN + " (stamp " + STAMP_SHORT + ") with the coordinator's rulings "
        + ADJ + ", which override the plan wherever they differ.\n\n"
        + "RULES OF EVIDENCE. Quote the command and its real output for every finding, or say you derived "
        + "it and from where. Severity: Blocking = this commit must not be exfilled as it stands; Major = a "
        + "real defect that must be fixed before this commit ships; Minor = everything else. A finding you "
        + "cannot point at a path:line or a log line for is not a finding. Do NOT report a defect inherited "
        + "from another lane's uncommitted bytes as seq 1's — attribute it or drop it. If your lens ran to "
        + "completion, lens_complete=true; if you were cut off, say so, because a partial round is not a "
        + "clean round. " + HOUSE_RULES;
let LENS_ATTR = "\n\nYOUR LENS: MISATTRIBUTION AND FOREIGN CODE. This commit is staged as synthetic "
            + "blobs, which makes one failure mode cheap and catastrophic: a hunk that belongs to another "
            + "lane or another sequence arriving inside seq 1's provenance body. For every path in the "
            + "manifest, re-derive the diff `git -C " + WT + " diff " + BASE + " -- <path>` and check that "
            + "each hunk seq 1 staged is one the plan assigns to seq 1 by its pinned @@ key — and, equally, "
            + "that seq 1 did NOT silently drop a hunk it owns. Specifically hunt: (a) anything touching "
            + "xai-grok-sampler/src/stream/responses.rs around the streaming ToolSearchCall arm — ruling R-1 "
            + "forbids seq 1 authoring there and two arms in one match is unreachable_patterns, which no "
            + "gate in this house can see; (b) apex-23h's card-status test lines that an earlier revision of "
            + "this plan wrongly pulled into seq 1; (c) the three sibling-staged paths the plan names; "
            + "(d) whether the removed-type census (WebSearchToolCallStatus) is actually ZERO in the staged "
            + "tree — that is the coverage the plan achieves only incidentally and never names.";
let LENS_WIRE = "\n\nYOUR LENS: WIRE INVARIANTS (AGENTS.md section 6). Seq 1 is a dependency re-pin "
            + "that the coordinator has already ruled is NOT wire-neutral. Attack these: (a) the decode-"
            + "direction change — fork `ReasoningItem.id` required String with no serde default vs 0.42.1's "
            + "Option<String>, so an absent/null id stops being a fatal parse and becomes the `xw_` "
            + "empty-id synthesis; is that pinned by a committed hermetic test in this commit, and does the "
            + "test assert both shapes? (b) the fail-closed decode-seam arm seq 1 authors — does it preserve "
            + "HEAD's turn-killing behaviour, or did it quietly become a log-only arm (forbidden by "
            + "section 6.5)? (c) §6.6 mint domains — does any hunk rewrite the bytes of an opaque handle "
            + "(`rs_`, `xw_`, `encitem_`, `litellm_enc:`) instead of stripping or re-pinning? (d) pairing "
            + "survival and the no-empty-item-id rule. (e) is the newly subsetted test fixture byte-"
            + "identical in the FRAMES it keeps, and does its doc comment state honestly what was subset?";
let LENS_EVID = "\n\nYOUR LENS: EVIDENCE INTEGRITY AND FALSE GREEN. Two fabricated completions have "
        + "already been written up in this campaign, at the coordinator layer, so assume nothing. Check: "
        + "(a) does every .rc file cited exist on disk with the value quoted, and does every gate log "
        + "contain at least one '^test result' line — a nonzero exit beside zero such lines is BLOCKED, not "
        + "FAILED, and a 'green' with zero lines is vacuous? (b) does the tree fingerprint in the gate "
        + "record still match the tree on disk, recomputed by you? (c) is any test weakened, ignored, "
        + "deleted, or given a tolerant assertion to reach green? (d) are the two apex-waj.54 names actually "
        + "green in this run's own log, quoted, rather than asserted? (e) is the fixture raw-key sweep "
        + "recorded and empty? (f) did the run touch the shared tree outside the two permitted new fixture "
        + "paths — verify with `git -C " + WT + " status --porcelain` against the precheck's count.";

let rounds_max = 3;
let round = 0;
let review_clean = false;
let blocking = [];
let major = [];
let last_blocking = [];
let last_major = [];
let fix_notes = "no fix pass yet";

while round < rounds_max && review_clean != true {
    round = round + 1;
    phase("Review");
    log("review round " + round.to_string() + " of max " + rounds_max.to_string());
    let ctx = "Fix pass since the last round: " + fix_notes
        + "\nGate state (separate axis from severity — a red gate is not a review finding and vice "
        + "versa): rc=" + as_text(gate_out.gate_rc) + ", new failures="
        + join_lines(as_array(gate_out.new_failures), "; ");
    let jobs = [
        #{ label: "seq1-lens-attrib-r" + round.to_string(), prompt: LENS_PRE + ctx + LENS_POST + LENS_ATTR,
           output_schema: schema_findings(), phase: "Review", capability_mode: "read_only" },
        #{ label: "seq1-lens-wire-r" + round.to_string(), prompt: LENS_PRE + ctx + LENS_POST + LENS_WIRE,
           output_schema: schema_findings(), phase: "Review", capability_mode: "read_only" },
        #{ label: "seq1-lens-evid-r" + round.to_string(), prompt: LENS_PRE + ctx + LENS_POST + LENS_EVID,
           output_schema: schema_findings(), phase: "Review", capability_mode: "read_only" },
    ];
    let results = parallel(jobs);
    blocking = [];
    major = [];
    let partial = 0;
    for r in results {
        if r == () || r.success != true || r.output == () || r.output.findings == () {
            partial = partial + 1;
            continue;
        }
        if r.output.lens_complete != true { partial = partial + 1; }
        for f in as_array(r.output.findings) {
            let line = as_text(f.title) + " | evidence: " + as_text(f.evidence) + " | fix: " + as_text(f.fix);
            if f.severity == "Blocking" { blocking.push(line); }
            else if f.severity == "Major" { major.push(line); }
        }
    }
    last_blocking = blocking;
    last_major = major;
    log("round " + round.to_string() + ": " + blocking.len().to_string() + " Blocking, "
        + major.len().to_string() + " Major, " + partial.to_string() + " lenses partial or dead");
    if partial > 0 {
        log("ROUND NOT CLEAN ON PARSING ALONE: " + partial.to_string()
            + " lens(es) came back dead or partial. A partial round never counts as a clean round, "
            + "whatever the severities say.");
    }
    if blocking.len() == 0 && major.len() == 0 && partial == 0 {
        review_clean = true;
        break;
    }
    phase("Fix");
    let fix_prompt = "You are the Fix seat for LANDING-PLAN seq 1, round " + round.to_string() + ". "
        + "Apply these review findings to the staged tree at " + SCRATCH + " (and to the records in "
        + OUT + " where the finding is about evidence). Edit ONLY " + SCRATCH + " — never " + WT + ".\n\n"
        + "BLOCKING:\n  - " + join_lines(blocking, "\n  - ") + "\n\nMAJOR:\n  - "
        + join_lines(major, "\n  - ") + "\n\n"
        + "If a finding is wrong, say so in " + OUT + "/FIX-r" + round.to_string()
        + ".md with the command that disproves it — do not silently skip it, and do not 'fix' it by "
        + "weakening a test or by adding an allow attribute. Re-run the narrowest check that proves each "
        + "fix, behind the lock, and report the real exit status. Then state which findings you did NOT "
        + "fix and why. " + verdict_rule("fix-r" + round.to_string(), OUT) + " " + HOUSE_RULES;
    let fx = agent(fix_prompt, #{ label: "seq1-fix-r" + round.to_string(),
                                 output_schema: schema_build(), phase: "Fix",
                                 capability_mode: "execute" });
    if fx.success == true && fx.output != () {
        fix_notes = as_text(fx.output.notes);
    } else {
        fix_notes = "fixer returned no structured result — findings are UNFIXED and the next round will "
            + "re-report them";
    }
}

// ---------------------------------------------------------------------------
// Phase 7 — Report. No commit; the coordinator exfils with the private-index driver.
// ---------------------------------------------------------------------------
phase("Report");

let rep_prompt = "Write the exfil packet for LANDING-PLAN seq 1 to " + OUT + "/EXFIL-PACKET.md. You "
    + "change nothing; you assemble what the coordinator commits.\n\n"
    + "It must contain, each with the artifact that backs it:\n"
    + "1. The pathspec list with mode + blob sha + size per path (from " + OUT + "/BLOBS.tsv, re-read from "
    + "the staged tree, not from the manifest alone).\n"
    + "2. The tree fingerprint the GATE actually ran against, and whether it still matches.\n"
    + "3. Every gate number: rc, '^test result' count, passed count, the new-failure list, and the two "
    + "apex-waj.54 names with their quoted status in this run's log.\n"
    + "4. A provenance body draft for the commit: donor/upstream refs, bead ids this commit closes or "
    + "advances (apex-6c6, apex-md5, apex-waj.53, apex-waj.54, apex-waj.71, apex-99sf), the gates run, and "
    + "in plain sight the two consequences the coordinator ruled and accepted: the fatal-to-silent-drop "
    + "inversion on the NON-streaming seam, and the streaming seam deliberately left as `_ => {}` because "
    + "that arm belongs to apex-waj.5 (P0 BLOCKED) — recording that the window is latent because admission "
    + "has no production producer, so no provider can mint the frame it would drop.\n"
    + "5. still_owed: everything this commit does NOT deliver, stated without euphemism. It must include "
    + "that after seq 1 AND seq 2 the operator still cannot test native backend search on either wire, "
    + "because `SearchAdmission` has no production construction and `extra_tool_entries_with_declaration` "
    + "is called only from a test — see the coordinator's finding N3.\n"
    + "6. The final review state: rounds run, per-round Blocking/Major counts, partial-lens counts, and "
    + "the exact residual findings if the loop never went clean.\n"
    + "7. The scratch worktree cleanup command for after exfil "
    + "(`git -C " + WT + " worktree remove " + SCRATCH + "`); explicitly note the shared tree was never "
    + "staged and that the precheck's dirty count and the final count match or differ by exactly the two "
    + "fixture paths.\n\n"
    + "Also produce the commit message body as its own file, " + OUT + "/COMMIT-BODY.txt, ready for "
    + "`git commit-tree -F`. " + HOUSE_RULES;

let rep = agent(rep_prompt, #{ label: "seq1-report", output_schema: schema_report(),
                              phase: "Report", capability_mode: "execute" });
if rep.success != true || rep.output == () {
    log("report seat returned nothing; the raw records are still in " + OUT + " for the coordinator");
    throw("REPORT_UNREADABLE");
}
log("packet: " + as_text(rep.output.packet_path));
log("pathspec (" + as_array(rep.output.pathspec).len().to_string() + " paths): "
    + join_lines(as_array(rep.output.pathspec), "\n  "));
log("still owed: " + join_lines(as_array(rep.output.still_owed), "\n  - "));
log("review state after " + round.to_string() + " round(s): clean=" + review_clean.to_string()
    + " residual_blocking=" + last_blocking.len().to_string()
    + " residual_major=" + last_major.len().to_string());

let verdict = "READY_FOR_COORDINATOR_EXFIL";
if review_clean != true { verdict = "REVIEW_NOT_CLEAN"; }
if gate_clean != true { verdict = "GATE_NEW_FAILURES"; }
log("verdict: " + verdict + " — nothing was committed; the coordinator exfils on a private index and "
    + "must re-verify every number in the packet first-hand before doing so.");
