# smoke/eval_harness — federated evaluation harness

First-slice infrastructure for the APEX zero-config deploy campaign
(Bead `apex-ayl.137`). The normative design lives outside the worktree:
`plans/eval-harness/DESIGN.md`. Python 3.14 **standard library only**.

## Package layout (canonical names)

```
smoke/eval_harness/
  __init__.py
  README.md
  catalog.json                      # hand-maintained strict registry
  schemas/
    catalog.schema.json             # normative registry schema (2020-12)
    evidence-index.schema.json      # normative index schema (2020-12)
  contract.py                       # validators, canonical JSON, sensitive scan
  paths.py                          # RootedPath, resolve_rooted, normalize_path_token
  inventory.py                      # scope walk, git/generation state, digests
  adapters/
    __init__.py
    plan_corpus.py                  # metadata-only plans-corpus adapter
    formalism.py                    # rules/EV/arms/categories -> typed entities
  tests/
    __init__.py
    fixtures/                       # synthetic catalogs and corpus sandboxes
    test_catalog.py
    test_paths.py
    test_inventory.py
    test_formalism.py
  report/                           # ignored, local raw benchmark evidence
```

`index.py`, `benchmark.py`, `browser_gate.py`, `lifecycle.py`, `site/` and
the remaining eight adapters are later tasks and are **not present yet**.

Every module is imported only by its canonical `smoke.eval_harness...`
package name. Directly executable entry scripts (added by later tasks)
bootstrap the repository root into `sys.path` only when `__package__` is
empty, then use the same absolute package imports; they never import a
sibling as top-level `contract`, `paths`, or `adapters`.

## Contracts (`contract.py`)

- `ContractError` is the **sole** validation exception of the harness.
- `validate_catalog(value)` / `validate_evidence_index(value)` return `None`
  or raise `ContractError`. The JSON Schema documents under `schemas/` are
  normative; `contract.py` implements the same used subset of JSON Schema
  2020-12 without requiring `jsonschema`. When `jsonschema` is importable,
  the tests run both validators over the fixture battery and require
  identical accept/reject decisions. Cross-item key uniqueness (duplicate
  component/root/scope ids with distinct row bodies) is a design rule JSON
  Schema cannot express; the standard-library validator enforces it
  additionally.
- `canonical_json_bytes(value)`: sorted keys, two-space indentation, UTF-8,
  exactly one trailing newline.
- `canonical_locator_key(locator)`: canonical compact JSON of a rooted
  source locator; an absent locator is the canonical empty object `{}`.
- `sensitive_matches(data)`: scans for the pinned raw-key expression
  `sk-[A0-9a-z]{16,}` and the canary `canary-key-DO-NOT-LEAK-0123456789`;
  returns only `{"pattern", "offset"}` records — never matched secret text.
- `AdapterContext(roots, catalog)` and immutable
  `AdapterResult(entities, relationships, runs, findings)` carry the
  adapter protocol of DESIGN.md §4.

## Path primitives (`paths.py`)

- `RootedPath(root, path)` / `RootedPath.parse(token)` accept only
  `ROOT:POSIX_RELATIVE_PATH`: lowercase-slug root, forward slashes only,
  non-empty segments, no `.`/`..` segments.
- `resolve_rooted(roots, rooted, *, require_regular=False)` enforces
  lexical containment without following inventory symlinks (no
  `resolve()`); with `require_regular=True` the final entry must be a
  regular file per `lstat` — a symlink is not regular and its target is
  never opened.
- `normalize_path_token(token, roots)`: relative tokens (symlink strings)
  are returned verbatim; an absolute token beneath a supplied named root
  becomes `@<root>/<relative-path>` (longest matching root prefix wins;
  equal-length aliases are a hard error); every other token becomes
  `@external/<basename>#<full-SHA-256-of-the-original-token>`. Purely
  lexical — nothing is resolved, opened, or followed.

## The catalog (`catalog.json`)

Strict, versioned (`schema_version: 1`), hand-maintained. It contains
exactly the six named roots, the three inventory scopes, and the twelve
component rows (`parity-formalism`, `formalism-linter`, `provenance`,
`xwire`, `redteam`, `wstream`, `xwfix`, `wiretap`, `lifecycle`, `dogfood`,
`parity-repro`, `session-triage`). Machine paths are supplied at
invocation; catalog rows never contain user-specific absolute paths.

- `entrypoints`, `inputs`, and `owner_docs` are paths relative to their
  declared named root; an `inputs` item may be a `ROOT:RELATIVE` token
  naming any declared root (a token naming an undeclared root fails).
  `outputs` are POSIX globs relative to the explicit run root selected
  through a lifecycle record or `--run` — never resolved by the base build.
- Owner-document headings match the normalized heading text exactly after
  stripping Markdown heading markers and surrounding whitespace; no
  substring or fuzzy matching.
- `curated_sources` names every allowed curated Markdown selector:
  `{"kind": "heading", "text": ...}` (exact normalized heading),
  `{"kind": "question", "marker": ...}` (the content of the `**Q: ... **`
  bold marker, with the bold delimiters stripped and internal whitespace
  collapsed), and `{"kind": "citations"}`
  (only the document-anchor headings cited for that file in the formalism
  citation namespace).
- Selftest entrypoints carry a fixed `argv` template. A template element
  may begin with the design's placeholder for a supplied named root —
  `<plans-root>` for the `plans` root, i.e. the root id followed by
  `-root` in angle brackets — which `health` replaces with the supplied
  named-root path, keeping the rest of the element as one argv element. No
  catalog value ever becomes a shell command.
- A component without lifecycle registration says so explicitly with
  `{"registry": null, "reason": ...}` — a fact, not a validation failure.
- `install` is `null` for every v1 row: installed copies are expressed as
  `role: "installed"` entrypoints under their optional named roots, and the
  verifiable absence of any in-tree install-manifest contract is reported
  by health, never invented.

## Corpus inventory (`inventory.py`)

- `inventory_scope(ctx, scope_id)` and `inventory_all(ctx)` return snapshots
  whose `records` are sorted by `(root, path)` within the scope, and whose
  `sha256` digests the canonical record array (DESIGN.md §3).
  `inventory_all` walks the scopes in scope-ID order, so the concatenation
  is the design's `(scope, root, path)` order.
- The `plans-corpus` scope is declared to walk `parity-formalism/`,
  `provenance/` and `xwire/` completely — regular files and symlinks,
  tracked, untracked and ignored alike. Symlinks inside the walk are never
  opened or resolved: the link string is read with `readlink` twice,
  bracketed by `lstat`. Regular files are opened with `O_NOFOLLOW`, so a
  path that became a symlink mid-scan is refused rather than followed. A
  scope *include* must itself be a real directory: a symlinked include is
  a hard `include-not-directory` `ContractError`. The live plans estate put
  its campaign entries behind symlinks on 2026-09-24 (plans commit
  `5839c1f`, Bead `apex-ayl.137.5`), so this scope currently fails loud
  instead of walking; the two declared-input scopes still scan.
- Every regular file is hashed between a pre-`lstat` and a post-`lstat`
  with a mid-scan `fstat`; any change in identity, size or mtime — or a
  changed symlink string, or a vanished entry — raises
  `SourceChangedError`. Callers re-scan; a partial snapshot is never
  published.
- `git_state` (five values: `tracked-clean`, `tracked-modified`,
  `untracked`, `ignored`, `outside-repository`) and `generation_state`
  (`source-authored`, `declared-generated`, `unknown`) are per-record
  facts. Git state is assembled from four NUL-delimited read-only Git
  queries: `ls-files -z` gives the tracked set, `status --porcelain=v2 -z`
  gives the changed-tracked and `?`-untracked sets,
  `ls-files -z --others --exclude-standard` gives untracked, and
  `ls-files -z --others --ignored --exclude-standard` gives ignored; the
  directory-collapsed `!` status records are discarded in favour of the
  per-file `ls-files` answer. A declared required source entrypoint, or a
  declared generated target, that resolves to nothing is a hard
  `ContractError` (DESIGN.md §8), scoped to the one scope that owns it.
- `content_policy` is derived from the catalog allowlist, never from file
  content. The inventory layer parses no document text at all.

## Adapters landed so far

- `plan_corpus` runs the `plans-corpus` scope so that a source change
  during the scan fails the adapter call, and emits no entities and no
  records of its own: the writer is required to pull inventory records
  through `inventory_scope` / `inventory_all` at the writer boundary
  (DESIGN.md §4). That duty binds a future module — `index.py` has not
  landed, so nothing enforces it yet — and the adapter currently discards
  the snapshot it computed, which is a second full corpus scan per build
  with no cross-check against the records the writer publishes.
  Any `requested_runs` is rejected — the corpus is a base scope.
- `formalism` parses the 10 Python-linter rules plus H-7's artifact-override
  row as one rule with three enforcement-layer states and the two retained
  superseded H-2/H-5 clauses (13 rule entities), 14 EV records, 3 boundary
  classes, the expected-verdict arms, request expectations and
  adjudications, the native `meta.json.kind` enum (preserved exactly,
  never collapsed), the C1–C7 categories, generated-rule artifacts,
  `OQ-*` references and supersession links into typed entities plus
  `defines` / `cites` / `belongs-to` / `supersedes` / `generated-from` /
  `enforced-by` relationships — six of the twelve relationship kinds
  DESIGN.md §3 requires; `checks`, `exercises`, `produced-by`,
  `captured-by`, `reported-by` and `references` are emitted by nothing yet.
  On the live corpus an `OQ-*` reference is *validated* against the
  `HARDENING-SPEC.md` §2.3 registry and emits one `cites` edge to that
  section (identity carried by the edge locator, not an OQ-typed edge or
  entity); the typed `open-question-id` citation shape exists in the
  citation resolver but no live corpus value currently reaches it.
  `discover()` invokes no linter: it imports only the rule generator, under
  forced `sys.dont_write_bytecode`, and the byte-for-byte generated-rule
  coupling check is a pure derive-and-compare with no subprocess.
- Requirements and defects not owned by landed code are tracked in Beads
  rather than here, under `apex-ayl.137`: `.137.1` (builder-derived
  cardinality gate), `.137.2` (RULE-DRIFT channel), `.137.3` (health
  selftest exposure), `.137.4` (the `source_dir` free-text delta awaiting
  adjudication), `.137.5` (the live plans corpus is symlinked, so the
  `plans-corpus` scope fails loud), `.137.6` (three read paths follow
  symlinked components out of the declared root), `.137.7` (Git state
  assumes the root is the repository top-level), `.137.8` (graph edge and
  citation coverage versus DESIGN §3/§6 — operator ruling open), and
  `.137.9`–`.137.11` (test-estate self-containment, skip reporting, and the
  five still-unowned DESIGN clauses) and `.137.12` (the collected
  non-blocking minors).

## Planned command line (later tasks — not yet implemented)

The intended interface, from DESIGN.md §5, from the worktree root:

```text
python3.14 smoke/eval_harness/index.py validate \
  --plans-root ../../grok/plans

python3.14 smoke/eval_harness/index.py build \
  --plans-root ../../grok/plans \
  --output smoke/eval_harness/site/evidence-index.json \
  --data-js smoke/eval_harness/site/data.js

python3.14 smoke/eval_harness/index.py check \
  --plans-root ../../grok/plans \
  --output smoke/eval_harness/site/evidence-index.json \
  --data-js smoke/eval_harness/site/data.js

python3.14 smoke/eval_harness/index.py health \
  --plans-root ../../grok/plans \
  --grok-home "$HOME/.grok" \
  --codex-home "$HOME/.codex" \
  --agents-home "$HOME/.agents" \
  --logscale-root "$HOME/Projects/logscale" \
  --json
```

Historical run evidence is opt-in through repeatable
`--run ROOT:RELATIVE_PATH` or `--runs-registry ROOT:RELATIVE_PATH`; a
normal base build accepts only the `worktree` and `plans` roots and emits
`runs: []`. **These commands do not exist yet** — landed so far are the
registry, schemas, contracts and path primitives, the corpus inventory, and
the `plan_corpus` and `formalism` adapters.

## Local evidence

`smoke/eval_harness/report/` is gitignored (the single ignore line this
package adds) and holds private native benchmark evidence; it is never
committed and never scanned implicitly by the base build.

## Tests

From the worktree root. `PYTHONDONTWRITEBYTECODE=1` is **required**, not
cosmetic: `test_formalism.NoWriteProofTest` fails the suite if bytecode
writing is enabled or if any `__pycache__` exists under this package, so
clear them before quoting a test count.

```text
PYTHONDONTWRITEBYTECODE=1 python3.14 -m unittest -v \
  smoke.eval_harness.tests.test_catalog \
  smoke.eval_harness.tests.test_paths \
  smoke.eval_harness.tests.test_inventory \
  smoke.eval_harness.tests.test_formalism
```

`test_inventory` and `test_formalism` both read the live plans repository
and the live worktree in addition to their own sandboxes, and they handle a
missing root **differently**: `test_inventory` skips (4 `skipTest` sites),
`test_formalism` fails — `RealCorpusPinTest.setUpClass` asserts the roots
exist and `_copy_corpus` copies from the plans estate, so 149 of its 182
tests are not self-contained (Bead `apex-ayl.137.9`). A green run is
therefore a statement about this host. Skips are not zero elsewhere either:
3 tests need the optional `jsonschema` package, 21 need the `git` CLI, and
9 skip when a live root is absent, so quote a test count together with its
skip count (Bead `apex-ayl.137.10`).

Under `pytest` add `-p no:cacheprovider`. `NoWriteProofTest` fails the suite
on any `__pycache__` directory or `*.pyc` file under this package and on
`PYTHONDONTWRITEBYTECODE` not being exactly `1` (`true` and `python -B` both
fail it); it does **not** look for `.pytest_cache`, so a default pytest run
leaves an untracked cache directory in the package without any test
catching it.
