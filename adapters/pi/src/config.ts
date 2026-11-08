// Where the server is and which token to use: ~/.config/relevo/config.json,
// overridden by RELEVO_URL and RELEVO_TOKEN (see docs/agents.md).

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface Config {
  url: string;
  token: string;
}

export function configPath(home: string = homedir()): string {
  return join(home, ".config", "relevo", "config.json");
}

/** The configuration, or null when the URL or the token is missing. */
export function loadConfig(env: NodeJS.ProcessEnv = process.env, home: string = homedir()): Config | null {
  let file: { url?: unknown; token?: unknown } = {};
  try {
    const parsed = JSON.parse(readFileSync(configPath(home), "utf8"));
    if (parsed && typeof parsed === "object") file = parsed;
  } catch {
    // No file, or not JSON: the environment may still have both.
  }
  const url = env.RELEVO_URL || (typeof file.url === "string" ? file.url : "");
  const token = env.RELEVO_TOKEN || (typeof file.token === "string" ? file.token : "");
  if (!url || !token) return null;
  return { url: url.replace(/\/+$/, ""), token };
}
