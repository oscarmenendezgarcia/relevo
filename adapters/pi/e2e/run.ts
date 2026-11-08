// End to end with the real pi CLI and a real relevo server. Uses a model, so it is
// not part of `npm test`: run it by hand with `npm run e2e`.
//
//   PI_BIN        pi executable (default: pi on PATH)
//   PI_E2E_MODEL  model for the runs (default: gb10/qwen3.8-flash-next)
//
// 1. Print mode: pi calls relevo_inbox (sees a note from ana) and relevo_send (answers her).
// 2. RPC mode: pi sits idle with no prompt; a note from ana wakes it, it answers in the
//    thread, the note is delivered once, and closing stdin shuts pi down.
//
// The extension is loaded only with -e; nothing in ~/.pi is read for extensions or changed.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { api, startServer } from "../test/server.ts";

const pi = process.env.PI_BIN || "pi";
const model = process.env.PI_E2E_MODEL || "gb10/qwen3.8-flash-next";
const extension = resolve(import.meta.dirname, "..");
const tools = "relevo_send,relevo_inbox,relevo_thread";
const base = ["-ne", "-e", extension, "--no-skills", "--no-mcp", "-nc", "-np", "--no-themes", "-na", "--no-session", "--offline", "--model", model, "--tools", tools];

const server = await startServer();
try {
  const oscar = server.invite("oscar", "relevo itself", "pi e2e");
  const ana = server.invite("ana", "payments backend", "ana's agent");
  const work = join(server.dir, "Relevo-Demo");
  mkdirSync(work);
  spawnSync("git", ["init", "-q", "-b", "main"], { cwd: work });
  const env = { ...process.env, RELEVO_URL: server.url, RELEVO_TOKEN: oscar };
  delete env.RELEVO_CONTEXT;

  // 1. Print mode.
  const first = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Hi Oscar, the refunds job uses queue Q7. Just so you know." });
  const prompt =
    "Call relevo_inbox once and tell me in one line what it shows. Then call relevo_send once with " +
    'to ["ana"] and text "Thanks, noted: Q7." Do nothing else.';
  const t1 = Date.now();
  const run = spawnSync(pi, [...base, "-p", prompt], { cwd: work, env, encoding: "utf8", timeout: 180_000 });
  console.log(`print run: exit ${run.status} in ${((Date.now() - t1) / 1000).toFixed(1)}s`);
  console.log(`pi said: ${run.stdout.trim().slice(0, 400)}`);
  assert.equal(run.status, 0, run.stderr.slice(-1000));
  assert.doesNotMatch(run.stderr, /extension error|Failed to load extension/i, run.stderr.slice(-1000));
  const seen = await api(server, ana, "GET", `/api/threads/${first.thread}`);
  const oscarDelivery = seen.delivery.find((d: any) => d.handle === "oscar");
  assert.equal(oscarDelivery.by?.label, "pi e2e", "relevo_inbox delivered ana's note to the pi session");
  assert.equal(oscarDelivery.by?.context, "relevo-demo@main", "context from git");
  const anaInbox = await api(server, ana, "GET", "/api/inbox");
  const answer = anaInbox.threads.find((t: any) => t.from === "oscar" && t.new_notes.some((n: any) => n.via === "pi e2e"));
  assert.ok(answer, "relevo_send opened a thread to ana");
  console.log(`print run ok: inbox delivered ${first.thread} in relevo-demo@main; send opened ${answer.id} from via "pi e2e"`);

  // 2. RPC mode, idle until a note arrives.
  const child = spawn(pi, [...base, "--mode", "rpc"], { cwd: work, env, stdio: ["pipe", "pipe", "pipe"] });
  const events: any[] = [];
  let buffer = Buffer.alloc(0);
  child.stdout.on("data", (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk]);
    let nl: number;
    while ((nl = buffer.indexOf(10)) >= 0) {
      const line = buffer.subarray(0, nl).toString("utf8").replace(/\r$/, "");
      buffer = buffer.subarray(nl + 1);
      if (line) events.push(JSON.parse(line));
    }
  });
  let stderr = "";
  child.stderr.on("data", (c) => (stderr += c));
  const exited = new Promise<number | null>((r) => child.once("exit", (code) => r(code)));
  const until = async (check: () => boolean, ms: number, what: string) => {
    const end = Date.now() + ms;
    while (!check()) {
      if (Date.now() > end || child.exitCode !== null) throw new Error(`timed out waiting for ${what}\n${stderr.slice(-1000)}`);
      await new Promise((r) => setTimeout(r, 100));
    }
  };

  // pi answers commands once its session has started (and with it the inbox loop).
  child.stdin.write(JSON.stringify({ id: "state-1", type: "get_state" }) + "\n");
  await until(() => events.some((e) => e.type === "response" && e.id === "state-1"), 60_000, "pi rpc to start");
  await new Promise((r) => setTimeout(r, 2000)); // The waiting inbox request is open by now.
  assert.equal(events.filter((e) => e.type === "agent_start").length, 0, "idle: no prompt was sent");

  const t2 = Date.now();
  const wake = await api(server, ana, "POST", "/api/send", {
    to: ["oscar"],
    text: "Quick check for Oscar's agent: is the relevo demo repo on main? Please answer in this thread.",
  });
  await until(() => events.some((e) => e.type === "agent_start"), 60_000, "the note to wake pi");
  console.log(`rpc: note sent, agent_start after ${((Date.now() - t2) / 1000).toFixed(1)}s`);
  await until(() => events.some((e) => e.type === "agent_settled"), 180_000, "pi to settle");
  const relevoMessages = events.filter((e) => e.type === "message_start" && e.message?.role === "custom" && e.message?.customType === "relevo");
  assert.equal(relevoMessages.length, 1, "delivered once");
  assert.deepEqual(events.filter((e) => e.type === "extension_error"), [], "no extension errors");
  const toolCalls = events.filter((e) => e.type === "tool_execution_start").map((e) => `${e.toolName}${e.args?.thread ? `(${e.args.thread})` : ""}`);
  console.log(`rpc: relevo messages ${relevoMessages.length}; tools called: ${toolCalls.join(", ") || "none"}`);
  const thread = await api(server, ana, "GET", `/api/threads/${wake.thread}`);
  const replies = thread.notes.filter((n: any) => n.author === "oscar");
  console.log(`rpc: ${replies.length} reply(ies) by oscar in ${wake.thread}, via ${replies.map((n: any) => `"${n.via}"`).join(", ")}`);
  assert.ok(replies.length >= 1, "the woken agent answered in the thread");
  for (const n of replies) console.log(`rpc: oscar's reply: ${JSON.stringify(n.text.slice(0, 300))}`);

  // Shut down: close stdin; pi exits and its waiting request ends with it.
  const t3 = Date.now();
  child.stdin.end();
  const code = await Promise.race([exited, new Promise((r) => setTimeout(() => r("timeout"), 15_000))]);
  console.log(`rpc: stdin closed, pi exited (${code}) after ${((Date.now() - t3) / 1000).toFixed(1)}s`);
  if (code === "timeout") {
    child.kill("SIGTERM");
    throw new Error("pi did not exit after stdin closed");
  }
  console.log("e2e ok");
} finally {
  await server.stop();
}
