// The session's context, worked out from git on every request (docs/agents.md):
// the repository (basename of the top-level directory) and, on a branch, "@branch".

import { execFile } from "node:child_process";
import { basename } from "node:path";

const REPO = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const BRANCH = /^[a-z0-9._/-]{1,100}$/;

function git(cwd: string, args: string[]): Promise<string | null> {
  return new Promise((resolve) => {
    execFile("git", args, { cwd, timeout: 5000, windowsHide: true }, (err, stdout) => {
      resolve(err ? null : stdout.trim());
    });
  });
}

export function repoName(topLevel: string): string {
  return basename(topLevel).toLowerCase().replace(/[^a-z0-9._-]/g, "-");
}

export function branchName(branch: string): string {
  return branch.toLowerCase().replace(/[^a-z0-9._/-]/g, "-");
}

/**
 * The context for a request made from `cwd`, or null for a general session.
 * RELEVO_CONTEXT, when set, wins (empty means general). A repository or branch
 * the server would refuse even after cleaning (a name starting with "." or over
 * the length limit) is left out rather than failing every request.
 */
export async function detectContext(cwd: string, env: NodeJS.ProcessEnv = process.env): Promise<string | null> {
  if (env.RELEVO_CONTEXT !== undefined) return env.RELEVO_CONTEXT.trim() || null;
  const [top, ref] = await Promise.all([
    git(cwd, ["rev-parse", "--show-toplevel"]),
    // Not --short: with a tag of the same name it prints "heads/<branch>".
    git(cwd, ["symbolic-ref", "--quiet", "HEAD"]),
  ]);
  if (!top) return null;
  const repo = repoName(top);
  if (!REPO.test(repo)) return null;
  const branch = ref?.startsWith("refs/heads/") ? ref.slice("refs/heads/".length) : null;
  if (!branch) return repo;
  const b = branchName(branch);
  return BRANCH.test(b) ? `${repo}@${b}` : repo;
}
