// A note stays pending for the person until one of their sessions handles it
// (docs/protocol.md, "Delivery state"): a pi session that starts gets what is
// pending at once, oldest first, and a thread the agent has dealt with is not
// handed over again.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, afterEach, before, test } from "node:test";
import { loadExtension, until } from "./mock-pi.ts";
import { api, startServer, type TestServer } from "./server.ts";

let server: TestServer;
let oscar: string;
let ana: string;
let home: string;

before(async () => {
  server = await startServer();
  oscar = server.invite("oscar", "relevo itself", "pi laptop");
  ana = server.invite("ana", "payments backend");
  home = mkdtempSync(join(tmpdir(), "relevo-pi-pending-"));
});

after(async () => {
  await server?.stop();
  rmSync(home, { recursive: true, force: true });
});

// Each test starts with nothing pending for oscar.
afterEach(async () => {
  for (let i = 0; i < 10; i++) {
    const inbox = await api(server, oscar, "GET", "/api/inbox?all=1", undefined, "cleanup-session");
    const pending = inbox.threads.filter((t: any) => t.pending);
    if (!pending.length && !inbox.more) return;
    for (const t of pending) await api(server, oscar, "POST", "/api/send", { thread: t.id, done: true }, "cleanup-session");
  }
});

function extension() {
  return loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" }, home, wait: 1 });
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const handed = (ext: ReturnType<typeof extension>) => ext.sent.map((s) => s.message.details.thread);
const oscarDelivery = async (thread: string) =>
  (await api(server, ana, "GET", `/api/threads/${thread}`)).delivery.find((d: any) => d.handle === "oscar");

test("notes pending when the extension starts are handed over at once, oldest first, each once", async () => {
  // Notes are timed to the second: one second apart, so the order is the order they were sent in.
  const sent: string[] = [];
  for (const text of ["First: the cron job?", "Second: the queue?", "Third: the retries?"]) {
    if (sent.length) await sleep(1100);
    sent.push((await api(server, ana, "POST", "/api/send", { text: `@oscar ${text}` })).thread);
  }
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await until(() => ext.seen.length === 3);
    assert.deepEqual(handed(ext), sent, "oldest first");
    for (const s of ext.sent) assert.deepEqual(s.options, { triggerTurn: true });
    assert.match(ext.seen[0].content, /New notes \(1\):[\s\S]*First: the cron job\?/);
    await sleep(1500); // Several waits of the loop: none is handed over again.
    assert.equal(ext.sent.length, 3);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a thread the agent wrote in, marked done or read whole while it waited is not handed over after", async () => {
  const first = (await api(server, ana, "POST", "/api/send", { text: "@oscar Oldest: deploy window?" })).thread;
  await sleep(1100);
  const later: string[] = [];
  for (const text of ["Can you answer this one?", "Thanks, all good now.", "FYI: the runbook moved."]) {
    later.push((await api(server, ana, "POST", "/api/send", { text: `@oscar ${text}` })).thread);
  }
  const ext = extension();
  ext.session.holdTurns(); // The agent works on the first thread while the others wait.
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await until(() => ext.seen.length === 1);
    assert.deepEqual(handed(ext), [first]);
    // During that turn the agent deals with the three others: it answers one, marks one done, reads one whole.
    assert.match(await ext.tool("relevo_send", { thread: later[0], text: "Yes: Tuesday." }, home), /^Sent note/);
    assert.match(await ext.tool("relevo_send", { thread: later[1], done: true }, home), /^Marked thread/);
    assert.match(await ext.tool("relevo_thread", { id: later[2] }, home), /the runbook moved/);
    await ext.session.finishTurn();
    await sleep(1500);
    assert.deepEqual(handed(ext), [first], "nothing more handed over");
    assert.equal((await oscarDelivery(later[1])).handled_by.label, "pi laptop");

    // A new note in a thread marked done makes it pending again, and it is handed over.
    await api(server, ana, "POST", "/api/send", { thread: later[1], text: "One more thing: the logs?" });
    await until(() => ext.sent.length === 2);
    assert.equal(handed(ext)[1], later[1]);
    assert.match(ext.sent[1].message.content, /New notes \(1\):[\s\S]*One more thing: the logs\?/);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("killed right after a note reached it: the next pi session gets the note when it starts", async () => {
  const child = spawn(process.execPath, [join(import.meta.dirname, "child.ts")], {
    env: { PATH: process.env.PATH, RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_TEST_HOME: home },
    stdio: ["ignore", "pipe", "inherit"],
  });
  const exited = new Promise<void>((r) => child.once("exit", () => r()));
  let out = "";
  child.stdout!.on("data", (d) => (out += d));
  let thread: string;
  try {
    await sleep(500); // The child's waiting request is open by now.
    thread = (await api(server, ana, "POST", "/api/send", { text: "@oscar Kill test: is the backup running?" })).thread;
    await until(() => out.includes(`received ${thread}`), 10_000);
  } finally {
    // Dropped mid-turn without a word: no shutdown, no DELETE /api/session.
    child.kill("SIGKILL");
    await exited;
  }
  // For the sender: it reached oscar's pi, and nobody has handled it.
  const seen = await oscarDelivery(thread);
  assert.equal(seen.by.label, "pi laptop");
  assert.equal(seen.handled_at, null);

  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    const t = Date.now();
    await until(() => ext.seen.length === 1);
    assert.ok(Date.now() - t < 3000, "at once");
    assert.deepEqual(handed(ext), [thread]);
    assert.deepEqual(ext.sent[0].options, { triggerTurn: true });
    assert.match(ext.seen[0].content, /New notes \(1\):[\s\S]*Kill test: is the backup running\?/);
    // This one answers it: handled.
    await ext.tool("relevo_send", { thread, text: "Yes, every night at 3." }, home);
    assert.equal((await oscarDelivery(thread)).handled_by.label, "pi laptop");
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
  // Handled: a session started now gets nothing.
  const next = extension();
  await next.emit("session_start", { reason: "startup" }, next.ctx(home, "tui"));
  await sleep(1500);
  await next.emit("session_shutdown", { reason: "quit" }, next.ctx(home, "tui"));
  assert.equal(next.sent.length, 0);
});

test("a note a session leaves alone stays pending: a session started later gets it, the first never again", async () => {
  const a = extension();
  await a.emit("session_start", { reason: "startup" }, a.ctx(home, "tui"));
  const b = extension();
  try {
    const thread = (await api(server, ana, "POST", "/api/send", { text: "@oscar Not for this one: the iOS build?" })).thread;
    await until(() => a.seen.length === 1); // Its turn ends without a word: not for it.
    await b.emit("session_start", { reason: "startup" }, b.ctx(home, "tui"));
    await until(() => b.seen.length === 1);
    assert.deepEqual(handed(b), [thread]);
    assert.equal((await oscarDelivery(thread)).handled_at, null);

    // Ended cleanly, a session that left it alone does not take it with it either.
    await a.emit("session_shutdown", { reason: "quit" }, a.ctx(home, "tui"));
    assert.equal(a.sent.length, 1, "once for the first");
    const c = extension();
    await c.emit("session_start", { reason: "startup" }, c.ctx(home, "tui"));
    try {
      await until(() => c.seen.length === 1);
      assert.deepEqual(handed(c), [thread]);
      // It needs nothing from this one either: marked done, no new session gets it.
      await c.tool("relevo_send", { thread, done: true }, home);
    } finally {
      await c.emit("session_shutdown", { reason: "quit" }, c.ctx(home, "tui"));
    }
    await sleep(1500);
    assert.equal(b.sent.length, 1, "once for the second");
    const d = extension();
    await d.emit("session_start", { reason: "startup" }, d.ctx(home, "tui"));
    await sleep(1500);
    await d.emit("session_shutdown", { reason: "quit" }, d.ctx(home, "tui"));
    assert.equal(d.sent.length, 0);
  } finally {
    await a.emit("session_shutdown", { reason: "quit" }, a.ctx(home, "tui"));
    await b.emit("session_shutdown", { reason: "quit" }, b.ctx(home, "tui"));
  }
});
