// Requests to a server whose certificate is pinned (`cert` in the config, or
// RELEVO_CERT): that certificate alone is trusted, never the system's
// authorities, as relevo mcp does (src/tls.rs). Node's fetch cannot take a CA
// without the undici package, so these go through node:https.

import { X509Certificate } from "node:crypto";
import { Agent, request } from "node:https";
import { checkServerIdentity, type PeerCertificate } from "node:tls";

export const NOT_PINNED =
  "the server's certificate is not the one `relevo connect` saved; if the server has a new certificate, run `relevo connect` again";

/**
 * What OpenSSL says when the server's chain does not lead to the one authority
 * it was given, the pinned certificate: the server has another certificate.
 */
const OTHER_CERT = new Set([
  "RELEVO_NOT_PINNED",
  "DEPTH_ZERO_SELF_SIGNED_CERT",
  "SELF_SIGNED_CERT_IN_CHAIN",
  "UNABLE_TO_GET_ISSUER_CERT",
  "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
  "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
  "CERT_SIGNATURE_FAILURE",
  "CERT_UNTRUSTED",
  "CERT_REJECTED",
]);

/** The pinned certificate out of its validity dates, as relevo mcp says it (src/tls.rs). */
export const outOfDate = (why: string) =>
  `the server's certificate, the one \`relevo connect\` saved, ${why}: the server needs a new certificate; then run \`relevo connect\` again`;

/** What OpenSSL says when the chain leads to the pinned certificate but it is out of its dates. */
const OUT_OF_DATE: Record<string, string> = {
  CERT_HAS_EXPIRED: "has expired",
  CERT_NOT_YET_VALID: "is not valid yet",
};

/** `err` from a request, said in Relevo's words when it is about the pinned certificate. */
export function explained(err: Error & { code?: string }): Error {
  const why = err.code ? OUT_OF_DATE[err.code] : undefined;
  if (why) return Object.assign(new Error(outOfDate(why), { cause: err }), { code: "RELEVO_OUT_OF_DATE" });
  if (!err.code || !OTHER_CERT.has(err.code)) return err;
  return Object.assign(new Error(NOT_PINNED, { cause: err }), { code: "RELEVO_NOT_PINNED" });
}

/** One agent per pinned certificate, so connections are kept and reused as fetch keeps them. */
const agents = new Map<string, Agent>();

function agent(pem: string): Agent {
  let found = agents.get(pem);
  if (!found) {
    const pinned = new X509Certificate(pem);
    found = new Agent({
      keepAlive: true,
      // That certificate as the only authority; partial, so a pinned certificate
      // that some office authority signed is accepted on its own.
      ca: pinned.toString(),
      allowPartialTrustChain: true,
      checkServerIdentity(host: string, cert: PeerCertificate) {
        // The very certificate saved, not merely one it signed, and valid for the host.
        if (!cert.raw?.equals(pinned.raw)) return Object.assign(new Error(NOT_PINNED), { code: "RELEVO_NOT_PINNED" });
        return checkServerIdentity(host, cert);
      },
    });
    agents.set(pem, found);
  }
  return found;
}

export interface Answer {
  status: number;
  ok: boolean;
  text: string;
}

/**
 * One request over https trusting `pem` alone. Like fetch: rejects with the
 * signal's reason once it aborts, whether waiting for the answer or reading it.
 */
export function pinnedRequest(
  url: string,
  init: { method: string; headers: Record<string, string>; body?: string; signal: AbortSignal },
  pem: string,
): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const { signal } = init;
    if (signal.aborted) return reject(signal.reason);
    const headers = { ...init.headers };
    if (init.body !== undefined) headers["Content-Length"] = String(Buffer.byteLength(init.body));
    const req = request(url, { method: init.method, headers, agent: agent(pem) });
    const abort = () => {
      req.destroy();
      reject(signal.reason);
    };
    signal.addEventListener("abort", abort, { once: true });
    const done = () => signal.removeEventListener("abort", abort);
    req.on("error", (err) => {
      done();
      reject(explained(err));
    });
    req.on("response", (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (chunk: Buffer) => chunks.push(chunk));
      res.on("end", () => {
        done();
        const status = res.statusCode ?? 0;
        resolve({ status, ok: status >= 200 && status < 300, text: Buffer.concat(chunks).toString("utf8") });
      });
      res.on("error", (err) => {
        done();
        reject(err);
      });
      res.on("close", () => {
        // Closed before the end: the connection was lost while reading.
        if (!res.complete) {
          done();
          reject(new Error("the connection to the Relevo server closed before the answer was complete"));
        }
      });
    });
    req.end(init.body);
  });
}
