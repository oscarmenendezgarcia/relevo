// The background waiting inbox request: while the pi session lives, one request
// with `wait` stays open; each thread that comes back with new notes is handed
// to `deliver` once. Errors back off (doubling up to a minute); a refused token or a
// malformed request (400) stops it. `pause` ends the waiting request and asks for
// nothing until `resume`, so notes that land meanwhile stay new on the server.

import { RelevoError, type Client, type Thread } from "./client.ts";

export interface LoopOptions {
  client: Client;
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

export class InboxLoop {
  options: LoopOptions;
  controller = new AbortController();
  running: Promise<void> | null = null;
  /** Latest note handed over per thread, so no note is delivered twice. */
  delivered = new Map<string, string>();
  paused = false;
  /** Ends the current request or back-off when paused; a new one per pause. */
  #pause = new AbortController();
  #resumed: (() => void) | null = null;

  constructor(options: LoopOptions) {
    this.options = options;
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
    while (!stopped.aborted) {
      if (this.paused) {
        await new Promise<void>((r) => (this.#resumed = r));
        continue;
      }
      const signal = AbortSignal.any([stopped, this.#pause.signal]);
      const started = Date.now();
      try {
        const inbox = await client.inbox({ wait }, signal);
        backoff = minBackoffMs;
        let handed = 0;
        // Handed over even if paused meanwhile: these notes are delivered on the server now.
        for (const thread of inbox.threads) {
          if (stopped.aborted) return;
          const notes = thread.new_notes ?? [];
          if (!notes.length) continue; // Only waiting: delivered before.
          const last = notes[notes.length - 1].id;
          if (this.delivered.get(thread.id) === last) continue;
          this.delivered.set(thread.id, last);
          handed++;
          try {
            this.options.deliver(thread, inbox.session?.you);
          } catch (err) {
            this.options.onError?.(err, false);
          }
        }
        // An answer at once with nothing to hand over should not turn into a busy loop.
        if (!handed && Date.now() - started < minBackoffMs) await sleep(minBackoffMs, signal);
      } catch (err) {
        if (stopped.aborted) return;
        if (signal.aborted) continue; // Paused: not an error.
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
