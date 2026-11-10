# Changelog

## 0.3.0 (unreleased)

Upgrading from 0.2.x: the protocol changes, and the database moves to layout 6. The server migrates a 0.1.0 to 0.2.x database in place the first time it opens it (take a `relevo backup` first). What 0.2.x showed as waiting for a reply (an open direct thread whose latest note is not yours) stays pending, whether or not it had reached you; everything else counts as handled. Delivery is never taken as handling: a thread is shown as handled (`handled_at`) only when you wrote after the other person's latest note. So after the upgrade, each person's first new session gets once, as new notes, those threads that still wait for them, including ones their agents had fetched and never answered; that is the point, but a person with many old open threads should reply, close them or mark them `done`.

**A note stays pending until the person handles it**, not until it is fetched:
- A person handles a direct thread by writing in it, closing it, or with the new `send(thread, done: true)` and no text ("seen, nothing to do": nothing written, nothing claimed, never limited), up to the latest note that had reached the session that acted: a note that lands between a fetch and the reply or `done` stays pending, and so does everything when a session answers without having fetched it. Claiming does not handle. Inbox threads say `pending`, which replaces `waiting_for_you` (that name is gone: clients older than 0.3.0 see nothing as waiting).
- A new session starts, in the direct threads still pending, from what the person handled: a note fetched by an agent that died before acting is new at once for the next one.
- `thread`'s delivery gains `handled_at` and `handled_by`, so the sender tells "seen by pi, not handled yet" from "handled by Claude Code at 19:30".
- `relevo mcp`: `send` takes `done`; threads show pending and handled; rules v7 (rule 3: a note that is not for you stays pending; one that needs nothing is marked done).
- pi extension: `relevo_send` takes `done`; threads show pending and handled as `relevo mcp` shows them; rules v7. A pi session that starts is handed at once what is still pending for its person (a note that reached a pi that died before handling it included), oldest first when several come together, and a thread the agent answered, marked done or read whole while it waited its turn is not handed over after.
- Hermes plugin: `relevo_send` takes `done`; threads show pending and handled as `relevo mcp` shows them; rules v7, with the platform hint saying how they map onto the chat (a note that is not for the agent: `[SILENT]`, and it stays pending; one that needs nothing: `done`, then `[SILENT]`). When the platform connects, before its first inbox request, it ends the profile's fixed session (`DELETE /api/session`) unless a live adapter of the same process has it, so a gateway that died gets at once, on its next start, what it had fetched and never handled, and the threads it held are released at once rather than in 15 minutes. A note handed over again that way to a chat whose interrupted turn Hermes may resume says so, so the agent checks the thread before acting on it twice. Upgrading: replace the plugin folder (`~/.hermes/plugins/relevo`) and restart the gateway when it suits you.

## 0.2.1

- **Hermes plugin:** the pinned certificate is trusted alone even though Hermes injects truststore into its process. Before, the plugin's context was truststore's: on macOS the platform verifier refused the 10-year certificate of `relevo serve --tls` ("not standards compliant"), and on every platform the comparison with the pinned certificate was skipped, so the platform's authorities decided instead. Upgrading: replace the plugin folder (`~/.hermes/plugins/relevo`) and restart the gateway. The server, the database and the other clients are unchanged.

## 0.2.0

Upgrading from 0.1.0: replace the binary; the database is unchanged. To turn HTTPS on, see docs/install.md part 3.

**HTTPS in `relevo serve` itself**, no proxy needed:
- `relevo serve --tls` makes a self-signed certificate (ECDSA P-256, 10 years) for this machine's names and addresses (or `--tls-name`), keeps it next to the database (`<db>.tls/`, key 0600) and prints its SHA-256 fingerprint; `relevo tls-fingerprint` prints it too. New names make a new certificate. `--tls-cert`/`--tls-key` use one you have. `RELEVO_TLS` and `RELEVO_TLS_NAME` turn it on from a systemd drop-in. Without `--tls-name`, it warns when the certificate has none of the machine's network addresses (the systemd unit allows `AF_NETLINK`, which reading them needs).
- `relevo connect` to an `https://` server trusts the system's authorities first; otherwise it shows the fingerprint and asks (or takes `--fingerprint`), and saves the certificate in the config. Every client then trusts that certificate alone for the server (`RELEVO_CERT` names another), within its validity dates.
- The server enforces its own limits, with or without TLS: 10 s and 16 KiB for the request head, 30 s for the body, 75 s idle, 90 s per request, 1,024 connections and 64 per address (loopback, where a proxy's connections come from, not counted).

## 0.1.0

The first version for a team to try.

**Server** (`relevo serve`): one per team, SQLite, plain HTTP behind a TLS proxy.
- Notes in threads: `send`, `inbox`, `thread`; `@team` (or `@equipo`) asks everyone, and only those who know answer.
- Sessions: each running agent is a session with its context (`repo@branch`); a note can go to a person, to `person/repo` or `person/repo@branch`.
- Claims as a state: a session takes a thread with `send(thread, claim: true)` (no note) or by answering; it never steals one from another session of the same person; a claim lapses after 15 minutes unseen, ends with the session (`DELETE /api/session`) and with the thread's close.
- Delivery per session; the inbox shows what is new, what waits for you, the latest note, and who else holds each thread; `inbox?wait=` holds the request until something arrives.
- Limits: 5 team questions a day per person, 20 notes an hour per thread, 60 notes a minute per person.
- Admin CLI: `invite`, `token add/list/revoke`, `people`, `about`, `deactivate`, `sessions`, `backup`.

**Agents**
- `relevo mcp`: MCP server for Claude Code and any MCP client; `--channel` pushes notes into Claude Code sessions (research preview, needs `--dangerously-load-development-channels`).
- `adapters/pi`: pi extension; wakes the agent when a note arrives.
- `adapters/hermes`: Hermes platform plugin; each thread is a chat.
- All three give the agent the same rules (docs/agents.md) and mark other people's text as untrusted.

**Install**: `deploy/install.sh`, systemd user units with a daily backup, Caddy and Cloudflare Tunnel examples; see docs/install.md.

**Upgrading**: there is nothing to upgrade from. Databases created by builds before 0.1.0 (schema versions below 5) are refused: start with a new database and invite the team again.
