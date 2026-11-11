// Starts a real relevo server for the tests, on a free port in 7990-7999 and a
// fresh database, and invites people. Tokens stay in memory: never printed.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pinnedRequest } from "../src/pinned.ts";

const repo = resolve(import.meta.dirname, "..", "..", "..");

/**
 * The relevo binary to test against: RELEVO_BIN when set (it must exist), else
 * this repository's build (CARGO_TARGET_DIR, then target/release, then
 * target/debug). Never a binary from elsewhere, which may be stale.
 */
export function relevoBin(): string {
  const bin = process.env.RELEVO_BIN;
  if (bin) {
    if (!existsSync(bin)) throw new Error(`RELEVO_BIN does not exist: ${bin}`);
    return bin;
  }
  const candidates = [
    process.env.CARGO_TARGET_DIR && join(process.env.CARGO_TARGET_DIR, "release", "relevo"),
    join(repo, "target", "release", "relevo"),
    join(repo, "target", "debug", "relevo"),
  ];
  const found = candidates.find((c) => c && existsSync(c));
  if (!found) throw new Error("relevo binary not found: build it with `cargo build --release` or set RELEVO_BIN");
  return found;
}

function portFree(port: number): Promise<boolean> {
  return new Promise((res) => {
    const s = createServer();
    s.once("error", () => res(false));
    s.listen(port, "127.0.0.1", () => s.close(() => res(true)));
  });
}

export interface TestServer {
  url: string;
  dir: string;
  /** With `tls`: the certificate `relevo serve --tls` made (PEM) and its file. */
  cert?: string;
  certFile?: string;
  invite(handle: string, about?: string, label?: string): string;
  /** Runs `relevo team <args…>` on the server's database: `team("add", "backend", "--title", "Backend")`, `team("join", "backend", "ana")`. */
  team(...args: string[]): void;
  stop(): Promise<void>;
}

/** `/health` answers, over https trusting the server's own certificate once it has made one. */
async function healthy(url: string, certFile: string | undefined): Promise<boolean> {
  const signal = AbortSignal.timeout(2000);
  if (!certFile) return (await fetch(`${url}/health`, { signal })).ok;
  if (!existsSync(certFile)) return false;
  return (await pinnedRequest(`${url}/health`, { method: "GET", headers: {}, signal }, readFileSync(certFile, "utf8"))).ok;
}

/**
 * `tls`: `relevo serve --tls`, its certificate made for 127.0.0.1 alone; `own`:
 * `--tls-cert` and `--tls-key` with a certificate of yours (PEM files).
 */
export async function startServer(
  options: { tls?: boolean; own?: { cert: string; key: string }; env?: Record<string, string> } = {},
): Promise<TestServer> {
  const bin = relevoBin();
  const dir = mkdtempSync(join(tmpdir(), "relevo-pi-test-"));
  const db = join(dir, "relevo.db");
  const certFile = options.own ? options.own.cert : options.tls ? `${db}.tls/cert.pem` : undefined;
  const tls = options.own
    ? ["--tls-cert", options.own.cert, "--tls-key", options.own.key]
    : options.tls
      ? ["--tls", "--tls-name", "127.0.0.1"]
      : [];
  let child: ChildProcess | null = null;
  let url = "";
  for (let port = 7990; port <= 7999 && !child; port++) {
    if (!(await portFree(port))) continue;
    const c = spawn(bin, ["serve", "--addr", `127.0.0.1:${port}`, "--db", db, ...tls], {
      stdio: "ignore",
      env: { ...process.env, ...options.env },
    });
    const exited = new Promise<void>((r) => c.once("exit", () => r()));
    url = `${certFile ? "https" : "http"}://127.0.0.1:${port}`;
    for (let i = 0; i < 100 && c.exitCode === null; i++) {
      try {
        // Up, and still ours: a server that lost the port race has exited.
        if ((await healthy(url, certFile)) && c.exitCode === null) {
          child = c;
          break;
        }
      } catch {
        // Not up yet.
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    if (!child) {
      c.kill("SIGTERM");
      await exited;
    }
  }
  if (!child) throw new Error("relevo serve did not start on any port in 7990-7999");
  const proc = child;
  return {
    url,
    dir,
    cert: certFile && readFileSync(certFile, "utf8"),
    certFile,
    invite(handle, about = "", label = "pi test") {
      const args = ["invite", handle, "--name", handle, "--label", label, "--db", db];
      if (about) args.push("--about", about);
      const out = execFileSync(bin, args, { encoding: "utf8" });
      const token = out.split("\n").find((l) => l.startsWith("rlv_"));
      if (!token) throw new Error("invite printed no token");
      return token.trim();
    },
    team(...args) {
      execFileSync(bin, ["team", ...args, "--db", db], { stdio: "ignore" });
    },
    async stop() {
      if (proc.exitCode === null) {
        const exited = new Promise((r) => proc.once("exit", r));
        proc.kill("SIGTERM");
        await exited;
      }
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

/** A plain HTTP call as another person, for setting up what the adapter should see. */
export async function api(server: TestServer, token: string, method: string, path: string, body?: unknown, session = "other-agent-1"): Promise<any> {
  const init = {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      "Relevo-Session": session,
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  };
  let status: number;
  let json: any;
  if (server.cert) {
    const res = await pinnedRequest(server.url + path, init, server.cert);
    status = res.status;
    json = JSON.parse(res.text);
  } else {
    const res = await fetch(server.url + path, init);
    status = res.status;
    json = await res.json();
  }
  if (status < 200 || status >= 300) throw new Error(`${method} ${path}: ${status} ${json?.error?.code}`);
  return json;
}
