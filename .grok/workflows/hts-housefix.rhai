// hts-housefix — close the four named residuals in the workflow house, one item at a time.
// Board rows: ow-039 (record-lint vocabulary + assemble cwd trap + hts-merge /tmp records),
// ow-037 (promote the general drivers, re-audit). Coordinator, 2026-09-30.
//
// Constraints this driver obeys, because each one is a scar:
//   - no `var`, no ternary, no banned host calls; NO workflow-level `let` inside an `fn` body
//     (invisible there — BP-57), every call site's arity equals its signature;
//   - one item at a time, and an item is not done until a fresh seat re-runs the commands instead
//     of reading the implementer's summary;
//   - never cargo, never a git write command: a sibling lane holds the build lock and has files
//     staged in a shared index (BP-56);
//   - records to the durable root, scratch to /tmp (D-33).

let meta = #{
  name: "hts-housefix",
  description: "Close the four residuals the corpus audit left open: make the record lint extension-first and wire the adversarial fixture into assemble.zsh's selftest, give assemble.zsh a cwd guard, move hts-merge's records off /tmp, then generalise and promote the drivers that are genuinely reusable.",
  when_to_use: "Board rows ow-039 and ow-037. Run when the workflow house itself is the work: a corpus audit has named concrete defects and they must land one at a time with a review each.",
  phases: [
    #{ title: "Item1-Lint", detail: "record_scan becomes extension-first; the adversarial fixture becomes a selftest case with an exact hit set" },
    #{ title: "Item2-Cwd", detail: "assemble.zsh resolves its own directory; running it from the campaign root must work" },
    #{ title: "Item3-Merge", detail: "hts-merge's record path leaves /tmp; re-bank and re-audit" },
    #{ title: "Item4-Promote", detail: "parameterise the general drivers' roots, promote what qualifies, re-run both batteries" },
    #{ title: "Report", detail: "what changed, what the batteries now say, what is still open" },
  ],
};

let WF = "/Users/palanisd/Projects/upstream/grok/plans/harness/workflow-design";
let EVID = WF + "/runs/housefix";
let SCRATCH = "/tmp/hts-housefix";
let REG_USER = "/Users/palanisd/.grok/workflows";
let REG_PROJ = "/Users/palanisd/Projects/upstream/wt/grok-build-responses/.grok/workflows";
let AUDIT = "python3 " + WF + "/tools/rhai_corpus_audit.py";

// COMMON is built once at top level and passed IN to every function: an `fn` body cannot see a
// workflow-level `let`, which is what killed hts-evidlint at runtime after validate passed.
let COMMON = "GROUND RULES. Work only in " + WF + " (the plans repo) and the two workflow registries ("
  + REG_USER + " and " + REG_PROJ + "). Do NOT run cargo. Do NOT run any git write command (no add, "
  + "commit, checkout, reset, stash, clean) - a sibling lane holds the build lock and has product files "
  + "staged in the shared index, and the coordinator is the one who commits. Do NOT delete anything; if "
  + "a file must move, move it under " + SCRATCH + ". Scratch goes under " + SCRATCH + ", durable "
  + "records under " + EVID + ". Never quote secret material: before quoting a log or capture run "
  + "`grep -rEil 'sk-[A0-9a-z]{16,}' <paths>` and expect 0 hits. Every claim you return must be a "
  + "command you actually ran plus its decisive output; if you could not run it, say NOT-REPRODUCIBLE "
  + "rather than reasoning about what it would print.\n\n";

let SCHEMA_IMPL = #{
  "type": "object",
  "properties": #{
    "changed": #{ "type": "array", "maxItems": 12, "items": #{ "type": "string" },
                  "description": "file:line for each edit" },
    "commands": #{ "type": "array", "maxItems": 16, "items": #{ "type": "string" },
                   "description": "each entry: the exact command, then ' -> ', then the decisive output tokens" },
    "notes": #{ "type": "string" },
    "verdict": #{ "type": "string", "enum": ["done", "blocked"] },
  },
  "required": ["changed", "commands", "notes", "verdict"],
};

let SCHEMA_REV = #{
  "type": "object",
  "properties": #{
    "findings": #{ "type": "array", "maxItems": 10, "items": #{ "type": "string" },
                   "description": "flat, shaped exactly: [Blocking] claim ... || evidence: file:line + the command output that shows it || fix: ...  Severity tag first, one of Blocking / Major / Minor. Return an empty array if nothing rises to Blocking or Major - do not manufacture findings to look thorough." },
    "lens": #{ "type": "string" },
    "verdict": #{ "type": "string", "enum": ["ship", "fix-first"] },
    "notes": #{ "type": "string" },
  },
  "required": ["findings", "lens", "verdict", "notes"],
};

fn usable(res) {
  if res == () { false; }
  else if type_of(res) != "map" { false; }
  else if res.success != true { false; }
  else { res.output != () && type_of(res.output) == "map"; }
}

fn text_of(res, key, fallback) {
  if usable(res) && res.output[key] != () { res.output[key].to_string(); }
  else { fallback; }
}

// arrays have no join() on this host (measured), so concatenate explicitly
fn lines(items, prefix) {
  let acc = "";
  if items != () && type_of(items) == "array" {
    for it in items { acc = acc + prefix + it.to_string() + "\n"; }
  }
  if acc == "" { "(none)"; }
  else { acc; }
}

fn findings_from(res) {
  let acc = [];
  if usable(res) && res.output.findings != () {
    for f in res.output.findings { acc.push(f.to_string()); }
  }
  acc;
}

fn changed_of(res) {
  let acc = "";
  if usable(res) && res.output.changed != () && type_of(res.output.changed) == "array" {
    for c in res.output.changed { acc = acc + "- " + c.to_string() + "\n"; }
  }
  if acc == "" { "(none reported)"; }
  else { acc; }
}

fn is_blocking(f) {
  f.contains("[Blocking]");
}

fn review_item(tag, brief, phase_name, common, schema) {
  let lens_a = "LENS 1 - does it actually do what it claims. Re-run the implementer's commands yourself "
    + "and compare the real output against its report. Then attack it: for a lint, invent two shapes it "
    + "has never seen and run them; for a path change, prove the new path is written and the old one is "
    + "not; for a guard, run the script from the directory that used to fail. A regression in the OTHER "
    + "direction is Blocking: a record lint that now flags legal scratch (the cargo lock, OUT_LOCAL, "
    + "/tmp/out.txt) bricks every build, and a generator that no longer assembles is worse than a lint "
    + "with a gap. Read-only - do not edit anything.\n\n";
  let lens_b = "LENS 2 - blast radius and artifact truthfulness. Which other files now assert something "
    + "this change made false (BEST-PRACTICES, DECISIONS, REGISTER, STATUS and the board rows all quote "
    + "line numbers, digests and hit counts that move)? Did the change touch anything outside its stated "
    + "allowlist, in the plans repo or the registries? Is any number in a report or commit-body line now "
    + "unre-derivable from the command printed beside it? Read-only.\n\n";
  let out = [];
  for lens in [lens_a, lens_b] {
    let res = agent(common + brief + lens, #{ label: tag, phase: phase_name, output_schema: schema });
    for f in findings_from(res) { out.push(f); }
  }
  out;
}

let SUMMARY = "THE CHANGE UNDER REVIEW is on disk, made by an implementer seat. Its own report follows; "
  + "trust the commands you re-run, not the summary.\n";

// ---- item 1

phase("Item1-Lint");
let b1 = "TASK. " + WF + "/scaffold/assemble.zsh defines record_scan(), which refuses to assemble a "
  + "driver that writes a RECORD under /tmp. It currently recognises a record only when the line names "
  + "one of six nouns (report|process|baseline|artifact|finding|verification). The coordinator's "
  + "adversarial fixture at scaffold/selftest-fixtures/record-tmp-adversarial.rhai proves by execution "
  + "that it MISSES `ledger.md`, `summary.json`, `notes.jsonl` and a bare `/tmp` record dir, while "
  + "producing zero false positives today. Make the predicate extension-first: a string literal "
  + "containing a /tmp path whose basename ends in .md/.markdown/.json/.jsonl/.patch/.diff is a record "
  + "unless the line carries `lint-allow:scratch-tmp`; keep .txt/.log/extension-less LEGAL (the standing "
  + "controls /tmp/out.txt and the clippy log depend on them); keep the existing six-noun rules so "
  + "nothing previously caught becomes unclosed. Then wire that fixture into the `selftest` path with its "
  + "own exact hit set and state in your report which hit set you asserted (the file's record-shaped "
  + "lines are 21-26; before your change only 21 and 22 hit). Prove BOTH directions: "
  + "`cd " + WF + "/scaffold && zsh assemble.zsh selftest` exits 0, and the four previously-missed "
  + "shapes are now hits with the five control lines still clean. Do not modify any driver.\n\n";
let i1 = agent(COMMON + b1, #{ label: "item1-implement", phase: "Item1-Lint", output_schema: SCHEMA_IMPL });
log("item1: " + text_of(i1, "verdict", "no-verdict") + " | " + text_of(i1, "notes", "no notes"));
let f1 = review_item("item1-review", SUMMARY + text_of(i1, "notes", "") + "\nChanged:\n" + changed_of(i1), "Item1-Lint", COMMON, SCHEMA_REV);
log("item1 findings:\n" + lines(f1, "  "));

// ---- item 2

phase("Item2-Cwd");
let b2 = "TASK. " + WF + "/scaffold/assemble.zsh resolves its snippets by BARE filename (`cat "
  + "scaffold-helpers.snippet.rhai`), so `zsh assemble.zsh` run from the campaign root exits 1 with "
  + "`cat: scaffold-helpers.snippet.rhai: No such file or directory` - which reads exactly like 'the "
  + "generator is broken' and already cost the coordinator a false finding. Give the script a "
  + "self-locating guard so it works from any cwd, including the documented `cd scaffold/` form, and "
  + "make sure the paths it WRITES (the six drivers into /Users/palanisd/.grok/workflows) are absolute "
  + "or recomputed from that guard, never relative to the caller. Prove it three ways: run it from " + WF
  + ", from " + WF + "/scaffold, and from " + SCRATCH + "; all three exit 0 and the six generated "
  + "drivers are byte-identical across the three runs (sha256 them and show it). Also make a missing "
  + "snippet fail with a message naming the directory it looked in, instead of a bare cat error. Do not "
  + "touch any lint predicate.\n\n";
let i2 = agent(COMMON + b2, #{ label: "item2-implement", phase: "Item2-Cwd", output_schema: SCHEMA_IMPL });
log("item2: " + text_of(i2, "verdict", "no-verdict") + " | " + text_of(i2, "notes", "no notes"));
let f2 = review_item("item2-review", SUMMARY + text_of(i2, "notes", "") + "\n", "Item2-Cwd", COMMON, SCHEMA_REV);
log("item2 findings:\n" + lines(f2, "  "));

// ---- item 3

phase("Item3-Merge");
let b3 = "TASK. " + REG_USER + "/hts-merge.rhai is the only driver in a live registry that writes its "
  + "RECORDS under /tmp (`let OUT = \"/tmp/hts-wf/merge\"`) - the defect DECISIONS D-33 exists to "
  + "prevent, because a lane resumed after a reboot has lost its ledger. Point its durable records at "
  + WF + "/runs/<slug>, mirroring how the six generated drivers do it (read scaffold-rules.snippet.rhai "
  + "and scaffold-phases1.snippet.rhai for the WF / EVID_ROOT / EVID convention FIRST and copy it), and "
  + "leave genuinely scratch material on /tmp. Update its banked copy at " + WF
  + "/library/live-drivers/hts-merge.rhai to match, and shasum both sides to prove byte-identity. Then "
  + "show `" + AUDIT + " --live` no longer reports a tmp-record error for it, and `" + AUDIT
  + " --selftest` still reports 8/8. Do not promote, rename or delete anything.\n\n";
let i3 = agent(COMMON + b3, #{ label: "item3-implement", phase: "Item3-Merge", output_schema: SCHEMA_IMPL });
log("item3: " + text_of(i3, "verdict", "no-verdict") + " | " + text_of(i3, "notes", "no notes"));
let f3 = review_item("item3-review", SUMMARY + text_of(i3, "notes", "") + "\n", "Item3-Merge", COMMON, SCHEMA_REV);
log("item3 findings:\n" + lines(f3, "  "));

// ---- item 4

phase("Item4-Promote");
let b4 = "TASK. The corpus audit scored 135 .rhai files and surfaced three genuinely general candidates "
  + "- machinery with no campaign-specific tokens and no baked machine paths: hts-deep-research.rhai "
  + "(596 lines, complexity 5/6, present in BOTH the project registry and the bank), "
  + "apex-review-round.rhai (205 lines, 4/6, user registry) and review-current-branch.rhai (611 lines, "
  + "4/6, stranded in /Users/palanisd/Projects/upstream/grok/hyper-grok-build/.grok/workflows). Decide "
  + "per driver one of PROMOTE / KEEP / BANK-ONLY against this rule: a driver belongs in " + REG_USER
  + " only if a session that is NOT this campaign could run it, which means its roots arrive as args or "
  + "parameters rather than literals naming these machines. For each you promote, make the minimal "
  + "parameterisation that achieves that (read args, document a default), re-run `" + AUDIT + "` and "
  + "record its verdict for that file, then copy it into the user registry byte-identically and shasum "
  + "both sides - no cargo, the engine validator is off limits here because it is a cargo invocation. "
  + "Prove you introduced no shadowing: `comm -12 <(ls " + REG_PROJ + "/*.rhai | xargs -n1 basename "
  + "| sort) <(ls " + REG_USER + "/*.rhai | xargs -n1 basename | sort)` must print nothing. Record the "
  + "choice AND the reason for all three, including any you refuse - a refusal with a reason is a "
  + "result. Update the REGISTER row in the EH-007 area with the outcome. Do not touch the six generated "
  + "bead drivers or any product code.\n\n";
let i4 = agent(COMMON + b4, #{ label: "item4-promote", phase: "Item4-Promote", output_schema: SCHEMA_IMPL });
log("item4: " + text_of(i4, "verdict", "no-verdict") + " | " + text_of(i4, "notes", "no notes"));
let f4 = review_item("item4-review", SUMMARY + text_of(i4, "notes", "") + "\n", "Item4-Promote", COMMON, SCHEMA_REV);
log("item4 findings:\n" + lines(f4, "  "));

// ---- report

phase("Report");
let allf = [];
for f in f1 { allf.push("[item1] " + f); }
for f in f2 { allf.push("[item2] " + f); }
for f in f3 { allf.push("[item3] " + f); }
for f in f4 { allf.push("[item4] " + f); }

let blocking = [];
for f in allf {
  if is_blocking(f) { blocking.push(f); }
}

let rp = agent(COMMON + "TASK. Write this run's durable report to " + EVID
  + "/REPORT.md and return its path in `notes` plus a one-paragraph verdict. Per item include: what "
  + "changed (file:line), the commands the review seats actually re-ran with their real outputs, and the "
  + "open residue. Close with a 'what the batteries now say' section quoting the current output of `"
  + AUDIT + "` and of `zsh assemble.zsh selftest`, and a 'NOT committed' section stating that the "
  + "coordinator commits. Review-lens findings verbatim:\n" + lines(allf, "") + "\n",
  #{ label: "report", phase: "Report", output_schema: SCHEMA_IMPL });

log("report: " + text_of(rp, "notes", "no report path"));

if blocking.len() > 0 {
  return "FIX-FIRST: " + blocking.len().to_string()
    + " Blocking finding(s) survive, so nothing is ready to commit. " + lines(blocking, "\n  ");
}
return "READY-TO-COMMIT | all four items reviewed with no Blocking finding | coordinator MUST re-run "
  + "`" + AUDIT + "` and `zsh assemble.zsh selftest` from BOTH cwds, then commit harness/workflow-design "
  + "plus the registries pathspec-limited (the plans repo has no remote: a checkpoint, not an exfil). "
  + "Findings for the commit body:\n" + lines(allf, "  ");
