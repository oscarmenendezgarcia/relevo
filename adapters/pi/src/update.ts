// The automatic update (docs/update.md): at session_start the extension runs
// `relevo update --auto --adapter pi --path <its folder>` in the background. The
// relevo binary does the work (the signature, the checks, the swap, at most one
// check an hour); the extension only starts it and tells the person what came of it.

import { spawn } from "node:child_process";
import { accessSync, constants, existsSync, statSync } from "node:fs";
import { delimiter, dirname, isAbsolute, join, resolve } from "node:path";

/** Whether `RELEVO_AUTO_UPDATE` turns the automatic update off, as relevo reads it. */
export function autoUpdateOff(env: NodeJS.ProcessEnv): boolean {
  return ["0", "false", "no", "off"].includes((env.RELEVO_AUTO_UPDATE ?? "").trim().toLowerCase());
}

function runnable(file: string): boolean {
  try {
    accessSync(file, constants.X_OK);
    return statSync(file).isFile();
  } catch {
    return false;
  }
}

/** The relevo binary: RELEVO_BIN when set, else `relevo` on PATH; null when there is none. */
export function findRelevo(env: NodeJS.ProcessEnv): string | null {
  if (env.RELEVO_BIN) return runnable(env.RELEVO_BIN) ? env.RELEVO_BIN : null;
  for (const dir of (env.PATH ?? "").split(delimiter)) {
    // An empty or relative entry would depend on pi's working directory.
    if (!dir || !isAbsolute(dir)) continue;
    const file = join(dir, "relevo");
    if (runnable(file)) return file;
  }
  return null;
}

/** Whether `dir` is in a git checkout: such a folder is updated with git, never replaced. */
export function inGitCheckout(dir: string): boolean {
  for (let d = resolve(dir); ; d = dirname(d)) {
    if (existsSync(join(d, ".git"))) return true;
    if (dirname(d) === d) return false;
  }
}

/** What the person is told; `level` as pi's `ctx.ui.notify` takes it. */
export type Notify = (text: string, level: "info" | "warning") => void;

/** Keeps the last `max` characters of what a stream wrote. */
function collect(stream: NodeJS.ReadableStream | null, max = 4000): () => string {
  let text = "";
  stream?.setEncoding("utf8");
  stream?.on("data", (chunk: string) => {
    text = (text + chunk).slice(-max);
  });
  return () => text.trim();
}

/**
 * Starts `relevo update --auto --adapter pi --path <folder>` and returns at once: it never
 * holds pi, nor keeps it from exiting. When it ends, `notify` hears what it installed (it
 * applies at the next start of pi) and what failed; nothing when there was nothing to do,
 * or no relevo to run. Returns whether it started.
 */
export function autoUpdate(env: NodeJS.ProcessEnv, folder: string, notify: Notify): boolean {
  if (autoUpdateOff(env) || inGitCheckout(folder)) return false;
  const bin = findRelevo(env);
  if (!bin) return false;
  let child;
  try {
    child = spawn(bin, ["update", "--auto", "--adapter", "pi", "--path", folder], {
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch {
    return false;
  }
  const out = collect(child.stdout);
  const err = collect(child.stderr);
  child.on("error", () => {}); // Could not run it after all: nothing to say.
  child.on("close", (code) => {
    const installed = out();
    if (installed) notify(`Relevo was updated; it applies at the next start of pi.\n${installed}`, "info");
    const failed = err();
    if (code !== 0 && failed) notify(`Relevo could not update itself:\n${failed}`, "warning");
  });
  // In the background: pi may exit while it runs, and the update goes on without it.
  child.unref();
  (child.stdout as any)?.unref?.();
  (child.stderr as any)?.unref?.();
  return true;
}
