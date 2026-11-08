// Starts a real relevo server for the tests, on a free port in 7990-7999 and a
// fresh database, and invites people. Tokens stay in memory: never printed.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repo = resolve(import.meta.dirname, "..", "..", "..");

export function relevoBin(): string {
  const candidates = [
    process.env.RELEVO_BIN,
    process.env.CARGO_TARGET_DIR && join(process.env.CARGO_TARGET_DIR, "release", "relevo"),
    "/tmp/relevo-target-pi/release/relevo",
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
  invite(handle: string, about?: string, label?: string): string;
  stop(): Promise<void>;
}

export async function startServer(): Promise<TestServer> {
  const bin = relevoBin();
  const dir = mkdtempSync(join(tmpdir(), "relevo-pi-test-"));
  const db = join(dir, "relevo.db");
  let child: ChildProcess | null = null;
  let url = "";
  for (let port = 7990; port <= 7999 && !child; port++) {
    if (!(await portFree(port))) continue;
    const c = spawn(bin, ["serve", "--addr", `127.0.0.1:${port}`, "--db", db], { stdio: "ignore" });
    const exited = new Promise<void>((r) => c.once("exit", () => r()));
    url = `http://127.0.0.1:${port}`;
    for (let i = 0; i < 100 && c.exitCode === null; i++) {
      try {
        // Up, and still ours: a server that lost the port race has exited.
        if ((await fetch(`${url}/health`)).ok && c.exitCode === null) {
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
    invite(handle, about = "", label = "pi test") {
      const args = ["invite", handle, "--name", handle, "--label", label, "--db", db];
      if (about) args.push("--about", about);
      const out = execFileSync(bin, args, { encoding: "utf8" });
      const token = out.split("\n").find((l) => l.startsWith("rlv_"));
      if (!token) throw new Error("invite printed no token");
      return token.trim();
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
  const res = await fetch(server.url + path, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      "Relevo-Session": session,
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await res.json();
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${json?.error?.code}`);
  return json;
}
