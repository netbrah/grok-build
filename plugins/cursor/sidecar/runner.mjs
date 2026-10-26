#!/usr/bin/env node
// Grok-Build mod protocol v1 sidecar for the Cursor SDK. Frames: NDJSON on stdin/stdout.
import { Agent, JsonlLocalAgentStore } from "@cursor/sdk";
import { randomUUID } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";

const KEY = process.env.CURSOR_API_KEY ?? "";
// The host already env_clear()s to mod.json envAllowlist; keep zsh from re-reading user rc files.
try {
  const z = mkdtempSync(join(tmpdir(), "grok-mod-zdot-"));
  process.env.ZDOTDIR = z;
  process.on("exit", () => rmSync(z, { recursive: true, force: true }));
} catch {}
const redact = (s) => (KEY ? String(s).split(KEY).join("[redacted]") : String(s));
const rawErr = process.stderr.write.bind(process.stderr);
process.stderr.write = (c, ...r) => rawErr(redact(typeof c === "string" ? c : Buffer.from(c).toString("utf8")), ...r);
console.log = console.info = console.debug = (...a) => process.stderr.write(a.join(" ") + "\n");
process.on("unhandledRejection", (e) => process.stderr.write(`unhandledRejection ${e?.message ?? e}\n`));
const emit = (o) => process.stdout.write(redact(JSON.stringify(o)) + "\n");

let start; // run.start frame
let agent;
let currentRun;
let turns = 0;
let toolCalls = 0;
let tokensUsed = 0;
let lastText = "";
const queued = []; // { message_id, mode, from, text }
let busy = false;
let finished = false;

// ---- MCP bridge client (host-owned Unix socket, line-delimited JSON RPC) ----
let bridgeSock;
const bridgeWaiters = new Map();
function bridgeConnect(info) {
  if (!info?.socket_path) return Promise.resolve();
  return new Promise((resolve, reject) => {
    bridgeSock = net.createConnection(info.socket_path, resolve);
    bridgeSock.once("error", reject);
    readline.createInterface({ input: bridgeSock }).on("line", (line) => {
      let resp;
      try { resp = JSON.parse(line); } catch { return; }
      const w = bridgeWaiters.get(resp.id);
      if (w) { bridgeWaiters.delete(resp.id); w(resp); }
    });
    bridgeSock.on("close", () => {
      for (const [, w] of bridgeWaiters) w({ ok: false, error: "bridge closed" });
      bridgeWaiters.clear();
    });
  });
}
function bridgeCall(tool, args) {
  const id = randomUUID();
  return new Promise((resolve) => {
    bridgeWaiters.set(id, resolve);
    bridgeSock.write(JSON.stringify({
      id, token: start.bridge.token, method: "tools/call",
      server: tool.server, tool: tool.tool, arguments: args ?? {},
    }) + "\n");
  });
}
const sanitize = (n) => String(n ?? "").replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 64);
function customTools() {
  if (!start.bridge || !start.bridged_tools?.length) return {};
  const out = {};
  for (const t of start.bridged_tools) {
    out[sanitize(t.name)] = {
      description: t.description ?? "",
      inputSchema: t.input_schema ?? { type: "object" },
      execute: async (args) => {
        const r = await bridgeCall(t, args);
        const text = r.ok ? (typeof r.result === "string" ? r.result : JSON.stringify(r.result)) : `error: ${r.error}`;
        return { content: [{ type: "text", text }], isError: !r.ok };
      },
    };
  }
  return { customTools: out };
}

// ---- Cursor SDK turn driver ----
function storeDir() {
  const dir = join(process.env.HOME || homedir(), ".grok", "mods", "cursor", "cursor-store", sanitize(start.parent_session_id));
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  return dir;
}
async function openAgent() {
  const tools = customTools();
  const opts = {
    apiKey: KEY,
    model: start.model,
    // read_only maps to Cursor "ask" mode at send() time and strips write-capable tools here.
    disallowedTools: start.read_only ? ["task", "webSearch", "webFetch", "edit", "shell"] : ["task", "webSearch", "webFetch"],
    local: { cwd: start.cwd, settingSources: [], store: new JsonlLocalAgentStore(storeDir()), ...tools },
  };
  if (start.system_prompt) opts.systemPrompt = start.system_prompt;
  agent = start.resume_vendor_session_id
    ? await Agent.resume(start.resume_vendor_session_id, opts)
    : await Agent.create(opts);
  emit({ type: "run.started", vendor_session_id: agent.agentId, effective_model: start.model });
}
async function runTurn(prompt) {
  busy = true;
  turns += 1;
  currentRun = await agent.send(prompt, {
    ...(start.read_only ? { mode: "ask" } : {}),
    local: { force: Boolean(start.resume_vendor_session_id) && turns === 1 },
    onDelta: ({ update }) => {
      if (update.type === "tool-call-started") {
        toolCalls += 1;
        const a = update.toolCall?.args ?? {};
        emit({
          type: "run.progress", turns, tool_calls: toolCalls, tokens_used: tokensUsed,
          tool: { name: String(update.toolCall?.type ?? "tool"), summary: String(a.path ?? a.command ?? a.query ?? "").slice(0, 200) },
        });
      } else if (update.type === "token-delta") {
        tokensUsed += Number(update.tokens ?? 0);
      }
    },
  });
  // run.wait() can hang after the stream ends; fall back to status 15s after a terminal status.
  let waited = false;
  let timer;
  const r = await Promise.race([
    currentRun.wait().finally(() => (waited = true)),
    new Promise((res) => {
      const arm = (st) => {
        if (st !== "running" && !timer) timer = setTimeout(() => !waited && res({ status: st, result: currentRun.result, usage: currentRun.usage }), 15000);
      };
      currentRun.onDidChangeStatus(arm);
      arm(currentRun.status);
    }),
  ]);
  clearTimeout(timer);
  currentRun = undefined;
  busy = false;
  if (r.usage?.totalTokens) tokensUsed = Math.max(tokensUsed, Number(r.usage.totalTokens));
  if (r.status === "error") throw new Error(redact(r.error?.message ?? "cursor run failed"));
  if (r.status === "cancelled") throw Object.assign(new Error("cancelled"), { cancelled: true });
  lastText = r.result ?? "";
  emit({ type: "run.idle", final_text: lastText });
}
function drainPrompt() {
  const batch = queued.splice(0);
  for (const m of batch) emit({ type: "run.consumed", message_id: m.message_id });
  return batch.map((m) => `Message from ${m.from} (${m.mode}):\n${m.text}`).join("\n\n");
}
function fail(e) {
  emit({
    type: "run.failed", error: redact(e?.message ?? e), cancelled: Boolean(e?.cancelled),
    turns, tool_calls: toolCalls, tokens_used: tokensUsed,
  });
  shutdown(e?.cancelled ? 130 : 1);
}
async function shutdown(code) {
  finished = true;
  try { await agent?.[Symbol.asyncDispose]?.(); } catch {}
  bridgeSock?.end();
  await new Promise((res) => process.stdout.write("", res));
  process.exit(code);
}

// ---- frame loop ----
readline.createInterface({ input: process.stdin, crlfDelay: Infinity }).on("line", async (line) => {
  if (!line.trim() || finished) return;
  let f;
  try { f = JSON.parse(line); } catch { return; }
  try {
    switch (f.type) {
      case "run.start":
        if (f.protocol !== 1) return fail(new Error(`unsupported protocol ${f.protocol}`));
        if (!KEY) return fail(new Error("CURSOR_API_KEY is not set"));
        start = f;
        await bridgeConnect(f.bridge);
        await openAgent();
        await runTurn(f.prompt);
        break;
      case "run.message":
        // The SDK has no mid-turn injection: steer degrades to queue (Spec §10 R3).
        queued.push(f);
        break;
      case "run.drain":
        if (!busy && queued.length) await runTurn(drainPrompt());
        else emit({ type: "run.idle", final_text: lastText });
        break;
      case "run.finish":
        emit({ type: "run.completed", final_text: lastText, turns, tool_calls: toolCalls, tokens_used: tokensUsed });
        await shutdown(0);
        break;
      case "run.interrupt":
        await currentRun?.cancel().catch(() => {});
        break;
      case "run.cancel":
        await currentRun?.cancel().catch(() => {});
        fail(Object.assign(new Error(f.reason || "cancelled"), { cancelled: true }));
        break;
    }
  } catch (e) {
    fail(e);
  }
});
process.stdin.on("end", () => !finished && fail(new Error("host closed stdin")));
