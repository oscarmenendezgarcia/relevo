// Handed over once (docs/agents.md): in every answer of the background loop, a direct thread
// pending for the person whose latest note this process never showed the agent is handed over
// again, once. That covers an answer lost on the way, which the server delivered all the same.

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, afterEach, before, test } from "node:test";
import type { Note, Thread } from "../src/client.ts";
import { handOverAgain } from "../src/loop.ts";
import { renderArrival } from "../src/render.ts";
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
  home = mkdtempSync(join(tmpdir(), "relevo-pi-handover-"));
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

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const oscarDelivery = async (thread: string) =>
  (await api(server, ana, "GET", `/api/threads/${thread}`)).delivery.find((d: any) => d.handle === "oscar");

/**
 * A proxy to the test server that, while `drop` is set, lets one inbox request with new notes
 * reach the server and get its whole answer, then cuts the connection instead of passing it on.
 */
async function lossyProxy(target: string) {
  const state = { drop: false, dropped: 0, server: null as unknown as Server, url: "" };
  state.server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(req.headers)) {
      if (typeof v === "string" && !["host", "connection", "content-length"].includes(k)) headers[k] = v;
    }
    let status: number;
    let text: string;
    try {
      const up = await fetch(target + req.url, {
        method: req.method,
        headers,
        body: chunks.length ? Buffer.concat(chunks) : undefined,
      });
      status = up.status;
      text = await up.text();
    } catch {
      return void req.socket.destroy();
    }
    if (state.drop && req.method === "GET" && req.url?.startsWith("/api/inbox")) {
      const threads: Thread[] = JSON.parse(text).threads ?? [];
      if (threads.some((t) => (t.new_notes ?? []).length)) {
        state.drop = false;
        state.dropped++;
        return void req.socket.destroy(); // Delivered on the server, lost on the way.
      }
    }
    res.writeHead(status, { "Content-Type": "application/json" });
    res.end(text);
  });
  await new Promise<void>((r) => state.server.listen(0, "127.0.0.1", r));
  state.url = `http://127.0.0.1:${(state.server.address() as any).port}`;
  return state;
}

test("an inbox answer lost on the way is handed over on the next answer, once", async () => {
  const proxy = await lossyProxy(server.url);
  const ext = loadExtension({
    env: { RELEVO_URL: proxy.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" },
    home,
    wait: 1,
    minBackoffMs: 100,
  });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300); // The waiting request is open by now.
    proxy.drop = true;
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Lost answer: is the cache warm?" });
    await until(() => proxy.dropped === 1);
    // The server delivered it to this pi session: it is no longer new there.
    assert.equal((await oscarDelivery(opened.thread)).by.label, "pi laptop");

    await until(() => ext.seen.length === 1);
    assert.equal(ext.sent.length, 1);
    assert.deepEqual(ext.sent[0].options, { triggerTurn: true });
    const [message] = ext.seen;
    assert.deepEqual(message.details, { thread: opened.thread, from: "ana", kind: "direct" });
    assert.match(message.content, /^Handed over again: it is still pending for your person, and its latest note was never shown in this session/m);
    assert.match(message.content, /Treat that note as new\./);
    assert.match(message.content, /New notes \(1\):[\s\S]*<untrusted from="@ana">\nLost answer: is the cache warm\?\n<\/untrusted>/);

    // Left alone (its turn ended without a word): still pending, never handed over again here.
    await sleep(2500); // Several answers of the loop list it, pending, with that latest note.
    assert.equal(ext.sent.length, 1);
    assert.equal((await oscarDelivery(opened.thread)).handled_at, null);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
    proxy.server.closeAllConnections();
    proxy.server.close();
  }
});

test("a thread the agent left alone is not handed over again in the same process", async () => {
  const ext = loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" }, home, wait: 1 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Left alone: the iOS build?" });
    await until(() => ext.seen.length === 1);
    assert.doesNotMatch(ext.seen[0].content, /Handed over again/);
    // Its turn ends without a word: not for it. The thread stays pending, and every answer of
    // the loop lists it with that note as its latest.
    await sleep(2500);
    assert.equal(ext.sent.length, 1);
    const inbox = await api(server, oscar, "GET", "/api/inbox", undefined, "handover-checker");
    assert.equal(inbox.threads.find((t: any) => t.id === opened.thread)?.pending, true);

    // A new note in it is handed over, once, as new: not as handed over again.
    await api(server, ana, "POST", "/api/send", { thread: opened.thread, text: "Or the Android one?" });
    await until(() => ext.seen.length === 2);
    assert.match(ext.seen[1].content, /New notes \(1\):[\s\S]*Or the Android one\?/);
    assert.doesNotMatch(ext.seen[1].content, /Handed over again/);
    await sleep(1500);
    assert.equal(ext.sent.length, 2);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a note listed by relevo_inbox or read with relevo_thread is not handed over again", async () => {
  const ext = loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" }, home, wait: 1 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    await ext.session.userPrompt(); // Busy: the loop asks for nothing.
    const listed = (await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Shown by the inbox: which bucket?" })).thread;
    assert.match(await ext.tool("relevo_inbox", {}, home), /Shown by the inbox: which bucket\?/);
    const read = (await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Read whole: which region?" })).thread;
    assert.match(await ext.tool("relevo_thread", { id: read }, home), /Read whole: which region\?/);
    await ext.session.finishTurn();
    await sleep(1500);
    assert.equal(ext.sent.length, 0, `nothing handed over (${listed}, ${read})`);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

const note = (id: string, author: string, text = "Deploy?"): Note => ({ id, author, via: "x", created_at: "2026-11-10T09:00:00Z", text, truncated: false });

test("only a pending open direct thread with nothing new, whose latest note by someone else was never shown, is handed over again", () => {
  const t: Thread = {
    id: "r_aaaaaaaaaaaa",
    kind: "direct",
    title: "Deploy?",
    from: "ana",
    participants: ["ana", "oscar"],
    status: "open",
    pending: true,
    context: null,
    claimed: null,
    new_notes: [],
    last_note: note("n_aaaaaaaaaaaa", "ana"),
  };
  const shown = new Set<string>();
  const again = handOverAgain(t, shown, "oscar");
  assert.deepEqual(again, { ...t, new_notes: [t.last_note], handed_over: true });
  for (const not of [
    { ...t, kind: "team" as const },
    { ...t, status: "closed" as const },
    { ...t, pending: false },
    { ...t, pending: undefined }, // A server before 0.3.0.
    { ...t, new_notes: [note("n_bbbbbbbbbbbb", "ana")] }, // New notes are handed over as such.
    { ...t, last_note: note("n_bbbbbbbbbbbb", "oscar", "mine") },
    { ...t, last_note: null },
  ]) {
    assert.equal(handOverAgain(not, shown, "oscar"), null, JSON.stringify(not));
  }
  // Shown once: not again. A later note by someone else is.
  shown.add("n_aaaaaaaaaaaa");
  assert.equal(handOverAgain(t, shown, "oscar"), null);
  assert.ok(handOverAgain({ ...t, last_note: note("n_cccccccccccc", "ana", "Or at 6?") }, shown, "oscar"));

  const out = renderArrival(again!, "oscar");
  assert.match(out, /^Handed over again: it is still pending for your person[^\n]*Treat that note as new\.$/m);
  assert.match(out, /New notes \(1\):/);
  assert.doesNotMatch(renderArrival({ ...t, new_notes: [t.last_note!] }, "oscar"), /Handed over again/);
});
