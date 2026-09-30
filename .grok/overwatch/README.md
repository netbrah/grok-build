# `.grok/overwatch/` — coordinator state for this project

Project-scoped, deliberately **inside a git repository** — `.grok/` here is currently untracked
but committable, so these files *can* be put under version control and then reviewed, blamed and
reverted; `plans/` is not a repository at all, which is why campaign state there has no history
whatsoever. Ported from prime-qwen's `~/.prime/qwen-agent/harness/` shape; full analysis and the
reasons for each choice are in
`/Users/palanisd/Projects/upstream/grok/plans/harness/workflow-design/designs/OVERWATCH.md`.

Three files, all **append-only JSONL** — never rewrite the file, append a line that supersedes,
so history is the data:

## `TASKS.jsonl` — the coordinator's board (layer 1)

One line per task. The coordinator reads this before deciding what to run next; it is the
source of truth about what is next, not the live run list, because a workflow run can be
`blocked` and still hold a slot.

```json
{"id":"ow-001","title":"fingerprint gate cannot read CC5/CC4","bead":"apex-x2n","owner":"coordinator","workflow":"sdd-implement","worktree":"wt/apex-ayl-hosted-tool-search","budget":70,"state":"candidate","evidence":["/tmp/hts-wf/x2n/baseline.md"],"next_action":"author a continuation script from the banked baseline; do not resume (resumes went 0-for-4)","ts":"2026-09-29T21:50:00Z"}
```

`state`: `ready | running | candidate | verifying | blocked | done`.
`evidence` is mandatory for `candidate` or better. `next_action` is mandatory **always**, in one
imperative sentence — a task left without one is how a lane stalls unnoticed.
**There is no `route` field.** Model selection is the operator's call, made at delegation; the
board does not encode models, and `AGENTS.md` §15 records why the routing table was removed on
2026-09-30. Rows written before that date carry a `route` key — it is **vestigial and must be
ignored**, not migrated, because `TASKS.jsonl` is append-only and history is never rewritten. If
you need to know what model actually ran something, read it from the harness
(`<session>/subagents/<id>/meta.json` → `effective_model_id`), not from the board.

## `verifier-queue.jsonl` — scarce, expensive checks (layer 3)

Serialised by definition: one entry runs at a time, because these are the checks that cannot be
parallelised across a shared `target/` or a live provider.

```json
{"id":"v-014","command":"cargo test --release -p xai-grok-sampler …","config":"GATE as written in AGENTS.md §5","commit":"ff2351c5","tree_fingerprint":"sha256:12 of git status --porcelain","trace":"/tmp/hts-wf/waj2/gate.log","outcome":"PASS","human_reviewed":false,"requested_by":"ow-004"}
```

**A worker's `done` means *candidate ready for verification*, not GREEN.** A check with no
`tree_fingerprint` proves little: the tree moved under it. That field exists because two lanes
once reported green that no second seat could reproduce.

## `refinements.jsonl` — the harness layer, H = (ρ, G, K, M)

One line per applied refinement. This is the only file that lets self-improvement be
evidence-backed and revertable.

```json
{"id":"refine_20260929215500","summary":"add seat-class routing","rationale":"operator direction","expectedOutcome":"fan-outs classify by thinking-kind; nested seats stay on the workhorse","appliedEdits":[{"action":"update","kind":"prompt","id":"seat_routing","path":"delegation/model-routing","before":{"content":"…prior text…"},"after":{"content":"…new text…"},"applied":true}],"scope":"project"}
```

`kind`: `prompt | subagent | skill | memory`. **`before` must carry the full prior content** —
that is what makes revert-by-ID real without a version-control system, and it is the reason an
apply is allowed here at all. An edit recorded without `before` cannot be undone and must be
refused.

`refine` proposes; **the coordinator applies.** A workflow child may write a proposal but must
never write this file, and nothing here may alter the base system prompt, the GATE, or a live
run's source.

## Why there is no overwatch workflow

Child sessions are denied the workflow tool, so a spawned agent cannot start a workflow. The
orchestrator is therefore the coordinator session itself, kept small on purpose: it reads a
queue and one file of state. Workflows are the workers and the verifier, not the boss.
