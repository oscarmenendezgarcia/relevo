// A small client for the Relevo HTTP API (docs/protocol.md).

import { randomBytes, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Config } from "./config.ts";
import { pinnedRequest, type Answer } from "./pinned.ts";
import type { Timeline } from "./waiting.ts";

/**
 * What every request says of this client (`Relevo-Client`, docs/protocol.md): `pi/<version>`, the
 * version in this extension's package.json, as it was when pi loaded it. `null` when it cannot be
 * read: the header is left out, which the server takes as an unknown client.
 */
export const CLIENT: string | null = (() => {
  try {
    const { version } = JSON.parse(readFileSync(resolve(import.meta.dirname, "..", "package.json"), "utf8"));
    return typeof version === "string" && version ? `pi/${version}` : null;
  } catch {
    return null;
  }
})();

export interface Person {
  handle: string;
  name: string;
  about: string;
  contexts: string[];
  /** The teams they are in (servers before 0.4.0 leave it out). */
  teams?: string[];
}

/** A team on the server: whom `@name` reaches. */
export interface Team {
  name: string;
  title: string;
  /** Its active members. */
  members: string[];
}

export interface Note {
  id: string;
  author: string;
  via: string;
  created_at: string;
  text: string;
  truncated: boolean;
  /** A note by the server's own person: the release notes (servers before 0.5.0, and other notes, leave it out). */
  system?: boolean;
}

export interface SessionRef {
  label: string;
  context: string | null;
}

export interface Delivery {
  handle: string;
  /** Whether the thread names them (servers before 0.4.0 leave it out: every participant of a direct thread is). */
  named?: boolean;
  note: string | null;
  delivered_at: string | null;
  by: SessionRef | null;
  /** `since`: when that session claimed the thread (servers before it leave it out). */
  claimed_by: (SessionRef & { since?: string }) | null;
  /** How many of their live sessions have that note. */
  seen_by: number;
  /** When the person handled that note, and from which session; `null` while not handled (servers before 0.3.0 leave both out). */
  handled_at?: string | null;
  handled_by?: SessionRef | null;
}

export interface Thread {
  id: string;
  /** `team` from a server before 0.4.0 is a question to everyone (it has no `teams`). */
  kind: "direct" | "team" | "global";
  title: string;
  from: string;
  participants: string[];
  /** The teams its opening note named (servers before 0.4.0 leave it out). */
  teams?: string[];
  /** Whether it names the person: only then can it be pending for them (servers before 0.4.0 leave it out). */
  named?: boolean;
  /** Always "open" from 0.5.0 on: threads no longer close (a 0.4.x server may still say "closed"). */
  status: string;
  /** Pending for the person: not handled yet (servers before 0.3.0 leave it out: nothing is pending then). */
  pending?: boolean;
  context: string | null;
  claimed: "this-session" | "other-session" | null;
  new_notes?: Note[];
  /** In the inbox: the latest note, whoever wrote it and whichever session it reached (servers before it leave it out). */
  last_note?: Note | null;
  notes?: Note[];
  delivery?: Delivery[];
  /** In the inbox: the other participants who hold the thread now, with their session and since when (servers before it leave it out). */
  held_by?: (SessionRef & { handle: string; since?: string })[];
  /** Set by the extension, never by the server: handed over again, its latest note never shown in this process. */
  handed_over?: boolean;
  /**
   * Set by the extension, never by the server: in the first answer after a request that may have lost its
   * answer, so notes of the thread before these may never have reached this session.
   */
  after_failure?: boolean;
}

export interface Inbox {
  /** Every active person on the server (the name is kept from 0.3). */
  team: Person[];
  /** Every team, with its members (servers before 0.4.0 leave it out). */
  teams?: Team[];
  /** `you`: the handle of this session's person (servers before it leave it out). */
  session: SessionRef & { you?: string };
  threads: Thread[];
  more: number;
}

/** Who a note is for is written in its `text` (@handle, @team-name, no @ for the global board): there is no `to`. */
export interface SendFields {
  thread?: string;
  text?: string;
  /** With `thread`: claims it for this session, as a state, without a note when there is no `text`. */
  claim?: boolean;
  /**
   * With `thread` and no `text`: the person has seen the thread up to the latest note this session has received, writing nothing;
   * this session lets go of it if it held it.
   */
  seen?: boolean;
  /** Deprecated: the name of `seen` up to 0.4.x. */
  done?: boolean;
  /** Deprecated: threads no longer close. Without `text` the server reads it as `seen`; with `text`, it is ignored. */
  close?: boolean;
}

export interface SendResult {
  thread: string;
  note: string | null;
  /** Always "open" from 0.5.0 on. */
  status: string;
  claimed: "this-session" | "other-session" | null;
  /** The rest from 0.4.0 on: the thread's kind, whom this note's mentions named, whom it added, and how many people besides the person take part. */
  kind?: Thread["kind"];
  to?: { people: string[]; teams: string[] };
  added?: string[];
  reached?: number;
}

/** An error answered by the server, with its code and any extra fields (`valid` for UNKNOWN_PERSON). */
export class RelevoError extends Error {
  code: string;
  status: number;
  extra: Record<string, unknown>;
  constructor(code: string, message: string, status: number, extra: Record<string, unknown> = {}) {
    super(message);
    this.name = "RelevoError";
    this.code = code;
    this.status = status;
    this.extra = extra;
  }
}

/** A random session id, `^[A-Za-z0-9_-]{8,64}$`, picked once per pi session. */
export function newSessionId(): string {
  return `pi-${randomBytes(16).toString("base64url")}`;
}

export interface ClientOptions {
  config: Config;
  session: string;
  /** Worked out again for every request. */
  context: () => Promise<string | null>;
}

const TIMEOUT_MS = 30_000;

export class Client {
  config: Config;
  session: string;
  context: () => Promise<string | null>;

  constructor(options: ClientOptions) {
    this.config = options.config;
    this.session = options.session;
    this.context = options.context;
  }

  async request<T>(method: string, path: string, body?: unknown, signal?: AbortSignal, timeoutMs = TIMEOUT_MS): Promise<T> {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.config.token}`,
      "Relevo-Session": this.session,
      // A reply's @ mentions name people (docs/protocol.md, `send`): this client knows it.
      "Relevo-Mentions": "1",
    };
    if (CLIENT) headers["Relevo-Client"] = CLIENT;
    const context = await this.context();
    if (context) headers["Relevo-Context"] = context;
    if (body !== undefined) headers["Content-Type"] = "application/json";
    const timeout = AbortSignal.timeout(timeoutMs);
    const init = {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    };
    const url = this.config.url + path;
    // A pinned certificate: trusted alone, over node:https (fetch cannot take it).
    const pem = this.config.cert;
    const { status, ok, text } = pem && url.startsWith("https://") ? await pinnedRequest(url, init, pem) : await fetched(url, init);
    let json: any = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      // Not JSON: a proxy page, say. Reported below by status.
    }
    if (!ok) {
      const err = json?.error;
      if (err && typeof err.code === "string") {
        const { code, message, ...extra } = err;
        throw new RelevoError(code, String(message ?? code), status, extra);
      }
      throw new RelevoError("HTTP", `HTTP ${status} from ${this.config.url}`, status);
    }
    return json as T;
  }

  inbox(options: { all?: boolean; wait?: number } = {}, signal?: AbortSignal): Promise<Inbox> {
    const params = new URLSearchParams();
    if (options.all) params.set("all", "1");
    if (options.wait) params.set("wait", String(options.wait));
    const query = params.size ? `?${params}` : "";
    // A waiting request may be held up to `wait` seconds before it answers.
    return this.request("GET", `/api/inbox${query}`, undefined, signal, TIMEOUT_MS + (options.wait ?? 0) * 1000);
  }

  /** The people and the teams on the server, and `you`: the handle of this token's person. */
  people(signal?: AbortSignal, timeoutMs = TIMEOUT_MS): Promise<{ team: Person[]; teams?: Team[]; you?: string }> {
    return this.request("GET", "/api/people", undefined, signal, timeoutMs);
  }

  /**
   * `GET /api/timeline?<query>`: read only, it delivers, handles and claims nothing, and the server
   * ignores the session headers (docs/protocol.md, "timeline").
   */
  timeline(query: string, signal?: AbortSignal): Promise<Timeline> {
    return this.request("GET", `/api/timeline?${query}`, undefined, signal);
  }

  thread(id: string, signal?: AbortSignal): Promise<Thread> {
    return this.request("GET", `/api/threads/${encodeURIComponent(id)}`, undefined, signal);
  }

  /** Ends this session on the server: it is forgotten and its claims released (`DELETE /api/session`). */
  async endSession(timeoutMs = TIMEOUT_MS): Promise<void> {
    await this.request("DELETE", "/api/session", undefined, undefined, timeoutMs);
  }

  /** One idempotency key per call; a request that fails on the way or on the server is retried once with it. */
  async send(fields: SendFields, signal?: AbortSignal): Promise<SendResult> {
    // Only the fields of `send` (docs/protocol.md): never the deprecated `to`. Threads no longer close: a `close` without
    // text is `seen` (and claims nothing, as it never did); with text it is the note alone. It never reaches the server,
    // which up to 0.4.x would close the thread. `seen` goes on the wire as `done`, its name up to 0.4.x, which servers
    // accept through 0.x: a 0.4.x server refuses `seen`, and this extension may be newer than its server. Given with
    // `done` too, both go, for the server to refuse.
    const body: Record<string, unknown> = { key: randomUUID() };
    let { seen, claim } = fields;
    if (fields.close === true && fields.text === undefined && seen === undefined && fields.done === undefined) {
      seen = true;
      claim = undefined;
    }
    for (const k of ["thread", "text", "done"] as const) if (fields[k] !== undefined) body[k] = fields[k];
    if (claim !== undefined) body.claim = claim;
    if (seen !== undefined) body[fields.done === undefined ? "done" : "seen"] = seen;
    try {
      return await this.request<SendResult>("POST", "/api/send", body, signal);
    } catch (err) {
      if (signal?.aborted || !retryable(err)) throw err;
      return await this.request<SendResult>("POST", "/api/send", body, signal);
    }
  }
}

async function fetched(url: string, init: RequestInit): Promise<Answer> {
  const res = await fetch(url, init);
  return { status: res.status, ok: res.ok, text: await res.text() };
}

/** Whether `err` came before Relevo answered: no answer at all, or a proxy's 502, 503 or 504 without Relevo's error body. */
export function unreachable(err: unknown): boolean {
  if (!(err instanceof RelevoError)) return true;
  return err.code === "HTTP" && err.status >= 502 && err.status <= 504;
}

function retryable(err: unknown): boolean {
  if (err instanceof RelevoError) return err.status >= 500;
  return true; // The request did not get an answer: network failure or timeout.
}
