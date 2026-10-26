import { spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";
import test from "node:test";
import assert from "node:assert/strict";

const STUB = `
export class JsonlLocalAgentStore { constructor(d) { this.d = d; } }
const mk = (id) => ({
  agentId: id,
  async send(prompt, o) {
    o.onDelta?.({ update: { type: "tool-call-started", toolCall: { type: "read", args: { path: "a.rs" } } } });
    const run = { status: "finished", result: "echo:" + prompt, usage: { totalTokens: 7 },
      wait: async () => ({ status: "finished", result: "echo:" + prompt, usage: { totalTokens: 7 } }),
      onDidChangeStatus() {}, cancel: async () => {} };
    return run;
  },
  async [Symbol.asyncDispose]() {},
});
export const Agent = { create: async () => mk("cur-new"), resume: async (id) => mk(id) };
`;

function harness() {
  const dir = mkdtempSync(join(tmpdir(), "grok-mod-cursor-"));
  mkdirSync(join(dir, "node_modules/@cursor/sdk"), { recursive: true });
  writeFileSync(join(dir, "node_modules/@cursor/sdk/package.json"), '{"name":"@cursor/sdk","type":"module","main":"index.js"}');
  writeFileSync(join(dir, "node_modules/@cursor/sdk/index.js"), STUB);
  cpSync(new URL("./runner.mjs", import.meta.url), join(dir, "runner.mjs"));
  const child = spawn(process.execPath, [join(dir, "runner.mjs")], {
    cwd: dir, env: { PATH: process.env.PATH, HOME: dir, CURSOR_API_KEY: "k-test" }, stdio: ["pipe", "pipe", "inherit"],
  });
  const frames = [];
  const waiters = [];
  readline.createInterface({ input: child.stdout }).on("line", (l) => {
    const f = JSON.parse(l);
    frames.push(f);
    waiters.splice(0).forEach((w) => w());
  });
  const send = (f) => child.stdin.write(JSON.stringify(f) + "\n");
  const until = async (type) => {
    while (!frames.some((f) => f.type === type)) await new Promise((r) => waiters.push(r));
    return frames.find((f) => f.type === type);
  };
  return { dir, child, frames, send, until };
}

const startFrame = (dir, extra = {}) => ({
  type: "run.start", protocol: 1, subagent_id: "s1", parent_session_id: "p1",
  subagent_type: "cursor:composer-2", agent_name: "composer-2", model: "composer-2",
  cwd: dir, prompt: "hi", system_prompt: null, resume_vendor_session_id: null,
  read_only: false, reasoning_effort: null, bridge: null, bridged_tools: [], ...extra,
});

test("start → started → progress → idle → drain(consumed) → finish → completed", async () => {
  const h = harness();
  h.send(startFrame(h.dir));
  assert.equal((await h.until("run.started")).vendor_session_id, "cur-new");
  assert.equal((await h.until("run.idle")).final_text, "echo:hi");
  assert.ok(h.frames.some((f) => f.type === "run.progress" && f.tool.summary === "a.rs"));
  h.send({ type: "run.message", message_id: "m1", mode: "steer", from: "root", text: "also x" });
  h.send({ type: "run.drain" });
  assert.equal((await h.until("run.consumed")).message_id, "m1");
  h.send({ type: "run.finish" });
  const done = await h.until("run.completed");
  assert.match(done.final_text, /also x/);
  assert.equal(done.turns, 2);
});

test("resume_vendor_session_id resumes the same Cursor agent", async () => {
  const h = harness();
  h.send(startFrame(h.dir, { resume_vendor_session_id: "cur-prev" }));
  assert.equal((await h.until("run.started")).vendor_session_id, "cur-prev");
  h.send({ type: "run.cancel", reason: "test" });
  assert.equal((await h.until("run.failed")).cancelled, true);
});
