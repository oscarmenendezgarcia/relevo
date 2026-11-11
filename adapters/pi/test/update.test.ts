// The automatic update at session_start, with a stand-in for relevo: a shell script
// that records how it was run and answers as the test says. No server, no network.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { after, test } from "node:test";
import { findRelevo, inGitCheckout } from "../src/update.ts";
import { loadExtension, until } from "./mock-pi.ts";

const root = mkdtempSync(join(tmpdir(), "relevo-pi-update-"));
after(() => rmSync(root, { recursive: true, force: true }));

let n = 0;

/**
 * A fresh case: a folder that stands for the installed extension, and a stub relevo in a
 * directory of its own that writes its arguments to `args`, waits for `go` when told
 * to, prints STUB_OUT on stdout and STUB_ERR on stderr, and exits with STUB_EXIT.
 */
function setup() {
  const dir = join(root, `case-${++n}`);
  const bin = join(dir, "bin");
  const folder = join(dir, "relevo-pi");
  mkdirSync(bin, { recursive: true });
  mkdirSync(folder);
  writeFileSync(join(folder, "package.json"), '{ "name": "relevo-pi", "version": "0.3.0" }\n');
  const args = join(dir, "args");
  const go = join(dir, "go");
  writeFileSync(
    join(bin, "relevo"),
    `#!/bin/sh
printf '%s\\n' "$@" > "${args}.tmp" && mv "${args}.tmp" "${args}"
if [ -n "$STUB_WAIT" ]; then while [ ! -e "${go}" ]; do sleep 0.02; done; fi
[ -n "$STUB_OUT" ] && printf '%s\\n' "$STUB_OUT"
[ -n "$STUB_ERR" ] && printf '%s\\n' "$STUB_ERR" >&2
exit "\${STUB_EXIT:-0}"
`,
    { mode: 0o755 },
  );
  return {
    bin,
    folder,
    go,
    /** The arguments relevo was run with, one per line, or null if it was not run. */
    args: () => (existsSync(args) ? readFileSync(args, "utf8").trimEnd().split("\n") : null),
    env: (extra: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv => ({ PATH: `${bin}:/usr/bin:/bin`, ...extra }),
  };
}

async function start(env: NodeJS.ProcessEnv, folder: string | undefined, mode = "interactive") {
  const ext = loadExtension({ env, home: root, folder });
  await ext.emit("session_start", { type: "session_start" }, ext.ctx(root, mode));
  return ext;
}

/** Long enough for a started process to have run, to check that none was. */
const settle = () => new Promise((r) => setTimeout(r, 300));

const INSTALLED = "relevo for pi (/somewhere): installed 0.4.0 over 0.3.0; it applies at the next start";

test("session_start runs relevo update --auto for the extension's own folder, and says what it installed", async () => {
  const c = setup();
  const ext = await start(c.env({ STUB_OUT: INSTALLED }), c.folder);
  await until(() => ext.notified.length > 0);
  assert.deepEqual(c.args(), ["update", "--auto", "--adapter", "pi", "--path", c.folder]);
  assert.equal(ext.notified.length, 1);
  assert.match(ext.notified[0], /^Relevo was updated; it applies at the next start of pi\.\n/);
  assert.ok(ext.notified[0].endsWith(INSTALLED), ext.notified[0]);
  assert.deepEqual(ext.levels, ["info"]);
});

test("the update never holds the session: session_start returns while relevo still runs", async () => {
  const c = setup();
  const ext = await start(c.env({ STUB_WAIT: "1", STUB_OUT: INSTALLED }), c.folder);
  // session_start has returned; relevo is still waiting for `go`.
  await until(() => c.args() !== null);
  await settle();
  assert.deepEqual(ext.notified, []);
  writeFileSync(c.go, "");
  await until(() => ext.notified.length > 0);
  assert.match(ext.notified[0], /applies at the next start of pi/);
});

test("nothing installed: nothing said", async () => {
  const c = setup();
  const ext = await start(c.env(), c.folder);
  await until(() => c.args() !== null);
  await settle();
  assert.deepEqual(ext.notified, []);
});

test("a failure is said as a warning, with what relevo said", async () => {
  const c = setup();
  const why = "relevo (/usr/local/bin/relevo): not installed: /usr/local/bin is not writable";
  const ext = await start(c.env({ STUB_ERR: why, STUB_EXIT: "1" }), c.folder);
  await until(() => ext.notified.length > 0);
  assert.deepEqual(ext.notified, [`Relevo could not update itself:\n${why}`]);
  assert.deepEqual(ext.levels, ["warning"]);
});

test("installed something and failed at something else: both are said", async () => {
  const c = setup();
  const ext = await start(c.env({ STUB_OUT: INSTALLED, STUB_ERR: "relevo for hermes (/x): not installed: gone", STUB_EXIT: "1" }), c.folder);
  await until(() => ext.notified.length === 2);
  assert.deepEqual(ext.levels, ["info", "warning"]);
});

test("RELEVO_AUTO_UPDATE=0 turns it off", async () => {
  for (const off of ["0", "false", "No", " off "]) {
    const c = setup();
    const ext = await start(c.env({ RELEVO_AUTO_UPDATE: off, STUB_OUT: INSTALLED }), c.folder);
    await settle();
    assert.equal(c.args(), null, off);
    assert.deepEqual(ext.notified, []);
  }
  const c = setup();
  await start(c.env({ RELEVO_AUTO_UPDATE: "1" }), c.folder);
  await until(() => c.args() !== null);
});

test("without relevo, nothing is run and nothing said", async () => {
  const c = setup();
  const ext = await start({ PATH: join(root, "nowhere"), STUB_OUT: INSTALLED }, c.folder);
  const missing = await start({ RELEVO_BIN: join(root, "nowhere", "relevo"), PATH: c.env().PATH, STUB_OUT: INSTALLED }, c.folder);
  await settle();
  assert.equal(c.args(), null);
  assert.deepEqual([...ext.notified, ...missing.notified], []);
});

test("RELEVO_BIN comes before PATH", async () => {
  const a = setup();
  const b = setup();
  assert.equal(findRelevo({ PATH: a.bin }), join(a.bin, "relevo"));
  assert.equal(findRelevo({ PATH: `relative:${a.bin}` }), join(a.bin, "relevo"));
  assert.equal(findRelevo({ PATH: "" }), null);
  await start(b.env({ RELEVO_BIN: join(a.bin, "relevo") }), b.folder);
  await until(() => a.args() !== null);
  assert.deepEqual(a.args(), ["update", "--auto", "--adapter", "pi", "--path", b.folder]);
  assert.equal(b.args(), null);
});

test("print and JSON runs do not update", async () => {
  for (const mode of ["print", "json"]) {
    const c = setup();
    await start(c.env({ STUB_OUT: INSTALLED }), c.folder, mode);
    await settle();
    assert.equal(c.args(), null, mode);
  }
});

test("a folder in a git checkout is never updated: git updates it", async () => {
  const c = setup();
  execFileSync("git", ["init", "-q"], { cwd: join(c.folder, ".."), stdio: "ignore" });
  assert.ok(inGitCheckout(c.folder));
  const ext = await start(c.env({ STUB_OUT: INSTALLED }), c.folder);
  // The default folder, this package, is in this repository's checkout too.
  const own = resolve(import.meta.dirname, "..");
  assert.ok(inGitCheckout(own));
  const d = setup();
  await start(d.env({ STUB_OUT: INSTALLED }), undefined);
  await settle();
  assert.equal(c.args(), null);
  assert.equal(d.args(), null);
  assert.deepEqual(ext.notified, []);
  assert.ok(!inGitCheckout(d.folder));
});

test("a session that ended before relevo did is not told, and nothing breaks", async () => {
  const c = setup();
  const ext = loadExtension({ env: c.env({ STUB_WAIT: "1", STUB_OUT: INSTALLED }), home: root, folder: c.folder });
  let calls = 0;
  const ctx = {
    ...ext.ctx(root),
    ui: {
      notify() {
        calls++;
        throw new Error("this extension ctx is stale after session replacement or reload");
      },
    },
  };
  await ext.emit("session_start", { type: "session_start" }, ctx);
  await until(() => c.args() !== null);
  writeFileSync(c.go, "");
  await until(() => calls > 0);
  await settle();
  assert.equal(calls, 1);
});
