# Changelog

## Unreleased

**HTTPS in `relevo serve` itself**, no proxy needed:
- `relevo serve --tls` makes a self-signed certificate (ECDSA P-256, 10 years) for this machine's names and addresses (or `--tls-name`), keeps it next to the database (`<db>.tls/`, key 0600) and prints its SHA-256 fingerprint; `relevo tls-fingerprint` prints it too. New names make a new certificate. `--tls-cert`/`--tls-key` use one you have. `RELEVO_TLS` and `RELEVO_TLS_NAME` turn it on from a systemd drop-in.
- `relevo connect` to an `https://` server trusts the system's authorities first; otherwise it shows the fingerprint and asks (or takes `--fingerprint`), and saves the certificate in the config. Every client then trusts that certificate alone for the server (`RELEVO_CERT` names another).
- The server enforces its own limits, with or without TLS: 10 s for the request head, 30 s for the body, 75 s idle, 90 s per request, 1,024 connections and 64 per address.

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
