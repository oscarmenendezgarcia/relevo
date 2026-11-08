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

export function renderThread(t: Thread): string {
  const lines = [
    `## Thread ${t.id} (${t.kind}, ${t.status})`,
    `Title: ${untrusted(t.from, t.title, true)}`,
    `Opened by @${t.from}; participants: ${t.participants.map((p) => `@${p}`).join(", ")}.`,
  ];
  if (t.context) lines.push(`Addressed to your person in context ${t.context}.`);
  if (t.waiting_for_you) lines.push("Waiting for your person's reply.");
  lines.push(holder(t.claimed));
  const out = [lines.join("\n")];
  if (t.notes) {
    out.push(`Notes (${t.notes.length}):`, ...t.notes.map(renderNote));
  } else if (t.new_notes?.length) {
    out.push(`New notes (${t.new_notes.length}):`, ...t.new_notes.map(renderNote));
  } else {
    out.push("No new notes.");
  }
  if (t.delivery) out.push(["Delivery:", ...t.delivery.map(renderDelivery)].join("\n"));
  return cap(out.join("\n\n"));
}

export function renderInbox(inbox: Inbox): string {
  const session = `This session: "${inbox.session.label}", ${inbox.session.context ? `context ${inbox.session.context}` : "general"}.`;
  const out = [renderTeam(inbox.team), session];
  if (!inbox.threads.length) out.push("Nothing new for this session and nothing waiting.");
  out.push(...inbox.threads.map(renderThread));
  if (inbox.more) out.push(`${inbox.more} more thread(s) not shown: call relevo_inbox again.`);
  return cap(out.join("\n\n"));
}

/** What a session receives when a note arrives on its own. */
export function renderArrival(t: Thread): string {
  return [
    `Relevo: new in thread ${t.id}. Decide from your context and current work whether it is for you. ` +
      `If not, stay silent. If it is, claim it first with relevo_send { thread: "${t.id}", text: "On it." } before acting.`,
    renderThread(t),
  ].join("\n\n");
}

function cap(text: string): string {
  if (text.length <= MAX_CHARS) return text;
  return `${text.slice(0, MAX_CHARS)}\n[Output cut at ${MAX_CHARS} characters.]`;
}
