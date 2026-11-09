// Where the server is, which token to use and which certificate to trust:
// ~/.config/relevo/config.json (or the file RELEVO_CONFIG names), overridden by
// RELEVO_URL, RELEVO_TOKEN and RELEVO_CERT (see docs/agents.md).

import { X509Certificate } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface Config {
  url: string;
  token: string;
  /** The server's certificate (PEM), trusted alone for it: `relevo connect` saves it when no authority of the system vouches for the server. */
  cert?: string;
}

/** The config file: RELEVO_CONFIG, else ~/.config/relevo/config.json. */
export function configPath(home: string = homedir(), env: NodeJS.ProcessEnv = {}): string {
  return env.RELEVO_CONFIG || join(home, ".config", "relevo", "config.json");
}

function normalize(url: string): string {
  return url.trim().replace(/\/+$/, "");
}

/**
 * The configuration, or null when the URL or the token is missing. Throws when
 * RELEVO_CERT cannot be read or the certificate to trust is not PEM.
 */
export function loadConfig(env: NodeJS.ProcessEnv = process.env, home: string = homedir()): Config | null {
  let file: { url?: unknown; token?: unknown; cert?: unknown } = {};
  try {
    const parsed = JSON.parse(readFileSync(configPath(home, env), "utf8"));
    if (parsed && typeof parsed === "object") file = parsed;
  } catch {
    // No file, or not JSON: the environment may still have both.
  }
  const saved = typeof file.url === "string" ? normalize(file.url) : "";
  const url = env.RELEVO_URL ? normalize(env.RELEVO_URL) : saved;
  const token = env.RELEVO_TOKEN || (typeof file.token === "string" ? file.token : "");
  if (!url || !token) return null;
  let cert: string | undefined;
  if (env.RELEVO_CERT) {
    try {
      cert = readFileSync(env.RELEVO_CERT, "utf8");
    } catch (err) {
      throw new Error(`Relevo: reading RELEVO_CERT ${env.RELEVO_CERT}: ${(err as Error).message}`);
    }
  } else if (typeof file.cert === "string" && file.cert && url === saved) {
    // The saved certificate goes with the saved server: not used for another RELEVO_URL.
    cert = file.cert;
  }
  if (cert === undefined) return { url, token };
  try {
    new X509Certificate(cert);
  } catch {
    throw new Error("Relevo: the saved certificate is not a PEM certificate; run `relevo connect` again");
  }
  return { url, token, cert };
}
