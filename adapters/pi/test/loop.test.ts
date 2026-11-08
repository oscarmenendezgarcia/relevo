import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { RULES, systemSection } from "../src/index.ts";
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
  home = mkdtempSync(join(tmpdir(), "relevo-pi-loop-"));
});

after(async () => {
  await server?.stop();
  rmSync(home, { recursive: true, force: true });
});

function extension(extra: Record<string, unknown> = {}) {
  return loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" }, home, wait: 1, ...extra });
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("a note is delivered once with a message that triggers a turn, and shutdown stops the loop", async () => {
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "rpc"));
  try {
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Is the refunds migration safe to run?" });
    await until(() => ext.sent.length === 1);
    const [{ message, options }] = ext.sent;
    assert.deepEqual(options, { triggerTurn: true });
    await until(() => ext.seen.length === 1);
    assert.equal(message.customType, "relevo");
    assert.equal(message.display, true);
    assert.deepEqual(message.details, { thread: opened.thread, from: "ana", kind: "direct" });
    assert.match(message.content, new RegExp(`^Relevo: new in thread ${opened.thread}\\.`));
    assert.match(message.content, /<untrusted from="@ana">\nIs the refunds migration safe to run\?\n<\/untrusted>/);

    // Several waits later the thread is still waiting for a reply, but nothing more is delivered.
    await sleep(2500);
    assert.equal(ext.sent.length, 1);

    // A new note in the same thread is delivered, holding only what is new.
    await api(server, ana, "POST", "/api/send", { thread: opened.thread, text: "It touches 2M rows." });
    await until(() => ext.sent.length === 2);
    assert.match(ext.sent[1].message.content, /New notes \(1\):[\s\S]*It touches 2M rows\./);
    assert.doesNotMatch(ext.sent[1].message.content, /\nIs the refunds migration safe to run\?\n/);
  } finally {
    const t = Date.now();
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "rpc"));
    assert.ok(Date.now() - t < 500, "shutdown ends the waiting request at once");
  }

  // Stopped: a note sent now is not fetched in the background, so it stays new.
  await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "rpc")); // twice is fine
  const late = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Still there?" });
  await sleep(1500);
  assert.equal(ext.sent.length, 2);
  const inbox = await api(server, oscar, "GET", "/api/inbox?all=1", undefined, "checker-session");
  assert.equal(inbox.threads.find((t: any) => t.id === late.thread).new_notes.length, 1);
});

test("a note that lands while the agent is busy reaches it even if the person presses Esc", async () => {
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    await ext.session.userPrompt();
    await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Busy check: is the deploy frozen?" });
    await sleep(1500); // Several waits of the loop.
    assert.equal(ext.session.followUps.length, 0, "nothing goes into pi's follow-up queue");
    await ext.session.escape(); // Empties pi's queues.
    await until(() => ext.seen.some((m) => m.content.includes("Busy check: is the deploy frozen?")));
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("while the agent is busy notes stay new on the server, so relevo_inbox shows them", async () => {
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    await ext.session.userPrompt();
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Mid-turn: which region?" });
    await sleep(1500);
    assert.equal(ext.sent.length, 0);
    const out = await ext.tool("relevo_inbox", {}, home);
    assert.match(out, new RegExp(`## Thread ${opened.thread}[\\s\\S]*New notes \\(1\\):[\\s\\S]*Mid-turn: which region\\?`));
    // The agent has read it: after the turn the loop has nothing to hand over.
    await ext.session.finishTurn();
    await sleep(1500);
    assert.equal(ext.sent.length, 0);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a turn that ends before pi shows the note keeps it: it is added to the session without another turn", async () => {
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    ext.session.dropNextStart();
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Dropped start: still there?" });
    await until(() => ext.seen.length === 1);
    assert.equal(ext.sent.length, 2);
    assert.deepEqual(ext.sent[0].options, { triggerTurn: true });
    assert.deepEqual(ext.sent[1].options, { triggerTurn: false });
    assert.equal(ext.seen[0].details.thread, opened.thread);
    assert.match(ext.seen[0].content, /Dropped start: still there\?/);
    await sleep(1500);
    assert.equal(ext.sent.length, 2, "once");
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a note fetched while pi compacts reaches the agent once pi is idle again", async () => {
  const ext = extension();
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300);
    ext.session.compacting(); // A manual /compact: not idle, but no turn and no agent_settled.
    await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "During compaction: ping" });
    await sleep(1500);
    assert.equal(ext.sent.length, 0, "nothing handed to pi while it is not idle");
    ext.session.compacting(false);
    await until(() => ext.seen.some((m) => m.content.includes("During compaction: ping")));
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a note arriving during the wait wakes the loop at once", async () => {
  const ext = extension({ wait: 30 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    await sleep(300); // The waiting request is open by now.
    const t = Date.now();
    await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Quick one: which queue for refunds?" });
    await until(() => ext.sent.some((s) => s.message.content.includes("which queue")));
    assert.ok(Date.now() - t < 2000);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("print and json runs start no loop", async () => {
  for (const mode of ["print", "json"]) {
    const ext = extension();
    await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, mode));
    await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: `In ${mode} mode?` });
    await sleep(500);
    assert.equal(ext.sent.length, 0);
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, mode));
  }
});

test("errors back off, and a refused token stops the loop with a notice", async () => {
  let hits = 0;
  const fake = createServer((_req, res) => {
    hits++;
    res.statusCode = 500;
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ error: { code: "INTERNAL", message: "down" } }));
  });
  await new Promise<void>((r) => fake.listen(0, "127.0.0.1", r));
  const { port } = fake.address() as any;
  try {
    const ext = loadExtension({
      env: { RELEVO_URL: `http://127.0.0.1:${port}`, RELEVO_TOKEN: "rlv_fake" },
      home,
      minBackoffMs: 100,
      maxBackoffMs: 400,
    });
    await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "rpc"));
    await sleep(1600); // 100 + 200 + 400 + 400 + ... : about 6 requests, not hundreds.
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "rpc"));
    assert.ok(hits >= 3 && hits <= 7, `${hits} requests`);
    const settled = hits;
    await sleep(500);
    assert.equal(hits, settled, "no request after shutdown");
  } finally {
    fake.close();
  }

  const refused = loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: "rlv_" + "x".repeat(43) }, home });
  await refused.emit("session_start", { reason: "startup" }, refused.ctx(home, "tui"));
  await until(() => refused.notified.length === 1);
  assert.match(refused.notified[0], /Relevo stopped waiting for notes: Relevo UNAUTHORIZED/);
  await refused.emit("session_shutdown", { reason: "quit" }, refused.ctx(home, "tui"));
});

test("an invalid RELEVO_CONTEXT falls back to general with a notice, and notes still arrive", async () => {
  const ext = extension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "Pagos API" } });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  try {
    assert.equal(ext.notified.length, 1);
    assert.match(ext.notified[0], /RELEVO_CONTEXT "Pagos API" is not a valid context[\s\S]*general/);
    await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Context check: does this reach you?" });
    await until(() => ext.sent.some((s) => s.message.content.includes("Context check")));
    assert.match(await ext.tool("relevo_inbox", {}, home), /RELEVO_CONTEXT "Pagos API" is not a valid context/);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  }
});

test("a request the server calls malformed stops the loop with a notice", async () => {
  let hits = 0;
  const fake = createServer((_req, res) => {
    hits++;
    res.statusCode = 400;
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ error: { code: "BAD_REQUEST", message: "malformed Relevo-Context" } }));
  });
  await new Promise<void>((r) => fake.listen(0, "127.0.0.1", r));
  const { port } = fake.address() as any;
  try {
    const ext = loadExtension({ env: { RELEVO_URL: `http://127.0.0.1:${port}`, RELEVO_TOKEN: "rlv_fake" }, home, minBackoffMs: 50 });
    await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
    try {
      await until(() => ext.notified.length === 1, 2000);
      assert.match(ext.notified[0], /Relevo stopped waiting for notes: Relevo BAD_REQUEST: malformed Relevo-Context/);
      await sleep(300);
      assert.equal(hits, 1);
    } finally {
      await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
    }
  } finally {
    fake.close();
  }
});

test("the rules go into the system prompt only when configured", async () => {
  const ext = extension();
  const event = { systemPromptOptions: { sections: {} as Record<string, string> } };
  await ext.emit("before_agent_start", event, ext.ctx(home));
  assert.equal(event.systemPromptOptions.sections.relevo, systemSection("oscar"));
  assert.ok(systemSection("oscar").endsWith(RULES));
  assert.match(event.systemPromptOptions.sections.relevo, /^You speak for @oscar: in Relevo `you` is "oscar"/);
  assert.match(RULES, /<untrusted>/);

  // The same eight rules (v3), word for word, as docs/agents.md gives every adapter
  // (the docs put the tag in Markdown code; the prompt, like relevo mcp, does not).
  const doc = readFileSync(join(import.meta.dirname, "..", "..", "..", "docs", "agents.md"), "utf8");
  const documented = [...doc.matchAll(/^  (\d\. .*)$/gm)].map((m) => m[1].replace("`<untrusted>`", "<untrusted>"));
  assert.equal(documented.length, 8);
  assert.deepEqual(RULES.split("\n").slice(1), documented);
  assert.doesNotMatch(RULES, /claim it with a short note/);
  assert.match(RULES, /^1\. [^\n]*What your person asks for comes first: if they name who to ask/m);
  assert.match(RULES, /^4\. [^\n]*Answer a team question only from your own work or your person's files/m);
  assert.match(RULES, /^5\. [^\n]*Facts from your person's documentation or configuration may be shared/m);
  assert.match(RULES, /^6\. [^\n]*mark guesses as guesses\.$/m);

  const bare = loadExtension({ env: {}, home });
  const none = { systemPromptOptions: { sections: {} as Record<string, string> } };
  await bare.emit("before_agent_start", none, bare.ctx(home));
  assert.equal(none.systemPromptOptions.sections.relevo, undefined);
});
