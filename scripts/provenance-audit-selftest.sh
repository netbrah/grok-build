#!/usr/bin/env bash
# provenance-audit-selftest.sh — offline fixture self-test for
# provenance-audit.sh PASS 5 (bead apex-ayl.61; donorsliving1-61-sdd.md §3.3).
#
# Stages a throwaway fixture (mktemp-unique under /tmp/donorsliving61-selftest.*)
# consisting of:
#   - a minimal git repo = the audit's REPO_ROOT (the audit derives its root
#     from its own location, so a COPY of provenance-audit.sh at the fixture
#     root isolates the run from the campaign worktree),
#   - a second git repo serving as BOTH donor checkouts (xli + netbrah/codex,
#     the spec-digest checkout of record — the machine block may point two
#     registry names at one checkout),
#   - a fixture donors.md (machine block + construct ledger rows):
#     good-code, good-spec-full-digest, good-fresh, PENDING, bad-file,
#     bad-commit, dup-slug, bad-marker, bad-digest (9 rows),
#   - a fixture plans dir (the PROVENANCE_PLANS_DIR target).
# Runs via the existing PROVENANCE_DONORS_MD / PROVENANCE_PLANS_DIR hooks.
# No network, no proxy, no campaign state touched, no `rm` (the fixture dir
# is left in /tmp for inspection).
#
# Expected (pinned): exit 1 (the 19 PASS 3 manifest gaps are inherent to a
# minimal fixture) with EXACTLY 5 P5 GAP lines — one each of P5-FILE,
# P5-COMMIT, P5-DUP, P5-MARKER, P5-DIGEST — passes 1-2 clean, rows=9, the
# PENDING row reported informational. Any deviation = fixture or impl
# drift: investigate, do not silently re-pin (SDD §6.4).
#
# Provenance: fresh — bead apex-ayl.61 (donorsliving1-61-sdd.md §3.3)
set -u

SRC_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC="$SRC_DIR/provenance-audit.sh"
[ -f "$SRC" ] || { echo "selftest FATAL: source script not found: $SRC" >&2; exit 2; }

FIX=$(mktemp -d /tmp/donorsliving61-selftest.XXXXXX) || { echo "selftest FATAL: mktemp failed" >&2; exit 2; }
echo "== selftest fixture: $FIX"

# --- fixture worktree (a minimal git repo = the audit's REPO_ROOT) ----------
mkdir -p "$FIX/scripts" "$FIX/crates/codegen/x" "$FIX/fixtures/plans"
cp "$SRC" "$FIX/scripts/provenance-audit.sh"
cat > "$FIX/crates/codegen/x/sample.rs" <<'RS'
placeholder (rewritten after the donor sha is known)
RS
printf 'fn other_fn() {}\n' > "$FIX/crates/codegen/x/other.rs"

# --- second git repo: donor checkout (xli) + spec checkout of record -------
mkdir -p "$FIX/checkout/docs/spec"
printf 'fn sample_fn() {}\n' > "$FIX/checkout/lib.rs"
printf 'fixture frozen spec body line 1\nfixture frozen spec body line 2\n' > "$FIX/checkout/docs/spec/fixture-spec.md"
git -C "$FIX/checkout" init -q
git -C "$FIX/checkout" -c user.name=selftest -c user.email=selftest@localhost add -A
git -C "$FIX/checkout" -c user.name=selftest -c user.email=selftest@localhost commit -qm fixture-donor
DONOR_SHA=$(git -C "$FIX/checkout" rev-parse --short=7 HEAD)
SPEC_SHA256=$(shasum -a 256 "$FIX/checkout/docs/spec/fixture-spec.md" | awk '{print $1}')

# --- finish the worktree file with the canonical (xli-citing) marker -------
printf '// Provenance: xli@%s src/lib.rs:1 :: sample_fn (adapted)\nfn sample_fn() {}\n' "$DONOR_SHA" > "$FIX/crates/codegen/x/sample.rs"
git -C "$FIX" init -q
git -C "$FIX" -c user.name=selftest -c user.email=selftest@localhost add -A
git -C "$FIX" -c user.name=selftest -c user.email=selftest@localhost commit -qm fixture-worktree
WT_SHA=$(git -C "$FIX" rev-parse --short=7 HEAD)

# --- fixture plans dir + fixture donors.md ---------------------------------
printf '# fixture doc\n\nused by the good-fresh and PENDING rows.\n' > "$FIX/fixtures/plans/fixture-doc.md"
cat > "$FIX/fixtures/donors.md" <<EOF
# fixture donors.md (selftest) — machine block + construct ledger rows

open-grok | https://example.invalid/open-grok.git | $FIX/checkout | $DONOR_SHA
xli | https://example.invalid/xli.git | $FIX/checkout | $DONOR_SHA
netbrah/codex | https://example.invalid/codex.git | $FIX/checkout | $DONOR_SHA

## Construct ledger (fixture)

| construct | donor | donor_ref | our file:line | bead | commit | adaptation |
|---|---|---|---|---|---|---|
| GOOD-CODE-1 — canonical code-pin row | xli | xli@$DONOR_SHA | crates/codegen/x/sample.rs:1 | apex-ayl.90 | $WT_SHA | fixture |
| GOOD-SPEC-1 — full 64-hex on-disk digest row | frozen spec (fixture) | docs/spec/fixture-spec.md r1 (body digest $SPEC_SHA256; on-disk $SPEC_SHA256) | crates/codegen/x/sample.rs:1 | apex-ayl.91 | $WT_SHA | fixture |
| GOOD-FRESH-1 — doc-ref row | fresh (fixture) | grok/plans/fixture-doc.md §1 | crates/codegen/x/sample.rs:1 | apex-ayl.92 | $WT_SHA | fixture |
| PENDING-1 — pre-close sentinel row | fresh (fixture) | grok/plans/fixture-doc.md §2 | crates/codegen/x/sample.rs:1 | apex-ayl.93 | PENDING-apex-ayl.93 | fixture |
| BAD-FILE-1 — missing our-file | fresh (fixture) | prose only, no machine refs | crates/codegen/x/missing.rs:1 | apex-ayl.94 | $WT_SHA | fixture |
| BAD-COMMIT-1 — unresolvable commit | fresh (fixture) | prose only, no machine refs | crates/codegen/x/sample.rs:1 | apex-ayl.95 | deadbeef | fixture |
| GOOD-CODE-1 — duplicate SLUG row | fresh (fixture) | prose only, no machine refs | crates/codegen/x/sample.rs:1 | apex-ayl.96 | $WT_SHA | fixture |
| BAD-MARKER-1 — donor cited by no marker in its file | xli | xli@$DONOR_SHA | crates/codegen/x/other.rs:1 | apex-ayl.97 | $WT_SHA | fixture |
| BAD-DIGEST-1 — wrong full 64-hex on-disk digest | frozen spec (fixture) | docs/spec/fixture-spec.md r1 (body digest $SPEC_SHA256; on-disk 0000000000000000000000000000000000000000000000000000000000000000) | crates/codegen/x/sample.rs:1 | apex-ayl.98 | $WT_SHA | fixture |
EOF

# --- run the audit against the fixture (env hooks only) --------------------
LOG="$FIX/selftest-run.log"
PROVENANCE_DONORS_MD="$FIX/fixtures/donors.md" PROVENANCE_PLANS_DIR="$FIX/fixtures/plans" \
  bash "$FIX/scripts/provenance-audit.sh" > "$LOG" 2>&1
rc=$?

# --- assertions (pinned expectations — any drift = STOP, do not re-pin) ----
fail=0
check() {
  if [ "$2" = "$3" ]; then
    echo "  ok: $1 = $2"
  else
    echo "  FAIL: $1 (got '$2', want '$3')"
    fail=1
  fi
}
echo "== assertions (exit=$rc)"
check "exit code" "$rc" 1
check "P5 gap lines total" "$(grep -c 'GAP \[P5-' "$LOG")" 5
for c in FILE COMMIT DUP MARKER DIGEST; do
  check "P5-$c gap count" "$(grep -c "GAP \[P5-$c\]" "$LOG")" 1
done
check "P5 gap class set" "$(grep -oE 'GAP \[P5-[A-Z]+\]' "$LOG" | sort -u | tr '\n' ' ')" "GAP [P5-COMMIT] GAP [P5-DIGEST] GAP [P5-DUP] GAP [P5-FILE] GAP [P5-MARKER] "
check "pass 1b gating count" "$(grep -oE '1b non-canonical \(neither form\): [0-9]+' "$LOG" | grep -oE '[0-9]+$')" 0
check "pass 2 gap lines" "$(grep -c 'GAP \[P2\]' "$LOG")" 0
check "rows census" "$(grep -oE 'P5 census: rows=9' "$LOG" | head -1)" "P5 census: rows=9"
check "gating gaps census" "$(grep -oE 'gating gaps=5' "$LOG" | head -1)" "gating gaps=5"
check "PENDING row reported informational" "$(grep -c 'PENDING sentinels=1 (PENDING-1' "$LOG")" 1
for s in GOOD-SPEC-1 GOOD-FRESH-1 PENDING-1; do
  check "row $s clean (no P5 gap)" "$(grep 'GAP \[P5-' "$LOG" | grep -c "$s")" 0
done
check "dup gap names the shared SLUG" "$(grep 'GAP \[P5-DUP\]' "$LOG" | grep -c 'GOOD-CODE-1')" 1
check "summary pass5=5" "$(grep -oE 'pass5=5' "$LOG" | head -1)" "pass5=5"
check "RESULT FAIL line" "$(grep -c '^RESULT: FAIL' "$LOG")" 1

if [ "$fail" -eq 0 ]; then
  echo "SELFTEST PASS (fixture: $FIX)"
else
  echo "SELFTEST FAIL (fixture: $FIX) — last 30 log lines:"
  tail -n 30 "$LOG"
  exit 1
fi
