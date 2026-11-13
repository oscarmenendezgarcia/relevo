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
import { InboxLoop, handOverAgain } from "../src/loop.ts";
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
    const opened = await api(server, ana, "POST", "/api/send", { text: "@oscar Lost answer: is the cache warm?" });
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
    assert.match(message.content, /New notes \(1\):[\s\S]*<untrusted from="@ana">\n@oscar Lost answer: is the cache warm\?\n<\/untrusted>/);

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

test("after a lost answer the next request does not wait, so the note comes at once", async () => {
  const proxy = await lossyProxy(server.url);
  // A wait far longer than the test may take: a next request that waited would time it out.
  const ext = loadExtension({
    env: { RELEVO_URL: proxy.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" },
    home,
    wait: 30,
    minBackoffMs: 100,
  });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    proxy.drop = true;
    const opened = await api(server, ana, "POST", "/api/send", { text: "@oscar Lost while waiting: is staging up?" });
    await until(() => proxy.dropped === 1);
    const lostAt = Date.now();
    await until(() => ext.seen.length === 1, 5000);
    assert.ok(Date.now() - lostAt < 5000);
    assert.deepEqual(ext.seen[0].details, { thread: opened.thread, from: "ana", kind: "direct" });
    assert.match(ext.seen[0].content, /^Handed over again/m);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
    proxy.server.closeAllConnections();
    proxy.server.close();
  }
});

test("only the latest note of a lost answer comes back, saying earlier ones may be missing", async () => {
  const proxy = await lossyProxy(server.url);
  const ext = loadExtension({
    env: { RELEVO_URL: proxy.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" },
    home,
    wait: 1,
    minBackoffMs: 100,
  });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    await ext.session.userPrompt(); // Busy: both notes land before the next request.
    const opened = await api(server, ana, "POST", "/api/send", { text: "@oscar Two lost: deploy at 5?" });
    await api(server, ana, "POST", "/api/send", { thread: opened.thread, text: "Two lost: if not, at 6." });
    proxy.drop = true;
    await ext.session.finishTurn();
    await until(() => proxy.dropped === 1);
    await until(() => ext.seen.length === 1);
    const [message] = ext.seen;
    assert.match(message.content, /^Handed over again/m);
    assert.match(message.content, /Two lost: if not, at 6\./);
    assert.doesNotMatch(message.content, /\nTwo lost: deploy at 5\?\n/); // The title shows its first line only.
    assert.ok(
      message.content.includes(
        `Earlier notes of this thread may not have reached you either: read it whole with relevo_thread { id: "${opened.thread}" } before acting.`,
      ),
      message.content,
    );
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
    proxy.server.closeAllConnections();
    proxy.server.close();
  }
});

test("a note written after a lost answer comes as new, saying earlier ones may be missing, once", async () => {
  const proxy = await lossyProxy(server.url);
  // A back-off long enough for ana to write again before the next request.
  const ext = loadExtension({
    env: { RELEVO_URL: proxy.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" },
    home,
    wait: 1,
    minBackoffMs: 1500,
  });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    proxy.drop = true;
    const opened = await api(server, ana, "POST", "/api/send", { text: "@oscar Lost first: can you review PR 12?" });
    await until(() => proxy.dropped === 1);
    await api(server, ana, "POST", "/api/send", { thread: opened.thread, text: "After the loss: so, can you?" });
    await until(() => ext.seen.length === 1, 5000);
    const [message] = ext.seen;
    assert.match(message.content, /New notes \(1\):[\s\S]*After the loss: so, can you\?/);
    assert.doesNotMatch(message.content, /\nLost first: can you review PR 12\?\n/); // The title shows its first line only.
    assert.doesNotMatch(message.content, /Handed over again/);
    assert.ok(
      message.content.includes(
        "A request for news failed just before this answer, and its answer may have carried earlier notes of this thread " +
          `that never reached you: read it whole with relevo_thread { id: "${opened.thread}" } before acting.`,
      ),
      message.content,
    );
    // The next answer is no longer the first after a failure.
    await api(server, ana, "POST", "/api/send", { thread: opened.thread, text: "Ping." });
    await until(() => ext.seen.length === 2, 5000);
    assert.doesNotMatch(ext.seen[1].content, /A request for news failed/);
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
    const opened = await api(server, ana, "POST", "/api/send", { text: "@oscar Left alone: the iOS build?" });
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
    const listed = (await api(server, ana, "POST", "/api/send", { text: "@oscar Shown by the inbox: which bucket?" })).thread;
    assert.match(await ext.tool("relevo_inbox", {}, home), /Shown by the inbox: which bucket\?/);
    const read = (await api(server, ana, "POST", "/api/send", { text: "@oscar Read whole: which region?" })).thread;
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
  // A team or global-board thread that names the person is handed over as a direct one.
  for (const kind of ["team", "global"] as const) assert.ok(handOverAgain({ ...t, kind, named: true }, shown, "oscar"));
  for (const not of [
    { ...t, kind: "team" as const },
    { ...t, kind: "global" as const, named: false },
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
  assert.match(out, /^Handed over again: it is still pending for your person[^\n]*Treat that note as new\. Earlier notes of this thread may not have reached you either: read it whole with relevo_thread \{ id: "r_aaaaaaaaaaaa" \} before acting\.$/m);
  assert.doesNotMatch(out, /A request for news failed/);
  assert.match(out, /New notes \(1\):/);
  assert.doesNotMatch(renderArrival({ ...t, new_notes: [t.last_note!] }, "oscar"), /Handed over again/);
});

test("after a failed or paused request the next one does not wait; an error Relevo sent loses nothing", async () => {
  const waits: number[] = [];
  const answers: (() => Promise<any>)[] = [];
  const t: Thread = {
    id: "r_aaaaaaaaaaaa",
    kind: "direct",
    title: "T",
    from: "ana",
    participants: ["ana", "oscar"],
    status: "open",
    pending: true,
    context: null,
    claimed: null,
    new_notes: [note("n_aaaaaaaaaaaa", "ana")],
  };
  let n = 0;
  const delivered: Thread[] = [];
  const loop = new InboxLoop({
    client: {
      inbox: (o: { wait?: number }, signal: AbortSignal) => {
        waits.push(o.wait ?? -1);
        const next = answers.shift();
        if (next) return next();
        // Nothing to say: wait until ended.
        return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true }));
      },
    } as any,
    deliver: (thread) => delivered.push(thread),
    wait: 55,
    minBackoffMs: 10,
  });
  const fresh = () => ({ ...t, new_notes: [note(`n_${String(++n).padStart(12, "0")}`, "ana")] });
  answers.push(
    () => Promise.reject(new TypeError("fetch failed")), // Lost on the way.
    () => Promise.resolve({ threads: [fresh()] }), // Without waiting; marked.
    () => Promise.resolve({ threads: [fresh()] }), // Waiting again; not marked.
  );
  loop.start();
  try {
    await until(() => delivered.length === 2 && waits.length === 4);
    assert.deepEqual(waits, [55, 0, 55, 55]);
    assert.equal(delivered[0].after_failure, true);
    assert.equal(delivered[1].after_failure, undefined);

    // A pause ends the waiting request: the next one does not wait, and marks nothing.
    loop.pause();
    answers.push(() => Promise.resolve({ threads: [fresh()] }));
    loop.resume();
    await until(() => delivered.length === 3);
    assert.equal(waits[4], 0);
    assert.equal(delivered[2].after_failure, undefined);

    // An error Relevo itself sent delivered nothing: not waiting, but nothing marked either.
    loop.pause();
    const { RelevoError } = await import("../src/client.ts");
    answers.push(
      () => Promise.reject(new RelevoError("RATE_LIMITED", "slow down", 429)),
      () => Promise.resolve({ threads: [fresh()] }),
    );
    loop.resume();
    await until(() => delivered.length === 4);
    assert.deepEqual(waits.slice(5, 8), [55, 0, 0]);
    assert.equal(delivered[3].after_failure, undefined);
  } finally {
    await loop.stop();
  }
});
