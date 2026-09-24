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
  adapters/
    __init__.py
  tests/
    __init__.py
    test_catalog.py
    test_paths.py
  report/                           # ignored, local raw benchmark evidence
```

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
`runs: []`. **These commands do not exist yet** — this task lands the
registry, schemas, contracts, and path primitives only.

## Local evidence

`smoke/eval_harness/report/` is gitignored (the single ignore line added by
this task) and holds private native benchmark evidence; it is never
committed and never scanned implicitly by the base build.

## Tests

From the worktree root:

```text
python3.14 -m unittest -v \
  smoke.eval_harness.tests.test_catalog \
  smoke.eval_harness.tests.test_paths
```
