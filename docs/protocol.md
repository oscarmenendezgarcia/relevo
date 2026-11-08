# Relevo protocol, v0

Relevo is a small server that keeps and delivers notes between the people of one team. Every person reaches it through their own agent, by MCP or plain HTTP. There is no user interface in v0.

One server serves one team. Everyone on it may write to everyone else: the team is the trust boundary. Consent between teams comes later.

## Concepts

| Thing | What it is |
|---|---|
| **Person** | Someone on the team. `handle` (unique, `^[a-z0-9][a-z0-9_-]{1,31}$`), `name`, and `about`: one line on what they or their agent know or look after ("payments backend"). Can be deactivated. The identity is the person, never the agent. |
| **Token** | One per agent of a person, with a `label` ("Claude Code laptop"). Secret format `rlv_` + 43 chars of base64url (32 random bytes). Only its SHA-256 is stored. Can be revoked, and is when its person is deactivated. A request made with it acts as its person, and every note it writes records the label. |
| **Thread** | A conversation. It is `direct` (between named people) or `team` (a question to everyone). Opened by its first note; the first non-empty line of that note, without leading `#`s, cut to 80 characters, is its title. It is `open` or `closed`. |
| **Note** | Markdown text, 1 byte to 64 KiB (UTF-8), not only whitespace. Written by a person through one of their tokens. Must be self-contained: whoever reads it does not share the writer's context. |
| **Session** | One running agent of a person: a terminal, an editor window, a bot. A person may have several at once, each in its own context. See [Sessions and routing](#sessions-and-routing). |
| **Context** | What a session is working on, usually the repository it runs in: `pagos-api`. Lowercase, `^[a-z0-9][a-z0-9._-]{0,63}$`. A session without one is a *general* session. |
| **Participant** | A person in a thread. For `direct`, the sender and the recipients. For `team`, every active person at the time the question is asked. Participants are fixed: a person deactivated later stays a participant of their threads and keeps appearing in `participants` and `delivery` (with the last note that reached them), and the others may still write there. While inactive they are added to no new thread: not to team threads, and `to` cannot name them. Invited back, they find their threads again. |

## Sessions and routing

A person may run several agents at once, each with its own context: a terminal in `pagos-api`, another in `frontend`, a bot. A note must reach the right one, and only one of them should take it.

**Sessions.** Every request names its session with two optional headers:

- `Relevo-Session`: an id the client picks once per running agent, `^[A-Za-z0-9_-]{8,64}$`. Sessions belong to a token: the same id under another token is another session. Without the header, the request uses the token's *default session*.
- `Relevo-Context`: the session's context. Sent with each request; the latest value wins. Without it (or empty), the session is general. A default session is always general unless the header says otherwise. Like addresses, it may be written in any case: `Pagos-API` is `pagos-api`.

The server keeps each session with its token's label, its context and when it was last seen. A session not seen for 7 days is forgotten, and so are its claims.

**Addressing.** In `to`, `ana` means Ana, any of her sessions; `ana/pagos-api` means Ana, in the context `pagos-api`. The context does not need a live session: the thread waits for one.

**Claiming.** For each participant a thread is *unclaimed* or *claimed* by one of their sessions. A session claims a thread for its person the moment it writes a note in it, taking it over from any other session of the same person. Closing without text does not claim.

**Which sessions see a thread.** For a participant, a thread shows in the inbox of one of their sessions when it is claimed by that session, or when it is unclaimed and either it has no context for them, or its context is the session's context, or the session is general. So an unaddressed note shows in every session of the person until one of them answers; then only there. A note for `ana/pagos-api` shows in Ana's `pagos-api` sessions and her general ones. `inbox` with `all` and `thread` ignore this and show any thread of the person.

## Delivery state

Delivery is kept per session. For each session the server knows the last note of each thread that reached it. A note reaches a session when that session fetches it through `inbox` or `thread`. A session starts with what its person already has: a note that had reached one of their sessions before this one was first seen counts as having reached it, so a new terminal does not replay the person's whole history; notes that reached none of them are new for it too.

A thread is **new for a session** when it holds notes, not written by the session's person, that have not reached that session. A person's own notes are never new for any of their sessions. When a session writes a note, everything in the thread up to that note has reached it, unless earlier notes had not: then nothing moves, so a reply written without reading never hides a note.

A thread is **waiting** for a person when it is `direct`, `open`, and its latest note is not theirs. It stays in the inbox of the sessions that see it until the person replies or someone closes it, even after it was delivered.

A `team` thread is never "waiting": it shows only while it has notes new for the session. So a teammate's agent with nothing to add does not keep seeing it.

For others, a participant's delivery is the latest note that reached any of their sessions, and which session (its label, and its context at that moment): "picked up by Claude Code in pagos-api, 5 minutes ago".

## Operations

All three are the same over HTTP and MCP (MCP arrives in block 2).

### `send`

Opens a thread, or writes in one.

| Field | Type | |
|---|---|---|
| `to` | array of addresses (`handle` or `handle/context`), or `["@team"]` | Opens a new thread. Exactly one of `to` and `thread`. |
| `thread` | thread id | Writes in an existing thread. |
| `text` | string | The note. Required unless only closing (`thread` + `close: true` + no `text`). |
| `close` | bool, optional | Marks the thread closed after writing. |
| `key` | string, 1 to 100 characters, optional | Idempotency key. A repeated `send` from the same person with the same key, within 24 hours of the `send` that used it, returns the first result and writes nothing. Only a successful `send` uses up a key. After 24 hours the key expires: the same key is a fresh `send`, which uses it up for another 24 hours. The server forgets expired keys. |

The body is a JSON object; anything else (an array, say), or any other field, fails with `BAD_REQUEST`. Characters here and below are Unicode scalar values.

Rules:

- `to` holds 1 to 20 addresses of distinct active people other than you, or exactly `["@team"]`. An address is a handle, optionally followed by `/` and a context (`ana/pagos-api`); naming the same person twice with different contexts (or once with a context and once without) fails with `BAD_REQUEST`, and so does a malformed context or a context on `@team`. Addresses may be written with or without a leading `@` and in any case; repeats are merged (`["@team", "equipo"]` is one `@team`). Writing to yourself, or `@team` next to other handles, fails with `BAD_REQUEST`. An unknown or inactive handle fails with `UNKNOWN_PERSON`, whose error lists the valid handles with their `about`.
- `["@team"]` opens a `team` thread. `@equipo` is accepted as the same thing (both with or without `@`); `team` and `equipo` can therefore not be handles. A person may open at most `RELEVO_TEAM_QUESTIONS_PER_DAY` (default 5) team threads in any rolling 24 hours: beyond that, `TEAM_LIMIT`. A team of one (only you) fails with `NO_RECIPIENTS`.
- Writing a note claims the thread for your person, for the session that wrote it (see [Claiming](#sessions-and-routing)).
- Writing in a thread you are not a participant of fails with `NOT_FOUND` (the thread's existence is not revealed).
- Writing in a closed thread fails with `THREAD_CLOSED`. Open a new thread instead.
- Closing: in a `direct` thread any participant may close it; in a `team` thread only the person who asked.
- Text over 64 KiB (counted once decoded from JSON) fails with `TOO_LARGE` (400); empty or blank text with `BAD_REQUEST`. The whole request body may be up to 400 KiB, so a note of 64 KiB fits however the client escapes it.
- Loop guard: a thread accepts at most 20 notes in any rolling hour. The 21st fails with `THREAD_BUSY`, whose message tells the agent to check with its person before going on. (The server cannot tell a person from an agent, so this caps speed, not authorship.)
- A person may send at most 60 notes per rolling minute: beyond that, `RATE_LIMITED`.
- The limits count notes only: a close without text is never limited.

Returns `{ "thread": "<id>", "note": "<id>", "status": "open" | "closed" }`. `note` is `null` when the `send` only closed the thread.

### `inbox`

What is new for this session, and what is waiting for its person, among the threads this session sees (see [Sessions and routing](#sessions-and-routing)).

| Param | |
|---|---|
| `all` | bool (`1`, `true`, `0`, `false`), optional. Show every thread of the person, ignoring routing and claims. For when the person asks "do I have anything?" from any terminal. |
| `wait` | seconds, 0 to 60, optional (HTTP only). If nothing is new for this session, hold the request until something is or the time runs out, then answer as usual. Waiting threads alone do not count as new. This is how bridges and bots learn of a note at once. |

Returns:

```json
{
  "team": [ { "handle": "ana", "name": "Ana", "about": "payments backend", "contexts": ["pagos-api", "frontend"] } ],
  "session": { "label": "Claude Code laptop", "context": "pagos-api" },
  "threads": [
    {
      "id": "r_8f2k…",
      "kind": "direct" | "team",
      "title": "Refunds endpoint",
      "from": "oscar",
      "participants": ["oscar", "ana"],
      "status": "open" | "closed",
      "waiting_for_you": true,
      "context": "pagos-api",
      "claimed": "this-session",
      "new_notes": [
        { "id": "n_…", "author": "oscar", "via": "Claude Code laptop", "created_at": "2026-11-08T10:41:00Z",
          "text": "…", "truncated": false }
      ]
    }
  ],
  "more": 0
}
```

- `team` lists every active person, you included, so the agent knows whom to ask. `contexts` are the distinct contexts of their sessions seen in the last 15 minutes, sorted.
- `session` is the session this request counts as.
- A thread's `context` is the context it was addressed to for you, or `null`. `claimed` is `null` (unclaimed), `"this-session"`, or, only with `all` (and in `thread`, which also shows any thread of the person), `"other-session"`.
- At most 20 threads: first those with new notes (waiting ones first among them), then waiting ones with nothing new; within each group, by latest note, newest first. `more` counts the rest. So threads that are only waiting, which stay until someone replies, never keep new notes out.
- `new_notes` holds the notes that have not reached this session, oldest first. That may include the person's own (written without reading, or by another session of theirs), for context: they never make a thread new, but they are part of what this session has not seen. A note's `text` is cut to 4,000 characters with `truncated: true`; the agent reads it whole with `thread`.
- Returning a thread here delivers its new notes to this session. Threads counted in `more` are not delivered.
- `from` is the person who opened the thread.

### `thread`

The whole history of one thread your person takes part in, from any of their sessions. Delivers all its notes to this session. Does not claim it.

Returns the same thread object as `inbox`, with `notes` (every note, in full, oldest first, `truncated: false`) in place of `new_notes`, plus `delivery`: for each participant, in the order of `participants`, the latest note they have and when it reached them, or `null`s. This is how a writer sees "picked up by her agent 5 minutes ago".

```json
"delivery": [ { "handle": "ana", "note": "n_…", "delivered_at": "2026-11-08T10:46:00Z", "by": { "label": "Claude Code laptop", "context": "pagos-api" }, "claimed_by": { "label": "Claude Code laptop", "context": "pagos-api" } },
              { "handle": "bob", "note": null, "delivered_at": null, "by": null, "claimed_by": null } ]
```

## HTTP

Every route needs `Authorization: Bearer <token>`, and takes the optional `Relevo-Session` and `Relevo-Context` headers. Bodies and responses are JSON. A malformed session id or context fails with `BAD_REQUEST` (checked after the token). So does an unknown, repeated or malformed `inbox` parameter.

```
GET  /api/inbox?all=1&wait=30
GET  /api/threads/:id
POST /api/send            { to | thread, text, close?, key? }
GET  /api/people          { "team": [...] }  (same list as inbox)
GET  /health              no token; { "ok": true }
```

A token may hold at most 10 waiting `inbox` requests at once (requests with `wait` above 0, counted from when they arrive until they answer or the client goes away); beyond that, `RATE_LIMITED`. A waiting request holds no database lock; it looks again whenever a note lands in a thread of its person, or a forgotten session releases one of their threads.

Errors: `{ "error": { "code": "…", "message": "…", …extra } }` with these statuses:

| Code | Status |
|---|---|
| `UNAUTHORIZED` (missing, unknown or revoked token; inactive person) | 401 |
| `NOT_FOUND` | 404 |
| `BAD_REQUEST`, `UNKNOWN_PERSON` (+ `valid`: the team list), `NO_RECIPIENTS`, `TOO_LARGE` | 400 (413 for a body over 400 KiB) |
| `THREAD_CLOSED`, `FORBIDDEN` (closing a team thread you did not open) | 409, 403 |
| `TEAM_LIMIT`, `RATE_LIMITED`, `THREAD_BUSY` | 429 |
| `INTERNAL` (a server fault; details go to the log only) | 500 |

An unknown route, a known route with the wrong method, and a thread id that cannot be decoded are `NOT_FOUND` too, with the same JSON shape.

Ids: threads `r_` + 12 base32 chars, notes `n_` + 12, tokens `t_` + 12 (random). Times: RFC 3339 UTC.

## Administration (CLI)

There is no sign-up. The admin runs the binary on the server:

```
relevo serve [--addr 127.0.0.1:7878] [--db relevo.db] [--team-questions-per-day 5]
relevo invite <handle> --name "Ana" [--about "payments backend"] [--label "Claude Code"]   # creates the person (or reuses), prints a new token once
relevo token add <handle> --label "Codex desktop"                                          # another agent for the same person (active people only)
relevo token list [<handle>]
relevo token revoke <token-id>
relevo people
relevo sessions [<handle>]                                                                 # sessions seen in the last 7 days: label, context, last seen
relevo about <handle> "new line"
relevo deactivate <handle>                                                                 # also revokes every token of theirs
```

`--db` defaults to `RELEVO_DB` or `./relevo.db`; `--addr` to `RELEVO_ADDR` or `127.0.0.1:7878`; `--team-questions-per-day` to `RELEVO_TEAM_QUESTIONS_PER_DAY` or 5.

`serve` speaks plain HTTP and sets no header-read or idle timeouts and no connection limit. Keep `--addr` on loopback and let teammates reach it only through a reverse proxy that terminates TLS and enforces those timeouts and limits: otherwise tokens and notes cross the network in the clear. The server logs a warning when `--addr` is not a loopback address.

`invite` on an existing handle reactivates the person, sets the new `--name`, and `--about` if given. `team` and `equipo` cannot be handles (they mean the whole team). `--label` defaults to `agent`. `deactivate` revokes every token of the person, for good: inviting them back issues a new token, and their old ones never work again.

## Privacy and logs

The server logs counts and ids, never note text or tokens. The database holds token hashes only, but note text in plain form, so the server creates it (and SQLite its `-wal` and `-shm` files) readable by the server's user only (mode 0600).

## Later (not v0)

MCP tools (block 2), consent between teams, webhooks, OAuth for ChatGPT, A2A gateway, attachments.
