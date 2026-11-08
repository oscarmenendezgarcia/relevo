// A small client for the Relevo HTTP API (docs/protocol.md).

import { randomBytes, randomUUID } from "node:crypto";
import type { Config } from "./config.ts";

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
  claimed_by: SessionRef | null;
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
  notes?: Note[];
  delivery?: Delivery[];
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
    const res = await fetch(this.config.url + path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    });
    const text = await res.text();
    let json: any = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      // Not JSON: a proxy page, say. Reported below by status.
    }
    if (!res.ok) {
      const err = json?.error;
      if (err && typeof err.code === "string") {
        const { code, message, ...extra } = err;
        throw new RelevoError(code, String(message ?? code), res.status, extra);
      }
      throw new RelevoError("HTTP", `HTTP ${res.status} from ${this.config.url}`, res.status);
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

function retryable(err: unknown): boolean {
  if (err instanceof RelevoError) return err.status >= 500;
  return true; // The request did not get an answer: network failure or timeout.
}
