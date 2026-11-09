// The extension against a real `relevo serve --tls`: it trusts the certificate
// pinned in the config (or RELEVO_CERT) alone, and plain http is unchanged.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { explained, NOT_PINNED, outOfDate } from "../src/pinned.ts";
import { loadExtension, until } from "./mock-pi.ts";
import { api, startServer, type TestServer } from "./server.ts";

let server: TestServer;
let other: TestServer;
let oscar: string;
let ana: string;
let home: string;

before(async () => {
  server = await startServer({ tls: true });
  // Another server, with a certificate of its own.
  other = await startServer({ tls: true });
  oscar = server.invite("oscar", "relevo itself", "pi laptop");
  ana = server.invite("ana", "payments backend");
  home = mkdtempSync(join(tmpdir(), "relevo-pi-tls-"));
});

after(async () => {
  await server?.stop();
  await other?.stop();
  rmSync(home, { recursive: true, force: true });
});

/** What `relevo connect` writes, in a file of its own named by RELEVO_CONFIG. */
function configFile(config: Record<string, unknown>): string {
  const file = join(home, `config-${Math.random().toString(36).slice(2)}.json`);
  writeFileSync(file, JSON.stringify(config), { mode: 0o600 });
  return file;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("the tools work over https with the certificate saved in the config", async () => {
  assert.match(server.url, /^https:\/\//);
  const RELEVO_CONFIG = configFile({ url: server.url, token: oscar, cert: server.cert });
  const ext = loadExtension({ env: { RELEVO_CONFIG, RELEVO_CONTEXT: "" }, home });
  const out = await ext.tool("relevo_send", { to: ["ana"], text: "Over TLS: is /refunds idempotent?" }, home);
  const id = out.match(/thread (r_\w+)/)?.[1];
  assert.ok(id, out);
  const seen = await api(server, ana, "GET", `/api/threads/${id}`);
  assert.equal(seen.notes[0].text, "Over TLS: is /refunds idempotent?");

  await api(server, ana, "POST", "/api/send", { thread: id, text: "Yes, by key." });
  const inbox = await ext.tool("relevo_inbox", {}, home);
  assert.match(inbox, /^You speak for @oscar/);
  assert.match(inbox, /Yes, by key\./);
  assert.match(await ext.tool("relevo_thread", { id }, home), /Over TLS: is \/refunds idempotent\?/);
});

test("RELEVO_CERT names the certificate to trust, with RELEVO_URL and RELEVO_TOKEN", async () => {
  const ext = loadExtension({
    env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CERT: server.certFile, RELEVO_CONTEXT: "" },
    home,
  });
  assert.match(await ext.tool("relevo_inbox", {}, home), /^You speak for @oscar/);
});

test("the waiting inbox request, its wake-up and the end of the session work over https", async () => {
  const RELEVO_CONFIG = configFile({ url: server.url, token: oscar, cert: server.cert });
  const ext = loadExtension({ env: { RELEVO_CONFIG, RELEVO_CONTEXT: "" }, home, wait: 1 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "rpc"));
  let thread: string;
  try {
    const opened = await api(server, ana, "POST", "/api/send", { to: ["oscar"], text: "Can you look at the TLS ticket?" });
    thread = opened.thread;
    await until(() => ext.sent.length === 1);
    assert.deepEqual(ext.sent[0].options, { triggerTurn: true });
    assert.match(ext.sent[0].message.content, /Can you look at the TLS ticket\?/);
    // Claimed by this session, so ending it shows in the thread.
    assert.match(await ext.tool("relevo_send", { thread, claim: true }, home), /this session has it now/);
    // Several waits pass over the same connection; nothing is delivered twice.
    await sleep(2500);
    assert.equal(ext.sent.length, 1);
  } finally {
    const t = Date.now();
    await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "rpc"));
    assert.ok(Date.now() - t < 500, "shutdown ends the waiting request at once");
  }
  const after = await api(server, ana, "GET", `/api/threads/${thread}`);
  assert.equal(after.delivery.find((d: any) => d.handle === "oscar").claimed_by, null, "the session ended on the server");
});

test("a server with another certificate than the pinned one is refused, before the token is sent", async () => {
  // The certificate of `server`, pinned, for `other`: valid for 127.0.0.1 too, but not that one.
  const RELEVO_CONFIG = configFile({ url: other.url, token: "rlv_never_sent", cert: server.cert });
  const ext = loadExtension({ env: { RELEVO_CONFIG, RELEVO_CONTEXT: "" }, home });
  await assert.rejects(ext.tool("relevo_inbox", {}, home), (err: Error) => err.message.includes(NOT_PINNED));

  // And the system's authorities are not asked instead: without a pinned certificate it fails too.
  const plain = loadExtension({ env: { RELEVO_URL: server.url, RELEVO_TOKEN: oscar, RELEVO_CONTEXT: "" }, home });
  await assert.rejects(plain.tool("relevo_inbox", {}, home));
});

test("a pinned certificate out of its dates says so, as relevo mcp does", () => {
  // What node:https rejects with for the pinned certificate itself once it has expired, or
  // before it is valid (OpenSSL checks the dates; relevo mcp checks them too).
  const expired = explained(Object.assign(new Error("certificate has expired"), { code: "CERT_HAS_EXPIRED" }));
  assert.equal(expired.message, outOfDate("has expired"));
  assert.match(expired.message, /expired: the server needs a new certificate; then run `relevo connect` again$/);
  const early = explained(Object.assign(new Error("certificate is not yet valid"), { code: "CERT_NOT_YET_VALID" }));
  assert.equal(early.message, outOfDate("is not valid yet"));
  // Another certificate is still not the pinned one; other errors pass as they are.
  assert.equal(explained(Object.assign(new Error("x"), { code: "DEPTH_ZERO_SELF_SIGNED_CERT" })).message, NOT_PINNED);
  const refused = Object.assign(new Error("connect ECONNREFUSED"), { code: "ECONNREFUSED" });
  assert.equal(explained(refused), refused);
});

test("the pinned certificate must name the host the URL has", async () => {
  // The server's certificate names 127.0.0.1 alone.
  const url = server.url.replace("127.0.0.1", "localhost");
  const ext = loadExtension({ env: { RELEVO_URL: url, RELEVO_TOKEN: oscar, RELEVO_CERT: server.certFile, RELEVO_CONTEXT: "" }, home });
  await assert.rejects(ext.tool("relevo_inbox", {}, home), /localhost/);
});

test("a background request refused for its certificate keeps retrying quietly, and shutdown is prompt", async () => {
  const RELEVO_CONFIG = configFile({ url: other.url, token: "rlv_never_sent", cert: server.cert });
  const ext = loadExtension({ env: { RELEVO_CONFIG, RELEVO_CONTEXT: "" }, home, wait: 1, minBackoffMs: 50, maxBackoffMs: 100 });
  await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
  await sleep(300);
  const t = Date.now();
  await ext.emit("session_shutdown", { reason: "quit" }, ext.ctx(home, "tui"));
  assert.ok(Date.now() - t < 2500, "shutdown is not held up");
  assert.equal(ext.sent.length, 0);
});

test("plain http is unchanged: a pinned certificate is not used for an http:// server", async () => {
  const plain = await startServer();
  try {
    const token = plain.invite("oscar", "relevo itself");
    const ext = loadExtension({
      env: { RELEVO_URL: plain.url, RELEVO_TOKEN: token, RELEVO_CERT: server.certFile, RELEVO_CONTEXT: "" },
      home,
    });
    assert.match(plain.url, /^http:\/\//);
    assert.match(await ext.tool("relevo_inbox", {}, home), /^You speak for @oscar/);
  } finally {
    await plain.stop();
  }
});

/** An office authority and a certificate it signed for 127.0.0.1, made with openssl; null without it. */
function officeCertificates(): { ca: string; cert: string; key: string } | null {
  const dir = mkdtempSync(join(home, "office-"));
  const f = (name: string) => join(dir, name);
  const ssl = (...args: string[]) => execFileSync("openssl", args, { stdio: "ignore" });
  try {
    ssl("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f("ca.key"),
      "-out", f("ca.pem"), "-days", "2", "-subj", "/CN=Office CA", "-addext", "basicConstraints=critical,CA:TRUE");
    ssl("req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", f("key.pem"),
      "-out", f("req.csr"), "-subj", "/CN=relevo");
    writeFileSync(f("ext.cnf"), "subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\n");
    ssl("x509", "-req", "-in", f("req.csr"), "-CA", f("ca.pem"), "-CAkey", f("ca.key"), "-CAcreateserial",
      "-out", f("cert.pem"), "-days", "2", "-extfile", f("ext.cnf"));
  } catch {
    return null;
  }
  return { ca: f("ca.pem"), cert: f("cert.pem"), key: f("key.pem") };
}

test("a pinned certificate an office authority signed is trusted on its own, and that authority alone is not", async (t) => {
  const office = officeCertificates();
  if (!office) return t.skip("openssl is not available");
  const signed = await startServer({ own: { cert: office.cert, key: office.key } });
  try {
    const token = signed.invite("oscar", "relevo itself");
    // The server's own certificate, pinned without its authority.
    const pinned = loadExtension({ env: { RELEVO_URL: signed.url, RELEVO_TOKEN: token, RELEVO_CERT: office.cert, RELEVO_CONTEXT: "" }, home });
    assert.match(await pinned.tool("relevo_inbox", {}, home), /^You speak for @oscar/);
    // The authority that signed it is not the certificate saved: refused.
    const authority = loadExtension({ env: { RELEVO_URL: signed.url, RELEVO_TOKEN: token, RELEVO_CERT: office.ca, RELEVO_CONTEXT: "" }, home });
    await assert.rejects(authority.tool("relevo_inbox", {}, home), (err: Error) => err.message.includes(NOT_PINNED));
  } finally {
    await signed.stop();
  }
});
