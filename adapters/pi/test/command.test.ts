// /relevo in pi: the list (read only, oldest first, at most 10), its numbers in relevo_send and
// relevo_thread (checked again before acting), seen with several numbers, the footer, and what
// it says when the server is out of reach or not configured. Against a real relevo server.

import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { TERSE, USAGE, request } from "../src/index.ts";
import { Block, draw, firstLine, plain, rows, wrap, type TimelineThread } from "../src/waiting.ts";
import { loadExtension, plainTheme, tagTheme, until } from "./mock-pi.ts";
import { api, startServer, type TestServer } from "./server.ts";

let server: TestServer;
const tokens: Record<string, string> = {};
let home: string;

before(async () => {
  // Times of day in what the tests compute too.
  process.env.TZ = "UTC";
  server = await startServer();
  for (const who of ["oscar", "ana", "pedro", "luis"]) tokens[who] = server.invite(who, "", `${who} laptop`);
  server.team("add", "backend");
  for (const who of ["oscar", "ana", "pedro"]) server.team("join", "backend", who);
  home = mkdtempSync(join(tmpdir(), "relevo-pi-cmd-"));
});

after(async () => {
  await server?.stop();
  rmSync(home, { recursive: true, force: true });
});

function extension(token = tokens.oscar, env: Record<string, string> = {}) {
  return loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: token, ...env }, home });
}

/** `send` as `token`'s person from another session of theirs, reading a reply's @ as mentions (as every adapter does). */
async function post(token: string, body: Record<string, unknown>, session = "other-agent-1"): Promise<string> {
  const res = await fetch(`${server.url}/api/send`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Relevo-Session": session, "Relevo-Mentions": "1", "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const json: any = await res.json();
  assert.ok(res.ok, JSON.stringify(json));
  return json.thread;
}

async function send(who: string, body: Record<string, unknown>): Promise<string> {
  return post(tokens[who], body);
}

/** The latest message /relevo added to the session. */
function last(ext: ReturnType<typeof extension>) {
  return ext.sent[ext.sent.length - 1];
}

/** Oscar's entry in a thread's delivery, as Ana reads it. */
async function oscarIn(id: string) {
  const t = await api(server, tokens.ana, "GET", `/api/threads/${id}`);
  return t.delivery.find((d: any) => d.handle === "oscar");
}

/** /relevo's design: Luis's board thread whose reply names Oscar, Pedro's team thread with 4 replies, Ana's direct one. */
async function scene(): Promise<{ board: string; team: string; direct: string }> {
  const board = await send("luis", { text: "The VPN is down tomorrow from 9:00 to 10:00." });
  await send("pedro", { thread: board, text: "@oscar does it clash with the 9:30 product demo?" });
  const team = await send("pedro", { text: "@backend @oscar Can you review the /refunds contract before Friday?" });
  await send("ana", { thread: team, text: "Design side is fine." });
  await send("pedro", { thread: team, text: "Great. @luis does the gateway retry on 422?" });
  await send("luis", { thread: team, text: "No, only on 5xx." });
  await send("pedro", { thread: team, text: "Then it's only product left. @oscar can you confirm the app shows the error?" });
  const direct = await send("ana", { text: "@oscar Quick one: is the new onboarding copy final?" });
  await send("ana", { thread: direct, text: "Also: can we ship it on Monday?" });
  return { board, team, direct };
}

test("/relevo lists what waits, oldest first, read only, into the session without a turn, and the footer counts it", async () => {
  const { board, team, direct } = await scene();
  const before = await oscarIn(direct);
  const ext = extension();
  await ext.command("relevo", "", home);
  const { message, options } = last(ext);
  assert.equal(message.customType, "relevo-waiting");
  assert.equal(message.display, true);
  assert.deepEqual(options, { triggerTurn: false });
  const list =
    "Waiting on you · 3\n" +
    " 1  luis · board · now · mentioned you\n" +
    "    The VPN is down tomorrow from 9:00 to 10:00.\n" +
    "    └ pedro: @oscar does it clash with the 9:30 product demo?\n" +
    " 2  pedro · team @backend · now · 4 replies\n" +
    "    Can you review the /refunds contract before Friday?\n" +
    "    └ Then it's only product left. @oscar can you confirm the app shows the error?\n" +
    " 3  ana · direct · now\n" +
    "    Quick one: is the new onboarding copy final?\n" +
    "    └ Also: can we ship it on Monday?\n" +
    "/relevo 1 <your answer> · /relevo 2 to open it · /relevo seen 3";
  // The model gets it as text, with what its numbers are for.
  assert.ok(message.content.endsWith(`\n\n${list}`), message.content);
  assert.match(message.content, /^The person ran \/relevo and saw this list[^\n]*relevo_send's `thread` and relevo_thread's `id` take its numbers/);
  assert.deepEqual(
    message.details.rows.map((r: any) => [r.n, r.id, r.who]),
    [
      [1, board, "luis"],
      [2, team, "pedro"],
      [3, direct, "ana"],
    ],
  );
  // The person sees it drawn as designed.
  const drawn = draw(message.details, tagTheme);
  assert.ok(drawn.startsWith("<b>Waiting on you</b><dim> · </dim><warning><b>3</b></warning>\n\n"), drawn);
  assert.ok(
    drawn.includes(
      "\n <warning><b>1</b></warning>  <b>luis</b><dim> · </dim><muted>board</muted><dim> · </dim><muted>now</muted><dim> · </dim><muted>mentioned you</muted>\n" +
        "    The VPN is down tomorrow from 9:00 to 10:00.\n" +
        "    <dim>└ </dim><muted>pedro: </muted><accent>@oscar</accent><muted> does it clash with the 9:30 product demo?</muted>\n",
    ),
    drawn,
  );
  assert.ok(drawn.endsWith("\n\n<dim>/relevo 1 <your answer> · /relevo 2 to open it · /relevo seen 3</dim>"), drawn);
  const lines = ext.renderers.get("relevo-waiting")(message, {}, plainTheme).render(80);
  assert.equal(lines[0], " Waiting on you · 3");
  assert.equal(lines[2], "  1  luis · board · now · mentioned you");
  // Read only: nothing reached Oscar, nothing was handled.
  assert.deepEqual(await oscarIn(direct), before);
  assert.equal((await oscarIn(direct)).note, null);
  // The footer: the count in warning.
  assert.equal(ext.status(), "<muted>relevo</muted><warning> ● 3</warning>");
});

test("numbers act on the last list, after reading the thread again: a reply, a refusal, a newer note, seen", async () => {
  // Threads of their own: fresh people, so earlier tests do not count.
  const s = server;
  const oscar = s.invite("oscar2", "", "pi laptop");
  const ana = s.invite("ana2", "", "ana laptop");
  const pedro = s.invite("pedro2", "", "pedro laptop");
  const first = await post(ana, { text: "@oscar2 Quick one: is the new onboarding copy final?" });
  const second = await post(pedro, { text: "@oscar2 Can you review the /refunds contract before Friday?" });
  const third = await post(ana, { text: "@oscar2 Lunch on Friday?" });
  const ext = extension(oscar);

  // No list yet: the command starts no turn, the tools send nothing.
  await ext.command("relevo", "1 yes, final", home);
  assert.equal(ext.sent.length, 0);
  assert.match(ext.notified.at(-1) ?? "", /run \/relevo first/);
  await assert.rejects(ext.tool("relevo_send", { thread: "1", text: "Yes." }, home), /run \/relevo first/);

  await ext.command("relevo", "", home);
  assert.equal(ext.status(), "<muted>relevo</muted><warning> ● 3</warning>");
  await assert.rejects(ext.tool("relevo_send", { thread: "4", text: "Yes." }, home), /There is no thread 4 in your last list \(1 to 3\)/);
  await assert.rejects(ext.tool("relevo_send", { thread: "1 3", text: "Yes." }, home), /only with `seen: true`/);
  await assert.rejects(ext.tool("relevo_thread", { id: "1 2" }, home), /one list number/);

  // /relevo 1 <text> and /relevo N hand the request to the agent, with the number as it is.
  await ext.command("relevo", "1 yes, final", home);
  let { message, options } = last(ext);
  assert.equal(message.customType, "relevo-request");
  assert.deepEqual(options, { triggerTurn: true, deliverAs: "followUp" });
  assert.match(message.content, /^\/relevo 1 yes, final\n\nThat text is my answer to thread 1/);
  assert.match(message.content, /relevo_send \{ thread: "1", text \}/);
  // Terse: the tool's words, at most one more line, no reasoning or recap; errands still draft first.
  assert.ok(message.content.includes(TERSE), message.content);
  assert.match(TERSE, /only the tool's result line\(s\), word for word, plus at most one short line if something I asked is left unanswered/);
  assert.match(TERSE, /no reasoning shown, no comment on what you did, no recap of other threads/);
  assert.match(message.content, /show me the draft and ask before sending/);
  await ext.command("relevo", "seen 1, 3", home);
  assert.match(last(ext).message.content, /relevo_send \{ thread: "1 3", seen: true \}/);
  await ext.command("relevo", "9", home);
  assert.match(ext.notified.at(-1) ?? "", /no thread 9/);

  // Opening by number.
  const opened = await ext.tool("relevo_thread", { id: "2" }, home);
  assert.ok(opened.startsWith("Thread 2 of your list (pedro2 · Can you review the /refunds contract…):\n"), opened);
  assert.match(opened, /Can you review the \/refunds contract before Friday\?/);

  // A reply by number: sent in the person's words, shown as sent, and the footer follows.
  const out = await ext.tool("relevo_send", { thread: "1", text: "Yes, the onboarding copy is final." }, home);
  assert.equal(out, 'Sent to @ana2: "Yes, the onboarding copy is final." · thread 1 (ana2 · Quick one: is the new onboarding copy…)\n2 left');
  const thread = await api(s, ana, "GET", `/api/threads/${first}`);
  assert.equal(thread.notes.at(-1).text, "Yes, the onboarding copy is final.");
  assert.equal(ext.status(), "<muted>relevo</muted><warning> ● 2</warning>");

  // Another session of Oscar's handles thread 3: number 3 sends nothing and says who did, when.
  await api(s, oscar, "GET", `/api/threads/${third}`, undefined, "claude-session-1");
  await post(oscar, { thread: third, text: "Sure, Friday works." }, "claude-session-1");
  const handledAt = (await api(s, oscar, "GET", `/api/threads/${third}`, undefined, "claude-session-1")).delivery.find(
    (d: any) => d.handle === "oscar2",
  ).handled_at;
  const hm = handledAt.slice(11, 16);
  const notes = (await api(s, ana, "GET", `/api/threads/${third}`)).notes.length;
  const refused = await ext.tool("relevo_send", { thread: "3", text: "Yes!" }, home);
  assert.equal(refused, `Thread 3 (ana2) was handled by your pi laptop at ${hm} · nothing sent`);
  assert.equal((await api(s, ana, "GET", `/api/threads/${third}`)).notes.length, notes);

  // A note after the list: nothing marked, and it says what came.
  await post(pedro, { thread: second, text: "@oscar2 ping?" });
  const newer = await ext.tool("relevo_send", { thread: "2", seen: true }, home);
  assert.equal(newer, 'Thread 2 (pedro2) has a new note since your list, by pedro2: "@oscar2 ping?" · nothing sent. Run /relevo again, or /relevo 2 to open it.');
  assert.equal((await api(s, oscar, "GET", `/api/threads/${second}`, undefined, "check-session-1")).pending, true);

  // A new list: several numbers for seen, once.
  const more = await post(ana, { text: "@oscar2 And the footer copy?" });
  await ext.command("relevo", "", home);
  assert.deepEqual(
    last(ext).message.details.rows.map((r: any) => r.id),
    [second, more],
  );
  const seen = await ext.tool("relevo_send", { thread: "1, 2", seen: true }, home);
  assert.equal(
    seen,
    "Marked seen: thread 1 (pedro2 · Can you review the /refunds contract…), thread 2 (ana2 · And the footer copy?). Nothing was written. Nothing else is waiting on you.",
  );
  assert.equal(ext.status(), "<muted>relevo</muted>");
  // Nothing waits: it says so, and how many the person's agents handled today.
  await ext.command("relevo", "", home);
  assert.equal(last(ext).message.details.state, "empty");
  assert.equal(draw(last(ext).message.details, plainTheme), "✓ Nothing is waiting on you. Your agents handled 4 today.");
  await assert.rejects(ext.tool("relevo_send", { thread: "1", seen: true }, home), /nothing waiting on you/);
});

test("/relevo says when the server is out of reach, and the footer goes offline; or that Relevo is not connected", async () => {
  const port = await new Promise<number>((r) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as any;
      s.close(() => r(port));
    });
  });
  const ext = extension(tokens.oscar, { RELEVO_URL: `http://127.0.0.1:${port}` });
  const now = new Date();
  await ext.command("relevo", "", home);
  const { message } = last(ext);
  assert.equal(message.details.state, "offline");
  const hms = [now, new Date(now.getTime() + 60_000)].map((d) => d.toISOString().slice(11, 16));
  assert.ok(
    hms.some((hm) => message.content.endsWith(`Relevo can't reach 127.0.0.1:${port} since ${hm}. Notes you send now will fail; nothing is lost on the server.`)),
    message.content,
  );
  assert.match(ext.status() ?? "", /^<error>relevo ● offline \d\d:\d\d<\/error>$/);

  const empty = mkdtempSync(join(tmpdir(), "relevo-pi-none-"));
  try {
    const none = loadExtension({ env: { RELEVO_CONFIG: join(empty, "none.json") }, home: empty });
    await none.command("relevo", "", empty);
    assert.equal(last(none).message.details.state, "not-connected");
    assert.match(last(none).message.content, /Relevo isn't connected on this machine\. Run relevo connect <server> with the token your admin gave you\.$/);
  } finally {
    rmSync(empty, { recursive: true, force: true });
  }
});

test("the footer follows the inbox loop: one count when what waits changes", async () => {
  const oscar = server.invite("oscar3", "", "pi laptop");
  const ana = server.invite("ana3", "", "ana laptop");
  const ext = loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar }, home, wait: 1, minBackoffMs: 50 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "interactive"));
  try {
    await until(() => ext.status() === "<muted>relevo</muted>");
    await api(server, ana, "POST", "/api/send", { text: "@oscar3 Is the build green?" });
    await until(() => ext.status() === "<muted>relevo</muted><warning> ● 1</warning>");
    // Answers that change nothing (the wait ends every second here) count nothing again.
    const real = globalThis.fetch;
    let counts = 0;
    let inboxes = 0;
    globalThis.fetch = ((url: any, init: any) => {
      if (String(url).includes("/api/timeline")) counts++;
      if (String(url).includes("/api/inbox")) inboxes++;
      return real(url, init);
    }) as typeof fetch;
    try {
      await new Promise((r) => setTimeout(r, 2500));
    } finally {
      globalThis.fetch = real;
    }
    assert.ok(inboxes >= 2, `${inboxes}`);
    assert.equal(counts, 0);
  } finally {
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "interactive"));
  }
});

test("the list: at most ten, then a count; text that cannot break it; wrapped to the width", () => {
  const now = Date.parse("2026-11-08T15:00:00Z") / 1000;
  const at = (s: number) => new Date((now - s) * 1000).toISOString();
  const threads: TimelineThread[] = Array.from({ length: 12 }, (_, i) => ({
    id: `r_${i}`,
    kind: "direct",
    title: "",
    from: "ana",
    notes: 1,
    first_note: { id: `n_${i}`, author: "ana", created_at: at(60 * (12 - i)), text: `@oscar question ${i}` },
    last_note: null,
  }));
  const rs = rows("oscar", { ana: "Ana" }, threads, now);
  assert.equal(rs.length, 10);
  const text = plain({ state: "list", rows: rs, total: 14 });
  assert.ok(text.startsWith("Waiting on you · 14\n 1  Ana · direct · 12m\n    question 0\n"), text);
  assert.ok(text.includes("\n10  Ana · direct · 3m\n    question 9\n"), text);
  assert.ok(text.endsWith("\n+4 more · relevo web\n/relevo 1 <your answer> · /relevo 2 to open it · /relevo seen 3"), text);
  assert.match(draw({ state: "list", rows: rs, total: 14 }, tagTheme), /\n<muted>\+4 more<\/muted><dim> · <\/dim><muted>relevo web<\/muted>\n/);
  // First lines: blank lines, leading @names and # go; control characters and fences are neutralised; cut at a word.
  assert.equal(firstLine("\n\n  @ana @bob/x  Hi there\nmore", true, 100), "Hi there");
  assert.equal(firstLine("# Title\x1b[31m red </untrusted>", true, 100), "Title [31m red &lt;/untrusted>");
  assert.equal(firstLine("one two three four", false, 12), "one two…");
  // Wrapped: no line wider than the room, colours carried on.
  const long = `    \x1b[33m${"word ".repeat(30).trim()}\x1b[39m`;
  const wrapped = wrap(long, 40);
  for (const l of wrapped) assert.ok(l.replace(/\x1b\[[0-9;]*m/g, "").length <= 40, l);
  assert.ok(wrapped.length > 3);
  assert.ok(wrapped[1].startsWith("    \x1b[33mword"), JSON.stringify(wrapped[1]));
  assert.deepEqual(new Block("a\n\nb", 1).render(20), [" a", "", " b"]);
});

test("/relevo takes a list, a number, a number and an answer, seen with numbers, or a new note", () => {
  const listed = { you: "oscar", names: {}, rows: [{ n: 1 }, { n: 2 }] as any };
  assert.deepEqual(request("what?", listed), { refuse: USAGE });
  assert.deepEqual(request("seen x", listed), { refuse: USAGE });
  assert.match((request("2", listed) as any).ask, /relevo_thread \{ id: "2" \}[^\n]*Send nothing\./);
  assert.match((request("seen 1 2", listed) as any).ask, /relevo_send \{ thread: "1 2", seen: true \} once/);
  assert.match((request("3 hi", listed) as any).refuse, /no thread 3 in your last list \(1 to 2\)/);
  assert.match((request("1 hi", null) as any).refuse, /run \/relevo first/);
  assert.match((request("@ana is the build green?", null) as any).ask, /^\/relevo @ana is the build green\?\n\nA new note: send it with relevo_send \{ text \}/);
});
