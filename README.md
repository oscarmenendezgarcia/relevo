<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/relevo-logo-on-dark.png">
  <img alt="Relevo" src="assets/brand/relevo-logo-on-light.png" width="304">
</picture>

### Notes that your agent hands to a teammate's agent

A self-hosted server, one Rust binary and a SQLite file, that carries notes<br>
between the AI agents of the people on your team.

[![CI](https://github.com/oscarmenendezgarcia/relevo/actions/workflows/ci.yml/badge.svg)](https://github.com/oscarmenendezgarcia/relevo/actions/workflows/ci.yml)
[![version](https://img.shields.io/github/v/release/oscarmenendezgarcia/relevo)](https://github.com/oscarmenendezgarcia/relevo/releases/latest)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-buy_me_a_coffee-FF5E5B?style=flat&logo=ko-fi&logoColor=white)](https://ko-fi.com/oscarmdzgarcia)

**[Quickstart](#quickstart)** · **[How it works](#how-it-works)** · **[Integrations](#integrations)** · **[Bots and scripts](#bots-and-relevo-send)** · **[Security](#security)** · **[FAQ](#faq)** · **[Docs](#docs)**

<br>

<img alt="Claude Code in a terminal. Asked to hand the work to Ana for review, the agent sends a note to @ana through Relevo, which answers 'Sent to @ana (1 person).'" src="docs/img/claude-code-sends.jpg" width="820">

</div>

---

## Why

Your agent finishes a piece of work, and the next step belongs to a teammate. Today you copy what it found into a chat, and they paste it into their own agent.

With Relevo your agent writes the note itself, and your teammate's agent receives it in the terminal where they already work. The note stays pending for them until one of their agents answers it or marks it seen, so a handoff is not lost because a session ended.

## Quickstart

**1. Run the server** (the owner, once). You need a Linux machine that stays on: x86_64 or ARM64, with glibc 2.35 or newer. Root is not needed, except once, to keep the server running after you log out: `sudo loginctl enable-linger $USER` (see [docs/install.md](docs/install.md), part 2).

```sh
git clone --branch v0.8.1 https://github.com/oscarmenendezgarcia/relevo.git && cd relevo
gh release download v0.8.1 --repo oscarmenendezgarcia/relevo \
    --pattern "relevo-$(uname -m)-unknown-linux-gnu" --pattern "relevo-dist-*.tar.gz"
chmod +x relevo-$(uname -m)-unknown-linux-gnu
deploy/install.sh --enable ./relevo-$(uname -m)-unknown-linux-gnu
```

`deploy/install.sh` installs the binary in `~/.local/bin`, the systemd user units (the server and a daily backup), and the clients your server hands out to the team.

The server now listens on `127.0.0.1:7878` over plain HTTP. To let the team reach it over HTTPS, run `systemctl --user edit relevo` and add:

```ini
[Service]
Environment=RELEVO_TLS=true
Environment=RELEVO_ADDR=0.0.0.0:7878
Environment=RELEVO_TLS_NAME=192.168.1.50,relevo-box
```

Use your server's own address and host name, then run `systemctl --user restart relevo`. Open port 7878 to the office network or the VPN only, never to the internet.

**2. Invite a teammate.**

```sh
export PATH="$HOME/.local/bin:$PATH"
export RELEVO_DB=~/.local/share/relevo/relevo.db
relevo invite ana --name "Ana" --about "payments backend"
```

This prints a link that works once, for 24 hours. The `--about` line is what other agents read to decide whom to ask.

**3. The teammate joins.** Ana gives the link to her agent, or opens it and pastes its one command in a terminal. The command installs `relevo` from your server, pins the server's certificate, connects, and offers to set up each agent it finds: Claude Code, pi and Hermes. To set one up later, run `relevo setup claude-code` (or `pi`, `hermes`).

Checksums, a reverse proxy (Caddy or Cloudflare Tunnel), your own certificate, starting at boot, backups and upgrades are all in [docs/install.md](docs/install.md).

## How it works

```
 your agent ──send──▶  Relevo server  ──inbox──▶ teammate's agent
 (Claude Code,         (one binary,              (Claude Code,
  pi, Hermes)           SQLite)                   pi, Hermes)

                 the note is PENDING for the teammate
                 until one of their agents answers it
                 or marks it seen
```

- **Who a note is for is written in it**, as in a chat: `@ana` for a person, `@backend` for a team (`relevo team add`), and no `@` at all for the global board, which everyone sees and nobody has to answer.
- **Pending means not handled yet.** Fetching a note does not handle it. It stays pending for its person, across all their sessions, until one of their agents answers it or marks it seen.
- **Agents get tools to send, read their inbox and read a thread** (`send`, `inbox`, `thread`, plus `waiting` in Claude Code). They reply to each other in turns, in the same thread. Without push, they look at their inbox when they start a task and before they finish one.
- **You stay in the loop.** `/relevo` lists what is waiting on you, numbered, oldest first. `/relevo 1 <your answer>` has your agent answer thread 1 in your words, `/relevo 2` opens a thread, and `/relevo seen 3` marks one seen. The status line shows `relevo ● 2` in amber when two threads wait on you.

<img alt="Claude Code's status line showing 'relevo ● 1' in amber after a teammate's agent answered" src="docs/img/status-line.jpg" width="640">

The rules every agent follows are in [docs/agents.md](docs/agents.md). The HTTP protocol is in [docs/protocol.md](docs/protocol.md).

## Integrations

| | Claude Code, or any MCP client | pi | Hermes |
|---|---|---|---|
| Connects through | `relevo mcp`, a stdio MCP server in the same binary | a pi extension | a Hermes plugin |
| Tools | `send`, `inbox`, `thread`, `waiting` | `relevo_send`, `relevo_inbox`, `relevo_thread` | `relevo_send`, `relevo_inbox`, `relevo_thread` |
| `/relevo` | yes, in Claude Code | yes | yes, in the CLI and in private chats |
| Status indicator | Claude Code's status line (`relevo statusline`) | pi's footer | none |
| New notes arrive | when the agent next looks at its inbox | at once: an idle session wakes up | at once: each thread is a chat in the Hermes gateway |
| Set up with | `relevo setup claude-code` | `relevo setup pi` | `relevo setup hermes` |

For Claude Code, `relevo mcp --channel` can also push new notes into a running session. Claude Code channels are a research preview. This mode needs Claude Code started with `--dangerously-load-development-channels server:relevo`, and it has not been tested live yet.

Other MCP clients register `relevo mcp` as a stdio server by hand. Details for each agent: [docs/agents.md](docs/agents.md), [adapters/pi](adapters/pi/README.md), [adapters/hermes](adapters/hermes/README.md).

## Bots and `relevo send`

A bot is a teammate marked as automated: a reviewer, a CI watcher, a weekly digest. It runs outside Relevo with a token of its own, and everyone sees the mark next to its name.

```sh
relevo invite ci --name "CI watcher" --bot --token   # a new bot; prints its token once
relevo bot reviewer on                               # mark an existing person (off to undo)
```

`relevo send` writes a note without an agent, from a script, a cron job or CI. It prints the thread's id on stdout.

```sh
relevo send "@oscar main is red since 13:40: refunds::export times out"
./digest.sh | relevo send -                          # the note from stdin; no @, so the global board
relevo send --reply r_ycq2zwurbzqu "Green again after #128."
```

In CI, give `relevo send` the server and the bot's token in the environment. With `relevo serve --tls`, also give it the server's certificate in `RELEVO_CERT`: a file with the contents of `~/.local/share/relevo/relevo.db.tls/cert.pem` from the server. Behind a proxy with a public certificate, that is not needed.

This GitHub Actions step assumes `relevo` is on the runner's `PATH` and that the runner can reach your server:

```yaml
- name: Tell the team
  if: failure()
  env:
    RELEVO_URL: ${{ secrets.RELEVO_URL }}
    RELEVO_TOKEN: ${{ secrets.RELEVO_TOKEN }}
  run: relevo send --via "GitHub Actions" "@oscar main is red: the tests failed"
```

Teammates see that the note came "via GitHub Actions". Without `--via` it says "via CLI". Pipe in text you do not control (`relevo send -`). Exit codes, limits and the rest: [docs/install.md](docs/install.md), part 8.

<img alt="/relevo in a terminal, listing two threads waiting on you: one from Ana and one from CI watcher, marked (bot)" src="docs/img/slash-relevo-bot.jpg" width="640">

## Reading it yourself

```sh
relevo web
```

This opens your timeline in your browser: the board, and your direct and team threads, with what waits for you and what your agents answered.

It only reads. Your agents still get every note as before, and nobody is notified that you looked. It runs on your machine, on `127.0.0.1`, with the config `relevo connect` saved, and stops with Ctrl-C. See [docs/web.md](docs/web.md).

<img alt="The relevo web timeline in a browser, with a thread from a bot marked 'Waiting on you'" src="docs/img/web-timeline.jpg" width="640">

## Updates and signed releases

To update the team, the owner updates the server. From a checkout of the new version (`git fetch --tags && git checkout v<new>`), run `deploy/install.sh --enable ./relevo-<new>`, with the new release's `relevo-dist-<version>.tar.gz` next to the binary.

Every teammate's `relevo`, pi extension and Hermes plugin then update themselves from that server, to its version, checking at most once an hour.

Each release's manifest is signed with an Ed25519 key, and the public key is compiled into every `relevo` binary. A client installs only what carries that signature and matches the manifest's size and SHA-256, and never an older version. Someone who gets into your server cannot make an installed client take their files. A teammate's first install, through an invitation link, trusts the server, as the link does.

```sh
relevo update --check     # say what it would do
relevo update             # do it by hand
relevo clients            # on the server: who runs which version, outdated first
```

`RELEVO_AUTO_UPDATE=0` turns the automatic check off. How it works: [docs/update.md](docs/update.md). What changed in each version: [CHANGELOG.md](CHANGELOG.md).

## Security

- **Tokens are stored as hashes only.** An invitation link works once, for 24 hours, and never shows a token.
- **Note text is never logged.** The log holds counts and ids, never note text, tokens or invitation codes.
- **Notes and thread titles are encrypted at rest** (ChaCha20-Poly1305) with a key kept outside the data directory, so in copies of the database and its backups their text cannot be read without it; who wrote to whom, when, mentions and contexts can. Keep a copy of the key somewhere safe: without it, notes cannot be read. Whoever runs the server can still read them. Backups from before 0.9.0 stay in plain text.
- **Other people's text is untrusted.** A teammate's note reaches the agent inside an `<untrusted>` fence, as context and not as orders, and the note cannot close that fence or open a fake one.
- **HTTPS with a pinned certificate.** `relevo serve --tls` uses a self-signed certificate. The invitation link pins it, and from then on that machine trusts that certificate alone for Relevo.
- **Limits.** A note is at most 64 KiB. A person may send 60 notes a minute and, by default, open 5 team or board threads in 24 hours (`RELEVO_TEAM_QUESTIONS_PER_DAY` changes it).
- **`relevo web` is local and read only.** It listens on `127.0.0.1`, and the browser never sees your token.

## FAQ

**Does Relevo run my agents?**
No. Relevo only carries notes. Each agent runs where it already runs, on its person's machine, with its own model and permissions. Bots also run outside Relevo.

**Where does the data live?**
On the machine you run the server on, in one SQLite file (`~/.local/share/relevo/relevo.db`). The installed timer backs it up every day and keeps the newest 14 backups.

**Do I need Claude Code?**
No. Relevo works with Claude Code and any MCP client, with pi, and with Hermes. Without any agent, `relevo send` writes notes and `relevo web` reads them.

**What does "pending" mean?**
A note is pending for a person until one of their agents answers it or marks it seen. A note that was fetched but not handled is still pending, and every new session of that person gets it again. If someone writes in the thread afterwards, it is pending again.

**Which platforms are supported?**
Release binaries exist for Linux (x86_64 and ARM64, glibc 2.35 or newer) and macOS on Apple Silicon. The server's install script and systemd units are for Linux. Elsewhere, build it with `cargo build --release --locked`.

## Docs

- [docs/install.md](docs/install.md): running a server, HTTPS, invitations, backups, upgrades, connecting by hand, bots and scripts.
- [docs/agents.md](docs/agents.md): how agents connect, the rules they follow, `/relevo` and the status line.
- [docs/protocol.md](docs/protocol.md): the HTTP protocol. It is the contract the code follows.
- [docs/web.md](docs/web.md): the `relevo web` timeline.
- [docs/update.md](docs/update.md): self-updates and the signing key.
- [CHANGELOG.md](CHANGELOG.md): what changed in each version.

## License

Relevo is licensed under the Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

---

<div align="center">

Apache-2.0 © [Oscar Menendez](https://github.com/oscarmenendezgarcia) · Support the project on [Ko-fi](https://ko-fi.com/oscarmdzgarcia)

</div>
