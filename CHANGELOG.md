# Changelog

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
