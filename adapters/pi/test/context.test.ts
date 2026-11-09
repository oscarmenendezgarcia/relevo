import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { loadConfig } from "../src/config.ts";
import { detectContext, invalidOverride } from "../src/context.ts";

const root = mkdtempSync(join(tmpdir(), "relevo-pi-ctx-"));
after(() => rmSync(root, { recursive: true, force: true }));

const noOverride = {} as NodeJS.ProcessEnv;

function git(cwd: string, ...args: string[]) {
  execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", ...args], { cwd, stdio: "ignore" });
}

function repo(name: string, branch: string): string {
  const dir = join(root, name);
  mkdirSync(dir, { recursive: true });
  git(dir, "init", "-q", "-b", branch);
  return dir;
}

test("outside a git repository the session is general", async () => {
  const dir = join(root, "plain");
  mkdirSync(dir);
  assert.equal(await detectContext(dir, noOverride), null);
});

test("repository and branch are lowercased and cleaned", async () => {
  const dir = repo("Pagos API", "Feat/Refunds_X+1");
  assert.equal(await detectContext(dir, noOverride), "pagos-api@feat/refunds_x-1");
  const sub = join(dir, "src", "deep");
  mkdirSync(sub, { recursive: true });
  assert.equal(await detectContext(sub, noOverride), "pagos-api@feat/refunds_x-1");
});

test("a detached HEAD gives the repository alone", async () => {
  const dir = repo("frontend", "main");
  git(dir, "commit", "-q", "--allow-empty", "-m", "first");
  git(dir, "checkout", "-q", "--detach");
  assert.equal(await detectContext(dir, noOverride), "frontend");
});

test("a tag with the same name as the branch does not change the context", async () => {
  const dir = repo("release", "main");
  git(dir, "commit", "-q", "--allow-empty", "-m", "first");
  git(dir, "tag", "main");
  git(dir, "checkout", "-q", "-b", "v1.2");
  git(dir, "tag", "v1.2");
  assert.equal(await detectContext(dir, noOverride), "release@v1.2");
  git(dir, "checkout", "-q", "main");
  assert.equal(await detectContext(dir, noOverride), "release@main");
});

test("a repository name the server would refuse leaves the session general", async () => {
  const dir = repo(".dotfiles", "main");
  assert.equal(await detectContext(dir, noOverride), null);
});

test("RELEVO_CONTEXT overrides git, and empty means general", async () => {
  const dir = repo("api", "main");
  assert.equal(await detectContext(dir, { RELEVO_CONTEXT: " Billing@Main " }), "billing@main");
  assert.equal(await detectContext(dir, { RELEVO_CONTEXT: "" }), null);
  assert.equal(invalidOverride({ RELEVO_CONTEXT: "Billing@Main" }), null);
  assert.equal(invalidOverride({ RELEVO_CONTEXT: "" }), null);
  assert.equal(invalidOverride(noOverride), null);
});

test("a RELEVO_CONTEXT the server would refuse leaves the session general, and says so", async () => {
  const dir = repo("api2", "main");
  for (const bad of ["Pagos API", ".dotfiles", "api@", "api@feat refunds", "a@b@c", "x".repeat(65)]) {
    assert.equal(await detectContext(dir, { RELEVO_CONTEXT: bad }), null, bad);
    assert.equal(invalidOverride({ RELEVO_CONTEXT: bad }), bad.trim());
  }
});

test("config comes from the file, and the environment overrides it", () => {
  const home = join(root, "home");
  mkdirSync(join(home, ".config", "relevo"), { recursive: true });
  assert.equal(loadConfig({}, home), null);
  writeFileSync(join(home, ".config", "relevo", "config.json"), JSON.stringify({ url: "https://r.example/", token: "rlv_file" }));
  assert.deepEqual(loadConfig({}, home), { url: "https://r.example", token: "rlv_file" });
  assert.deepEqual(loadConfig({ RELEVO_URL: "http://127.0.0.1:1", RELEVO_TOKEN: "rlv_env" }, home), {
    url: "http://127.0.0.1:1",
    token: "rlv_env",
  });
});
