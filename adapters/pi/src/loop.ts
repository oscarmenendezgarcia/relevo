// The background waiting inbox request: while the pi session lives, one request
// with `wait` stays open; each thread that comes back with new notes is handed
// to `deliver` once. Errors back off (doubling up to a minute); a refused token or a
// malformed request (400) stops it.

import { RelevoError, type Client, type Thread } from "./client.ts";

export interface LoopOptions {
  client: Client;
  deliver: (thread: Thread) => void;
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
    await this.running;
  }

  async run(): Promise<void> {
    const { client, wait = 55, minBackoffMs = 1000, maxBackoffMs = 60_000 } = this.options;
    const signal = this.controller.signal;
    let backoff = minBackoffMs;
    while (!signal.aborted) {
      const started = Date.now();
      try {
        const inbox = await client.inbox({ wait }, signal);
        backoff = minBackoffMs;
        let handed = 0;
        for (const thread of inbox.threads) {
          if (signal.aborted) return;
          const notes = thread.new_notes ?? [];
          if (!notes.length) continue; // Only waiting: delivered before.
          const last = notes[notes.length - 1].id;
          if (this.delivered.get(thread.id) === last) continue;
          this.delivered.set(thread.id, last);
          handed++;
          try {
            this.options.deliver(thread);
          } catch (err) {
            this.options.onError?.(err, false);
          }
        }
        // An answer at once with nothing to hand over should not turn into a busy loop.
        if (!handed && Date.now() - started < minBackoffMs) await sleep(minBackoffMs, signal);
      } catch (err) {
        if (signal.aborted) return;
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
