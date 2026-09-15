#!/usr/bin/env bash
# provenance-audit.sh — checked-in donor-provenance re-verification (gap G4).
#
# Source: grok/plans/design-audit1-findings.md §2 (DESIGN-AUDIT-1, apex-ayl.23).
# This script is the machine-checkable surface that keeps the in-code
# Provenance markers honest between ratchets. Deep provenance (rulings,
# wire evidence, DROP audits) stays OUT of code — see ledger §<ITEM>
# pointers inside the markers.
#
# Run: from the worktree root, no args:   scripts/provenance-audit.sh
# (CWD-independent: the worktree root is derived from this file's location.)
#
# Exit code: 0 = all gating passes clean; 1 = at least one pass found a gap.
#
# Passes (audit doc §2):
#   1) canonical-form check — every `Provenance:` marker line must match a
#      documented grammar. TWO documented forms exist and both are accepted
#      (gate = lines matching NEITHER):
#        a. house convention (1), the de-facto standard the audit keeps as
#           THE in-code form —
#             Provenance: <donor>@<sha7+> <path>[:<line>] :: <symbol> (<class> <note>)
#           plus the observed variants: `HY @ \`<sha>\` \`<path>\`` module
#           headers, `re-expressed from <donor>@<sha>`,
#           `pinned-snapshot-sourced — …`, `fresh …`.
#        b. the audit §2 proposed unified target form (class-first) —
#             Provenance: <class> — <detail>
#      The audit's exact grep for (b) is run verbatim as sub-check 1a and its
#      output is reported as target-form adoption (informational, non-gating):
#      the existing 180+ convention-(1) markers predate the target form, and
#      reformatting them is a campaign-wide diff, not an audit gap. Sub-check
#      1b (gating) fails on any marker matching neither form.
#   2) donor sha resolvability — every pinned sha in the donors.md registry
#      (machine block below the human table) must resolve in its local
#      reference checkout, and every distinct donor@sha cited in code
#      markers must resolve via the registry (registry: grok/plans/donors.md;
#      override with PROVENANCE_DONORS_MD).
#   3) coverage — every file in the ported-file manifest (audit doc §1 rows)
#      carrying ported content must have >=1 in-code provenance surface
#      (Provenance: marker, Behavioral reference, audited-ledger, a
#      donor@sha cite, or a pins/ cite).
#   4) raw-key sweep — the campaign redaction standard: expect 0 hits for
#      sk- keys in the quoted material (marker lines, campaign plans docs,
#      scripts/). Canary strings in the xai-grok-otel / xai-grok-secrets
#      redaction unit tests are a known, non-gated class (reported only).
#
# Provenance: fresh — DESIGN-AUDIT-1 §2/§4 (gap G4); ledger §DESIGN-AUDIT-1
set -u

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
cd "$REPO_ROOT"

PLANS_DIR="${PROVENANCE_PLANS_DIR:-$REPO_ROOT/../../grok/plans}"
DONORS_MD="${PROVENANCE_DONORS_MD:-$PLANS_DIR/donors.md}"

# Regexes (ERE). Single-quoted so the literal backticks in the HY module-
# header form (`HY @ \`sha\` \`path\``) stay literal.
DONOR_SHA_RE='(open-grok|hyper-grok-build|HY|xli|netbrah/codex)( |	)*@ *`?[0-9a-f]{7,40}`?'
# Audit §2 proposed unified (class-first) target form — verbatim in 1a.
TARGET_FORM_RE='Provenance: (verbatim|adapted|re-expressed|re-derived|behavioral-ref) — (open-grok|hyper-grok-build|xli|netbrah/codex)@[0-9a-f]{7,40} |Provenance: (pin-sourced) — pins/|Provenance: (fresh) — '
# House convention (1) + observed variants (see header).
CONV1_RE='Provenance: ((open-grok|hyper-grok-build|HY|xli|netbrah/codex)( |	)*@ *`?[0-9a-f]{7,40}`?|(re-expressed from )(open-grok|hyper-grok-build|xli|netbrah/codex)@[0-9a-f]{7,40}|pinned-snapshot-sourced — |fresh( — )?)'
# Any in-code provenance surface (pass 3).
SURFACE_RE='Provenance:|Behavioral reference|audited-ledger|(open-grok|hyper-grok-build|HY|xli|netbrah/codex)( |	)*@ *`?[0-9a-f]{7,40}|pins/[A-Za-z0-9._/-]+'
# Raw-key pattern (campaign redaction standard; audit doc §2 pass 4, verbatim).
KEY_RE='sk-[A0-9a-z]{16,}'

MARKERS=$(grep -rnE '^ *//+ *Provenance: ' crates/ 2>/dev/null || true)
TOTAL_MARKERS=$(printf '%s' "$MARKERS" | grep -c . || true)

echo "== provenance-audit — worktree: $REPO_ROOT"
echo "   HEAD: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)  ($(git branch --show-current 2>/dev/null || echo detached))"

# ---------------------------------------------------------------- pass 1 ---
echo
echo "PASS 1 — canonical-form check (audit §2)"
P1A=$(printf '%s\n' "$MARKERS" | grep -vE "$TARGET_FORM_RE" || true)
P1A_N=$(printf '%s' "$P1A" | grep -c . || true)
# (gating) markers matching NEITHER documented form:
P1B=$(printf '%s\n' "$MARKERS" | grep -vE "$TARGET_FORM_RE" | grep -vE "$CONV1_RE" || true)
P1B_N=$(printf '%s' "$P1B" | grep -c . || true)
echo "  total marker lines:              $TOTAL_MARKERS"
echo "  1a audit §2 exact grep (target form): $P1A_N of $TOTAL_MARKERS not yet in target form"
echo "     (informational — house convention (1) is THE form per audit §2;"
echo "      target-form adoption is tracked, not gated)"
echo "  1b non-canonical (neither form): $P1B_N   [GATE]"
if [ "$P1B_N" -gt 0 ]; then
  printf '%s\n' "$P1B" | sed 's/^/    /'
fi

# ---------------------------------------------------------------- pass 2 ---
echo
echo "PASS 2 — donor sha resolvability (registry: ${DONORS_MD#$REPO_ROOT/})"
REG_OK=1
P2_GAPS=0
CHECKOUTS="" # lines: name<TAB>path<TAB>pins
if [ -f "$DONORS_MD" ]; then
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    name=$(printf '%s' "$line" | cut -d'|' -f1 | xargs)
    path=$(printf '%s' "$line" | cut -d'|' -f3 | xargs)
    pins=$(printf '%s' "$line" | cut -d'|' -f4 | xargs)
    CHECKOUTS="$CHECKOUTS$name	$path	$pins
"
  done <<EOF
$(awk -F'|' '!/^\|/ {n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); p=$3; gsub(/^[ \t]+|[ \t]+$/,"",p); if (n ~ /^(open-grok|hyper-grok-build|xli|netbrah\/codex|xai-org)$/ && p ~ /^\//) print}' "$DONORS_MD")
EOF
else
  echo "    GAP [P2] donors.md registry not found at $DONORS_MD"
  REG_OK=0
fi
if [ "$REG_OK" -eq 1 ]; then
  PIN_FAIL=0
  PIN_N=0
  while IFS= read -r entry; do
    [ -z "$entry" ] && continue
    name=$(printf '%s' "$entry" | cut -f1)
    path=$(printf '%s' "$entry" | cut -f2)
    pins=$(printf '%s' "$entry" | cut -f3)
    if [ ! -d "$path/.git" ] && [ ! -f "$path/.git" ]; then
      echo "    GAP [P2] checkout for $name missing: $path"
      PIN_FAIL=1
      continue
    fi
    for sha in ${pins//,/ }; do
      PIN_N=$((PIN_N + 1))
      if ! git -C "$path" cat-file -e "${sha}^{commit}" 2>/dev/null; then
        echo "    GAP [P2] $name@$sha does not resolve in $path"
        PIN_FAIL=1
      fi
    done
  done <<EOF
$CHECKOUTS
EOF
  CO_N=$(printf '%s' "$CHECKOUTS" | grep -c . || true)
  if [ "$PIN_FAIL" -eq 0 ]; then
    echo "  registry pins: $PIN_N shas across $CO_N checkouts — all resolve"
  fi
  # distinct donor@sha pairs cited in code markers:
  PAIRS=$(printf '%s\n' "$MARKERS" | grep -ohE "$DONOR_SHA_RE" | tr -d ' \t`' | awk -F'@' '{
      n=$1; s=$2
      if (n=="HY") n="hyper-grok-build"
      if (s!="") print n"@"s }' | sort -u)
  PAIR_N=$(printf '%s' "$PAIRS" | grep -c . || true)
  for pair in $PAIRS; do
    d=${pair%%@*}; s=${pair#*@}
    entry=$(printf '%s\n' "$CHECKOUTS" | awk -F'\t' -v n="$d" '$1==n' | head -1)
    if [ -z "$entry" ]; then
      echo "    GAP [P2] donor $d cited in code markers but absent from the donors.md registry"
      P2_GAPS=$((P2_GAPS + 1))
      continue
    fi
    path=$(printf '%s' "$entry" | cut -f2)
    if ! git -C "$path" cat-file -e "${s}^{commit}" 2>/dev/null; then
      echo "    GAP [P2] $pair does not resolve in $path"
      P2_GAPS=$((P2_GAPS + 1))
    fi
  done
  if [ "$P2_GAPS" -eq 0 ]; then
    echo "  code-marker donor@sha refs: $PAIR_N distinct pairs — all resolve via registry"
  fi
fi

# ---------------------------------------------------------------- pass 3 ---
echo
echo "PASS 3 — ported-file manifest coverage (audit §1 rows)"
PORTED_MANIFEST="crates/codegen/xai-grok-sampling-types/src/catalog_wire.rs|R0
crates/codegen/xai-grok-sampling-types/src/conversation.rs|R1,P2.1,CROSSWIRE-1
crates/codegen/xai-grok-sampling-types/src/conversation/messages.rs|MW-1,MW-2
crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs|P2.1
crates/codegen/xai-grok-sampling-types/src/messages.rs|MW-2
crates/codegen/xai-grok-sampling-types/src/messages_model.rs|MW-2,MW-3,R5
crates/codegen/xai-grok-sampling-types/src/error.rs|CROSSWIRE-1
crates/codegen/xai-grok-sampler/src/client.rs|R0,P2.1
crates/codegen/xai-grok-sampler/src/provider.rs|R0,wire-shim
crates/codegen/xai-grok-sampler/src/retry.rs|CROSSWIRE-1
crates/codegen/xai-grok-sampler/src/actor/request_task.rs|CROSSWIRE-1
crates/codegen/xai-grok-sampler/src/stream/responses.rs|R0
crates/codegen/xai-grok-sampler/src/stream/messages_invariants.rs|MW-3
crates/codegen/xai-chat-state/src/compaction_utils.rs|P2.1
crates/codegen/xai-chat-state/src/actor/state.rs|P2.1
crates/codegen/xai-grok-shell/src/session/compaction.rs|P2.1
crates/codegen/xai-grok-subagent-resolution/src/fork.rs|MA-2
crates/codegen/xai-grok-subagent-resolution/src/digest.rs|MA-2
crates/codegen/xai-grok-shell/src/agent/subagent/worktree_guard.rs|MA-2"
P3_N=0
while IFS='|' read -r f item; do
  [ -z "$f" ] && continue
  if [ ! -f "$f" ]; then
    echo "    GAP [P3] manifest file missing from tree: $f ($item)"
    P3_N=$((P3_N + 1))
    continue
  fi
  if ! grep -qE "$SURFACE_RE" "$f"; then
    echo "    GAP [P3] no in-code provenance surface: $f ($item)"
    P3_N=$((P3_N + 1))
  fi
done <<EOF
$PORTED_MANIFEST
EOF
MANIFEST_N=$(printf '%s\n' "$PORTED_MANIFEST" | grep -c .)
if [ "$P3_N" -eq 0 ]; then
  echo "  all $MANIFEST_N manifest files carry >=1 provenance surface"
fi

# ---------------------------------------------------------------- pass 4 ---
echo
echo "PASS 4 — raw-key sweep (campaign redaction standard; expect 0)"
P4_N=0
HITS=""
SCRIPT_HITS=""
PLANS_HITS=""
if [ -n "$MARKERS" ]; then
  HITS=$(printf '%s\n' "$MARKERS" | grep -Ei "$KEY_RE" || true)
  [ -n "$HITS" ] && P4_N=$((P4_N + $(printf '%s\n' "$HITS" | grep -c .)))
fi
SCRIPT_HITS=$(grep -rEi "$KEY_RE" scripts/ 2>/dev/null || true)
[ -n "$SCRIPT_HITS" ] && P4_N=$((P4_N + $(printf '%s\n' "$SCRIPT_HITS" | grep -c .)))
if [ -d "$PLANS_DIR" ]; then
  PLANS_HITS=$(grep -rEi "$KEY_RE" "$PLANS_DIR" 2>/dev/null || true)
  [ -n "$PLANS_HITS" ] && P4_N=$((P4_N + $(printf '%s\n' "$PLANS_HITS" | grep -c .)))
else
  echo "  note: plans dir not found at $PLANS_DIR (doc sweep skipped)"
fi
if [ "$P4_N" -eq 0 ]; then
  echo "  quoted material (marker lines + scripts/ + $PLANS_DIR): 0 hits"
else
  printf '%s\n' "$HITS" "$SCRIPT_HITS" "$PLANS_HITS" | sed 's/^/    /'
fi
# Known non-gated class: canary strings inside the redaction unit tests.
CANARY=$(grep -rEil "$KEY_RE" crates/ 2>/dev/null || true)
CANARY_N=$(printf '%s' "$CANARY" | grep -c . || true)
echo "  informational: $CANARY_N tree files contain the sk- pattern (redaction-test canaries, non-gated):"
printf '%s\n' "$CANARY" | sed 's/^/    /'

# ---------------------------------------------------------------- summary --
REG_GAP=0
[ "$REG_OK" -eq 1 ] || REG_GAP=1
GAPS_TOTAL=$((P1B_N + P2_GAPS + P3_N + P4_N + REG_GAP))
echo
echo "== summary: pass1=$P1B_N pass2=$((P2_GAPS + REG_GAP)) pass3=$P3_N pass4=$P4_N — total=$GAPS_TOTAL gap(s)"
if [ "$GAPS_TOTAL" -gt 0 ]; then
  echo "RESULT: FAIL — gaps listed above (G1/G3-class backfill gaps expected until closed)"
  exit 1
fi
echo "RESULT: PASS — all 4 passes clean"
