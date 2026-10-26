#!/usr/bin/env node
import { spawn } from "node:child_process";
import fs from "node:fs";
import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const emit = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");

let turns = 0;
let toolCalls = 0;
let lastText = "";
let queued = [];
let mode = "normal";
let waitingForActiveMsg = false;

rl.on("line", (line) => {
  if (!line.trim()) return;
  const msg = JSON.parse(line);
  if (msg.type === "run.start") {
    if (msg.prompt === "CRASH") {
      process.stderr.write("fatal mock crash\n");
      process.exit(42);
    }
    if (msg.prompt.startsWith("HANG:")) {
      mode = "hang";
      const pidFile = msg.prompt.slice(5);
      const grandchild = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
        stdio: "ignore",
      });
      fs.writeFileSync(pidFile, String(grandchild.pid));
      emit({ type: "run.started", vendor_session_id: "v-hang", effective_model: msg.model });
      return;
    }
    const vsid = msg.resume_vendor_session_id || `v-${msg.subagent_id}`;
    emit({ type: "run.started", vendor_session_id: vsid, effective_model: msg.model });
    turns += 1;
    toolCalls += 1;
    emit({
      type: "run.progress",
      turns,
      tool_calls: toolCalls,
      tokens_used: 120,
      tool: { name: "read_file", summary: "Cargo.toml" },
    });
    if (msg.prompt === "JSON_RETRY") {
      lastText = msg.resume_vendor_session_id
        ? JSON.stringify({ ok: true, vsid: msg.resume_vendor_session_id })
        : "not-json";
    } else {
      lastText = `reply:${msg.prompt}:vsid=${vsid}`;
    }
    if (msg.prompt.startsWith("WAIT_FOR_ACTIVE_MSG")) {
      waitingForActiveMsg = true;
      return;
    }
    setTimeout(() => emit({ type: "run.idle", final_text: lastText }), 20);
  } else if (msg.type === "run.message") {
    if (waitingForActiveMsg) {
      waitingForActiveMsg = false;
      emit({ type: "run.consumed", message_id: msg.message_id });
      lastText += `|steered:${msg.mode}:${msg.text}`;
      emit({ type: "run.idle", final_text: lastText });
      return;
    }
    queued.push(msg);
  } else if (msg.type === "run.drain") {
    turns += 1;
    for (const q of queued.splice(0)) {
      emit({ type: "run.consumed", message_id: q.message_id });
      lastText += `|steered:${q.mode}:${q.text}`;
    }
    emit({ type: "run.idle", final_text: lastText });
  } else if (msg.type === "run.finish") {
    emit({
      type: "run.completed",
      final_text: lastText,
      turns,
      tool_calls: toolCalls,
      tokens_used: 120 * turns,
    });
    process.exit(0);
  } else if (msg.type === "run.cancel") {
    if (mode === "hang") return; // force graceful_kill_ms escalation to SIGKILL group
    emit({
      type: "run.failed",
      error: msg.reason || "cancelled",
      cancelled: true,
      turns,
      tool_calls: toolCalls,
      tokens_used: 0,
    });
    process.exit(0);
  }
});
