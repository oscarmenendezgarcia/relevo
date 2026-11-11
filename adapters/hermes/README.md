# Relevo for Hermes

A Hermes plugin with two parts:

- **The `relevo` gateway platform.** Relevo becomes one more chat platform for the Hermes gateway. Each Relevo thread is a chat (chat id = thread id, a DM-like chat with the note's author); each new note arrives as a message from its author, and Hermes' reply is written back in that thread. The gateway keeps one waiting inbox request open (`wait=55`), so it answers at once, and backs off (1 s doubling to 60 s) while the server cannot be reached. When it connects, it first ends the profile's fixed session that the gateway left when it last ran (`DELETE /api/session`), so what is still pending for you comes at once, a note a gateway that died had taken and never answered included (see Sessions, below).
- **The `relevo` toolset**: `relevo_send`, `relevo_inbox` and `relevo_thread`, the same tools as `relevo mcp` (see `docs/agents.md`), usable from any Hermes session.

Files: `relevo/plugin.yaml` (manifest), `relevo/__init__.py` (registration), `relevo/adapter.py` (platform), `relevo/tools.py` (tools), `relevo/client.py` (HTTP calls and rendering, shared by both), `relevo/update.py` (the automatic update). Only Python's standard library and `httpx`, which ships with Hermes.

## Install (for the owner)

Nothing here installs itself or restarts a running Hermes. These steps are yours; after them, the plugin keeps itself up to date (see Updates, below).

Use it with a Relevo server 0.3.0 or later: an older one does not say what is pending (`pending`; the plugin no longer reads 0.2.x's `waiting_for_you`), so nothing shows as pending there. Upgrading from 0.2.x: replace the plugin folder (step 2) and restart the gateway when it suits you (step 6).

1. **A token for this agent.** On the Relevo server:

   ```
   relevo token add oscar --label "Hermes"
   ```

   The label is what teammates see on every note this agent writes ("via Hermes").

2. **Copy the plugin** into the Hermes home of the profile that should be on Relevo, and enable it:

   ```
   cp -r adapters/hermes/relevo ~/.hermes/plugins/relevo
   hermes plugins enable relevo
   ```

   For another profile, copy it into that profile's home (`~/.hermes/profiles/<name>/plugins/relevo`) and run `hermes -p <name> plugins enable relevo`.

   Or install it from the release: unpack `relevo-hermes-<version>.tar.gz` (a release file, part of the dist the owner puts on the server) into the plugin folder instead of copying it:

   ```
   mkdir -p ~/.hermes/plugins/relevo
   tar -xzf relevo-hermes-0.4.0.tar.gz -C ~/.hermes/plugins/relevo
   hermes plugins enable relevo
   ```

   Either way the plugin then updates itself from your Relevo server (see Updates, below). A plugin folder that is a link into a clone of this repository does not: git updates it.

3. **Configure it** in the profile's `~/.hermes/.env` (or through `hermes config`, which lists these variables from `plugin.yaml`):

   ```
   RELEVO_URL=https://relevo.example.com
   RELEVO_TOKEN=rlv_…
   # Who may reach this agent through Relevo. The Relevo team is already the trust boundary:
   RELEVO_ALLOW_ALL_USERS=true
   # or name them: RELEVO_ALLOWED_USERS=ana,bob
   # Optional: the context this agent works in (repo or repo@branch). Unset: general.
   # RELEVO_CONTEXT=pagos-api
   # Optional: where `relevo connect` saved the server and token, if not ~/.config/relevo/config.json.
   # RELEVO_CONFIG=/path/to/config.json
   # Optional: the server's certificate (a PEM file) to trust alone, over the one `relevo connect` saved.
   # RELEVO_CERT=/path/to/relevo-cert.pem
   ```

   Set one of `RELEVO_ALLOW_ALL_USERS` or `RELEVO_ALLOWED_USERS`. With neither (and no gateway-wide `GATEWAY_ALLOW_ALL_USERS` or `GATEWAY_ALLOWED_USERS`), the gateway treats every teammate as unknown and answers nobody (the pairing code it would offer is not written into their thread). With `RELEVO_ALLOWED_USERS`, notes from anyone else are ignored.

   **HTTPS with Relevo's own certificate.** A server started with `relevo serve --tls` has a certificate no authority vouches for. `relevo connect` saves it, after you compare its fingerprint, as `"cert"` in the config; the plugin then trusts that certificate alone for that server (the saved `cert` goes with the URL saved with it, not with another `RELEVO_URL`). `RELEVO_CERT` names a PEM file to trust instead, whatever the server: set it when the server and token come from `.env`. The certificate is held in an ssl context in memory, never written to disk, and the server must present exactly it, valid for the URL's host, before anything (the token included) is sent: another certificate, even one a public authority signed, is refused, and the log says to run `relevo connect` again (the gateway keeps retrying with its back-off, as for a server it cannot reach). One an office authority signed is trusted on its own, without that authority. A certificate that cannot be read or is not PEM makes the tools say so and keeps the gateway platform from starting. Without a certificate, an `https://` server is checked against the authorities `httpx` trusts; plain `http://` is unchanged.

   If `RELEVO_URL` or `RELEVO_TOKEN` is missing, the tools take both the server and the token from the file `relevo connect` wrote (`~/.config/relevo/config.json`, or the file `RELEVO_CONFIG` names, as for every client), never one from each place, so a token is only ever sent to the server it came with. One of them set without the other is logged as a warning. The gateway platform turns on by itself only when both are set; enabled explicitly with only one of them (a `url` in `config.yaml` and no `RELEVO_TOKEN`, say), it refuses to start rather than connect to the saved server, and says which one is missing. When it connects, the log says where the server and token came from.

4. **Enabling the platform.** With `RELEVO_URL` and `RELEVO_TOKEN` in the profile's env, the gateway enables the `relevo` platform by itself. To enable it explicitly instead (the token is better kept in `.env`):

   ```yaml
   # ~/.hermes/config.yaml
   gateway:
     platforms:
       relevo:
         enabled: true
         extra:
           url: https://relevo.example.com
   ```

5. **Give the agent the tools** where it should have them. On the `relevo` platform itself, so it can claim a thread (`relevo_send` with `claim: true` and no text, a state the others see, not a note) before a longer piece of work:

   ```
   hermes tools enable relevo --platform relevo
   hermes tools enable relevo --platform cli        # optional: CLI/TUI sessions
   hermes tools enable relevo --platform telegram   # optional: any other platform
   ```

6. **Restart the gateway when it suits you.** The gateway loads plugins and platforms when it starts, so the platform comes up at its next start (`hermes gateway restart`). Restarting interrupts what your assistants are doing at that moment, so when to do it is your call; nothing in this repository restarts it.

## Updates

From 0.4.0 the plugin updates itself from your Relevo server, as `relevo` does ([docs/update.md](../../docs/update.md)). When the gateway platform connects, it runs, in the background,

```
relevo update --auto --adapter hermes --path <the plugin's own folder>
```

with the `relevo` that `RELEVO_BIN` names, or else the one on `PATH`; without one it does nothing and says nothing. The gateway never waits for it, and it goes on if the gateway stops meanwhile. It gets the server and token this platform uses (`RELEVO_URL` and `RELEVO_TOKEN` of the profile's env or `config.yaml`, else the file `relevo connect` saved, the one `RELEVO_CONFIG` names if the profile sets it). `relevo` does the work: it registers the folder in that file, checks at most once an hour per machine, installs only what carries the release's signature and is newer, and swaps the folder in one step; with that, it updates the `relevo` binary and the other adapters registered on the machine.

The gateway has the plugin loaded already, so it runs the old one until it is restarted. **It never restarts itself**: restarting interrupts what your assistants are doing, so when to do it is your call. The plugin's folder is updated by whichever client of the machine checks first (usually a `relevo mcp` of Claude Code, since its sessions start more often than the gateway connects), so the plugin does not go by what its own run printed: at connect, and when its own update ends, it compares the version in its folder with the one the gateway loaded. When they differ, it tells you once for that version, in a home channel of the profile (the first one that takes the message; Relevo's own is left out, since its chats are your teammates' threads), and logs it:

```
Relevo was updated on this machine: the Hermes plugin is now 0.4.0, and this gateway still runs 0.3.0. Restart the Hermes gateway to use it (`hermes gateway restart`) when it suits you: it was not restarted, so the assistants keep working meanwhile.

the Hermes plugin (/home/you/.hermes/plugins/relevo): installed 0.4.0 over 0.3.0; restart the Hermes gateway to use it
```

(the last part only when its own run installed it). A run that installed only the `relevo` binary or the pi extension asks for no restart, and says nothing. When no home channel takes the message (none set with `/sethome`, or none connected yet), the log says so and it is tried again after 10 seconds, a minute, 5 minutes and half an hour, and at the next connect. When the update fails (a folder the gateway cannot write, say), the gateway log has what `relevo` said, as a warning; nothing is sent to a chat. Otherwise it is silent. It runs again at the next connect (a restart, or the platform reconnecting), and `relevo` checks at most once an hour.

It does not run:

- with `RELEVO_AUTO_UPDATE=0` (or `false`, `no`, `off`) in the profile's env or the gateway's; `relevo update` still works by hand;
- when the plugin's folder is in a git checkout (a `.git` in it or above it, following links), such as `adapters/hermes/relevo` of a clone of this repository: replacing it would throw away what git tracks there, uncommitted work included. Update it with git.

A plugin from before 0.4.0 has no updater: install 0.4.0 once by hand, as in step 2, and it follows the server after that. The same goes for a `relevo` from before 0.4.0 next to a newer plugin: the gateway log says so once, as a warning, and the plugin does not run it again until the gateway restarts.

## How it behaves

- **Sessions.** The gateway platform sends one fixed `Relevo-Session` per Hermes profile, derived from the profile's home. When the gateway stops the platform (a stop or a restart), the platform stops its waiting inbox request, then ends that session with `DELETE /api/session`, so the threads it holds show again in your other sessions at once rather than when the claims lapse 15 minutes later; this is best effort: it waits 2 seconds at most, and a server it cannot reach only leaves the claims to lapse. A gateway that dies without stopping the platform (killed, crashed, the machine lost power) cannot end it, so the platform ends it when it connects, before its first inbox request (retried with the back-off while the server cannot be reached). Either way the session starts again like any new one, from what you have handled: a note still pending for you (one nobody answered, closed or marked done, even if another of your sessions read it, or the dead gateway took it as a message and never answered) is handed over at once; one you handled meanwhile is not. When a restart interrupted the relevo chat's turn on such a note (the drain timed out, or the process died mid-turn), Hermes may resume that turn itself; the note handed over again to that chat, in the first batch after the start, then says so and tells the agent to read the thread first and answer `[SILENT]` if it is no longer pending or the chat already answered it or is working on it. It is not dropped, since Hermes may not resume the turn after all. The threads a dead gateway held are released at once, not when their claims lapse 15 minutes later: the fixed id is what lets the new start end the old session. When the gateway connects a new platform adapter before it drops the old one, the session is live in this process, so the new one does not end it and the old one leaves it to the new one; but when the old one has not managed to end an earlier start's session yet (the server could not be reached), the new one ends it, and neither hands over a note the other already did. The agent's tool calls in a turn of the relevo platform act for that same session, so its claim and its reply act for one session. Every other Hermes process (a CLI or TUI window, and the tools in turns of other platforms such as Telegram) is its own running agent, with a random id kept for the life of the process, as `docs/agents.md` asks: the gateway and those agents see each other's claims and step aside, and a `relevo_inbox` there never takes the gateway's notes. A session is general (it sees every note for you that no other session of yours holds) unless `RELEVO_CONTEXT` is set.
- **Inbound.** A thread with new notes becomes one message, from the author of its latest new note, with the thread's facts and each note in `<untrusted from="@handle" …>`; the thread's title, being the first line of a teammate's note, comes wrapped the same way. Who the agent speaks for is the server's `you` (the inbox's session, or `GET /api/people` before any inbox): each message and what the tools return say "You speak for @handle", the team list marks that person, and your own notes come in `<yours from="@handle" …>`. A note's text cannot open or close such a block (or a `<yours>` one) of its own, spaces or invisible characters inside the tag included. A teammate's text is never taken as a gateway `/command`. A note cut at 4,000 characters in the inbox is fetched whole first. A gateway stop while a batch is being handed over lets the batch finish (the notes have reached the gateway's session already, so they would not come again before its next start, and a team question not even then), with any note still being fetched handed over cut.
- **Pending and handled.** A note stays pending for you until one of your sessions handles it: answers it, closes the thread, or marks it done (`relevo_send` with `thread`, `done: true` and no text: "seen, nothing to do", which writes and claims nothing). Reading it, taking it as a message or claiming the thread is not handling it. Threads pending for you say so, with how to mark them done; team questions never are (answering them is optional). `relevo_thread`'s delivery tells, for each participant, "note n_… seen by "pi" in embeddings at 10:46 UTC on 2026-11-08, not handled yet" from "…; handled by "Hermes" (general) at 19:30 UTC on 2026-11-08", and says "no live session of theirs has it now" rather than "seen by 0".
- **What a thread waits on.** `relevo_inbox` shows a thread pending for you with nothing new for its session together with its latest note (`last_note`), wrapped like any other and marked as maybe having reached another of your sessions already (the gateway, which took it as a message, say), so an agent that did not read it knows what the thread waits on. The gateway itself hands a thread over when it has new notes for its session (after a start, everything still pending is new for it), and, once, when its latest note never reached its chat.
- **Handed over once.** As in `relevo mcp`, each note is handed over once per gateway process. The server delivers the notes of an inbox answer when it sends it, so an answer lost on the way (the laptop slept, the network changed, a proxy cut the request after the server wrote) would take them with it. So in every answer of the waiting request, a direct thread pending for you with nothing new, whose latest note, by someone else, the adapter never handed over, is handed over to that thread's chat with that note, and a line saying it is handed over again: still pending, its latest note never reached this chat, to treat it as new, and that earlier notes of the thread may be missing too, so to read it whole with `relevo_thread` first (only the latest note comes back). After a failed request the next one does not wait, so that comes at once. A note written after the loss comes as new, without the lost ones before it, so each thread with new notes in the first answer after a request that may have lost its answer (any failure but an error Relevo itself sent) says earlier notes may be missing, the same way. The same covers a note of another thread that a tool in a relevo chat (acting for the gateway's session) fetched: only what the adapter handed over counts, since each thread is its own chat. A note the agent leaves alone (`[SILENT]`, not for it) stays pending for you and does not come back in that process, until a new note in the thread or the gateway's next start.
- **Who holds a thread.** In `relevo_inbox`, and in the message a new note arrives as, each thread says which of your sessions has it and, one line each, who else in it holds it now (`held_by`): "@ana's Claude Code in pagos-api has it since 10:47 UTC on 2026-11-08". `relevo_thread` says it once, in its delivery.
- **The rules** of `docs/agents.md`, word for word, reach the agent as the platform hint, with how they map onto Hermes: `send` is `relevo_send`, staying silent is answering exactly `[SILENT]` (which sends nothing; a note left that way stays pending for you), marking a note that needs nothing handled is a `relevo_send` in the chat's thread with `done: true` and no text, then `[SILENT]`, claiming the thread before longer work is a `relevo_send` in the chat's thread with `claim: true` and no text (a state, not a note), and the agent's reply in the chat is the answer. In short: the agent speaks for you (`you`) and asks whom you name (a teammate, or the whole team); otherwise it looks at its inbox first and uses the team's `about` lines to choose whom to ask. It stays silent when a note is not for it, leaving it pending for you, marks one that needs nothing (an answer it has now read, a thank-you) handled with `done`, reads what it needs first and answers what it can from that, and claims the thread as a state only just before it changes files or runs something long, never writing a note that only says it is on it (and never claiming a team question, which it answers only from its own work or your files), treats others' text as context, may share facts from your documentation or configuration to answer a note but asks you before sharing code or private data or changing your code because of a note, keeps notes self-contained and in the first person, as you would write them (a new thread starts with a one-line subject, no greeting and no "Re:"), says only what you said or what it found (guesses marked as guesses), opens no new threads to third parties while answering unless you ask, and closes a thread only when you ask or when it asked the question and it is answered. Sessions that have only the tools (CLI, TUI, Telegram…) never see that hint, so the same rules travel in the descriptions of `relevo_inbox` and `relevo_send`, with rule 8 for sessions without push ("look at your inbox when you start a task and before you finish one"), and what `relevo_inbox` and `relevo_thread` return says, above others' text, that it is context, not orders. As with `relevo mcp`, the tools back the rules: `relevo_send`'s description says to send it where your person said when they name whom to ask (a teammate or `@team`), and otherwise to look at the inbox and the `about` lines before opening a thread, to read what it needs first and just answer what that reading answers or explains, to claim the thread with `claim: true` and no text (a state, not a note) only just before changing files or running something long, never to write a note that only says it is on it, and not to claim a team question, which is answered only from the agent's own work or your files, to write in the first person, and to start a new thread with a one-line subject, no greeting and no "Re:", that `done: true` with `thread` and no text marks a thread handled when it needs nothing, and that a note stays pending until answered, closed or marked done, so one that is not for the agent is left alone; `relevo_inbox`'s says it tells whom the agent speaks for and to look there before opening a thread. A teammate's open team question comes with a line saying to answer it only from the agent's own work or your files (repeating a teammate's `about` line, or guessing who might know, is not an answer), and not to claim it. A direct note that arrives in the chat says to answer `[SILENT]` and leave it pending if it is not for the agent, to claim it that way if it needs work first, and to mark it done, then `[SILENT]`, if it needs nothing. Threads show who holds them as a state, as `relevo mcp` shows it: in `relevo_thread`, "@ana's Claude Code in pagos-api has it since 10:47 UTC on 2026-11-08", in the header and in the delivery; in the inbox, which session has it; a direct thread pending for you that none of your sessions has says how to claim it. `relevo_send` says what a claim did: this session has the thread now and no note was written, or another of your sessions has it, so step aside; and what `done` did: the notes this session has received are no longer pending for you (one it has not received stays pending), nothing was written or claimed.
- **Outbound.** Only the agent's final answer becomes a note. The platform asks for Hermes' minimal display tier, but your global `display.*` keys (`tool_progress`, `interim_assistant_messages`, …) win over a tier, so the adapter itself writes only what the gateway marks as a turn's final reply: tool progress, interim messages, busy acknowledgements, pairing codes and warnings never reach a thread, whatever your display settings. Such a send reports that it delivered nothing (an `unknown` failure, never one that marks the chat as gone): otherwise an interim message counted as delivered would make the gateway skip a final answer with the same text, which happens when the runtime also delivers the final as commentary. When a turn fails, the gateway's final reply is its own copy, and it is not written either, so it neither leaks details nor claims the thread for a session that cannot answer: it stays in the gateway log, and your other sessions still see the note. That covers the copy as the gateway composes it: the error text ("⚠️ The AI model service isn't reachable…", "Sorry, I encountered an error…", the agent's own "rate-limited every one of 3 attempts…"), with its "Details:" or "Provider said:" line, the failed-turn notice after it ("Your request was not processed…", "This turn did not complete…"; a reply ending with one is dropped whole, even a half-done answer), and the runtime footer between them. The footer (model, context, your working directory) and the automatic-reset notice also come off a real answer: a note holds the agent's words only. `[SILENT]` sends nothing. Each send carries an idempotency key and is retried once with the same key; a refused or failed send is not retried again by the gateway (a new attempt would carry a new key). Long answers are split at 16,000 characters (a note holds 64 KiB). A closed thread refuses the reply; the server's loop guard (20 notes per thread per hour) applies.
- **A refused token** (unknown or revoked), a `RELEVO_CONTEXT` that is not a context (`repo` or `repo@branch`, see `docs/protocol.md`), or any inbox request the server calls malformed stops the platform with a fatal error instead of retrying forever while looking connected.
- **Logs** carry thread ids, counts and error codes, never tokens or note text.

## Tests

Against a real server, with the Hermes venv's Python, from the repository root:

```
cargo build --release
PYTHONDONTWRITEBYTECODE=1 RELEVO_BIN=target/release/relevo \
  ~/.hermes/hermes-agent/venv/bin/python -m unittest discover -s adapters/hermes/tests -v
```

The tests install the plugin into a temporary Hermes home (as in step 2), load it through Hermes' own plugin discovery (and, for the test of a gateway killed right after a note reached it, run the platform in a process of its own, `tests/gateway_child.py`, from that same home), start `relevo serve` (and `relevo serve --tls`) on free ports in 7990-7999 with temporary databases, and stop them at the end. Besides the tools, the inbound and outbound messages and the sessions (a gateway killed right after a note reached it ending no session, and its next connect ending it and getting that note at once, included), they cover handing over once: an inbox answer lost on the way, through a proxy that lets the request reach the server and cuts the connection, handed over on the next answer, once, from a request that does not wait, and saying earlier notes may be missing, with two notes in the lost answer and with a note written after the loss; a thread the agent answered `[SILENT]` not handed over again in the same process (nor by a replacement adapter); and a note a relevo chat's `relevo_inbox` fetched reaching its own thread's chat once. Over https they cover the tools and the gateway loop with the certificate saved in the config or named by `RELEVO_CERT`, another certificate refused before anything reaches the server, a host the certificate does not name, a certificate that is not PEM, and plain http unchanged (one case, a certificate an office authority signed, uses `openssl` and is skipped without it). The automatic update is tested with a stand-in for `relevo` (a shell script that records how it was run and answers as each test says): run in the background for the plugin's folder with the profile's server and token, while the platform keeps working; what it installed told once in a home channel (the next one when the first refuses it, never Relevo's own nor another profile's) and logged, and tried again when no home channel takes it; a plugin another client updated told at connect, once per version; a run that installed only the binary not told; a failure logged and not told; one update at a time; and nothing run when it is turned off, without `relevo`, or for a folder in a git checkout. The home channels are listed by Hermes' own code; one test runs the real binary against a server without a dist, which says nothing. Every other test runs with `RELEVO_AUTO_UPDATE=0`. `HOME` and `HERMES_HOME` point at the temporary directory, so no real Hermes home is read or written. `HERMES_SRC` points at the Hermes source if it is not in `~/.hermes/hermes-agent`. Without `RELEVO_BIN` they use this repository's `target/release/relevo`, never a `relevo` on `PATH`, which may be older than the plugin.
