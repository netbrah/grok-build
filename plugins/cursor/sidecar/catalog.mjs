#!/usr/bin/env node
// Refreshes agents.generated/<model>.md from the live Cursor model catalog. Never blocks a session:
// --detach re-spawns itself in the background and exits 0 immediately.
import { spawn } from "node:child_process";
import { mkdirSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "agents.generated");
const staticStems = new Set(["composer-2", "gpt-5-4", "claude-opus-4-6", "claude-sonnet-4-6"]);

if (process.argv.includes("--detach")) {
  const args = process.argv.slice(1).filter((a) => a !== "--detach");
  spawn(process.execPath, args, { detached: true, stdio: "ignore", env: process.env }).unref();
  process.exit(0);
}
if (!process.argv.includes("--write-agents")) {
  process.stderr.write("usage: catalog.mjs --write-agents [--detach]\n");
  process.exit(2);
}
if (!process.env.CURSOR_API_KEY) process.exit(0); // nothing to refresh; static agents still work

const stem = (id) => String(id).toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64);
const yaml = (s) => JSON.stringify(String(s)); // JSON strings are valid YAML scalars

try {
  const { Cursor } = await import("@cursor/sdk");
  const models = await Cursor.models.list();
  mkdirSync(outDir, { recursive: true });
  const keep = new Set([".gitkeep"]);
  for (const m of models) {
    const id = m.id ?? m.name;
    const s = stem(id);
    if (!s || staticStems.has(s)) continue;
    const body = [
      "---",
      `name: ${s}`,
      `description: ${yaml(`Cursor ${m.displayName ?? id} subagent (via grok-mod cursor)`)}`,
      `model: ${yaml(id)}`,
      "capabilityMode: full",
      "---",
      "",
    ].join("\n");
    const tmp = join(outDir, `.${s}.md.${process.pid}.tmp`);
    writeFileSync(tmp, body, { mode: 0o644 });
    renameSync(tmp, join(outDir, `${s}.md`)); // atomic on the same filesystem
    keep.add(`${s}.md`);
  }
  for (const f of readdirSync(outDir)) if (!keep.has(f)) rmSync(join(outDir, f), { force: true });
} catch (e) {
  const key = process.env.CURSOR_API_KEY;
  process.stderr.write(`catalog refresh failed: ${String(e?.message ?? e).split(key).join("[redacted]")}\n`);
}
// The SDK keeps HTTP/2 handles open for ~80s; do not wait on them.
process.exit(0);
