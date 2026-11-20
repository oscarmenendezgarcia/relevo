// /relevo in pi (docs/agents.md, "The /relevo command" and the pi section): the threads waiting
// on the person, numbered, from the read-only timeline, and the lines its actions answer with.
// The same list and the same checks as relevo mcp's `waiting` (src/waiting.rs): rows for pi's
// renderer, plain text for the model. Nothing here talks to the server.

import { neutralize, untrusted } from "./render.ts";

/** The most threads the list shows; the rest are counted. */
export const MAX_SHOWN = 10;
/** How much of a note's first line the list shows. */
const LINE_CHARS = 100;
/** How much of a thread's first line names it in an action's answer. */
const SUBJECT_CHARS = 40;

/** A note as the timeline gives it (docs/protocol.md, "timeline"). */
export interface TimelineNote {
  id: string;
  author: string;
  created_at: string;
  text: string;
}

/** A thread as the timeline gives it. */
export interface TimelineThread {
  id: string;
  kind: "direct" | "team" | "global";
  title: string;
  from: string;
  teams?: string[];
  notes?: number;
  first_note?: TimelineNote | null;
  last_note?: TimelineNote | null;
}

export interface Timeline {
  you: string;
  names?: Record<string, string>;
  /** Who of them are bots (servers from 0.8.0). */
  bots?: string[];
  counts: { waiting?: number; handled?: number };
  threads: TimelineThread[];
  cursor?: string | null;
}

/** A thread of the list, as its number refers to it and as pi draws it. */
export interface Row {
  n: number;
  id: string;
  /** Whoever opened it ("You" for the person). */
  who: string;
  /** Their handle, and whether it is the person's own thread (its text is not fenced for the model). */
  opener?: string;
  own?: boolean;
  /** The opener is a bot: `Reviewer (bot)`. */
  bot?: boolean;
  /** `direct`, `team @backend`, `board`. */
  kind: string;
  /** How long since its latest note: `now`, `5m`, `2h`, `3d`. */
  age: string;
  /** Replies, said from two on (0 otherwise). */
  replies: number;
  /** A board thread: it waits on the person because it names them. */
  mentioned: boolean;
  /** The first line of its first note, without the `@` names it starts with. */
  first: string;
  /** Its latest note, with its author's name when not the opener's (and their handle). */
  reply: { by: string | null; text: string; author?: string; own?: boolean; bot?: boolean } | null;
  /** Its first line, short: names it in an action's answer. */
  subject: string;
  /** The latest note the list showed: an action refuses when a newer one came since. */
  lastNote: string;
}

/** The last list shown: whom it was for, the names it knew, and its threads. */
export interface Listed {
  you: string;
  names: Record<string, string>;
  rows: Row[];
}

/** What /relevo shows, kept in the message's `details` for pi to draw it again. */
export type Shown =
  | { state: "list"; rows: Row[]; total: number; handled?: number | null }
  | { state: "empty"; handled: number | null }
  | { state: "offline"; host: string; since: string }
  | { state: "not-connected" }
  | { state: "error"; message: string };

/**
 * A teammate's text on one line, as the list shows it (a terminal shows it): control characters
 * (C0, C1) and the line and paragraph separators U+2028 and U+2029 become spaces, and Unicode
 * format characters (category Cf: U+202E and the other direction overrides, zero-width spaces and
 * joiners, U+FEFF…) are dropped. As relevo mcp's `plain` (src/waiting.rs).
 */
export function scrub(text: string): string {
  return text.replace(/\p{Cf}/gu, "").replace(/[\p{Cc}\u2028\u2029]/gu, " ");
}

function nameOf(names: Record<string, string>, you: string, handle: string): string {
  if (handle === you) return "You";
  const name = typeof names[handle] === "string" ? scrub(names[handle]).trim() : "";
  return name || scrub(handle);
}

/** `text` cut to `max` characters, at a word when one ends in the second half, with `…` when cut. */
function cut(text: string, max: number): string {
  const chars = Array.from(text);
  if (chars.length <= max) return text;
  const head = chars.slice(0, max);
  let at = max;
  for (let i = head.length - 1; i >= 0; i--) {
    if (/\s/.test(head[i])) {
      if (i >= Math.floor(max / 2)) at = i;
      break;
    }
  }
  return `${chars.slice(0, at).join("").trimEnd()}…`;
}

/**
 * The first line of `text` that is not blank (U+2028 and U+2029 end a line too), `scrub`bed, and,
 * with `mentions`, without the `@` names it starts with: the list is for the person, who knows a
 * note names them.
 */
export function firstLine(text: string, mentions: boolean, max: number): string {
  const raw = text.split(/\r?\n|[\u2028\u2029]/).map(scrub).find((l) => l.trim() !== "") ?? "";
  let line = raw
    .trim()
    .replace(/^#+/, "")
    .trim();
  if (mentions) {
    let rest = line;
    while (/^@[\p{L}\p{N}]/u.test(rest)) {
      const space = rest.search(/\s/);
      rest = space < 0 ? "" : rest.slice(space).trimStart();
    }
    if (rest) line = rest;
  }
  return neutralize(cut(line, max));
}

/** `2026-11-08T10:47:00Z` in unix seconds; null when it is not a time. */
export function unix(at: string | undefined): number | null {
  const ms = at ? Date.parse(at) : NaN;
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

/** How long ago, short: `now`, `5m`, `2h`, `3d`. */
export function age(secs: number): string {
  const s = Math.max(0, secs);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/** When the local day of `now` (unix seconds) began. */
export function startOfDay(now: number): number {
  const d = new Date(now * 1000);
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

const pad = (n: number) => String(n).padStart(2, "0");

/** `11:20` in local time when it is the day of `now`, else `11:20 on 2026-11-14`. */
export function clock(at: number, now: number): string {
  const d = new Date(at * 1000);
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (startOfDay(at) === startOfDay(now)) return hm;
  return `${hm} on ${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** The host (and port) of a server URL: `relevo.office:8443`. */
export function host(url: string): string {
  const rest = url.includes("://") ? url.slice(url.indexOf("://") + 3) : url;
  return rest.split(/[/?#]/)[0];
}

function kindOf(t: TimelineThread): string {
  if (t.kind === "team") return t.teams?.length ? `team ${t.teams.map((x) => `@${scrub(x)}`).join(" ")}` : "team";
  if (t.kind === "global") return "board";
  return t.kind;
}

/**
 * The rows of the list: `threads` are the timeline's, oldest first; at most MAX_SHOWN of them.
 * `now` in unix seconds.
 */
export function rows(you: string, names: Record<string, string>, threads: TimelineThread[], now: number, bots: string[] = []): Row[] {
  const isBot = (handle: string) => handle !== you && bots.includes(handle);
  return threads.slice(0, MAX_SHOWN).map((t, i) => {
    const first = t.first_note ?? null;
    const last = t.last_note ?? null;
    const opener = first?.author ?? t.from;
    const latest = last ?? first;
    const at = unix(latest?.created_at) ?? now;
    const text = first?.text ?? t.title ?? "";
    const replies = (t.notes ?? 1) - 1;
    return {
      n: i + 1,
      id: t.id,
      who: nameOf(names, you, opener),
      opener,
      own: opener === you,
      bot: isBot(opener),
      kind: kindOf(t),
      age: age(now - at),
      replies: replies > 1 ? replies : 0,
      mentioned: t.kind === "global",
      first: firstLine(text, true, LINE_CHARS),
      reply: last
        ? {
            by: last.author === opener ? null : nameOf(names, you, last.author),
            text: firstLine(last.text, false, LINE_CHARS),
            author: last.author,
            own: last.author === you,
            bot: last.author !== opener && isBot(last.author),
          }
        : null,
      subject: firstLine(text, true, SUBJECT_CHARS),
      lastNote: latest?.id ?? "",
    };
  });
}

/** `direct · 2h · 4 replies · mentioned you`. */
function meta(r: Row): string {
  return [r.kind, r.age, r.replies ? `${r.replies} replies` : "", r.mentioned ? "mentioned you" : ""].filter(Boolean).join(" · ");
}

/** The help line under a list, with the numbers it has. */
export function help(len: number): string {
  return `/relevo 1 <your answer> · /relevo ${Math.min(2, len)} to open it · /relevo seen ${Math.min(3, len)}`;
}

export const NOT_CONNECTED = "Relevo isn't connected on this machine. Run relevo connect <server> with the token your admin gave you.";

/**
 * What /relevo shows, as plain text: what the model reads (the person sees it drawn by `draw`).
 * With `fence`, every name, first line and reply of a teammate goes inside `<untrusted
 * from="@handle">` (rule 5), as in the inbox: the person's own stay as they are.
 */
export function plain(shown: Shown, fence = false): string {
  switch (shown.state) {
    case "empty":
      return `✓ Nothing is waiting on you.${shown.handled ? ` Your agents handled ${shown.handled} today.` : ""}`;
    case "offline":
      return `Relevo can't reach ${shown.host} since ${shown.since}. Notes you send now will fail; nothing is lost on the server.`;
    case "not-connected":
      return NOT_CONNECTED;
    case "error":
      return shown.message;
    case "list": {
      const out = [`Waiting on you · ${shown.total}${shown.handled ? ` · your agents handled ${shown.handled} today` : ""}`];
      // `text` by `handle`, fenced when asked and not the person's own.
      const by = (handle: string | undefined, own: boolean | undefined, text: string) =>
        fence && !own ? untrusted(handle || "unknown", text, true) : text;
      for (const r of shown.rows) {
        out.push(`${String(r.n).padStart(2)}  ${by(r.opener, r.own, r.who)}${r.bot ? " (bot)" : ""} · ${meta(r)}`, `    ${by(r.opener, r.own, r.first)}`);
        if (r.reply) {
          const name = r.reply.by ? `${by(r.reply.author, r.reply.own, r.reply.by)}${r.reply.bot ? " (bot)" : ""}: ` : "";
          out.push(`    └ ${name}${by(r.reply.author ?? r.opener, r.reply.own ?? r.own, r.reply.text)}`);
        }
      }
      if (shown.total > MAX_SHOWN) out.push(`+${shown.total - MAX_SHOWN} more · relevo web`);
      out.push(help(shown.rows.length));
      return out.join("\n");
    }
  }
}

/** pi's theme, as the renderer and the footer use it. */
export interface Theme {
  fg(color: string, text: string): string;
  bold(text: string): string;
}

/** `@names` in the accent colour, the rest in `base` (none: as it is). */
function mentions(theme: Theme, text: string, base?: string): string {
  const paint = (s: string) => (base && s ? theme.fg(base, s) : s);
  let out = "";
  let last = 0;
  for (const m of text.matchAll(/@[\p{L}\p{N}][\p{L}\p{N}_./@-]*/gu)) {
    const at = m.index ?? 0;
    // `@` inside a word (an email) is not a mention.
    if (at > 0 && /[\p{L}\p{N}]/u.test(text[at - 1])) continue;
    const name = m[0].replace(/[.\-]+$/, "");
    out += paint(text.slice(last, at)) + theme.fg("accent", name);
    last = at + name.length;
  }
  return out + paint(text.slice(last));
}

/**
 * What /relevo shows, drawn with pi's theme: the title bold with the count in `warning` (then, when
 * any, how many the person's agents handled today in `muted`, after a dim ` · `), numbers
 * `warning` and bold, names bold, the rest of the head `muted` with ` · ` dim, `@names` in
 * `accent`, `└ ` dim and the latest note `muted`, the help line dim.
 */
export function draw(shown: Shown, theme: Theme): string {
  const dot = theme.fg("dim", " · ");
  switch (shown.state) {
    case "empty":
      return (
        theme.fg("success", "✓") +
        " Nothing is waiting on you." +
        (shown.handled ? theme.fg("muted", ` Your agents handled ${shown.handled} today.`) : "")
      );
    case "offline":
      return (
        theme.fg("error", `Relevo can't reach ${shown.host} since ${shown.since}.`) +
        theme.fg("muted", " Notes you send now will fail; nothing is lost on the server.")
      );
    case "not-connected":
      return (
        theme.bold("Relevo isn't connected on this machine.") +
        " Run " +
        theme.fg("accent", "relevo connect <server>") +
        " with the token your admin gave you."
      );
    case "error":
      return theme.fg("error", shown.message);
    case "list": {
      let out = theme.bold("Waiting on you") + dot + theme.fg("warning", theme.bold(String(shown.total)));
      if (shown.handled) out += dot + theme.fg("muted", `your agents handled ${shown.handled} today`);
      out += "\n";
      for (const r of shown.rows) {
        const head = meta(r)
          .split(" · ")
          .map((part) => theme.fg("muted", part))
          .join(dot);
        out += `\n${r.n < 10 ? " " : ""}${theme.fg("warning", theme.bold(String(r.n)))}  ${theme.bold(r.who)}${r.bot ? ` ${theme.fg("muted", "(bot)")}` : ""}${dot}${head}\n`;
        out += `    ${mentions(theme, r.first)}\n`;
        if (r.reply) {
          const text = `${r.reply.by ? `${r.reply.by}${r.reply.bot ? " (bot)" : ""}: ` : ""}${r.reply.text}`;
          out += `    ${theme.fg("dim", "└ ")}${mentions(theme, text, "muted")}\n`;
        }
      }
      if (shown.total > MAX_SHOWN) out += `\n${theme.fg("muted", `+${shown.total - MAX_SHOWN} more`)}${dot}${theme.fg("muted", "relevo web")}\n`;
      return `${out}\n${theme.fg("dim", help(shown.rows.length))}`;
    }
  }
}

/** The person's entry in a thread's `delivery` (`GET /api/threads/:id`). */
function deliveryOf(thread: any, you: string): any {
  return Array.isArray(thread?.delivery) ? thread.delivery.find((d: any) => d?.handle === you) : undefined;
}

/**
 * Why an action on list number `n` must not go ahead, from the thread as it is now (`GET
 * /api/threads/:id`): it no longer waits on the person (another of their sessions handled it),
 * or a note came after the one the list showed. Null when it may.
 */
export function refusal(n: number, row: Row, thread: any, listed: Listed, now: number): string | null {
  const who = row.who;
  if (thread?.pending !== true) {
    const d = deliveryOf(thread, listed.you);
    const at = unix(d?.handled_at ?? undefined);
    let out = `Thread ${n} (${who}) `;
    if (at !== null) {
      out += "was handled";
      const by = d?.handled_by;
      if (by && typeof by === "object") {
        const label = typeof by.label === "string" ? scrub(by.label).trim() : "";
        out += ` by your ${label || "agent"}`;
        const context = typeof by.context === "string" ? scrub(by.context).trim() : "";
        if (context) out += ` in ${context}`;
      }
      out += ` at ${clock(at, now)}`;
    } else {
      out += "is no longer waiting on you";
    }
    return `${out} · nothing sent`;
  }
  const notes = Array.isArray(thread?.notes) ? thread.notes : [];
  const latest = notes[notes.length - 1];
  if (!latest || latest.id === row.lastNote) return null;
  return (
    `Thread ${n} (${who}) has a new note since your list, by ${nameOf(listed.names, listed.you, latest.author)}: ` +
    `"${firstLine(latest.text ?? "", false, LINE_CHARS)}" · nothing sent. Run /relevo again, or /relevo ${n} to open it.`
  );
}

/** Whom a reply in `thread` (`GET /api/threads/:id`) goes to: `@ana`, `@ana and @bob`, `@backend`, or the board. */
export function recipients(thread: any, you: string): string {
  const at = (v: unknown) => (Array.isArray(v) ? v.filter((h) => typeof h === "string" && h !== you).map((h) => `@${h}`) : []);
  if (thread?.kind === "global") return "the board";
  const names = thread?.kind === "team" && thread.teams?.length ? at(thread.teams) : at(thread?.participants);
  if (!names.length) return "the thread";
  if (names.length === 1) return names[0];
  if (names.length <= 3) return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  return `${names[0]}, ${names[1]}, ${names[2]} and ${names.length - 3} more`;
}

/** The list numbers in `thread` (`"1"`, `"1 3"`, `"1, 3"`), when that is all it holds. */
export function numbers(thread: string): number[] | null {
  const parts = thread.split(/[ ,]/).filter(Boolean);
  if (!parts.length || !parts.every((p) => /^\d+$/.test(p))) return null;
  return parts.map(Number);
}

/** `thread 1 (Ana · onboarding copy)`. */
export function named(row: Row): string {
  return `thread ${row.n} (${row.who} · ${row.subject})`;
}

/** What is left waiting after an action: `2 left`, or nothing. */
export function left(waiting: number | null): string {
  if (waiting === null) return "";
  return waiting === 0 ? "Nothing else is waiting on you." : `${waiting} left`;
}

/**
 * A list number as a tool takes it: `1`, or, as /relevo hands a request to the agent, bound to
 * the thread and the latest note that number showed when the person typed it (`1@r_abc:n_def`).
 */
export interface ListNumber {
  n: number;
  id?: string;
  note?: string;
}

/** The list numbers in a tool's `thread` or `id` (`"1"`, `"1 3"`, `"1@r_abc:n_def 3@r_ghi:n_jkl"`), when that is all it holds. */
export function listNumbers(thread: string): ListNumber[] | null {
  const parts = thread.split(/[ ,]/).filter(Boolean);
  const out: ListNumber[] = [];
  for (const p of parts) {
    const m = /^(\d+)(?:@(r_[A-Za-z0-9_-]+):([A-Za-z0-9_-]*))?$/.exec(p);
    if (!m) return null;
    out.push(m[2] === undefined ? { n: Number(m[1]) } : { n: Number(m[1]), id: m[2], note: m[3] });
  }
  return out.length ? out : null;
}

/** Row `row`'s number bound to its thread and latest note, as the request /relevo hands over carries it. */
export function bind(row: Row): string {
  return `${row.n}@${row.id}:${row.lastNote}`;
}

/**
 * The threads of the last list that `ns` name, or why not (nothing sent). A bound number must
 * still be that thread with that latest note: a /relevo run between the person typing and the
 * tool running may have numbered the threads anew.
 */
export function resolve(listed: Listed | null, ns: Array<number | ListNumber>): Row[] | string {
  if (!listed) return "There is no list in this session yet: run /relevo first, then use its numbers. Nothing was sent.";
  const out: Row[] = [];
  for (const x of ns) {
    const { n, id, note } = typeof x === "number" ? { n: x, id: undefined, note: undefined } : x;
    const row = listed.rows[n - 1];
    if (id !== undefined && row?.id !== id) {
      return `Thread ${n} of your list is no longer the one you typed /relevo ${n} for: the list changed since. Run /relevo again. Nothing was sent.`;
    }
    if (!row) {
      return listed.rows.length
        ? `There is no thread ${n} in your last list (1 to ${listed.rows.length}): run /relevo to see it again. Nothing was sent.`
        : "Your last list had nothing waiting on you: run /relevo again. Nothing was sent.";
    }
    // A bound number is judged against the note the person saw when they typed it: a newer one
    // since refuses as any newer note does.
    if (!out.some((r) => r.id === row.id)) out.push(id !== undefined && note !== undefined ? { ...row, lastNote: note } : row);
  }
  return out;
}

/** How many terminal columns a character takes: 2 for wide ones (CJK, emoji), 0 for joiners and marks. */
function columns(ch: string): number {
  const c = ch.codePointAt(0) ?? 0;
  if ((c >= 0x300 && c <= 0x36f) || (c >= 0x200b && c <= 0x200f) || c === 0xfe0f) return 0;
  if (
    (c >= 0x1100 && c <= 0x115f) ||
    (c >= 0x2e80 && c <= 0xa4cf) ||
    (c >= 0xac00 && c <= 0xd7a3) ||
    (c >= 0xf900 && c <= 0xfaff) ||
    (c >= 0xfe30 && c <= 0xfe4f) ||
    (c >= 0xff00 && c <= 0xff60) ||
    (c >= 0xffe0 && c <= 0xffe6) ||
    (c >= 0x1f300 && c <= 0x1faff) ||
    c >= 0x20000
  )
    return 2;
  return 1;
}

const RESET = "\x1b[0m";

/**
 * `line` (with colour codes) in lines of at most `room` columns, broken at spaces when it can,
 * each next line indented as the first and starting with the colours in force.
 */
export function wrap(line: string, room: number): string[] {
  const tokens = line.match(/\x1b\[[0-9;]*m|[\s\S]/gu) ?? [];
  const indent = " ".repeat(Math.min(line.length - line.trimStart().length, Math.floor(room / 2)));
  const out: string[] = [];
  let cur: string[] = [];
  let width = 0;
  const codes: string[] = [];
  let space = -1;
  let spaceCodes: string[] = [];
  const width0 = (ts: string[]) => ts.reduce((w, t) => w + (t.startsWith("\x1b") ? 0 : columns(t)), 0);
  for (const tok of tokens) {
    if (tok.startsWith("\x1b")) {
      cur.push(tok);
      codes.push(tok);
      continue;
    }
    const w = columns(tok);
    if (width + w > room) {
      if (tok === " ") {
        out.push(cur.join("") + RESET);
        cur = [indent, ...codes];
        width = indent.length;
        space = -1;
        continue;
      }
      if (space > 0) {
        const tail = cur.slice(space + 1);
        out.push(cur.slice(0, space).join("") + RESET);
        cur = [indent, ...spaceCodes, ...tail];
        width = indent.length + width0(tail);
      } else {
        out.push(cur.join("") + RESET);
        cur = [indent, ...codes];
        width = indent.length;
      }
      space = -1;
    }
    cur.push(tok);
    width += w;
    if (tok === " " && width > indent.length + 1) {
      space = cur.length - 1;
      spaceCodes = [...codes];
    }
  }
  out.push(cur.join(""));
  return out;
}

/** A block of text for pi's TUI (its `Component`): wrapped to the width, `padX` columns each side. */
export class Block {
  text: string;
  padX: number;
  constructor(text: string, padX = 1) {
    this.text = text;
    this.padX = padX;
  }
  render(width: number): string[] {
    const room = Math.max(1, width - this.padX * 2);
    const left = " ".repeat(this.padX);
    return this.text.split("\n").flatMap((line) => wrap(line, room).map((l) => (l ? left + l : "")));
  }
  invalidate(): void {}
}
