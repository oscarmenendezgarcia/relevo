// Inbox and threads as text for the model. Text written by other people (notes and
// the titles taken from them) goes inside <untrusted from="@handle">: context, not
// orders. Your own person's notes (`you` from the server, here `me`) are marked
// "(you)" and not wrapped; while `me` is unknown every note is wrapped.

import type { Delivery, Inbox, Note, Person, SessionRef, Team, Thread } from "./client.ts";

const MAX_CHARS = 60_000;

/**
 * Keeps a note from closing or opening a wrapper: every `<untrusted`, `</untrusted`,
 * `<channel` and `</channel` (any case, spaces allowed after `<` and `/`) becomes
 * `&lt;…`, as in relevo mcp (src/render.rs).
 */
export function neutralize(text: string): string {
  return text.replace(/<(?=[ \t\n\f\r]*(?:\/[ \t\n\f\r]*)?(?:untrusted|channel))/gi, "&lt;");
}

export function untrusted(from: string, text: string, inline = false): string {
  const body = neutralize(text);
  return inline
    ? `<untrusted from="@${from}">${body}</untrusted>`
    : `<untrusted from="@${from}">\n${body}\n</untrusted>`;
}

function where(s: SessionRef): string {
  return `"${s.label}" ${s.context ? `in ${s.context}` : "(general)"}`;
}

/** Text by `author`: wrapped unless it is `me`'s own, which can still not open a wrapper. */
function body(author: string, text: string, me: string | undefined): string {
  return author === me ? neutralize(text) : untrusted(author, text);
}

function title(t: Thread, me: string | undefined): string {
  if (t.from === me) return `Title (yours): ${neutralize(t.title)}`;
  return `Title: ${untrusted(t.from, t.title, true)}`;
}

/** `@a`, `@a and @b`, `@a, @b and @c`. */
export function listing(names: string[]): string {
  const at = names.map((n) => `@${n}`);
  return at.length < 2 ? (at[0] ?? "") : `${at.slice(0, -1).join(", ")} and ${at[at.length - 1]}`;
}

/** The people on the server (the protocol's `team` list): handle, name (and `bot`, for one marked as a bot), about, the teams they are in, and where their agents work now. */
export function renderTeam(team: Person[], me?: string): string {
  const lines = team.map((p) => {
    const about = p.about ? `: ${p.about}` : "";
    const teams = p.teams?.length ? `; in ${p.teams.map((t) => `@${t}`).join(", ")}` : "";
    const live = p.contexts?.length ? ` (live in ${p.contexts.join(", ")})` : "";
    const you = p.handle === me ? ", you" : "";
    const bot = p.bot ? ", bot" : "";
    return `- @${p.handle} (${p.name}${bot}${you})${about}${teams}${live}`;
  });
  return ["People (mention one with @handle):", ...lines].join("\n");
}

/** The teams on the server: name, title and members. Null from a server that does not list them (before 0.4.0). */
export function renderTeams(teams: Team[] | undefined, me?: string): string | null {
  if (!Array.isArray(teams)) return null;
  if (!teams.length) return "Teams: none on this server.";
  const lines = teams.map((t) => {
    const title = t.title ? ` (${t.title})` : "";
    const members = t.members.map((m) => (m === me ? `@${m} (you)` : `@${m}`)).join(", ");
    return `- @${t.name}${title}: ${members || "nobody active"}`;
  });
  return ["Teams (mention one with @name: it reaches its members):", ...lines].join("\n");
}

/** Whether the thread names the person: always a direct one (and from a server that does not say). */
export function namesYou(t: Thread): boolean {
  return typeof t.named === "boolean" ? t.named : t.kind === "direct";
}

/** A team or global-board thread that does not name the person: answering it is optional. */
export function optional(t: Thread): boolean {
  return t.kind !== "direct" && !namesYou(t);
}

/**
 * Whether the notes by others shown with the thread (`notes`, or else `new_notes`; `me`'s own do not
 * count) are release notes only (`system: true`): informative, so nothing in them asks for an answer
 * or starts a turn.
 */
export function releaseOnly(t: Thread, me?: string): boolean {
  const others = (t.notes ?? t.new_notes ?? []).filter((n) => n.author !== me);
  return others.length > 0 && others.every((n) => n.system === true);
}

/** What kind of thread it is: `direct`, `team: @backend` or `global board` (a team question to everyone from a server before 0.4.0). */
export function kindLabel(t: Thread): string {
  if (t.kind === "global") return "global board";
  if (t.kind === "team") return t.teams?.length ? `team: ${t.teams.map((n) => `@${n}`).join(", ")}` : "team question to everyone";
  return "direct";
}

/** `since` (RFC 3339, UTC) as `10:47 UTC on 2026-11-08`, or as it is if it is not that. */
function since(at: string): string {
  const m = /^(\d{4}-\d\d-\d\d)T(\d\d:\d\d)/.exec(at);
  return m ? `${m[2]} UTC on ${m[1]}` : at;
}

/** A claim as a state, as relevo mcp shows it: `@ana's Claude Code in pagos-api has it since 10:47 UTC on 2026-11-08`. */
function claimState(handle: string, c: NonNullable<Delivery["claimed_by"]>): string {
  const at = c.since ? ` since ${since(c.since)}` : "";
  return `@${handle}'s ${c.label} ${c.context ? `in ${c.context}` : "(general)"} has it${at}`;
}

/** Whether the thread is pending for the person (a server before 0.3.0 does not say: nothing is). */
export function isPending(t: Thread): boolean {
  return t.pending === true;
}

/** Who of the person has the thread: who and since when only `relevo_thread`'s delivery says. */
function holder(t: Thread, me: string | undefined): string {
  const mine = me === undefined ? undefined : t.delivery?.find((d) => d.handle === me && d.claimed_by);
  if (t.claimed === "this-session") {
    return mine?.claimed_by?.since ? `This session has it since ${since(mine.claimed_by.since)}.` : "This session has it.";
  }
  if (t.claimed === "other-session") {
    return mine?.claimed_by
      ? `${claimState(mine.handle, mine.claimed_by)}: another of your person's sessions, so leave it to that session.`
      : "Another of your person's sessions has it: leave it to that session.";
  }
  // Threads that do not name the person are not claimed.
  if (isPending(t)) {
    return `None of your person's sessions has it. If it needs work first, claim it with relevo_send { thread: "${t.id}", claim: true } and no text just before you start.`;
  }
  return "None of your person's sessions has it.";
}

function cut(n: Note): string {
  return n.truncated ? `\n[Cut at 4,000 characters: read it whole with relevo_thread.]` : "";
}

/** The line after a note by the server's own person (`relevo`), who posts each new version's release notes. */
export const RELEASE_NOTES = "These are the release notes of the Relevo server's new version: they need no answer.";

function renderNote(n: Note, me: string | undefined): string {
  const you = n.author === me ? " (you)" : "";
  const head = `--- note ${n.id} by @${n.author}${you} via "${n.via}", ${n.created_at}`;
  // By the server's flag, never by the author's handle: a person may have had `relevo`.
  const release = n.system === true ? `\n${RELEASE_NOTES}` : "";
  return `${head}\n${body(n.author, n.text, me)}${cut(n)}${release}`;
}

/** A pending thread's latest note (`last_note`), which may have reached another session of the person. */
function renderLatest(n: Note, me: string | undefined): string {
  const you = n.author === me ? " (you)" : "";
  const head = `Latest: @${n.author}${you} at ${n.created_at} (note ${n.id} via "${n.via}"; it may have reached another of your sessions):`;
  return `${head}\n${body(n.author, n.text, me)}${cut(n)}`;
}

/**
 * One participant's delivery, as relevo mcp says it: the latest note by someone else they have,
 * which session saw it first and when, whether they handled it (when, from where), how many of
 * their live sessions have it, and who of theirs holds the thread. `seen_by` counts live sessions
 * only, so with none live it says the session that saw it has ended, never "seen by 0".
 * Only a person the thread names has something to handle: for the others nothing is "not handled yet".
 */
function renderDelivery(d: Delivery, named: boolean): string {
  if (!d.note) return `- @${d.handle}: nothing by others has reached them yet.`;
  const parts: string[] = [];
  const handled = typeof d.handled_at === "string";
  if (d.handled_at === undefined) {
    // A server that does not say whether it was handled.
    parts.push(`has up to ${d.note} since ${d.delivered_at}`);
    if (d.by) parts.push(`reached ${where(d.by)}`);
    parts.push(`seen by ${d.seen_by} of their sessions`);
  } else {
    const at = d.delivered_at ? ` at ${since(d.delivered_at)}` : "";
    let seen = `note ${d.note} seen${d.by ? ` by ${where(d.by)}` : ""}${at}`;
    if (!handled && named) seen += ", not handled yet";
    parts.push(seen);
    if (handled) {
      parts.push(`handled${d.handled_by ? ` by ${where(d.handled_by)}` : ""} at ${since(d.handled_at as string)}`);
    } else if (d.seen_by === 0) {
      parts.push("no live session of theirs has it now");
    } else if (d.seen_by > 1) {
      parts.push(`${d.seen_by} of their live sessions have it`);
    }
  }
  if (d.claimed_by) parts.push(claimState(d.handle, d.claimed_by));
  else if (d.handled_at === undefined || (d.seen_by > 0 && !handled)) parts.push("nobody of theirs has it");
  return `- @${d.handle}: ${parts.join("; ")}.`;
}

export interface ThreadOptions {
  /** Characters to aim for; the newest note in range is shown whole even beyond it. */
  budget?: number;
  /** Only notes before this one (paging through a long thread with relevo_thread). */
  before?: string;
  /** The handle of this session's person: their notes are not wrapped. */
  me?: string;
}

/**
 * A thread as text. Never cut inside a note: when its notes do not fit in the
 * budget, the newest that fit are shown whole (oldest first) and a line says how
 * to read the earlier ones. The delivery section is always shown.
 */
export function renderThread(t: Thread, options: ThreadOptions = {}): string {
  const budget = options.budget ?? MAX_CHARS;
  const me = options.me;
  // A global thread holds everyone on the server: no list of them (as relevo mcp and Hermes), which
  // would cost a line of handles in every render of every board note.
  const opened =
    t.kind === "global"
      ? `Posted by @${t.from} on the global board, to everyone on the server`
      : `Opened by @${t.from}; participants: ${t.participants.map((p) => `@${p}`).join(", ")}`;
  const named = t.kind === "direct" ? "" : namesYou(t) ? " It names your person." : " It does not name your person.";
  const lines = [`## Thread ${t.id} (${kindLabel(t)})`, title(t, me), `${opened}.${named}`];
  if (t.context) lines.push(`Addressed to your person in context ${t.context}.`);
  // Set by the background loop: its latest note was never shown in this process. The answer that
  // lost it may have carried earlier notes too, and only the latest one comes back.
  if (t.handed_over) {
    lines.push(
      "Handed over again: it is still pending for your person, and its latest note was never shown in this session " +
        "(an answer that carried it may have been lost on the way, or it was fetched and not shown). Treat that note as new. " +
        `Earlier notes of this thread may not have reached you either: read it whole with relevo_thread { id: "${t.id}" } before acting.`,
    );
  } else if (t.after_failure) {
    // Set by the background loop: the first answer after a failed request, whose lost answer may
    // have carried notes of this thread that came before these.
    lines.push(
      "A request for news failed just before this answer, and its answer may have carried earlier notes of this thread " +
        `that never reached you: read it whole with relevo_thread { id: "${t.id}" } before acting.`,
    );
  }
  if (isPending(t)) {
    lines.push(
      "Pending for your person: they have not handled it yet. If it is for you and needs nothing, " +
        `mark it seen with relevo_send { thread: "${t.id}", seen: true } and no text.`,
    );
  }
  lines.push(holder(t, me));
  // Who else in the thread holds it (`held_by`, from the inbox): `relevo_thread`'s delivery says it
  // for every participant already.
  if (!t.delivery) for (const h of t.held_by ?? []) lines.push(`${claimState(h.handle, h)}.`);
  // Release notes need no answer, so answering them is not offered.
  if (optional(t) && t.from !== me && !releaseOnly(t, me)) {
    lines.push(
      "It does not name your person, so answering is optional: answer only from your own work or your person's files " +
        "(repeating someone's `about` line, or guessing who might know, is not an answer); do not claim it.",
    );
  }
  const head = lines.join("\n");
  const delivery = t.delivery
    ? ["Delivery (the latest note by someone else that each person has):", ...t.delivery.map((d) => renderDelivery(d, d.named ?? t.kind === "direct"))].join("\n")
    : null;

  const whole = t.notes !== undefined && t.notes !== null;
  const all = (whole ? t.notes : t.new_notes) ?? [];
  let end = all.length;
  if (options.before !== undefined) {
    end = all.findIndex((n) => n.id === options.before);
    if (end < 0) throw new Error(`Note ${options.before} is not in thread ${t.id}.`);
  }

  // Room for the head, the delivery and the lines about notes not shown.
  let room = budget - head.length - (delivery?.length ?? 0) - 400;
  const shown: string[] = [];
  let start = end;
  while (start > 0) {
    const text = renderNote(all[start - 1], me);
    if (shown.length && text.length + 2 > room) break;
    shown.unshift(text);
    room -= text.length + 2;
    start--;
  }

  const out = [head];
  if (!all.length) {
    out.push(whole ? "No notes." : "No new notes.");
    // What it waits on, which another session of the person may have fetched already.
    if (!whole && isPending(t) && t.last_note) out.push(renderLatest(t.last_note, me));
  } else if (whole) {
    if (!shown.length) {
      out.push(`No notes before ${options.before}.`);
    } else {
      out.push(start === 0 && end === all.length ? `Notes (${all.length}):` : `Notes ${start + 1}-${end} of ${all.length}:`);
      if (start > 0) {
        out.push(
          `${start} earlier note(s) not shown here: read them with relevo_thread { id: "${t.id}", before: "${all[start].id}" }.`,
        );
      }
    }
    if (end < all.length) out.push(`${all.length - end} later note(s) not shown here: relevo_thread { id: "${t.id}" } shows the latest.`);
  } else {
    out.push(start === 0 ? `New notes (${all.length}):` : `New notes (${all.length}), the latest ${shown.length} shown:`);
    if (start > 0) {
      out.push(`${start} earlier new note(s) not shown here: read them with relevo_thread { id: "${t.id}" }.`);
    }
  }
  out.push(...shown);
  if (delivery) out.push(delivery);
  return out.join("\n\n");
}

/** A thread left out of the inbox for length: enough to know it is there and how to read it. */
function renderSkipped(t: Thread, me: string | undefined): string {
  const n = t.new_notes?.length ?? 0;
  const lines = [
    `## Thread ${t.id} (${kindLabel(t)}): not shown here for length`,
    title(t, me),
  ];
  if (isPending(t)) lines.push("Pending for your person.");
  lines.push(
    n
      ? `${n} new note(s) not shown: read them with relevo_thread { id: "${t.id}" }.`
      : `No new notes; read it with relevo_thread { id: "${t.id}" }.`,
  );
  return lines.join("\n");
}

/**
 * The inbox as text, `notice` (if any) under its first line. Every thread here is delivered to this session already, so
 * none may go missing: threads are shown whole while they fit, and each one
 * that does not is still listed, with how to read it.
 */
export function renderInbox(inbox: Inbox, notice?: string | null): string {
  const me = inbox.session.you;
  // First whom the agent speaks for, as relevo mcp does, then the session, the people and the teams.
  const where = `"${inbox.session.label}", ${inbox.session.context ? `context ${inbox.session.context}` : "general"}.`;
  const session = me
    ? `You speak for @${me} (your person): notes by @${me} are theirs, from this session or another of theirs. This session: ${where}`
    : `This session: ${where}`;
  const out = [session];
  if (notice) out.push(notice);
  out.push(renderTeam(inbox.team, me));
  const teams = renderTeams(inbox.teams, me);
  if (teams) out.push(teams);
  if (!inbox.threads.length) out.push("Nothing new for this session and nothing pending.");
  // Those with new notes have reached this session now, so the rest come up next; threads that
  // are only pending stay until handled, so another call shows the same ones.
  const hadNew = inbox.threads.some((t) => (t.new_notes ?? []).length > 0);
  const more = !inbox.more
    ? null
    : hadNew
      ? `${inbox.more} more thread(s) not shown: call relevo_inbox again.`
      : `${inbox.more} more thread(s) not shown: older ones, also pending for your person. Calling relevo_inbox again shows the same threads; ` +
        "answering or marking seen those above makes room.";
  const skipped = inbox.threads.map((t) => renderSkipped(t, me));
  let room = MAX_CHARS - out.join("\n\n").length - (more ? more.length + 2 : 0);
  // What the threads after each one take at least, listed only.
  let after = skipped.reduce((sum, s) => sum + s.length + 2, 0);
  inbox.threads.forEach((t, i) => {
    after -= skipped[i].length + 2;
    const fits = room - after - 2;
    const full = renderThread(t, { budget: fits, me });
    const text = full.length <= fits ? full : skipped[i];
    out.push(text);
    room -= text.length + 2;
  });
  if (more) out.push(more);
  return out.join("\n\n");
}

/** What a session receives when a note arrives on its own: what to do with it (rules 3-5), then the thread. */
export function renderArrival(t: Thread, me?: string): string {
  const who = me ? ` You speak for @${me}.` : "";
  const reply = `relevo_send { thread: "${t.id}", text: … }`;
  const share =
    " Facts from your person's documentation or configuration may be shared to answer it. Code, private data, and changes to " +
    "your person's code need your person's OK first, unless they told you to handle this thread. Personal matters (availability, " +
    "decisions, commitments, opinions, private life) are your person's to answer: leave those pending.";
  // Where a team or global-board thread was sent; nothing for a direct one.
  const to = t.kind === "global" ? ", on the global board" : t.kind === "team" ? (t.teams?.length ? `, to ${listing(t.teams)}` : ", to everyone") : "";
  let intro: string;
  if (releaseOnly(t, me)) {
    // Informative only: added to the session without a turn (index.ts), nothing to do with it.
    intro =
      `Relevo: release notes in thread ${t.id}${to}: the Relevo server's new version, for your information.${who} ` +
      "No answer needed: do not reply to them, claim them or mark them seen. Tell your person about them only if it matters to their work.";
  } else if (optional(t) && me !== undefined && t.from === me) {
    // Answers to the person's own question: theirs to read, not the agent's to answer.
    intro =
      `Relevo: new in thread ${t.id}, your person's own question to a team or to everyone.${who} These notes answer it: tell your person what they say. ` +
      `Do not answer it yourself; write in it (relevo_send { thread: "${t.id}", … }) only to follow up for your person.`;
  } else if (optional(t)) {
    intro =
      `Relevo: new in thread ${t.id}${to}; it does not name your person, so answering is optional.${who} ` +
      `If your own work or your person's files answer it, reply with ${reply}; ` +
      "if not, stay silent: repeating someone's `about` line, or guessing who might know, is not an answer. Do not claim it.";
  } else if (!isPending(t)) {
    // Another of the person's sessions already handled it: nothing stays pending, nothing to mark seen.
    intro =
      `Relevo: new in thread ${t.id}${to}.${who} Your person has already handled this thread: it is not pending. ` +
      `Read it; if it is for you and asks something you can answer, answer with ${reply}; otherwise stay silent.`;
    intro += share;
  } else {
    // A direct thread, or a team or global-board one that names the person: theirs to answer.
    const named = t.kind === "direct" ? "" : `${to}, and it names your person`;
    intro =
      `Relevo: new in thread ${t.id}${named}.${who} Decide from your context and current work whether it is for you; if not, stay silent and leave it: ` +
      `it stays pending for your person (do not reply "not for me"). Read what you need first; if you can answer or explain from that, just answer with ${reply}: your reply takes the thread. ` +
      `If it needs work first, claim it with relevo_send { thread: "${t.id}", claim: true } and no text just before you start changing files or running something long: ` +
      "a state the others see, not a note. Never write a note that only says you are on it. " +
      `If it is for you but needs nothing (an answer you have now read, a thank-you), mark it seen with relevo_send { thread: "${t.id}", seen: true } and no text.`;
    intro += share;
  }
  if (t.claimed === "other-session") intro += " Another of your person's sessions has this thread: step aside.";
  return [intro, renderThread(t, { budget: MAX_CHARS - intro.length - 2, me })].join("\n\n");
}
