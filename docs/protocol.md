# Relevo protocol, v0

Relevo is a small server that keeps and delivers notes between the people of one team. Every person reaches it through their own agent, by MCP or plain HTTP. There is no user interface in v0.

One server serves one team. Everyone on it may write to everyone else: the team is the trust boundary. Consent between teams comes later.

## Concepts

| Thing | What it is |
|---|---|
| **Person** | Someone on the team. `handle` (unique, `^[a-z0-9][a-z0-9_-]{1,31}$`), `name`, and `about`: one line on what they or their agent know or look after ("payments backend"). Can be deactivated. The identity is the person, never the agent. |
| **Token** | One per agent of a person, with a `label` ("Claude Code laptop"). Secret format `rlv_` + 43 chars of base64url (32 random bytes). Only its SHA-256 is stored. Can be revoked. A request made with it acts as its person, and every note it writes records the label. |
| **Thread** | A conversation. It is `direct` (between named people) or `team` (a question to everyone). Opened by its first note; the first non-empty line of that note, without leading `#`s, cut to 80 characters, is its title. It is `open` or `closed`. |
| **Note** | Markdown text, 1 byte to 64 KiB (UTF-8), not only whitespace. Written by a person through one of their tokens. Must be self-contained: whoever reads it does not share the writer's context. |
| **Participant** | A person in a thread. For `direct`, the sender and the recipients. For `team`, every active person at the time the question is asked. |

## Delivery state

For each participant the server keeps the last note of the thread that reached them (`delivered_note_id`, `delivered_at`). A note reaches a person when their agent fetches it through `inbox` or `thread`. The writer of a note has it delivered to them the moment they write it, unless notes before it had not reached them yet: then nothing moves, and those notes (with the writer's own) stay new for them, so a reply written without reading never hides a note.

A thread is **new for you** when it holds notes not yet delivered to you.

A thread is **waiting for you** when it is `direct`, `open`, and its latest note is not yours. It stays in your inbox until you reply or someone closes it, even after it was delivered.

A `team` thread is never "waiting": it shows in your inbox only while it has notes new for you. So a teammate's agent that has nothing to add does not keep seeing it.

## Operations

All three are the same over HTTP and MCP (MCP arrives in block 2).

### `send`

Opens a thread, or writes in one.

| Field | Type | |
|---|---|---|
| `to` | array of handles, or `["@team"]` | Opens a new thread. Exactly one of `to` and `thread`. |
| `thread` | thread id | Writes in an existing thread. |
| `text` | string | The note. Required unless only closing (`thread` + `close: true` + no `text`). |
| `close` | bool, optional | Marks the thread closed after writing. |
| `key` | string, 1 to 100 characters, optional | Idempotency key. A repeated `send` from the same person with the same key returns the first result and writes nothing. Only a successful `send` uses up a key; keys do not expire. |

Any other field fails with `BAD_REQUEST`. Characters here and below are Unicode scalar values.

Rules:

- `to` holds 1 to 20 distinct handles of active people other than you, or exactly `["@team"]`. Handles may be written with or without a leading `@` and in any case; repeats are merged. Writing to yourself, or `@team` next to other handles, fails with `BAD_REQUEST`. An unknown or inactive handle fails with `UNKNOWN_PERSON`, whose error lists the valid handles with their `about`.
- `["@team"]` opens a `team` thread. `@equipo` is accepted as the same thing (both with or without `@`); `team` and `equipo` can therefore not be handles. A person may open at most `RELEVO_TEAM_QUESTIONS_PER_DAY` (default 5) team threads in any rolling 24 hours: beyond that, `TEAM_LIMIT`. A team of one (only you) fails with `NO_RECIPIENTS`.
- Writing in a thread you are not a participant of fails with `NOT_FOUND` (the thread's existence is not revealed).
- Writing in a closed thread fails with `THREAD_CLOSED`. Open a new thread instead.
- Closing: in a `direct` thread any participant may close it; in a `team` thread only the person who asked.
- Text over 64 KiB fails with `TOO_LARGE` (400); empty or blank text with `BAD_REQUEST`.
- Loop guard: a thread accepts at most 20 notes in any rolling hour. The 21st fails with `THREAD_BUSY`, whose message tells the agent to check with its person before going on. (The server cannot tell a person from an agent, so this caps speed, not authorship.)
- A person may send at most 60 notes per rolling minute: beyond that, `RATE_LIMITED`.
- The limits count notes only: a close without text is never limited.

Returns `{ "thread": "<id>", "note": "<id>", "status": "open" | "closed" }`. `note` is `null` when the `send` only closed the thread.

### `inbox`

What is new for you, and what is waiting for you.

Returns:

```json
{
  "team": [ { "handle": "ana", "name": "Ana", "about": "payments backend" } ],
  "threads": [
    {
      "id": "r_8f2k…",
      "kind": "direct" | "team",
      "title": "Refunds endpoint",
      "from": "oscar",
      "participants": ["oscar", "ana"],
      "status": "open" | "closed",
      "waiting_for_you": true,
      "new_notes": [
        { "id": "n_…", "author": "oscar", "via": "Claude Code laptop", "created_at": "2026-11-08T10:41:00Z",
          "text": "…", "truncated": false }
      ]
    }
  ],
  "more": 0
}
```

- `team` lists every active person, you included, so the agent knows whom to ask.
- At most 20 threads, waiting ones first, then by latest note, newest first. `more` counts the rest.
- `new_notes` holds the notes not yet delivered to you, oldest first. A note's `text` is cut to 4,000 characters with `truncated: true`; the agent reads it whole with `thread`.
- Returning a thread here delivers its new notes to you. Threads counted in `more` are not delivered.
- `from` is the person who opened the thread.

### `thread`

The whole history of one thread you take part in. Delivers all its notes to you.

Returns the same thread object as `inbox`, with `notes` (every note, in full, oldest first, `truncated: false`) in place of `new_notes`, plus `delivery`: for each participant, in the order of `participants`, the latest note they have and when it reached them, or `null`s. This is how a writer sees "picked up by her agent 5 minutes ago".

```json
"delivery": [ { "handle": "ana", "note": "n_…", "delivered_at": "2026-11-08T10:46:00Z" },
              { "handle": "bob", "note": null, "delivered_at": null } ]
```

## HTTP

Every route needs `Authorization: Bearer <token>`. Bodies and responses are JSON.

```
GET  /api/inbox
GET  /api/threads/:id
POST /api/send            { to | thread, text, close?, key? }
GET  /api/people          { "team": [...] }  (same list as inbox)
GET  /health              no token; { "ok": true }
```

Errors: `{ "error": { "code": "…", "message": "…", …extra } }` with these statuses:

| Code | Status |
|---|---|
| `UNAUTHORIZED` (missing, unknown or revoked token; inactive person) | 401 |
| `NOT_FOUND` | 404 |
| `BAD_REQUEST`, `UNKNOWN_PERSON` (+ `valid`: the team list), `NO_RECIPIENTS`, `TOO_LARGE` | 400 (413 for a body over 80 KiB) |
| `THREAD_CLOSED`, `FORBIDDEN` (closing a team thread you did not open) | 409, 403 |
| `TEAM_LIMIT`, `RATE_LIMITED`, `THREAD_BUSY` | 429 |
| `INTERNAL` (a server fault; details go to the log only) | 500 |

An unknown route is `NOT_FOUND` too.

Ids: threads `r_` + 12 base32 chars, notes `n_` + 12, tokens `t_` + 12 (random). Times: RFC 3339 UTC.

## Administration (CLI)

There is no sign-up. The admin runs the binary on the server:

```
relevo serve [--addr 127.0.0.1:7878] [--db relevo.db] [--team-questions-per-day 5]
relevo invite <handle> --name "Ana" [--about "payments backend"] [--label "Claude Code"]   # creates the person (or reuses), prints a new token once
relevo token add <handle> --label "Codex desktop"                                          # another agent for the same person
relevo token list [<handle>]
relevo token revoke <token-id>
relevo people
relevo about <handle> "new line"
relevo deactivate <handle>
```

`--db` defaults to `RELEVO_DB` or `./relevo.db`; `--addr` to `RELEVO_ADDR` or `127.0.0.1:7878`; `--team-questions-per-day` to `RELEVO_TEAM_QUESTIONS_PER_DAY` or 5.

`invite` on an existing handle reactivates the person, sets the new `--name`, and `--about` if given. `team` and `equipo` cannot be handles (they mean the whole team). `--label` defaults to `agent`. `deactivate` keeps the person's tokens, which work again if they are invited back; revoke them to end them for good.

## Privacy and logs

The server logs counts and ids, never note text or tokens. The database holds token hashes only.

## Later (not v0)

MCP tools (block 2), consent between teams, webhooks to wake always-on agents, OAuth for ChatGPT, A2A gateway, attachments.
