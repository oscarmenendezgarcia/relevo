// A small client for the Relevo HTTP API (docs/protocol.md).

import { randomBytes, randomUUID } from "node:crypto";
import type { Config } from "./config.ts";
import { pinnedRequest, type Answer } from "./pinned.ts";

export interface Person {
  handle: string;
  name: string;
  about: string;
  contexts: string[];
}

export interface Note {
  id: string;
  author: string;
  via: string;
  created_at: string;
  text: string;
  truncated: boolean;
}

export interface SessionRef {
  label: string;
  context: string | null;
}

export interface Delivery {
  handle: string;
  note: string | null;
  delivered_at: string | null;
  by: SessionRef | null;
  /** `since`: when that session claimed the thread (servers before it leave it out). */
  claimed_by: (SessionRef & { since?: string }) | null;
  seen_by: number;
}

export interface Thread {
  id: string;
  kind: "direct" | "team";
  title: string;
  from: string;
  participants: string[];
  status: "open" | "closed";
  waiting_for_you: boolean;
  context: string | null;
  claimed: "this-session" | "other-session" | null;
  new_notes?: Note[];
  /** In the inbox: the latest note, whoever wrote it and whichever session it reached (servers before it leave it out). */
  last_note?: Note | null;
  notes?: Note[];
  delivery?: Delivery[];
  /** In the inbox: the other participants who hold the thread now, with their session and since when (servers before it leave it out). */
  held_by?: (SessionRef & { handle: string; since?: string })[];
}

export interface Inbox {
  team: Person[];
  /** `you`: the handle of this session's person (servers before it leave it out). */
  session: SessionRef & { you?: string };
  threads: Thread[];
  more: number;
}

export interface SendFields {
  to?: string[];
  thread?: string;
  text?: string;
  close?: boolean;
  /** With `thread`: claims it for this session, as a state, without a note when there is no `text`. */
  claim?: boolean;
}

export interface SendResult {
  thread: string;
  note: string | null;
  status: "open" | "closed";
  claimed: "this-session" | "other-session" | null;
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
    };
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

  /** The team, and `you`: the handle of this token's person. */
  people(signal?: AbortSignal, timeoutMs = TIMEOUT_MS): Promise<{ team: Person[]; you?: string }> {
    return this.request("GET", "/api/people", undefined, signal, timeoutMs);
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
    const body = { ...fields, key: randomUUID() };
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

function retryable(err: unknown): boolean {
  if (err instanceof RelevoError) return err.status >= 500;
  return true; // The request did not get an answer: network failure or timeout.
}
