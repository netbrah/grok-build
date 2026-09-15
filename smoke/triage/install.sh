#!/bin/sh
# TRIAGE-1 T3: install grok-triage into the operator home (read-only contract
# preserved: writes go to ~/.grok/bin and ~/.agents/skills — the sanctioned
# install locations — and never touch sessions/config/logs).
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
HOME_DIR="${HOME:-$(pwd)}"
BIN_DIR="$HOME_DIR/.grok/bin"
SKILL_SRC="$HOME_DIR/.grok/skills/grok-session-triage"
SKILL_LINK="$HOME_DIR/.agents/skills/grok-session-triage"

mkdir -p "$BIN_DIR"
cp "$HERE/grok-triage" "$BIN_DIR/grok-triage"
chmod +x "$BIN_DIR/grok-triage"
# default catalog resolves next to the script (same filename); the M-1 guard
# makes --write-catalog on the default (under-home) path fail closed, so
# catalog refreshes go to an explicit --catalog outside ~/.grok, then this
# re-install step re-syncs the copy.
cp "$HERE/signatures.json" "$BIN_DIR/signatures.json"

# skill: operator-placed in ~/.grok/skills (repo is not the skill home);
# symlink it into ~/.agents/skills so Codex sessions discover it too.
if [ ! -d "$SKILL_SRC" ]; then
    echo "install: skill dir missing: $SKILL_SRC (place SKILL.md first)" >&2
    exit 2
fi
mkdir -p "$HOME_DIR/.agents/skills"
if [ -L "$SKILL_LINK" ]; then
    ln -sfn "$SKILL_SRC" "$SKILL_LINK"
elif [ -e "$SKILL_LINK" ]; then
    echo "install: $SKILL_LINK exists and is not a symlink — not touching" >&2
    exit 2
else
    ln -s "$SKILL_SRC" "$SKILL_LINK"
fi

echo "install: $BIN_DIR/grok-triage + catalog -> $BIN_DIR/signatures.json"
echo "install: skill -> $SKILL_LINK (-> $SKILL_SRC)"
echo "verify: grok-triage scan   (expect <2 s warm, exit 0)"
