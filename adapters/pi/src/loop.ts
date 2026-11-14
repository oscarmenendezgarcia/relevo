// The background waiting inbox request: while the pi session lives, one request
// with `wait` stays open; each thread that comes back with new notes is handed
// to `deliver` once, oldest first. A new session gets, as new, the notes still
// pending for its person (one that reached a session that died before handling
// it, say), so the first request hands those over at once. In every answer, a
// thread that names the person (any direct one) pending for them whose latest note, by someone
// else, this process has not shown the agent is handed over too, once: the answer
// that carried it was lost on the way (the server delivered it all the same), or
// it was fetched and never shown. Since only that latest note comes back, it says
// earlier ones may be missing; so does each thread with new notes in the first
// answer after a request that may have lost its answer (a note written after the
// loss comes without the lost ones). After a failed or paused request the next
// one does not wait: a lost note is no longer new and would not end a wait.
// Errors back off (doubling up to a minute); a refused token or a malformed
// request (400) stops it. `pause` ends the waiting
// request and asks for nothing until `resume`, so notes that land meanwhile stay
// new on the server.

import { RelevoError, type Client, type Thread } from "./client.ts";

export interface LoopOptions {
  client: Client;
  /**
   * The ids of the notes this process has shown the agent: handed over here, listed by
   * relevo_inbox or read with relevo_thread. The loop adds what it hands over.
   */
  shown?: Set<string>;
  /** `you` is the handle of the session's person, when the server says it. */
  deliver: (thread: Thread, you?: string) => void;
  /** Told about each failed request; `fatal` when the loop stops because of it. */
  onError?: (err: unknown, fatal: boolean) => void;
  wait?: number;
  minBackoffMs?: number;
  maxBackoffMs?: number;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
  });
}

/**
 * When the oldest new note of a thread was written (its latest note, with nothing new); threads
 * hand over oldest first by it.
 */
function oldestNote(t: Thread): string {
  return t.new_notes?.[0]?.created_at ?? t.last_note?.created_at ?? "";
}

/**
 * The threads oldest first: by their oldest new note, the one that has waited longest. The
 * server lists them newest first, so reversed first, threads of the same second keep its order
 * backwards (a stable sort).
 */
export function oldestFirst(threads: Thread[]): Thread[] {
  return [...threads].reverse().sort((a, b) => (oldestNote(a) < oldestNote(b) ? -1 : oldestNote(a) > oldestNote(b) ? 1 : 0));
}

/**
 * `t`, from an inbox answer, to hand over again when it is a thread that names the person (any
 * direct one) pending for them with nothing new, whose latest note (`last_note`), by someone else, is not in `shown`:
 * as it was, with that note as its new note and `handed_over` set. Otherwise null. Once shown, a
 * note the agent leaves alone (not for it) stays pending without coming back.
 */
export function handOverAgain(t: Thread, shown: Set<string>, me?: string): Thread | null {
  const last = t.last_note;
  if (!(t.kind === "direct" || t.named === true) || t.pending !== true || !last?.id) return null;
  if ((t.new_notes ?? []).length || last.author === me || shown.has(last.id)) return null;
  return { ...t, new_notes: [last], handed_over: true };
}

export class InboxLoop {
  options: LoopOptions;
  controller = new AbortController();
  running: Promise<void> | null = null;
  /** The notes shown to the agent in this process, so none is handed over twice. */
  shown: Set<string>;
  paused = false;
  /** Ends the current request or back-off when paused; a new one per pause. */
  #pause = new AbortController();
  #resumed: (() => void) | null = null;

  constructor(options: LoopOptions) {
    this.options = options;
    this.shown = options.shown ?? new Set();
  }

  start(): void {
    if (this.running || this.controller.signal.aborted) return;
    this.running = this.run();
  }

  /** Ends the waiting request at once and resolves when the loop has stopped. Safe to call twice. */
  async stop(): Promise<void> {
    this.controller.abort();
    this.#resumed?.();
    await this.running;
  }

  /** Ends the waiting request and asks for nothing until `resume`. */
  pause(): void {
    if (this.paused) return;
    this.paused = true;
    this.#pause.abort();
  }

  resume(): void {
    if (!this.paused) return;
    this.paused = false;
    this.#pause = new AbortController();
    this.#resumed?.();
    this.#resumed = null;
  }

  async run(): Promise<void> {
    const { client, wait = 55, minBackoffMs = 1000, maxBackoffMs = 60_000 } = this.options;
    const stopped = this.controller.signal;
    let backoff = minBackoffMs;
    // A request failed or was ended by a pause since the last answer: the next one does not wait.
    let failed = false;
    // The last failure may have lost an answer (anything but an error Relevo itself sent): the
    // first answer after it marks its threads with new notes. A pause is not counted: ending a
    // waiting request loses an answer only if the server wrote it in that very moment.
    let lost = false;
    while (!stopped.aborted) {
      if (this.paused) {
        await new Promise<void>((r) => (this.#resumed = r));
        continue;
      }
      const signal = AbortSignal.any([stopped, this.#pause.signal]);
      const started = Date.now();
      try {
        const inbox = await client.inbox({ wait: failed ? 0 : wait }, signal);
        const afterLoss = lost;
        backoff = minBackoffMs;
        failed = lost = false;
        let handed = 0;
        // Handed over even if paused meanwhile: these notes have reached this session on the
        // server now. Oldest first: the first one handed over starts the agent's turn.
        const you = inbox.session?.you;
        for (const listed of oldestFirst(inbox.threads)) {
          if (stopped.aborted) return;
          const notes = listed.new_notes ?? [];
          // Only the person's own notes (from another of their sessions): nothing to answer, as
          // relevo mcp and Hermes see it; unless the answer before may have been lost, with notes
          // by others in it: then it goes, saying to read the thread whole.
          if (!afterLoss && notes.length && you !== undefined && notes.every((n) => n.author === you)) continue;
          // Only pending: it reached this session before, and is handed over again only if the
          // agent was never shown its latest note.
          const thread = notes.length ? (afterLoss ? { ...listed, after_failure: true } : listed) : handOverAgain(listed, this.shown, you);
          if (!thread) continue;
          const news = thread.new_notes ?? [];
          if (this.shown.has(news[news.length - 1].id)) continue;
          for (const n of news) this.shown.add(n.id);
          handed++;
          try {
            this.options.deliver(thread, you);
          } catch (err) {
            this.options.onError?.(err, false);
          }
        }
        // An answer at once with nothing to hand over should not turn into a busy loop.
        if (!handed && Date.now() - started < minBackoffMs) await sleep(minBackoffMs, signal);
      } catch (err) {
        if (stopped.aborted) return;
        failed = true;
        if (signal.aborted) continue; // Paused: not an error.
        if (!(err instanceof RelevoError && err.code !== "HTTP")) lost = true;
        // A refused token, or a request the server calls malformed: asking again cannot help.
        if (err instanceof RelevoError && (err.status === 401 || err.status === 400)) {
          this.options.onError?.(err, true);
          return;
        }
        this.options.onError?.(err, false);
        await sleep(backoff, signal);
        backoff = Math.min(backoff * 2, maxBackoffMs);
      }
    }
  }
}
