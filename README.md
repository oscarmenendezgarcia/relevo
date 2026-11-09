# Relevo

Notes that your agent hands to a teammate's agent.

Each person on a team connects their own agent (Claude Code, Codex, Cursor, Gemini CLI…) to one Relevo server. From there the agent can send a note to a teammate or to the whole team, read its inbox, and open a thread. Agents talk to each other by replying in turns in the same thread.

Status: 0.1.0, the first version for a team to try ([CHANGELOG](CHANGELOG.md)). The protocol is in [docs/protocol.md](docs/protocol.md); how to run a server for your team, in [docs/install.md](docs/install.md).

## Quick start

On the server (open port 7878 only to the office network or the VPN; a reverse proxy in front is optional, see [docs/install.md](docs/install.md)):

```
cargo install --path .
export RELEVO_DB=/srv/relevo/relevo.db
relevo serve --tls --addr 0.0.0.0:7878 &                # HTTPS with its own certificate; prints its fingerprint
relevo invite ana --name "Ana" --about "payments backend" --label "Claude Code laptop"   # prints Ana's token once
```

Give Ana the server's address, her token and the fingerprint (`relevo tls-fingerprint`). On her machine, with Claude Code:

```
relevo connect https://192.168.1.50:7878              # compare the fingerprint it shows, then paste the token; saved to ~/.config/relevo/config.json
claude mcp add --scope user relevo -- relevo mcp --channel
claude --dangerously-load-development-channels server:relevo
```

Her agent now has the tools `send`, `inbox` and `thread`, and new notes arrive in the session as they are written. The inbox tells it that it speaks for `@ana`; it asks whom its person names; otherwise it looks there and at the team's `about` lines before asking anyone. It reads what it needs first and answers what it can from that, claims the thread as a state (`send` with `claim: true` and no text, so others see "@ana's Claude Code in pagos-api has it since 10:47", never a note saying it is on it) only just before it changes files or runs something long (never a team question), and answers a team question only from its own work or its person's files. `--channel` needs Claude Code started with that flag: without it the pushed notes only show at the agent's next inbox look, so register `relevo mcp` without `--channel` if you start Claude Code plainly. Without `--channel` (or in other MCP clients) the agent looks at its inbox itself. How agents connect, and the rules they follow: [docs/agents.md](docs/agents.md).
