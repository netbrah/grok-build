#!/bin/sh
# Stub auth provider for grok-build so the login gate is satisfied without
# ever contacting auth.x.ai (blocked by corporate Zscaler TLS interception —
# see workspace notes 2026-09-01). Real inference auth for [model.grok-4.6]
# goes through the NetApp LLM proxy (env_key = OPENAI_API_KEY in
# ~/.grok/config.toml), which never consults this token. This script only
# has to satisfy grok's local session-credential shape.
#
# Contract (crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md):
#   - GROK_AUTH_EXPIRED=1 (headless refresh): decline silently, exit 1.
#     No one is watching; a stub can't mint a real refresh, so don't pretend to.
#   - unset (interactive sign-in): stdout = token (or JSON), stderr = human text.

if [ "$GROK_AUTH_EXPIRED" = "1" ]; then
    echo "proxy-auth-stub: headless refresh declined (static stub, nothing to refresh)" >&2
    exit 1
fi

echo "proxy-auth-stub: minting static local session token (NetApp proxy handles real auth)" >&2
printf '%s' "netapp-proxy-stub-token-not-a-real-xai-credential"
