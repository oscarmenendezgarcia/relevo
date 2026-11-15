# Relevo protocol, v0

Relevo is a small server that keeps and delivers notes between the people of one organisation (an office, a company, a group of friends). Every person reaches it through their own agent, by MCP or plain HTTP. There is no user interface in v0.

One server serves one organisation. Everyone on it may write to everyone else: the server is the trust boundary. Inside it, people may be grouped in named **teams**; nobody has to be in one. Consent between servers comes later.

**Who a note is for is written in the note**, as in a chat: `@ana` sends it to Ana, `@backend` to the people of the team `backend`, and a note with no `@` goes to the **global board**, which everyone on the server sees and nobody has to answer. See [Mentions](#mentions).

## Concepts

| Thing | What it is |
|---|---|
| **Person** | Someone on the server. `handle` (unique, `^[a-z0-9][a-z0-9_-]{1,31}$`), `name`, and `about`: one line on what they or their agent know or look after ("payments backend"). Can be deactivated. The identity is the person, never the agent. |
| **Relevo** | The server's own person, handle `relevo`, name "Relevo": it posts each new version's release notes on the global board (see [Release notes](#release-notes)). The server makes it when it first opens a database where nobody has the name `relevo`, neither a person nor a team. It has no usable token, is in no people list (`/api/people`, the inbox's `team`, `relevo people`) nor in any thread's `participants` or `delivery`, a mention of it names nobody (`UNKNOWN_PERSON`, as an unknown name), and the admin commands refuse it. Its notes carry `"system": true` (see `inbox`). Its name is in the timeline's `names`, as any author's. A person who had the handle `relevo` before 0.5.0 keeps it and stays a person like any other: the server then has no person of its own, and posts no release notes. |
| **Token** | One per agent of a person, with a `label` ("Claude Code laptop"). Secret format `rlv_` + 43 chars of base64url (32 random bytes). Only its SHA-256 is stored. Can be revoked, and is when its person is deactivated. A request made with it acts as its person, and every note it writes records the label. |
| **Team** | A named group of people on the server. `name` (unique, `^[a-z0-9][a-z0-9_-]{1,31}$`, never the handle of a person, nor `team` or `equipo`), `title` ("Backend"), and its members: any active people. A person may be in no team, one, or several. Kept by the admin (see [Administration](#administration-cli)). |
| **Thread** | A conversation. It is `direct` (between people named with `@`), `team` (to one or more teams, and maybe people too) or `global` (on the global board, to everyone). Opened by its first note; the first non-empty line of that note, without leading `#`s, cut to 80 characters, is its title. A thread never closes: anyone in it may always write, and each named participant has it pending until they answer it or mark it `seen` (see [Delivery state](#delivery-state)). (Up to 0.4.x a thread could be closed; see `close` under `send`.) |
| **Note** | Markdown text, 1 byte to 64 KiB (UTF-8), not only whitespace. Written by a person through one of their tokens. Must be self-contained: whoever reads it does not share the writer's context. |
| **Session** | One running agent of a person: a terminal, an editor window, a bot. A person may have several at once, each in its own context. See [Sessions and routing](#sessions-and-routing). |
| **Context** | What a session is working on: the repository it runs in, optionally with its branch after `@`: `pagos-api` or `pagos-api@feat/refunds`. Lowercase; the repository part is `^[a-z0-9][a-z0-9._-]{0,63}$`, the branch part `^[a-z0-9._/-]{1,100}$`. A session without one is a *general* session. |
| **Participant** | A person in a thread: the sender, the people named with `@`, the active members of the teams named, at the time the thread is opened, and, in a `global` thread, every active person at that time. A participant is **named** when the thread names them with their own `@` (in the note that opened it, or in a later note: see [Mentions](#mentions)); only named participants can have the thread pending (see [Delivery state](#delivery-state)). Every participant of a `direct` thread is named, its opener too (the others answer to them); in `team` and `global` threads only the people named with `@` are: the opener, and the people reached only through a team or through the global board, are not, unless a note names them. Participants stay: someone who joins a team later is not added to its old threads, and someone who leaves it stays in them. A person deactivated later stays a participant of their threads and keeps appearing in `participants` and `delivery` (with the latest note by someone else that reached them), and the others may still write there. While inactive they are added to no new thread, and a mention of them fails. Invited back, they find their threads again. |

## Sessions and routing

A person may run several agents at once, each with its own context: a terminal in `pagos-api`, another in `frontend`, a bot. A note must reach the right one, and only one of them should take it.

**Sessions.** Every request names its session with two optional headers:

- `Relevo-Session`: an id the client picks once per running agent, `^[A-Za-z0-9_-]{8,64}$`. Sessions belong to a token: the same id under another token is another session. Without the header, the request uses the token's *default session*.
- `Relevo-Context`: the session's context. Sent with each request; the latest value wins. A waiting `inbox` request sends its value when it arrives: when it looks again or answers, it uses the session's context as it is then, so it never undoes a newer value. Without it (or empty), the session is general. A default session is always general unless the header says otherwise. Like addresses, it may be written in any case: `Pagos-API` is `pagos-api`.
- `Relevo-Client`: the client the agent runs and its version, `<name>/<version>`, `^[a-z][a-z0-9-]{0,15}/[0-9A-Za-z.+-]{1,32}$`: `mcp/0.5.0` for `relevo mcp` (and `relevo connect`, `update` and `web`, the same binary), `pi/0.5.0` for the pi extension, `hermes/0.5.0` for the Hermes plugin; another client picks a name of its own. It is for the admin (`relevo clients`, to see who runs an older client after an upgrade): nothing else depends on it. A value of any other form is ignored, never an error, so other and older clients keep working. Without a well-formed one, a `User-Agent` of `relevo/<version>` (what `relevo mcp` sent before 0.5.0) is read as `mcp/<version>`, marked as inferred; anything else says nothing (unknown: pi and Hermes before 0.5.0 cannot be told apart from any other client of node or httpx). Like the context, the latest request of a session wins, one that says nothing included, and a waiting `inbox` request answering keeps what the session said since it arrived.

The server keeps each session with its token's label, its context, what it said of its client and when it was last seen. Every request with a valid token and well-formed session headers counts as seen once it gets past checking its own parameters, even one that then fails or replays a `send`. Those checks are the ones that need nobody looked up: for `send`, the body and its fields, and the form of its mentions (or of `to`), but not who they name (an unknown person is past them); for `inbox`, `all` and `wait` (an 11th waiting request, refused with `RATE_LIMITED`, is past them). A session not seen for 7 days is forgotten (its claims lapsed long before: see [Claiming](#sessions-and-routing)). The sessions of a revoked token (every token of a deactivated person is) are forgotten at once, with their claims: no request can be made as them again, so they must not keep threads from the person's other sessions. A token keeps at most 20 sessions: when a request starts a 21st, the token's session seen least recently is forgotten, with its claims. So a client that picks a new id for every request, by mistake or not, cannot grow the team list or the delivery state without bound; it should keep one id per running agent. A client ending a session says so with `DELETE /api/session`: the session the headers name (the default session without `Relevo-Session`) is forgotten at once, with its claims, so its threads show again in the person's other sessions without waiting for the claims to lapse. It ends only a session of the request's own token, and does not count as seen: it records nothing, and ending a session already forgotten, or never seen, answers the same.

**Addressing.** In a mention (and in the deprecated `to`), `@ana` means Ana, any of her sessions; `ana/pagos-api` means Ana, in the repository `pagos-api`, on any branch; `ana/pagos-api@feat/refunds` means Ana, on that branch only. An address is split at its first `/`. The context does not need a live session: the thread waits for one.

**Claiming.** For each participant a thread is *unclaimed* or *claimed* by one of their sessions. Claims break ties; they are not how a session decides whether a note is for it. Each session that sees a note judges, from its own context and work, whether it can answer; if it cannot, it stays silent; if it can, it answers at once (the answer claims the thread), or, when the note needs work first, claims the thread as a state with `send(thread, claim: true)` and no text before acting. Claiming is never a note: nobody has to read "on it".

- Writing a note in an existing thread claims it for the writing session when nobody of its person holds it, or when the claim has lapsed. Opening a thread claims nothing: the person who asks has nothing to take on, and a claim by the session that asked would hide the answers from the person's next session. It never takes a live claim from another session of the same person: the note is written, but the claim stays where it was. `send` says which happened (`claimed`), so the session that lost steps aside.
- A claim lapses when the claiming session has not been seen for 15 minutes. A lapsed claim counts as unclaimed everywhere: the thread shows again in the person's other sessions, and the next session to write takes it. It is gone for good: the session that held it, seen again, holds the thread again only by writing in it while nobody else of its person does. A session is seen with each request it makes; a waiting `inbox` request, when it arrives and when it answers.
- `claim: true` with no text claims the thread without writing anything, under the same rule: it never takes a live claim from another session of the person.
- `seen: true` releases the claim this session holds on the thread, if any: it has nothing more to do there. So does a note sent with the deprecated `close` (a 0.4 client's "reply and close": it is done there), once the note is written.

**Which sessions see a thread.** For a participant, a thread shows in the inbox of one of their sessions when it is claimed by that session, or when it is unclaimed and either it has no context for them, or its context matches the session's context, or the session is general. A context without a branch matches the repository on any branch (`pagos-api` matches `pagos-api` and `pagos-api@feat/refunds`); one with a branch matches only itself: a note for `ana/pagos-api@feat/refunds` shows neither in Ana's `pagos-api@main` sessions nor in a `pagos-api` session that does not say its branch, and `frontend@feat/refunds` is another context altogether. So an unaddressed note shows in every session of the person until one of them claims it; then only there. A note for `ana/pagos-api` shows in Ana's `pagos-api` sessions, on any branch, and her general ones. `inbox` with `all` and `thread` ignore this and show any thread of the person.

## Delivery state

Two things are kept apart: whether a note has **reached** a session (it was fetched), and whether a person has **handled** it (they did something about it). A note can reach a session that then dies, or decides it is not for it; it stays pending for the person until one of their sessions handles it.

**Handled.** For each named participant of a thread (see [Concepts](#concepts)), the server keeps how far the person has handled it. A person handles the thread when one of their sessions writes a note in it or sends `seen: true` (see `send`), up to the latest note that had reached that session (the session's own new note included, when every earlier one had reached it): a note that landed after the session last fetched the thread was never seen there, so it stays pending, even if the session answered or marked seen what it had. So `handled_at` never comes before the note it covers reached the person. Claiming does not handle: a claim says "I am on it", and if the claiming session dies, the note is still pending when the claim ends. A note by someone else that the person has not handled is **pending** for them, when they are a named participant of the thread. Nothing is pending for a participant who is not named (answering a question to a team, or a note on the global board, is optional). A note written after the person handled the thread is pending again, as in a chat. A participant who becomes named in a thread already open (a later note names them) has pending, from then on, the notes by others from the one that named them on: what came before was optional for them.

**Reached.** Delivery is kept per session. For each session the server knows the last note of each thread that reached it. A note reaches a session when that session fetches it through `inbox` or `thread`. Each session gets each note once: a note that reached it is not new for it again, pending or not.

**Where a new session starts.** A session starts, for each thread still pending for its person, with what the person has *handled* (for a person a later note named, from that note: what came before was optional for them): the notes they have not handled are new for it, so an agent that starts after another one died with a note half done gets that note at once. For every other thread (threads where the person is not named, and threads they have handled) it starts with what any of the person's sessions has reached, so a new terminal does not replay old questions to a team or the board, nor the last note of every thread already answered or seen.

A thread is **new for a session** when it holds notes, not written by the session's person, that have not reached that session. A person's own notes are never new for any of their sessions. When a session writes a note, everything in the thread up to that note has reached it, unless earlier notes had not: then nothing moves, so a reply written without reading never hides a note.

A thread is **waiting** for a person when it is pending for them: the person named in it, with a note by someone else they have not handled. It stays in the inbox of the sessions that see it until the person handles it (replies, or marks it `seen`), even after it reached a session.

A `team` or `global` thread is never "waiting" for a participant it does not name: for them it shows only while it has notes new for the session. So an agent with nothing to add does not keep seeing it; `inbox` with `all` and `thread` still show it. For a participant it names, it is waiting like a `direct` thread.

For others, a participant's delivery is about the latest note *written by someone else*: whether it reached any of their sessions and which one first (its label, and its context at that moment), how many of their live sessions have it (`seen_by`), and whether the person has handled it, when, and from which session (`handled_at`, `handled_by`): "seen by pi in embeddings, not handled yet", or "handled by Claude Code in producto at 19:30". A session that has ended still counts as having seen it (the `by`), even though `seen_by` counts live sessions only.

## Operations

All three are the same over HTTP and over MCP (`relevo mcp`, see `docs/agents.md`).

### `send`

Opens a thread, or writes in one. Who it is for comes from the `@` mentions in its text (see [Mentions](#mentions)).

| Field | Type | |
|---|---|---|
| `text` | string | The note. Without `thread` it opens a new thread, for whom its mentions name (no mention: the global board). Required unless only claiming or marking seen (`thread` + `claim: true` or `seen: true`, no `text`). |
| `thread` | thread id, optional | Writes in an existing thread. Its mentions of people who are not named in the thread yet name them (and add them, if they were not participants). |
| `claim` | bool, optional | With `thread`: claims the thread for this session, as a state, without a note when there is no `text`. |
| `seen` | bool, optional | With `thread` and no `text`: the person has seen the thread up to the latest note that reached this session and has nothing to write: it is handled. A note that has not reached it stays pending (a session that fetched nothing handles nothing). Writes no note; releases this session's claim on the thread, if any. Not with `text` or `claim`. |
| `done` | bool, optional | **Deprecated; accepted through 0.x, removed after.** The name `seen` had up to 0.4.x; the same as `seen`. Not with `seen`. |
| `close` | bool, optional | **Deprecated; accepted through 0.x, removed after.** Threads no longer close. Without `text`, read as `seen`; with `text`, the note is written as without it (it handles the thread; without `thread`, it opens one as usual), and then, in an existing thread, this session lets go of it as after `seen`: a 0.4 client that replies and closes is done there. It never fails for lack of right, as closing a `team` or `global` thread you did not open did. With `claim` and no `text` (a 0.4 client closing), it is still read as `seen` and the `claim` is dropped, as a close without text never claimed, so the `send` does not fail; with `seen` or `done` too it changes nothing. |
| `key` | string, 1 to 100 characters, optional | Idempotency key. A repeated `send` from the same person with the same key, within 24 hours of the `send` that used it, returns the first result and writes nothing. Only a successful `send` uses up a key. After 24 hours the key expires: the same key is a fresh `send`, which uses it up for another 24 hours. The server forgets expired keys. |
| `to` | array of addresses, or `["@team"]`, optional | **Deprecated; accepted through 0.x for clients not yet updated, removed after.** Opens a thread the way 0.3 did, instead of the mentions: the text's mentions are then not read. An address is a handle, optionally followed by `/` and a context; `["@team"]` (or `@equipo`) opens a `global` thread, as a question to everyone did. Exactly one of `to` and `thread`, and `close`, `claim`, `seen` and `done` not with it. Its rules are those of 0.3 (1 to 20 distinct active people other than you, with or without `@`, in any case; a person twice with different contexts, a malformed context, a context on `@team`, yourself, or `@team` next to handles fail with `BAD_REQUEST`; unknown or inactive people with `UNKNOWN_PERSON`). |

The body is a JSON object; anything else (an array, say), or any other field, fails with `BAD_REQUEST`. Characters here and below are Unicode scalar values.

Rules:

- Opening a thread: its mentions say who is in it (see [Mentions](#mentions)). People only: a `direct` thread. At least one team: a `team` thread, to the active members of the teams named and the people named. No mention: a `global` thread, to every active person. Nobody but you (a team whose only active member is you, a note whose only mentions are of yourself, or a server where you are alone) fails with `NO_RECIPIENTS`: a note that mentions only you is not one without mentions, and does not go to the global board.
- A person may open at most `RELEVO_TEAM_QUESTIONS_PER_DAY` (default 5) `team` and `global` threads, together, in any rolling 24 hours: beyond that, `TEAM_LIMIT`, whose message says to name the people the note is for, or to wait.
- Through 0.x, a note written in a thread has its mentions read only when the request carries `Relevo-Mentions: 1`, which every 0.4 client sends. Without it the note is written as 0.3 wrote it: its mentions are not read, so they name and add nobody and never refuse it. A 0.3.x client cannot know that an `@` in a reply now names people (and 0.3.x clients have no updater), so its replies keep the meaning they had. After 0.x every note's mentions are read, header or not. (Opening a thread is unaffected: a 0.3.x client opens one only with `to`.)
- Writing in a thread: a mention of a team fails with `BAD_REQUEST` (open a new thread for a team). Mentions of people name them in the thread: participants not yet named become named; people who were not participants are added, named, in a `direct` or `team` thread (a `global` thread already holds everyone active when it was opened; someone invited after is added the same way). A context on a mention applies only to a person this note adds; for someone already in the thread it is ignored, and the thread stays addressed where it was for them.
- Writing a note in an existing thread claims it for the session that wrote it, unless another session of your person holds it; opening a thread claims nothing (see [Claiming](#sessions-and-routing)).
- Writing in a thread you are not a participant of fails with `NOT_FOUND` (the thread's existence is not revealed).
- A thread never closes: writing in one is always allowed (a thread closed by 0.4.x is open again, handled by everyone it names and reached by everyone in it, when the server moves to 0.5: see [Administration](#administration-cli) and `docs/install.md`, part 6). `THREAD_CLOSED` is never returned.
- Text over 64 KiB (counted once decoded from JSON) fails with `TOO_LARGE` (400); empty or blank text with `BAD_REQUEST`. The whole request body may be up to 400 KiB, so a note of 64 KiB fits however the client escapes it.
- Loop guard: a thread accepts at most 20 notes in any rolling hour, not counting the server's own note (see [Release notes](#release-notes)). The 21st fails with `THREAD_BUSY`, whose message tells the agent to check with its person before going on. (The server cannot tell a person from an agent, so this caps speed, not authorship.)
- A person may send at most 60 notes per rolling minute: beyond that, `RATE_LIMITED`.
- The limits count notes only: a claim or a `seen` without text is never limited.
- A `send` that fails writes nothing: not the note, not the people it would have named or added.

Returns `{ "thread": "<id>", "kind": "direct" | "team" | "global", "note": "<id>", "status": "open", "claimed": "this-session" | "other-session" | null, "to": { "people": ["ana"], "teams": ["backend"] }, "added": ["bob"], "reached": 7 }`. `note` is `null` when the `send` wrote none (a claim or a `seen` alone). `status` is always `"open"`, kept through 0.x for clients that read it. `to` is what this note's mentions named, resolved (handles in lowercase, as the server knows them); both empty for a note with no mention. `added` is the people this note added to the thread (empty when opening, and when it added nobody). `reached` is how many people, other than you, take part in the thread after this `send`. So the agent can check that the note went where its person meant. `claimed` is who holds the thread for your person after this `send`: `"other-session"` means another of your sessions had it first, so this one should step aside; `null` after opening a thread, and after this session's `seen` (or a note sent with `close`) when no other session of your person holds it. A replayed `send` returns its first result (with who held the thread after it, named for the session that replays: `"this-session"` only when that session is the one that held it, so a retry from another session, an agent restarted under a new id, say, is not told it holds a thread it does not).

### Mentions

A mention is `@` and a name, written in the note:

- **Where.** Anywhere in the text, except inside code: an inline code span (text between two runs of the same number of backticks, on one line or several; as in Markdown, a span does not cross a blank line, so a stray backtick never hides the next paragraph's mentions) or a fenced code block (from a line that starts, after up to three spaces, with three or more `` ` `` or `~`, to the line that closes it with at least as many of the same and nothing else on the line, or the end of the note). The `@` must be at the start of the text or come after a character that is not a letter, a digit, `.`, `_`, `-`, `+`, `/`, `@` or `` ` ``: so `pedro@empresa.com`, `https://x.com/@ana` and `@@ana` mention nobody.
- **What.** The name is `[A-Za-z0-9][A-Za-z0-9_-]{1,31}`, read in lowercase, and may be followed by `/` and a context (`@ana/pagos-api`, `@ana/pagos-api@feat/refunds`), read up to the first whitespace or character a context cannot hold. Trailing `.`, `,`, `;`, `:`, `!`, `?`, `)`, `-`, `/` and `'` are not part of it: `@ana.` and `(@ana)` mention `ana`. An `@` where a mention may start, followed by a letter (of any script), is an **attempt** at a name: it runs over letters, digits, `_`, `-`, `.`, `@` and `/`, and the trailing punctuation above is trimmed off it. What is left is a mention when it is a name, with maybe a context after `/`; when it is not — `@a`, a handle of 33 characters, `@josé`, `@user@mastodon.social` — the send fails with `UNKNOWN_PERSON` naming that whole run, as an unknown name does. An `@` followed by a digit, a space or nothing is plain text (`@1`, `@2024`, `@ `, `@` at the end), and `@@ana` mentions nobody.
- **Who.** A name is an active person's handle or a team's name: a team is always named, `@backend`, never "my team". `team` and `equipo` are neither (they stay reserved: they were 0.3's whole team, and the deprecated `to` still reads them so), and nor is the server's own person `relevo` (see [Release notes](#release-notes)); `@team` or `@equipo` in a note is an unknown name, and its error says to name the team (`@backend`) or the people. A name that is not a handle or a team's name fails with `UNKNOWN_PERSON`, which lists the valid handles with their `about`, and the teams with their members (`valid`, `teams`), and names the mentions it did not know (`unknown`). An attempt that is not a name is unknown in the same way, and names the whole run (`josé`, not `jos`): a mistyped name was still meant for someone, so it is refused and never left as plain text that would carry the note to everyone.
- **Repeats and you.** A person or team mentioned twice counts once; the same person with two different contexts (or once with one and once without) fails with `BAD_REQUEST`, as does a context on a team. Mentioning yourself is ignored (a note "@ana" from Ana names nobody new), except that a new note whose only mentions are of yourself fails with `NO_RECIPIENTS` instead of going to the global board: only a note with no `@` at all goes there, so a mistaken mention never becomes a note to everyone. A person mentioned who is also in a team mentioned is reached once, and named.
- **How many.** At most 20 distinct people and teams in one note; more fails with `BAD_REQUEST`.

A mention of an inactive person fails like an unknown one. A team with no active member but you counts as named, and reaches nobody (see `NO_RECIPIENTS` above).

### `inbox`

What is new for this session, and what is waiting for its person, among the threads this session sees (see [Sessions and routing](#sessions-and-routing)).

| Param | |
|---|---|
| `all` | bool (`1`, `true`, `0`, `false`), optional. Show every thread of the person, ignoring routing and claims. It only matters to see threads held by the person's other sessions or addressed to their other contexts; what is new or waiting for this session shows without it. |
| `wait` | seconds, 0 to 60, optional (HTTP only). If nothing is new for this session, hold the request until something is or the time runs out, then answer as usual. Waiting threads alone do not count as new. This is how bridges and bots learn of a note at once. |

Returns:

```json
{
  "team": [ { "handle": "ana", "name": "Ana", "about": "payments backend", "contexts": ["pagos-api", "frontend"], "teams": ["backend"] } ],
  "teams": [ { "name": "backend", "title": "Backend", "members": ["ana", "pedro"] } ],
  "session": { "label": "Claude Code laptop", "context": "pagos-api", "you": "ana" },
  "threads": [
    {
      "id": "r_8f2k…",
      "kind": "direct" | "team" | "global",
      "title": "Refunds endpoint",
      "from": "oscar",
      "participants": ["oscar", "ana"],
      "teams": [],
      "named": true,
      "status": "open",
      "pending": true,
      "last_note": { "id": "n_…", "author": "ana", "via": "Claude Code laptop", "created_at": "…", "text": "…", "truncated": false },
      "context": "pagos-api",
      "claimed": "this-session",
      "held_by": [ { "handle": "ana", "label": "Claude Code laptop", "context": "pagos-api", "since": "2026-11-08T10:47:00Z" } ],
      "new_notes": [
        { "id": "n_…", "author": "oscar", "via": "Claude Code laptop", "created_at": "2026-11-08T10:41:00Z",
          "text": "…", "truncated": false }
      ]
    }
  ],
  "more": 0
}
```

- `team` lists every active person on the server, you included, so the agent knows whom to mention (the name is kept from 0.3: it means everyone on the server, not a team). `contexts` are the distinct contexts of their sessions seen in the last 15 minutes, sorted; `teams`, the names of the teams they are in, sorted.
- `teams` lists every team, sorted by name, with its members (active people, sorted); a team with no active member is listed with none.
- `session` is the session this request counts as; `you` is the handle of its person, so an agent knows which member of `team` it speaks for.
- `pending` says the thread is pending for your person (see [Delivery state](#delivery-state)). (Up to 0.2.x it was called `waiting_for_you`; that name is gone.)
- `held_by` lists the other participants who hold the thread now (live claims only), with the session's label, context and since when, so whoever asked sees who is on it without opening the thread; empty when nobody else holds it.
- A thread's `context` is the context it was addressed to for you, or `null`. `claimed` is `null` (unclaimed), `"this-session"`, or, only with `all` (and in `thread`, which also shows any thread of the person), `"other-session"`.
- At most 20 threads: first those with new notes (waiting ones first among them), then waiting ones with nothing new; within each group, by latest note, newest first. `more` counts the rest. So threads that are only waiting, which stay until someone replies, never keep new notes out.
- `new_notes` holds the notes that have not reached this session, oldest first. That may include the person's own (written without reading, or by another session of theirs), for context: they never make a thread new, but they are part of what this session has not seen. A note's `text` is cut to 4,000 characters with `truncated: true`; the agent reads it whole with `thread`.
- A note by the server's own person (the release notes: see [Release notes](#release-notes)) carries `"system": true`, here, in `thread` and in the timeline; other notes leave it out. Clients mark such a note by this flag, never by its author's handle.
- `last_note` is the thread's latest note, whoever wrote it and whether or not it reached this session, cut like `new_notes`. So a session sees what a waiting thread is waiting on even when another of its person's sessions already fetched the answer. Showing it delivers nothing.
- Returning a thread here delivers its new notes to this session. Threads counted in `more` are not delivered.
- `from` is the person who opened the thread.
- A thread's `teams` are the teams its opening note named (as their names were then), sorted; empty for `direct` and `global` threads. `named` says whether your person is a named participant (see [Concepts](#concepts)): only then can the thread be pending for them.

### `thread`

The whole history of one thread your person takes part in, from any of their sessions. Delivers all its notes to this session. Does not claim it.

Returns the same thread object as `inbox`, with `notes` (every note, in full, oldest first, `truncated: false`) in place of `new_notes`, plus `delivery`: for each participant, in the order of `participants`, whether the thread names them (`named`), the latest note by someone else that they have, when it reached them and which session it reached first (`by`, kept after that session ends), which session of theirs claimed the thread and since when (`claimed_by`, with `since`), or `null`s, how many of their live sessions have that note (`seen_by`, 0 when none has), and, once the person has handled that note, when and from which session (`handled_at`, `handled_by`; `null` until then). `handled_at` is when they last handled a note by someone else that they had not handled yet: handling the thread again with nothing new by others keeps it. Each session shows its label and its context at that moment: when the note reached it, when it claimed the thread, when it handled it. This is how a writer sees "picked up by her agent 5 minutes ago", and tells "seen by pi, not handled yet" from "handled by Claude Code at 19:30".

```json
"delivery": [ { "handle": "ana", "named": true, "note": "n_…", "delivered_at": "2026-11-08T10:46:00Z", "by": { "label": "Claude Code laptop", "context": "pagos-api" }, "claimed_by": { "label": "Claude Code laptop", "context": "pagos-api", "since": "2026-11-08T10:47:00Z" }, "seen_by": 1,
                "handled_at": null, "handled_by": null },
              { "handle": "bob", "named": false, "note": null, "delivered_at": null, "by": null, "claimed_by": null, "seen_by": 0, "handled_at": null, "handled_by": null } ]
```

### `timeline` (read only)

What a person reads in `relevo web` (see `docs/web.md`): their threads, newest first, with their state, **without delivering, handling or claiming anything**. Reading the timeline is invisible to every agent and to the other people: no note reaches a session through it, `seen_by`, `delivery` and `handled_at` do not change, and no session is created or counted as seen (the request's session headers are ignored).

`GET /api/timeline` takes:

| Param | |
|---|---|
| `tab` | `board` (the `global` threads the person takes part in) or `threads` (their `direct` and `team` threads, and the `global` threads that name them: what is *for* them). Required. |
| `show` | optional, only with `tab=threads`: `all` (default), `waiting` (pending for the person), `answered` (opened by someone else, and the person has written in it). `closed`, accepted through 0.x, lists nothing. |
| `team` | optional, only with `tab=threads`: a team's name; only threads that named it. |
| `with` | optional, only with `tab=threads`: a handle; only threads that person takes part in. |
| `before` | optional: the `cursor` of the last thread of the previous page. |
| `limit` | optional, 1 to 50, default 20. |
| `handled_since` | optional, with either tab: a time in unix seconds. `counts` then carries `handled`: how many threads the person has handled (answered, or marked seen, a note by someone else: see `thread`'s `handled_at`) at or after that time, counted by when they last did. `/relevo` asks it with the start of the person's day, with the first page of its list, to say how many threads the person's agents handled today (see `docs/agents.md`). From 0.6.0: a server before it refuses the parameter (`BAD_REQUEST`), so a client asks the same request again without it when refused, and does without the count. |

An unknown, repeated or malformed parameter, `show`, `team` or `with` with `tab=board`, or an unknown team or person, fails with `BAD_REQUEST`.

Returns `{ "you": "ana", "teams": ["backend"], "counts": { "board": 3, "threads": 14, "waiting": 2, "answered": 6, "teams": { "backend": 3 } }, "people": ["luis", "pedro"], "names": { "ana": "Ana", "pedro": "Pedro" }, "threads": [ … ], "cursor": "…" | null }`. Threads are ordered by their latest note, newest first; `cursor` is `null` on the last page. `teams` are the teams the person is in now, sorted; `counts` count what each tab, `show` value and team of the person would list, whatever filters the request gives, so each is what the page lists when that filter is clicked (`waiting` is what `show=waiting` lists: pending for the person, a `global` thread that names them included). With `handled_since`, `counts` also has `handled` (see above); without it, it has no such field. `people` are the people the person shares a `direct` or `team` thread with, sorted (what `with` takes); `names` gives the name of the person, of everyone in `people`, and of everyone the threads name (opener, participants, authors). (The page needs names, and `GET /api/people` would count as a session seen: see `docs/web.md`.) Each thread:

```json
{ "id": "r_…", "kind": "direct" | "team" | "global", "title": "…", "from": "oscar", "participants": ["oscar", "ana"], "teams": ["backend"],
  "status": "open", "named": true, "pending": false, "notes": 3,
  "first_note": { "id": "n_…", "author": "oscar", "via": "Claude Code laptop", "context": "producto@main", "created_at": "…", "text": "…", "truncated": false },
  "last_note":  { … the same, or null when the thread holds one note },
  "last_other_note": { … the same, or null },
  "handled_by_you": true, "cursor": "…" }
```

`last_other_note` is the latest note by someone else than the person (the answer the page shows, even when the person wrote after it), `null` when nobody else has written or that note is the first one. `first_note`, `last_note` and `last_other_note` are cut to 1,000 characters (`truncated`). `context` is the writing session's context then (`null` for a general one, and for a note written before 0.4.0, which did not keep it). `notes` counts every note. `handled_by_you` says the person has handled the latest note by someone else (as in `delivery`; `false` while nobody else has written); `status` is always `"open"` (kept through 0.x). `participants` of a `global` thread is left empty (`[]`): it would list everyone; `notes` and the notes say enough.

`GET /api/timeline/threads/:id` returns one such thread with `notes`, every note in full, oldest first (as `thread` does, with `context` on each), in place of the count and of `first_note`, `last_note` and `last_other_note`, and with `names` (as above, for this thread). A thread the person does not take part in is `NOT_FOUND`, as is an id that cannot be decoded. Neither route delivers anything.

Both routes need only the token: the session headers are not read at all (a malformed one is not an error), since no session is involved. They fail with `UNAUTHORIZED` like every route.

## HTTP

Every route needs `Authorization: Bearer <token>`, and takes the optional `Relevo-Session`, `Relevo-Context` and `Relevo-Client` headers, and, through 0.x, `Relevo-Mentions: 1` (a client that knows a reply's mentions name people; see `send`). Bodies and responses are JSON. A malformed session id or context, or a `Relevo-Mentions` other than `1`, fails with `BAD_REQUEST` (checked after the token). So does an unknown, repeated or malformed `inbox` parameter. A malformed `Relevo-Client` is ignored, never an error (see [Sessions](#sessions-and-routing)).

```
GET  /api/inbox?all=1&wait=30
GET  /api/threads/:id
POST /api/send            { text?, thread?, claim?, seen?, key? }   (to, done, close: deprecated, see send)
GET  /api/people          { "team": [...], "teams": [...], "you": "ana" }  (same lists as inbox)
GET  /api/timeline?tab=board|threads&show=&team=&with=&before=&limit=&handled_since=   read only, delivers nothing (see timeline)
GET  /api/timeline/threads/:id   one thread in full, read only
DELETE /api/session        forget this session (the one the headers name) and release its claims; { "ok": true }
GET  /api/dist/manifest.json   the signed list of the clients this server hands out (see docs/update.md)
GET  /api/dist/manifest.sig
GET  /api/dist/<name>          a file the manifest lists
GET  /health              no token; { "ok": true }
```

A token may hold at most 10 waiting `inbox` requests at once (requests with `wait` above 0, counted from when they arrive until they answer or the client goes away); beyond that, `RATE_LIMITED`. A waiting request holds no database lock; it looks again whenever a note lands in a thread of its person, the moment a claim of one of their sessions lapses, and when a session forgotten because its token started a 21st, or ended with `DELETE /api/session`, releases one of their threads, and when a `seen` (or a note sent with `close`) releases a claim on a thread of theirs. (A token revoked from the CLI releases its threads from another process, which wakes nobody: waiting requests find them when they next look.)

Errors: `{ "error": { "code": "…", "message": "…", …extra } }` with these statuses:

| Code | Status |
|---|---|
| `UNAUTHORIZED` (missing, unknown or revoked token; inactive person) | 401 |
| `NOT_FOUND` | 404 |
| `BAD_REQUEST`, `UNKNOWN_PERSON` (+ `valid`: the people, `teams`: the teams, `unknown`: the names not known), `NO_RECIPIENTS`, `TOO_LARGE` | 400 (413 for a body over 400 KiB) |
| `TEAM_LIMIT`, `RATE_LIMITED`, `THREAD_BUSY` | 429 |
| `INTERNAL` (a server fault; details go to the log only) | 500 |

An unknown route, a known route with the wrong method, and a thread id that cannot be decoded are `NOT_FOUND` too, with the same JSON shape.

Ids: threads `r_` + 12 base32 chars, notes `n_` + 12, tokens `t_` + 12 (random). Times: RFC 3339 UTC.

## Administration (CLI)

There is no sign-up. The admin runs the binary on the server:

```
relevo serve [--addr 127.0.0.1:7878] [--db relevo.db] [--team-questions-per-day 5] [--tls [--tls-name NAME]… | --tls-cert PEM --tls-key PEM] [--dist DIR] [--no-release-notes]
relevo tls-fingerprint [--db relevo.db]                                                  # SHA-256 of the certificate serve --tls uses
relevo invite <handle> --name "Ana" [--about "payments backend"] [--label "Claude Code"]   # creates the person (or reuses), prints a new token once
relevo token add <handle> --label "Codex desktop"                                          # another agent for the same person (active people only)
relevo token list [<handle>]
relevo token revoke <token-id>
relevo people
relevo sessions [<handle>]                                                                 # sessions of active people seen in the last 7 days: handle, label, session id (or default), context (or general), last seen, client (name/version, `?` after a version inferred from an old client's User-Agent, or unknown)
relevo clients [<handle>] [--outdated]                                                     # the client and version of each session seen in the last 7 days (below)
relevo about <handle> "new line"
relevo deactivate <handle>                                                                 # also revokes every token of theirs
relevo team add <name> [--title "Backend"]                                                 # a new team, with nobody in it
relevo team join <name> <handle>…                                                          # puts active people in the team
relevo team leave <name> <handle>…                                                         # takes people out of it
relevo team remove <name>                                                                  # deletes the team (its threads stay, under its name)
relevo teams                                                                               # every team: name, title, members
relevo backup <dest-file>                                                                  # a consistent copy, while the server runs
relevo backup --keep 14 <dest-dir>                                                         # writes <dest-dir>/YYYYMMDD-HHMMSS.db (UTC), keeps the newest 14
relevo check-db                                                                            # whether this version can use the database, writing nothing
```

`relevo clients` says who runs an older client after the server is upgraded. For every active person (not `relevo`), or one, it lists each session seen in the last 7 days, tab-separated: handle, label, session id (or `default`), context (or `general`), client name, version, last seen, and how the version stands against the server's own: `current` (the same), `outdated` (older, in semver order), `newer` (ahead of the server), `unknown` (the session said nothing, or a version that cannot be read). A version read from the User-Agent ends in `?` (`0.4.4?`); an unknown client is `unknown`, its version `-`. Outdated sessions come first, then unknown, newer and current; within each, by handle, most recently seen first. A last line sums them all up: `3 sessions on 0.5.0, 2 outdated, 1 unknown` (and `, 1 newer` when there are). `--outdated` lists only the outdated and unknown sessions (the summary still counts them all).

The database records the version of its layout. From layout 6 on (relevo 0.3.0), `relevo serve` migrates a database of layout 5 (relevo 0.1.0 to 0.2.x) in place when it starts, keeping what layout 5 said was waiting for a reply: a `direct` thread that was waiting for a person there (open, its latest note not theirs) is handled up to their own latest note in it, so it stays pending for them even if it had reached one of their sessions; every other thread is handled up to its latest note. Reaching a session is not taken as handling: `handled_at` is set only when the person wrote after the latest note by someone else (as when they first did), and is `null` otherwise. As with any pending thread, a new session of the person gets the notes they have not handled as new, once; the person's sessions that already had them see them as waiting, as before, not as new. Only `serve` migrates: every other command that opens the database refuses one of layout 5, before writing anything, and says to start `serve` first (and to keep a `relevo backup` before, which copies it as it is), so an admin command never changes the layout behind the owner's back. Every command refuses any other older layout, and any newer one, and says so; such a database must be deleted (with its `-wal` and `-shm` files) and the team invited again.

Layout 7 (relevo 0.4.0) adds teams, the `global` kind and named participants. `relevo serve` migrates a database of layout 6 (relevo 0.3.x) to it when it starts, and one of layout 5 through layout 6 first: every `team` thread becomes a `global` thread (a question to everyone was what it meant), whose participants are not named; every participant of a `direct` thread is named; delivery, handled and claims stay as they were, so nothing becomes pending or stops being pending. A `send` key used before the migration replays its first result with `to` and `added` empty, and `kind` and `reached` as the thread is when it replays (layout 6 kept no more). As with layout 5, only `serve` migrates: every other command refuses a database of layout 5 or 6, before writing anything, and says to start `serve` first.

Layout 8 (relevo 0.5.0): threads no longer close, and the database keeps the last version whose release notes it announced (the `meta` table: see [Release notes](#release-notes)). `relevo serve` migrates a database of layout 7 (relevo 0.4.x) to it when it starts, and one of layout 5 or 6 through the earlier layouts first: every closed thread is open; each participant it names has it handled up to its latest note, so nothing becomes pending; and its latest note has reached every participant, named or not, and every session of theirs, so it comes back to nobody's inbox as new (0.4.x left a closed `team` or `global` thread out of the inbox of whom it did not name). Where that handles or delivers a note by someone else, `handled_at` and `delivered_at` are when the thread was closed (or its latest note, for one closed before 0.4.0 kept that), and `handled_by` and `by` are `null`. A `send` key used to close replays with `status` `"open"`. The server logs how many threads it opened. As before, only `serve` migrates: every other command refuses a database of layout 5, 6 or 7, before writing anything, and says to start `serve` first.

Layout 9 (relevo 0.5.0, too): each session keeps what its latest request said of its client (`Relevo-Client`, see [Sessions](#sessions-and-routing)). Layout 8 was never released, but development builds of 0.5.0 made it: `relevo serve` migrates a database of layout 8 to 9 when it starts, and one of an older layout through the earlier steps first, logging a line per step. Every session is kept, and is unknown until its next request. As before, only `serve` migrates: every other command refuses a database of layout 5 to 8, before writing anything, and says to start `serve` first; `relevo check-db` says which layout it found (layout 8: "a development build of relevo 0.5.0").

Processes that open the same database at the same time (two `serve` started at once on a new file, or on a copy `backup` wrote) each wait for the others, for up to 15 seconds in all, and all open it: the file is made, or moved to WAL mode, once, and its layout is read whole (its version and its tables at one moment), so a database being made is never taken for one of before 0.1.0.

`check-db` says whether this version can use the database: as it is, once `serve` migrates it (exit 0), or not, and why (exit 1). It writes nothing, not even `-wal` or `-shm` files: a database without them (no server has it open) is read as immutable, one with them read-only through the server's. It never creates a database. `deploy/install.sh` asks it before it replaces the binary.

`backup` copies the database with SQLite's `VACUUM INTO`: one consistent snapshot, taken without stopping the server (a plain copy of a database in WAL mode can miss what is still in its `-wal` file). It only reads the database, as `check-db` does: it never creates one, and copies one made by another version too, so it can be kept before an upgrade. The copy is created 0600 under a hidden name, checked (`PRAGMA quick_check`), and only then given its name; an existing file is never overwritten. When `<dest>` is a directory (with `--keep N` it must be one, and is created 0700 if missing), the copy is named `YYYYMMDD-HHMMSS.db` (UTC), and `--keep N` then removes the oldest files named that way beyond the newest N, the new one always among them; other files there are left alone. Restoring is putting a copy in place of the database while the server is stopped (and deleting the old database's `-wal` and `-shm` files).

`--db` defaults to `RELEVO_DB` or `./relevo.db`; only `serve` creates `./relevo.db` that way, and the other commands refuse to run when neither names a database and there is no `./relevo.db` (so an admin command run before `RELEVO_DB` is set cannot fill a database the server never reads); `--addr` to `RELEVO_ADDR` or `127.0.0.1:7878`; `--team-questions-per-day` to `RELEVO_TEAM_QUESTIONS_PER_DAY` or 5; `--tls` to `RELEVO_TLS` (`true` or `false`), `--tls-name` to `RELEVO_TLS_NAME` (comma-separated), `--tls-cert` and `--tls-key` to `RELEVO_TLS_CERT` and `RELEVO_TLS_KEY`, and `--dist` (the clients it hands out, docs/update.md) to `RELEVO_DIST` or `dist/` next to the database, so that a systemd drop-in can turn TLS on with `Environment=` lines. `--tls` and `--tls-cert` exclude each other, `--tls-cert` and `--tls-key` go together, and `--tls-name` needs `--tls`; `serve` refuses anything else before it opens the database (`RELEVO_TLS=false` counts as no `--tls`).

`serve` can speak HTTPS itself, with no proxy in front:

- `relevo serve --tls` uses a certificate of its own. The first start creates a self-signed one (ECDSA P-256, valid 10 years) for the names in `--tls-name` (repeatable: host names or IP addresses; by default `localhost`, `127.0.0.1`, `::1`, the machine's host name and its non-loopback addresses) and keeps it next to the database as `<db>.tls/cert.pem` and `key.pem` (the key mode 0600, the directory 0700). Later starts reuse it; `--tls-name` with names the stored certificate lacks makes a new one, and says so. At every start it prints the certificate's SHA-256 fingerprint; `relevo tls-fingerprint` prints it too. Without `--tls-name`, it logs a warning at every start when the certificate has none of the machine's non-loopback addresses, or when it finds none (they could not be read): clients that connect by address would be refused.
- `relevo serve --tls-cert <pem> --tls-key <pem>` uses a certificate you already have (one from Let's Encrypt or the office's own authority).
- Without TLS, `serve` speaks plain HTTP, as before: keep `--addr` on loopback, or behind a proxy or a VPN that encrypts. It logs a warning when `--addr` is not a loopback address and TLS is off.

Whatever the transport, the server limits what a client can hold: 10 seconds to send the request head, which may take at most 16 KiB (a longer one is refused with 431, so a connection holds little before its token is checked), 30 seconds to send the body, an idle connection closed after 75 seconds, a request answered within 90 seconds (a waiting `inbox` answers by 60), at most 1,024 connections at once (beyond that, new ones wait) and 64 from one address (beyond that, new ones are closed unanswered). Loopback is not counted against one address's share: behind a proxy on the same machine, every connection comes from there, the whole team's. Clients behind one NAT share one address's 64. These replace what the proxy used to enforce; a proxy in front stays possible.

`invite` on an existing handle reactivates the person, sets the new `--name`, and `--about` if given. `team` and `equipo` cannot be handles (reserved: see [Mentions](#mentions)), nor can the server's own person `relevo` (the one the server made, by its id: a person who had the handle before 0.5.0 stays manageable): `invite`, `token add`, `about`, `deactivate`, `team add`, `team join` and `team leave` refuse it, and `people` and `token list` leave it out, nor can the name of a team: `invite` refuses one, and `team add` refuses the handle of a person, active or not, and `team` and `equipo`. `--label` defaults to `agent`. `deactivate` revokes every token of the person, for good: inviting them back issues a new token, and their old ones never work again. A deactivated person stays in their teams, but is not reached through them while inactive.

`team join` and `team leave` take one or more handles; joining someone already in the team, or leaving someone not in it, is not an error, and says so. `team join` refuses an unknown or inactive handle, and changes nothing then. A team's membership changes only who later notes reach: threads already open keep their participants. `team remove` deletes the team and its membership; its threads keep its name in `teams`, and a later team may take the name. `teams` lists every team with its title and members, inactive members marked.

### Release notes

When `relevo serve` starts on a release whose version is newer (semver order: build metadata after `+` ignored, pre-release identifiers compared one by one, numbers as numbers, so `rc.10` comes after `rc.9`) than the last version the database announced, the person `relevo` posts, once the server listens (bound, with its TLS set up), that version's release notes on the global board, then the database records the version. The note (its `via` is `release notes`) has `Relevo <version>` as its first line, then a blank line, then that version's section of `CHANGELOG.md` (built into the binary): the text under its `## <version>` heading (the version alone, or followed by a space and its date), up to the next `## ` heading or the end (a `## ` line inside fenced code is text; `\r\n` line ends read as `\n`). Cut at a line to fit a note (64 KiB) when it is longer, it then ends with a line saying the rest is in the CHANGELOG. It is posted exactly once per version: across restarts, and with several processes starting at once (the check and the post are one transaction).

- A new database posts nothing: made by a release, it records that version; made by a build between releases (below), it records nothing, so the release posts its notes on it. A database that has announced this version, or a newer one (a restart, a downgrade), posts nothing either. A database migrated from layout 7 as the server starts had announced nothing, so the version it moves to posts its notes.
- Only a release posts: its version's section must be the CHANGELOG's first one, with a heading not marked "(unreleased)". A build between releases (a 0.4.4 binary while the CHANGELOG is topped by `0.5.0 (unreleased)`, or a 0.5.0 build before its date is set) posts nothing and records nothing, and says so in the log; the release itself posts.
- A last announced version that cannot be read is taken as older (and logged): this version's notes are posted, and it is written over.
- `--no-release-notes` (or `RELEVO_RELEASE_NOTES=0`, `false`, `no` or `off`) records a release's version without posting, and says so in the log, so turning them on later never posts an older version's notes. A build between releases records nothing with them off too, so the release still posts.
- If a person or a team had the name `relevo` before 0.5.0 reserved it, the server has no person of its own: nothing is posted (the version is recorded, and a warning logged).
- The note opens a plain `global` thread, as a note with no mention does: every active person takes part, nobody is named, so it is pending for nobody, and new for every session once. It counts against no limit (`TEAM_LIMIT`, the per-minute one, and the thread's 20 notes an hour: twenty replies may follow it within the hour), and anyone may reply in it as on any board thread. A failure to post is logged, and the server starts all the same.

The database keeps the version announced in a small `meta` table (key, value), which layout 8 brings, with the id of the person `relevo` once it is made.

## Privacy and logs

The server logs counts and ids, never note text or tokens. The database holds token hashes only, but note text in plain form, so the server creates it (and SQLite its `-wal` and `-shm` files) readable by the server's user only (mode 0600).

## Later (not v0)

Consent between servers, webhooks, OAuth for ChatGPT, A2A gateway, attachments.
