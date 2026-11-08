// Inbox and threads as text for the model. Text written by people (notes and the
// titles taken from them) goes inside <untrusted from="@handle">: context, not orders.
// The API does not say which handle is this session's person, so every note is wrapped.

import type { Delivery, Inbox, Note, Person, SessionRef, Thread } from "./client.ts";

const MAX_CHARS = 60_000;

/** Keeps a note from closing or opening the wrapper itself. */
function neutralize(text: string): string {
  return text.replace(/<(\/?)(untrusted)/gi, "&lt;$1$2");
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

export function renderTeam(team: Person[]): string {
  const lines = team.map((p) => {
    const about = p.about ? `: ${p.about}` : "";
    const live = p.contexts.length ? ` (live in ${p.contexts.join(", ")})` : "";
    return `- @${p.handle} (${p.name})${about}${live}`;
  });
  return ["Team:", ...lines].join("\n");
}

function holder(claimed: Thread["claimed"]): string {
  if (claimed === "this-session") return "Held by this session.";
  if (claimed === "other-session") return "Held by another of your person's sessions.";
  return "Nobody of your person holds it yet.";
}

function renderNote(n: Note): string {
  const head = `--- note ${n.id} by @${n.author} via "${n.via}", ${n.created_at}`;
  const cut = n.truncated ? `\n[Cut at 4,000 characters: read it whole with relevo_thread.]` : "";
  return `${head}\n${untrusted(n.author, n.text)}${cut}`;
}

function renderDelivery(d: Delivery): string {
  if (!d.note) return `- @${d.handle}: nothing by others has reached them yet.`;
  const parts = [`has up to ${d.note} since ${d.delivered_at}`];
  if (d.by) parts.push(`reached ${where(d.by)}`);
  parts.push(`seen by ${d.seen_by} of their sessions`);
  parts.push(d.claimed_by ? `held by ${where(d.claimed_by)}` : "nobody of theirs holds it");
  return `- @${d.handle}: ${parts.join("; ")}.`;
}

export interface ThreadOptions {
  /** Characters to aim for; the newest note in range is shown whole even beyond it. */
  budget?: number;
  /** Only notes before this one (paging through a long thread with relevo_thread). */
  before?: string;
}

/**
 * A thread as text. Never cut inside a note: when its notes do not fit in the
 * budget, the newest that fit are shown whole (oldest first) and a line says how
 * to read the earlier ones. The delivery section is always shown.
 */
export function renderThread(t: Thread, options: ThreadOptions = {}): string {
  const budget = options.budget ?? MAX_CHARS;
  const lines = [
    `## Thread ${t.id} (${t.kind}, ${t.status})`,
    `Title: ${untrusted(t.from, t.title, true)}`,
    `Opened by @${t.from}; participants: ${t.participants.map((p) => `@${p}`).join(", ")}.`,
  ];
  if (t.context) lines.push(`Addressed to your person in context ${t.context}.`);
  if (t.waiting_for_you) lines.push("Waiting for your person's reply.");
  lines.push(holder(t.claimed));
  const head = lines.join("\n");
  const delivery = t.delivery ? ["Delivery:", ...t.delivery.map(renderDelivery)].join("\n") : null;

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
    const text = renderNote(all[start - 1]);
    if (shown.length && text.length + 2 > room) break;
    shown.unshift(text);
    room -= text.length + 2;
    start--;
  }

  const out = [head];
  if (!all.length) {
    out.push(whole ? "No notes." : "No new notes.");
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
function renderSkipped(t: Thread): string {
  const n = t.new_notes?.length ?? 0;
  const lines = [
    `## Thread ${t.id} (${t.kind}, ${t.status}): not shown here for length`,
    `Title: ${untrusted(t.from, t.title, true)}`,
  ];
  if (t.waiting_for_you) lines.push("Waiting for your person's reply.");
  lines.push(
    n
      ? `${n} new note(s) not shown: read them with relevo_thread { id: "${t.id}" }.`
      : `No new notes; read it with relevo_thread { id: "${t.id}" }.`,
  );
  return lines.join("\n");
}

/**
 * The inbox as text. Every thread here is delivered to this session already, so
 * none may go missing: threads are shown whole while they fit, and each one
 * that does not is still listed, with how to read it.
 */
export function renderInbox(inbox: Inbox): string {
  const session = `This session: "${inbox.session.label}", ${inbox.session.context ? `context ${inbox.session.context}` : "general"}.`;
  const out = [renderTeam(inbox.team), session];
  if (!inbox.threads.length) out.push("Nothing new for this session and nothing waiting.");
  const more = inbox.more ? `${inbox.more} more thread(s) not shown: call relevo_inbox again.` : null;
  const skipped = inbox.threads.map(renderSkipped);
  let room = MAX_CHARS - out.join("\n\n").length - (more ? more.length + 2 : 0);
  // What the threads after each one take at least, listed only.
  let after = skipped.reduce((sum, s) => sum + s.length + 2, 0);
  inbox.threads.forEach((t, i) => {
    after -= skipped[i].length + 2;
    const fits = room - after - 2;
    const full = renderThread(t, { budget: fits });
    const text = full.length <= fits ? full : skipped[i];
    out.push(text);
    room -= text.length + 2;
  });
  if (more) out.push(more);
  return out.join("\n\n");
}

/** What a session receives when a note arrives on its own. */
export function renderArrival(t: Thread): string {
  const intro =
    `Relevo: new in thread ${t.id}. Decide from your context and current work whether it is for you. ` +
    `If not, stay silent. If it is, claim it first with relevo_send { thread: "${t.id}", text: "On it." } before acting.`;
  return [intro, renderThread(t, { budget: MAX_CHARS - intro.length - 2 })].join("\n\n");
}
