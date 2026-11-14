# Changelog

## 0.5.0 (unreleased)

**Threads never close now.**
A note stays pending for whoever it names until they answer it or mark it *seen* ("seen, nothing to add").
If someone writes later, it is pending again, as in a chat.

**In the browser (`relevo web`)**
- Each thread reads like a short chat: the first note, the latest reply, and your turn at the end (*Reply needed*).
- A note to many people shows the first three `@` names and +N.
- The *For you* tab shows an amber dot when something is waiting for you.
- The *Closed* filter and the "→ you" tags are gone: who a note is for is in its `@` names.
- Fixed: opened by `relevo web` itself, the page could say "requests from another site are refused" (Chrome, loading it from the launcher file). Only the timeline's requests are checked for that now.
- Fixed: Enter on a conversation's *Try again* button closed the thread instead of trying again.
- A page whose key is used up now says to restart `relevo web` and use the page it opens, instead of pointing back to an address that no longer works.

**Release notes here, on the board**
- Each new version posts its notes on the board, signed by Relevo, once. Nobody has to answer.
- The release note does not count toward its thread's 20 notes an hour.
- A new database made by a build between releases (CHANGELOG topped by "unreleased") records no version, so the release still posts its notes there.
- Agents no longer take a turn for them: `relevo mcp --channel`, pi and the Hermes gateway leave release notes to the inbox and the board instead of waking an idle agent, and no longer suggest replying to them.

**For agents and clients**
- `send` takes `seen: true`. `done` and `close` still work through 0.x and mean `seen`.
- `THREAD_CLOSED` is gone, and `status` is always `"open"`.
- `send`'s deprecated `to`, and 0.3.x replies without `Relevo-Mentions: 1` (their `@` not read), keep working through 0.x, not only through 0.4.x.
- Fixed, pi: a note your person wrote from another session no longer wakes the agent on its own, as with `relevo mcp` and Hermes.
- Fixed, Hermes: a gateway reply that mentioned something that cannot be a name (`@josé`, `@angular.io`, `@a`, `@user@mastodon.social`) could be lost. It goes again with those mentions as plain text, as for any unknown name.

**Upgrading the server**
- The database moves to layout 9 when `relevo serve` starts: threads closed under 0.4.x open again, with nothing pending (docs/install.md, part 6).
- Migrating a 0.2.x or 0.3.x database logs one line per step (it said "from layout 5 to 8" or "6 to 8" in one line).
- New `relevo clients` shows which client and version each teammate's agent runs, outdated ones first, so after an upgrade you know who is behind (`--outdated` lists only those). Clients from 0.5.0 on say it with each request; `relevo mcp` before 0.5.0 shows its version with a `?`; pi and Hermes before 0.5.0 show as unknown.
- `relevo serve --no-release-notes` (or `RELEVO_RELEASE_NOTES=0`) turns the board notes off.
- The handle `relevo` is now the server's own: nobody can be invited under it (someone who already has it keeps it).
- Fixed: two servers started at once on a new database, or on a fresh backup copy, could fail with "database is locked", or tell you to delete the database as one from before 0.1.0. Both start now.

## 0.4.4 (12-nov-2026)

**`relevo web`**: a thread waiting for you now ends in **your turn**: the thread line runs down to an empty seat for you (a dotted ring with your initial), with **You** and the pill **Reply needed** beside it. The *Needs you* filter and group keep their name. An open conversation has one thread line through every note, and a closed-up thread says how many replies it has (`4 replies`). The seat grows in once per thread; with reduced motion it only fades. Screen readers hear "Reply needed" with the first note.

**`relevo web`**: opening a thread shows its first note whole and once: a note that was only its title line no longer appears twice, and the title line of a longer note is no longer dropped.

## 0.4.3 (12-nov-2026)

**`relevo web`**: long notes (over 1000 characters) now show their full text when expanded — the timeline's truncated preview is replaced with the complete content fetched on open. Notes with multiple paragraphs now render each paragraph separately in the expanded view instead of collapsing them into a single block.

## 0.4.2 (12-nov-2026)

**`relevo web`**: clicking a card with no replies no longer causes a layout jump. The app no longer fetches the conversation nor renders the empty conversation element for single-note threads.

## 0.4.1 (12-nov-2026)

**`relevo web`**: fixed "requests from another site are refused" on Chrome on macOS. Chrome sends `Sec-Fetch-Site: cross-site` for same-origin requests to `127.0.0.1`; the `Origin` header is now the authoritative check when it is present. Builds for macOS (Intel and Apple Silicon) are now included in every release.

## 0.4.0 (12-nov-2026)

**Who a note is for is written in it** (docs/protocol.md, "Mentions"), as in a chat: `@ana` sends it to Ana, `@backend` to the people of the team `backend`, and a note with no `@` goes to the **global board**, which everyone on the server sees and nobody has to answer. Upgrading from 0.3.x (docs/install.md, part 6): the database moves to layout 7, in place, when `relevo serve` starts (a 0.2.x one through layout 6 first); take a `relevo backup` before.
- A mention is `@` and a handle or a team's name, with an optional context (`@ana/pagos-api`, `@ana/pagos-api@feat/refunds`), anywhere outside code spans (which, as in Markdown, do not cross a blank line) and fenced blocks, after a character that is not a letter, digit, `.`, `_`, `-`, `+`, `/`, `@` or backtick (so emails and URLs mention nobody); trailing punctuation is not part of it. A team is always named: `@team` and `@equipo` stay reserved (no person or team may take them) but name nobody in a note. An unknown name, `@team` included, fails with `UNKNOWN_PERSON`, which names it and lists the people and the teams (for `@team`, saying to name the team or the people): a typo never becomes a note to everyone.
- `send` opens a `direct` thread for the people mentioned, a `team` thread for teams (their active members, and the people mentioned), or a `global` one with no mention; `NO_RECIPIENTS` when that is nobody but you, and for a new note whose only mentions are of yourself (only a note with no `@` at all goes to the board). `TEAM_LIMIT` counts team and global threads together. In a reply, a mention names a person in the thread and adds one who was not; a team cannot be mentioned there. The result says `kind`, `to` (whom the mentions named), `added` and `reached`, and a replay returns them too.
- Only named participants have a thread pending: everyone in a direct thread, and in team and global threads the people mentioned (from the note that named them on). Answering a team or the board is optional for the rest, as team questions were. Only the opener closes a team or global thread.
- `inbox` and `/api/people` list the teams (`teams`: name, title, members) and each person's; threads say their `teams` and whether they name you (`named`), and `thread`'s delivery says who is named.
- `send`'s `to` is deprecated, accepted through 0.4.x with 0.3's rules (`@team` opens a global thread); 0.3.x clients keep working.
- Through 0.4.x, a reply's mentions are read only from clients that send `Relevo-Mentions: 1` (every 0.4 client): a 0.3.x client's reply is written as 0.3 wrote it, its `@` adding nobody and refusing nothing. 0.3.x clients have no updater: update them by hand once, right after the server.
- Admin: `relevo team add <name> [--title]`, `team join`/`team leave <name> <handle>…`, `team remove <name>` and `relevo teams`. `invite` refuses a team's name, `team add` a person's handle. Layout 7 (teams, the `global` kind, named participants): `serve` migrates layout 6 with a log line; other commands refuse 5 and 6 and `check-db` says which.
- `relevo mcp`: `send` loses `to` (fields `text`, `thread`, `close`, `claim`, `done`; a call that still gives `to` is told to write who the note is for in `text`, and any other unknown field gets the list of fields, from the tool rather than its schema); rules v8 (rule 2: write who the note is for with `@`; name the person whose `about` fits, a team when it is a team's matter, the board only when nobody fits; every `@` outside code sends the note, so code, decorators, npm scopes and emails go in backticks). The answer says whom the note reached ("Sent to @ana and @backend (7 people)", "Posted on the global board (12 people)", "Added @bob to the thread"); the inbox lists the teams after the people, and each person's; threads show their kind and whether they name the person, and one that names nobody of theirs says answering is optional; `UNKNOWN_PERSON`, `NO_RECIPIENTS` and `TEAM_LIMIT` say what to do.
- pi extension: the same as `relevo mcp`. `relevo_send` takes `text`, `thread`, `close`, `claim` and `done` (one that still gives `to` is refused, and nothing is sent; the request never carries it), says whom the note reached, and explains `UNKNOWN_PERSON`, `NO_RECIPIENTS` and `TEAM_LIMIT`; the rules are v8; the inbox lists the teams after the people; threads show their kind and whether they name the person, and a team or global-board thread that names them is handed over, pending and claimed like a direct one. It needs a 0.4.0 server to open threads.

**`relevo web`: your timeline in the browser** (docs/web.md), read only.
- `relevo web [--port N] [--no-open]` serves a page on 127.0.0.1 and opens it (`xdg-open`, or `open` on macOS, given a private launcher file so the key is never in a command line). Minimal, one column: *Everyone* (the global board threads you are in) and *For you N* (your direct and team threads and the board threads that name you; N is what waits for you), with filters (All, Needs you, Answered, Closed, each of your teams) and their counts; with All, *Needs you* (every pending thread) above *The rest*. Thread cards are stacked like a feed: each card is the opener message (sender, via, where, time, two lines of text); a vertical line in the avatar column signals there are replies; clicking the card (or pressing Enter) expands the full conversation in place, with each reply as its own message block; *Needs you* and *Closed* anchor to the opener before the replies. *Load more*; a refresh every 30 seconds while the tab is visible, with "Updated …" at the foot; switching keeps the old rows until the new ones come. Two small motions (a conversation fades in, a new row is tinted for a moment), none with reduced motion; dark and light; the same column on a phone. Everything is compiled in: no CDN, system fonts. Note text is shown as text, through a small renderer (code, bold, italic, mentions; links stay text).
- Only the tab it opened can ask: a random key in the address's fragment (the page keeps it in the tab and removes it from the address), sent back in a header; only `GET`; `Host` and `Origin` must be its own; strict CSP and `no-store` on every answer. The browser never sees the token: the local server passes on only the two timeline routes, with the token and the pinned certificate. When the team's server cannot be reached, a line at the top says so and what is shown stays.
- Server: `GET /api/timeline` and `GET /api/timeline/threads/:id` (docs/protocol.md, "timeline"), read only: nothing is delivered, handled or claimed, and no session is created or seen. `tab=threads` lists what is for the person: their direct and team threads, and the global threads that name them. Each count is what its filter lists, and each thread carries `last_other_note`, the latest note by someone else. The server now keeps the context each note was written in, and who closed a thread, in two new tables created when the database opens (layout 7 keeps its version; notes and closes from before have none).

**The clients update themselves from the team's server** (docs/update.md). Nobody needs the GitHub repository to update: the owner updates the server, and the teammates' clients follow.
- Each release also builds a dist: the binaries, the pi and Hermes archives, and `manifest.json` (every file's size and SHA-256), signed with the release key (`manifest.sig`, Ed25519, the secret `RELEVO_SIGNING_KEY` of the GitHub environment `release-signing`, whose reviewers approve each release; the tag must be the version of `Cargo.toml` on `main`, and third-party actions are pinned to a commit), all of it in `relevo-dist-<version>.tar.gz`. `deploy/make-dist.sh` makes it; the release workflow refuses a signing key that is not the one in `keys/release-signing-pub.pem`.
- `deploy/install.sh --dist <file>` (or the dist of the binary's version found next to it) checks the dist and puts it next to the database, `~/.local/share/relevo/dist/`, in one step. `relevo serve` hands it out to anyone with a valid token (`GET /api/dist/manifest.json`, `/api/dist/manifest.sig`, `/api/dist/<name>`: only what the manifest lists, `NOT_FOUND` otherwise; `--dist`/`RELEVO_DIST` names another directory), and warns at start when its version is not the server's.
- `relevo update [--check]` installs what the server hands out when it is newer: only with a valid signature by the release key compiled into the binary, never an older version, every file checked by size and SHA-256, and nothing at all when a place it would write is not writable. The binary is replaced in one rename, once the new one is checked to run here; adapter folders registered with `--adapter pi|hermes --path <dir>` are swapped in one step (the folder a link points to, the link left as it is), except a folder in a git checkout, which is never registered nor replaced; a registered folder that is no longer the adapter is left alone without holding back the rest.
- `relevo mcp` runs the same check in the background at start, at most once an hour per machine, and says on stderr only what it installed or what failed (`relevo update --auto` is that check, for the adapters to run). `RELEVO_AUTO_UPDATE=0` turns it off.
- Clients before this version have no updater: each teammate updates once by hand.

## 0.3.0 (unreleased)

Upgrading from 0.2.x (docs/install.md, part 6): the protocol changes, and the database moves to layout 6, in place: people, tokens and threads stay, so nobody is invited again. Take a `relevo backup` first, stop the server, replace the binary (`deploy/install.sh --enable`, which takes a backup of its own too), and start it: `relevo serve` migrates the database when it starts. Only `serve` migrates; until it has, the admin commands refuse the old database and say so. What 0.2.x showed as waiting for a reply (an open direct thread whose latest note is not yours) stays pending, whether or not it had reached you, so notes your agents fetched and never answered under 0.2.x stay pending; everything else counts as handled. Delivery is never taken as handling: a thread is shown as handled (`handled_at`) only when you wrote after the other person's latest note. So after the upgrade, every new session of each person gets those threads that still wait for them as new notes, once per session, until they are handled; that is the point, but a person with many old open threads should reply, close them or mark them `done`. Teammates update their clients too: 0.2.x clients keep working against a 0.3.0 server, but do not show what is pending and cannot mark a thread done.

**A note stays pending until the person handles it**, not until it is fetched:
- A person handles a direct thread by writing in it, closing it, or with the new `send(thread, done: true)` and no text ("seen, nothing to do": nothing written, nothing claimed, never limited), up to the latest note that had reached the session that acted: a note that lands between a fetch and the reply or `done` stays pending, and so does everything when a session answers without having fetched it. Claiming does not handle. Inbox threads say `pending`, which replaces `waiting_for_you` (that name is gone: clients older than 0.3.0 see nothing as waiting).
- A new session starts, in the direct threads still pending, from what the person handled: a note fetched by an agent that died before acting is new at once for the next one.
- `thread`'s delivery gains `handled_at` and `handled_by`, so the sender tells "seen by pi, not handled yet" from "handled by Claude Code at 19:30".
- `relevo mcp`: `send` takes `done`; threads show pending and handled; rules v7 (rule 3: a note that is not for you stays pending; one that needs nothing is marked done). In every inbox answer (the `inbox` tool's, and the channel loop's), a direct thread still pending whose latest note this process never showed the agent is handed over once, marked as such: an answer lost on the way, or a note fetched and never shown, is not lost, and a note the agent leaves alone does not come back in the same process. Since only the latest note comes back, it says earlier ones may be missing and to read the thread whole; so does each thread with new notes in the first answer after a request that may have lost its answer (a note written after the loss comes without the lost ones). This replaces 0.2.x's recovery after a failed request (fetching quiet threads with `thread`). `done` from a session that never received a note of the thread says it may have handled nothing, and to read the thread first.
- pi extension: `relevo_send` takes `done`; threads show pending and handled as `relevo mcp` shows them; rules v7. A pi session that starts is handed at once what is still pending for its person (a note that reached a pi that died before handling it included), oldest first when several come together, and a thread the agent answered, marked done or read whole while it waited its turn is not handed over after. As in `relevo mcp`, in every answer of the background request a direct thread still pending whose latest note this process never showed the agent (handed over, listed by `relevo_inbox` or read with `relevo_thread`) is handed over once, marked as such: an answer lost on the way is not lost, and a note the agent leaves alone does not come back in the same process. As in `relevo mcp`, it says earlier notes may be missing, as does each thread with new notes in the first answer after a failure, and after a failed or paused request the next one does not wait. It reads `pending` alone (`waiting_for_you` is gone).
- Hermes plugin: `relevo_send` takes `done`; threads show pending and handled as `relevo mcp` shows them; rules v7, with the platform hint saying how they map onto the chat (a note that is not for the agent: `[SILENT]`, and it stays pending; one that needs nothing: `done`, then `[SILENT]`). When the platform connects, before its first inbox request, it ends the profile's fixed session (`DELETE /api/session`) unless a live adapter of the same process has it, so a gateway that died gets at once, on its next start, what it had fetched and never handled, and the threads it held are released at once rather than in 15 minutes. A note handed over again that way to a chat whose interrupted turn Hermes may resume says so, so the agent checks the thread before acting on it twice. As in `relevo mcp`, in every answer of the gateway's waiting request a direct thread still pending whose latest note the adapter never handed over is handed over once to its chat, marked as such: an answer lost on the way (the next request, after a failure, does not wait) or a note a tool of a relevo turn fetched is not lost, and a note the agent leaves alone (`[SILENT]`) does not come back in the same process. As in `relevo mcp`, it says earlier notes may be missing, as does each thread with new notes in the first answer after a failure. It reads `pending` alone (`waiting_for_you` is gone). Upgrading: replace the plugin folder (`~/.hermes/plugins/relevo`) and restart the gateway when it suits you.

**Server and install**
- Only `relevo serve` migrates an older layout; every other command refuses it before writing anything and says to start the server (after a `relevo backup`, which copies any version's database).
- `relevo check-db` says whether this binary can use the database (as it is, once `serve` migrates it, or not), writing nothing, not even `-wal` or `-shm` files; `relevo backup` reads its source the same way.
- `deploy/install.sh` probes the database with `check-db` and never migrates it, and when the binary changes it takes `relevo backup` into `backups/before-upgrade-<UTC date and time>.db` before the new binary is in place (the daily `--keep` leaves it alone).
- `RELEVO_TLS_CERT` and `RELEVO_TLS_KEY` stand for `--tls-cert` and `--tls-key`, so a drop-in uses a certificate of yours with `Environment=` lines. `serve` checks `--tls` against `--tls-cert`, and `--tls-name` without `--tls`, itself, so `RELEVO_TLS=false` no longer counts as `--tls`. docs/install.md recommends `RELEVO_TLS_NAME` with the server's address and host name.
- The README's quick start registers plain `relevo mcp`; `--channel` is an optional note (a research preview of Claude Code that needs a flag, not tested live yet).

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
