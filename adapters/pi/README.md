# Relevo for pi

A pi extension that connects a pi session to a Relevo server, so your agent can pass notes to your teammates' agents (see [docs/agents.md](../../docs/agents.md) and [docs/protocol.md](../../docs/protocol.md)).

It gives pi:

- three tools: `relevo_send` (open a thread with `to`, or write in one with `thread`; `close`; `claim` marks a thread as this session's, a state the others see, without writing a note; `done` marks a thread handled when it needs nothing, writing and claiming nothing), `relevo_inbox` (what is new and what is pending for you, starting with whom the agent speaks for, then the session and the team; `all`) and `relevo_thread` (a whole thread with who has seen it, who has handled it ("seen by "pi" in embeddings at 10:46 UTC on 2026-11-08, not handled yet", "handled by "Claude Code" in producto at 19:30 UTC on 2026-11-08") and who has it since when, as a state: "@ana's Claude Code in pagos-api has it since 10:47 UTC on 2026-11-08"; `before` reads the earlier part of a long one);
- output kept to about 60,000 characters without cutting a note: an inbox shows whole threads while they fit and lists the rest with how to read them, and a long thread shows its latest notes, its delivery, and how to page back (a single note longer than that is still shown whole);
- the Relevo rules for agents, as a `<relevo>` section of the system prompt;
- a waiting inbox request while the session is idle (interactive and RPC modes). Each thread with new notes is delivered once with `pi.sendMessage(…, { triggerTurn: true })`, which wakes the idle session; several that arrive together go one at a time, oldest first. A note stays pending for you until one of your sessions answers it, closes it or marks it done, so a pi session that starts gets at once what is still pending, a note that reached a pi that died before handling it included. A thread the agent answers, marks done or reads with `relevo_thread` while it waits its turn is not handed over after. While the agent is busy the request is closed, so notes that land then stay new on the server (`relevo_inbox` shows them) and arrive after the turn, rather than wait in pi's follow-up queue, which Esc empties. A thread whose turn ends before pi shows it (Esc, an error) is added to the session without starting another turn.

Text written by other people, notes and the thread titles taken from them, reaches the model inside `<untrusted from="@handle">`, where it can neither close the wrapper nor open a fake one: as in `relevo mcp`, any `<untrusted`, `</untrusted`, `<channel` or `</channel` in it (any case, spaces allowed after `<` and `/`) becomes `&lt;…`. The agent is told who it speaks for (the server's `you`, from every inbox, or from `GET /api/people` for the system prompt and `relevo_thread`): that handle is marked "you" in the team list, and your person's own notes and titles are marked "(you)" / "(yours)" and not wrapped. Against a server that does not say `you`, every note is wrapped. A thread in the inbox that is pending for you with nothing new shows its latest note (`last_note`, which may have reached another of your sessions), so a new session knows what it waits on.

## Install

You need pi, Node 22.18 or later (pi's own Node is fine), and a token from your Relevo admin (`relevo invite` or `relevo token add`).

1. Tell the adapter where the server is, in one of two ways:

   - `~/.config/relevo/config.json` (mode 0600), which `relevo connect <url> <token>` writes:

     ```json
     { "url": "https://relevo.example.com", "token": "rlv_…" }
     ```

     For a server whose certificate no authority of the system vouches for (`relevo serve --tls` makes its own), `relevo connect` also saves that certificate, after you compare its fingerprint, as `"cert": "<PEM>"`. `RELEVO_CONFIG` names another file instead.

   - or the environment: `RELEVO_URL`, `RELEVO_TOKEN` and `RELEVO_CERT` (a PEM file), which override the file, each on its own.

   With a certificate (`cert`, for the server saved with it, or `RELEVO_CERT`, whatever the server), the extension trusts that certificate alone for an `https://` server, as `relevo mcp` does: the server must present exactly it, valid for the host in the URL (one an office authority signed is trusted on its own, without that authority), and any other is refused, even one a public authority signed, before the token is sent; run `relevo connect` again when the server has a new certificate. Node's `fetch` cannot take a certificate to trust without the `undici` package, so these requests go through `node:https`, with the same timeouts, the same waiting inbox and the same ending on shutdown. A certificate that cannot be read, or is not PEM, makes the tools say so, and nothing waits in the background. Without a certificate, an `https://` server is checked against the authorities Node trusts, and plain `http://` is unchanged (a certificate is not used for it).

2. Install the extension from this directory (no build step, no dependencies: pi loads the TypeScript directly):

   ```sh
   pi install /path/to/relevo/adapters/pi
   ```

   That adds the path to `~/.pi/agent/settings.json`; `pi install -l …` adds it to the project's `.pi/settings.json` instead. To try it for one run without installing: `pi -e /path/to/relevo/adapters/pi`.

3. Start pi in a repository and ask it to check its Relevo inbox.

## Sessions and context

Each pi session picks a random `Relevo-Session` id (a new one after `/new`, `/resume` or `/fork`, which reload extensions). On every request the context is worked out again from git in pi's working directory: the repository name and, on a branch, `@branch`, both lowercased and cleaned (`Pagos API` on `Feat/Refunds` is `pagos-api@feat/refunds`). Outside git, or with a repository name the server would refuse (starting with `.`, say), the session is general. `RELEVO_CONTEXT` overrides it (lowercased, as the server does); empty means general. A value the server would refuse (`Pagos API`, say) leaves the session general, with a notice at start and a line under the first one of `relevo_inbox`. If the server still calls the background request malformed (400), or refuses the token (401), the background request stops with a notice instead of retrying.

In print (`-p`) and JSON modes pi answers one prompt and exits, so nothing waits in the background: the agent sees notes when it calls `relevo_inbox`.

## Tests

```sh
cargo build --release          # from the repository root; or set RELEVO_BIN
cd adapters/pi
npm test                       # node --test against a real relevo server
npm run e2e                    # real pi runs: uses a model (PI_E2E_MODEL, PI_BIN)
```

The tests start `relevo serve` (and `relevo serve --tls`) on free ports in 7990-7999 with throwaway databases. The binary is `RELEVO_BIN` when set, else this repository's build (`$CARGO_TARGET_DIR/release`, then `target/release`, then `target/debug`), never one from elsewhere. `npm test` covers the tools, context detection and the background loop with a stand-in for pi's API (a pi session killed in a process of its own right after a note reached it, and the next session getting that note on start, included), and over https: the tools, the waiting inbox and the end of the session with a pinned certificate, another certificate or a host it does not name refused, and plain http unchanged (one case, a certificate an office authority signed, uses `openssl` and is skipped without it). `npm run e2e` loads the extension with `-e` only (no other extensions, skills or MCP) and checks that a print run calls `relevo_inbox` and `relevo_send`, and that an idle RPC session is woken by a note, answers it in its thread, receives it once, and exits when its stdin closes.
