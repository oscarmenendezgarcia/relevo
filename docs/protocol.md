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
| **Context** | What a session is working on: the repository it runs in, optionally with its branch after `@`: `pagos-api` or `pagos-api@feat/refunds`. Lowercase; the repository part is `^[a-z0-9][a-z0-9._-]{0,63}$`, the branch part `^[a-z0-9._/-]{1,100}$`. A session without one is a *general* session. |
| **Participant** | A person in a thread. For `direct`, the sender and the recipients. For `team`, every active person at the time the question is asked. Participants are fixed: a person deactivated later stays a participant of their threads and keeps appearing in `participants` and `delivery` (with the latest note by someone else that reached them), and the others may still write there. While inactive they are added to no new thread: not to team threads, and `to` cannot name them. Invited back, they find their threads again. |

## Sessions and routing

A person may run several agents at once, each with its own context: a terminal in `pagos-api`, another in `frontend`, a bot. A note must reach the right one, and only one of them should take it.

**Sessions.** Every request names its session with two optional headers:

- `Relevo-Session`: an id the client picks once per running agent, `^[A-Za-z0-9_-]{8,64}$`. Sessions belong to a token: the same id under another token is another session. Without the header, the request uses the token's *default session*.
- `Relevo-Context`: the session's context. Sent with each request; the latest value wins. A waiting `inbox` request sends its value when it arrives: when it looks again or answers, it uses the session's context as it is then, so it never undoes a newer value. Without it (or empty), the session is general. A default session is always general unless the header says otherwise. Like addresses, it may be written in any case: `Pagos-API` is `pagos-api`.

The server keeps each session with its token's label, its context and when it was last seen. Every request with a valid token and well-formed session headers counts as seen once it gets past checking its own parameters, even one that then fails or replays a `send`. Those checks are the ones that need nobody looked up: for `send`, the body and its fields, `to` included (an unknown person is past them); for `inbox`, `all` and `wait` (an 11th waiting request, refused with `RATE_LIMITED`, is past them). A session not seen for 7 days is forgotten (its claims lapsed long before: see [Claiming](#sessions-and-routing)). The sessions of a revoked token (every token of a deactivated person is) are forgotten at once, with their claims: no request can be made as them again, so they must not keep threads from the person's other sessions. A token keeps at most 20 sessions: when a request starts a 21st, the token's session seen least recently is forgotten, with its claims. So a client that picks a new id for every request, by mistake or not, cannot grow the team list or the delivery state without bound; it should keep one id per running agent. A client ending a session says so with `DELETE /api/session`: the session the headers name (the default session without `Relevo-Session`) is forgotten at once, with its claims, so its threads show again in the person's other sessions without waiting for the claims to lapse. It ends only a session of the request's own token, and does not count as seen: it records nothing, and ending a session already forgotten, or never seen, answers the same.

**Addressing.** In `to`, `ana` means Ana, any of her sessions; `ana/pagos-api` means Ana, in the repository `pagos-api`, on any branch; `ana/pagos-api@feat/refunds` means Ana, on that branch only. An address is split at its first `/`. The context does not need a live session: the thread waits for one.

**Claiming.** For each participant a thread is *unclaimed* or *claimed* by one of their sessions. Claims break ties; they are not how a session decides whether a note is for it. Each session that sees a note judges, from its own context and work, whether it can answer; if it cannot, it stays silent; if it can, it answers at once (the answer claims the thread), or, when the note needs work first, claims the thread as a state with `send(thread, claim: true)` and no text before acting. Claiming is never a note: nobody has to read "on it".

- Writing a note in an existing thread claims it for the writing session when nobody of its person holds it, or when the claim has lapsed. Opening a thread claims nothing: the person who asks has nothing to take on, and a claim by the session that asked would hide the answers from the person's next session. It never takes a live claim from another session of the same person: the note is written, but the claim stays where it was. `send` says which happened (`claimed`), so the session that lost steps aside.
- A claim lapses when the claiming session has not been seen for 15 minutes. A lapsed claim counts as unclaimed everywhere: the thread shows again in the person's other sessions, and the next session to write takes it. It is gone for good: the session that held it, seen again, holds the thread again only by writing in it while nobody else of its person does. A session is seen with each request it makes; a waiting `inbox` request, when it arrives and when it answers.
- `claim: true` with no text claims the thread without writing anything, under the same rule: it never takes a live claim from another session of the person. Closing without text does not claim.

**Which sessions see a thread.** For a participant, a thread shows in the inbox of one of their sessions when it is claimed by that session, or when it is unclaimed and either it has no context for them, or its context matches the session's context, or the session is general. A context without a branch matches the repository on any branch (`pagos-api` matches `pagos-api` and `pagos-api@feat/refunds`); one with a branch matches only itself: a note for `ana/pagos-api@feat/refunds` shows neither in Ana's `pagos-api@main` sessions nor in a `pagos-api` session that does not say its branch, and `frontend@feat/refunds` is another context altogether. So an unaddressed note shows in every session of the person until one of them claims it; then only there. A note for `ana/pagos-api` shows in Ana's `pagos-api` sessions, on any branch, and her general ones. `inbox` with `all` and `thread` ignore this and show any thread of the person.

## Delivery state

Two things are kept apart: whether a note has **reached** a session (it was fetched), and whether a person has **handled** it (they did something about it). A note can reach a session that then dies, or decides it is not for it; it stays pending for the person until one of their sessions handles it.

**Handled.** For each participant of a `direct` thread, the server keeps how far the person has handled it. A person handles the thread when one of their sessions writes a note in it, closes it, or sends `done: true` (see `send`), up to the latest note that had reached that session (the session's own new note included, when every earlier one had reached it): a note that landed after the session last fetched the thread was never seen there, so it stays pending, even if the session answered or marked done what it had. So `handled_at` never comes before the note it covers reached the person. Claiming does not handle: a claim says "I am on it", and if the claiming session dies, the note is still pending when the claim ends. A note by someone else that the person has not handled is **pending** for them. `team` threads and closed threads are never pending: answering a team question is optional, and a closed thread asks nothing more.

**Reached.** Delivery is kept per session. For each session the server knows the last note of each thread that reached it. A note reaches a session when that session fetches it through `inbox` or `thread`. Each session gets each note once: a note that reached it is not new for it again, pending or not.

**Where a new session starts.** A session starts, for each `direct` thread still pending for its person, with what the person has *handled*: the notes they have not handled are new for it, so an agent that starts after another one died with a note half done gets that note at once. For every other thread (`team` threads, and `direct` threads that are closed or handled) it starts with what any of the person's sessions has reached, so a new terminal does not replay old team questions, nor the last note of every thread the other person closed.

A thread is **new for a session** when it holds notes, not written by the session's person, that have not reached that session. A person's own notes are never new for any of their sessions. When a session writes a note, everything in the thread up to that note has reached it, unless earlier notes had not: then nothing moves, so a reply written without reading never hides a note.

A thread is **waiting** for a person when it is pending for them: `direct`, `open`, with a note by someone else they have not handled. It stays in the inbox of the sessions that see it until the person handles it (replies, closes it, or marks it `done`), even after it reached a session.

A `team` thread is never "waiting": it shows only while it is `open` and has notes new for the session. So a teammate's agent with nothing to add does not keep seeing it, and once the asker closes it (resolved), it leaves every inbox, even with notes the session has not fetched; `inbox` with `all` and `thread` still show it. A closed `direct` thread with new notes still shows until they are fetched, since a closing note is often meant to be read.

For others, a participant's delivery is about the latest note *written by someone else*: whether it reached any of their sessions and which one first (its label, and its context at that moment), how many of their live sessions have it (`seen_by`), and whether the person has handled it, when, and from which session (`handled_at`, `handled_by`): "seen by pi in embeddings, not handled yet", or "handled by Claude Code in producto at 19:30". A session that has ended still counts as having seen it (the `by`), even though `seen_by` counts live sessions only.

## Operations

All three are the same over HTTP and over MCP (`relevo mcp`, see `docs/agents.md`).

### `send`

Opens a thread, or writes in one.

| Field | Type | |
|---|---|---|
| `to` | array of addresses (`handle` or `handle/context`, the context with or without a branch), or `["@team"]` | Opens a new thread. Exactly one of `to` and `thread`. |
| `thread` | thread id | Writes in an existing thread. |
| `text` | string | The note. Required unless only closing, claiming or marking done (`thread` + `close: true`, `claim: true` or `done: true`, no `text`). |
| `close` | bool, optional | Marks the thread closed after writing. |
| `claim` | bool, optional | With `thread`: claims the thread for this session, as a state, without a note when there is no `text`. Not with `to`. |
| `done` | bool, optional | With `thread` and no `text`: the person has handled the thread up to the latest note that reached this session, with nothing to write ("seen, nothing to do"). A note that has not reached it stays pending (a session that fetched nothing handles nothing). Writes no note and does not claim. Not with `to`, `text` or `claim`. |
| `key` | string, 1 to 100 characters, optional | Idempotency key. A repeated `send` from the same person with the same key, within 24 hours of the `send` that used it, returns the first result and writes nothing. Only a successful `send` uses up a key. After 24 hours the key expires: the same key is a fresh `send`, which uses it up for another 24 hours. The server forgets expired keys. |

The body is a JSON object; anything else (an array, say), or any other field, fails with `BAD_REQUEST`. Characters here and below are Unicode scalar values.

Rules:

- `to` holds 1 to 20 addresses of distinct active people other than you, or exactly `["@team"]`. An address is a handle, optionally followed by `/` and a context (`ana/pagos-api`, `ana/pagos-api@feat/refunds`: everything after the first `/`); naming the same person twice with different contexts (or once with a context and once without) fails with `BAD_REQUEST`, and so does a malformed context or a context on `@team`. Addresses may be written with or without a leading `@` and in any case; repeats are merged (`["@team", "equipo"]` is one `@team`). Writing to yourself, or `@team` next to other handles, fails with `BAD_REQUEST`. An unknown or inactive handle fails with `UNKNOWN_PERSON`, whose error lists the valid handles with their `about`.
- `["@team"]` opens a `team` thread. `@equipo` is accepted as the same thing (both with or without `@`); `team` and `equipo` can therefore not be handles. A person may open at most `RELEVO_TEAM_QUESTIONS_PER_DAY` (default 5) team threads in any rolling 24 hours: beyond that, `TEAM_LIMIT`. A team of one (only you) fails with `NO_RECIPIENTS`.
- Writing a note in an existing thread claims it for the session that wrote it, unless another session of your person holds it; opening a thread claims nothing (see [Claiming](#sessions-and-routing)).
- Writing in a thread you are not a participant of fails with `NOT_FOUND` (the thread's existence is not revealed).
- Writing in a closed thread fails with `THREAD_CLOSED`. Open a new thread instead.
- Closing: in a `direct` thread any participant may close it; in a `team` thread only the person who asked. Closing releases every claim on the thread: a closed thread is nobody's work any more.
- Text over 64 KiB (counted once decoded from JSON) fails with `TOO_LARGE` (400); empty or blank text with `BAD_REQUEST`. The whole request body may be up to 400 KiB, so a note of 64 KiB fits however the client escapes it.
- Loop guard: a thread accepts at most 20 notes in any rolling hour. The 21st fails with `THREAD_BUSY`, whose message tells the agent to check with its person before going on. (The server cannot tell a person from an agent, so this caps speed, not authorship.)
- A person may send at most 60 notes per rolling minute: beyond that, `RATE_LIMITED`.
- The limits count notes only: a close, a claim or a `done` without text is never limited.

Returns `{ "thread": "<id>", "note": "<id>", "status": "open" | "closed", "claimed": "this-session" | "other-session" | null }`. `note` is `null` when the `send` only closed the thread. `claimed` is who holds the thread for your person after this `send`: `"other-session"` means another of your sessions had it first, so this one should step aside; `null` after opening a thread, and after any close, which releases every claim. A replayed `send` returns who held the thread after its first result, named for the session that replays: `"this-session"` only when that session is the one that held it, so a retry from another session (an agent restarted under a new id, say) is not told it holds a thread it does not.

### `inbox`

What is new for this session, and what is waiting for its person, among the threads this session sees (see [Sessions and routing](#sessions-and-routing)).

| Param | |
|---|---|
| `all` | bool (`1`, `true`, `0`, `false`), optional. Show every thread of the person, ignoring routing and claims. It only matters to see threads held by the person's other sessions or addressed to their other contexts (and resolved team threads with new notes); what is new or waiting for this session shows without it. |
| `wait` | seconds, 0 to 60, optional (HTTP only). If nothing is new for this session, hold the request until something is or the time runs out, then answer as usual. Waiting threads alone do not count as new. This is how bridges and bots learn of a note at once. |

Returns:

```json
{
  "team": [ { "handle": "ana", "name": "Ana", "about": "payments backend", "contexts": ["pagos-api", "frontend"] } ],
  "session": { "label": "Claude Code laptop", "context": "pagos-api", "you": "ana" },
  "threads": [
    {
      "id": "r_8f2k…",
      "kind": "direct" | "team",
      "title": "Refunds endpoint",
      "from": "oscar",
      "participants": ["oscar", "ana"],
      "status": "open" | "closed",
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

- `team` lists every active person, you included, so the agent knows whom to ask. `contexts` are the distinct contexts of their sessions seen in the last 15 minutes, sorted.
- `session` is the session this request counts as; `you` is the handle of its person, so an agent knows which member of `team` it speaks for.
- `pending` says the thread is pending for your person (see [Delivery state](#delivery-state)). (Up to 0.2.x it was called `waiting_for_you`; that name is gone.)
- `held_by` lists the other participants who hold the thread now (live claims only), with the session's label, context and since when, so whoever asked sees who is on it without opening the thread; empty when nobody else holds it.
- A thread's `context` is the context it was addressed to for you, or `null`. `claimed` is `null` (unclaimed), `"this-session"`, or, only with `all` (and in `thread`, which also shows any thread of the person), `"other-session"`.
- At most 20 threads: first those with new notes (waiting ones first among them), then waiting ones with nothing new; within each group, by latest note, newest first. `more` counts the rest. So threads that are only waiting, which stay until someone replies, never keep new notes out.
- `new_notes` holds the notes that have not reached this session, oldest first. That may include the person's own (written without reading, or by another session of theirs), for context: they never make a thread new, but they are part of what this session has not seen. A note's `text` is cut to 4,000 characters with `truncated: true`; the agent reads it whole with `thread`.
- `last_note` is the thread's latest note, whoever wrote it and whether or not it reached this session, cut like `new_notes`. So a session sees what a waiting thread is waiting on even when another of its person's sessions already fetched the answer. Showing it delivers nothing.
- Returning a thread here delivers its new notes to this session. Threads counted in `more` are not delivered.
- `from` is the person who opened the thread.

### `thread`

The whole history of one thread your person takes part in, from any of their sessions. Delivers all its notes to this session. Does not claim it.

Returns the same thread object as `inbox`, with `notes` (every note, in full, oldest first, `truncated: false`) in place of `new_notes`, plus `delivery`: for each participant, in the order of `participants`, the latest note by someone else that they have, when it reached them and which session it reached first (`by`, kept after that session ends), which session of theirs claimed the thread and since when (`claimed_by`, with `since`), or `null`s, how many of their live sessions have that note (`seen_by`, 0 when none has), and, once the person has handled that note, when and from which session (`handled_at`, `handled_by`; `null` until then). `handled_at` is when they last handled a note by someone else that they had not handled yet: handling the thread again with nothing new by others keeps it. Each session shows its label and its context at that moment: when the note reached it, when it claimed the thread, when it handled it. This is how a writer sees "picked up by her agent 5 minutes ago", and tells "seen by pi, not handled yet" from "handled by Claude Code at 19:30".

```json
"delivery": [ { "handle": "ana", "note": "n_…", "delivered_at": "2026-11-08T10:46:00Z", "by": { "label": "Claude Code laptop", "context": "pagos-api" }, "claimed_by": { "label": "Claude Code laptop", "context": "pagos-api", "since": "2026-11-08T10:47:00Z" }, "seen_by": 1,
                "handled_at": null, "handled_by": null },
              { "handle": "bob", "note": null, "delivered_at": null, "by": null, "claimed_by": null, "seen_by": 0, "handled_at": null, "handled_by": null } ]
```

## HTTP

Every route needs `Authorization: Bearer <token>`, and takes the optional `Relevo-Session` and `Relevo-Context` headers. Bodies and responses are JSON. A malformed session id or context fails with `BAD_REQUEST` (checked after the token). So does an unknown, repeated or malformed `inbox` parameter.

```
GET  /api/inbox?all=1&wait=30
GET  /api/threads/:id
POST /api/send            { to | thread, text, close?, claim?, done?, key? }
GET  /api/people          { "team": [...], "you": "ana" }  (same list as inbox)
DELETE /api/session        forget this session (the one the headers name) and release its claims; { "ok": true }
GET  /health              no token; { "ok": true }
```

A token may hold at most 10 waiting `inbox` requests at once (requests with `wait` above 0, counted from when they arrive until they answer or the client goes away); beyond that, `RATE_LIMITED`. A waiting request holds no database lock; it looks again whenever a note lands in a thread of its person, the moment a claim of one of their sessions lapses, and when a session forgotten because its token started a 21st, or ended with `DELETE /api/session`, releases one of their threads, and when a close releases the claims on a thread of theirs. (A token revoked from the CLI releases its threads from another process, which wakes nobody: waiting requests find them when they next look.)

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
relevo serve [--addr 127.0.0.1:7878] [--db relevo.db] [--team-questions-per-day 5] [--tls [--tls-name NAME]… | --tls-cert PEM --tls-key PEM]
relevo tls-fingerprint [--db relevo.db]                                                  # SHA-256 of the certificate serve --tls uses
relevo invite <handle> --name "Ana" [--about "payments backend"] [--label "Claude Code"]   # creates the person (or reuses), prints a new token once
relevo token add <handle> --label "Codex desktop"                                          # another agent for the same person (active people only)
relevo token list [<handle>]
relevo token revoke <token-id>
relevo people
relevo sessions [<handle>]                                                                 # sessions seen in the last 7 days: handle, label, session id (or default), context (or general), last seen
relevo about <handle> "new line"
relevo deactivate <handle>                                                                 # also revokes every token of theirs
relevo backup <dest-file>                                                                  # a consistent copy, while the server runs
relevo backup --keep 14 <dest-dir>                                                         # writes <dest-dir>/YYYYMMDD-HHMMSS.db (UTC), keeps the newest 14
relevo check-db                                                                            # whether this version can use the database, writing nothing
```

The database records the version of its layout. From layout 6 on (relevo 0.3.0), `relevo serve` migrates a database of layout 5 (relevo 0.1.0 to 0.2.x) in place when it starts, keeping what layout 5 said was waiting for a reply: a `direct` thread that was waiting for a person there (open, its latest note not theirs) is handled up to their own latest note in it, so it stays pending for them even if it had reached one of their sessions; every other thread is handled up to its latest note. Reaching a session is not taken as handling: `handled_at` is set only when the person wrote after the latest note by someone else (as when they first did), and is `null` otherwise. As with any pending thread, a new session of the person gets the notes they have not handled as new, once; the person's sessions that already had them see them as waiting, as before, not as new. Only `serve` migrates: every other command that opens the database refuses one of layout 5, before writing anything, and says to start `serve` first (and to keep a `relevo backup` before, which copies it as it is), so an admin command never changes the layout behind the owner's back. Every command refuses any other older layout, and any newer one, and says so; such a database must be deleted (with its `-wal` and `-shm` files) and the team invited again.

`check-db` says whether this version can use the database: as it is, once `serve` migrates it (exit 0), or not, and why (exit 1). It writes nothing, not even `-wal` or `-shm` files: a database without them (no server has it open) is read as immutable, one with them read-only through the server's. It never creates a database. `deploy/install.sh` asks it before it replaces the binary.

`backup` copies the database with SQLite's `VACUUM INTO`: one consistent snapshot, taken without stopping the server (a plain copy of a database in WAL mode can miss what is still in its `-wal` file). It only reads the database, as `check-db` does: it never creates one, and copies one made by another version too, so it can be kept before an upgrade. The copy is created 0600 under a hidden name, checked (`PRAGMA quick_check`), and only then given its name; an existing file is never overwritten. When `<dest>` is a directory (with `--keep N` it must be one, and is created 0700 if missing), the copy is named `YYYYMMDD-HHMMSS.db` (UTC), and `--keep N` then removes the oldest files named that way beyond the newest N, the new one always among them; other files there are left alone. Restoring is putting a copy in place of the database while the server is stopped (and deleting the old database's `-wal` and `-shm` files).

`--db` defaults to `RELEVO_DB` or `./relevo.db`; only `serve` creates `./relevo.db` that way, and the other commands refuse to run when neither names a database and there is no `./relevo.db` (so an admin command run before `RELEVO_DB` is set cannot fill a database the server never reads); `--addr` to `RELEVO_ADDR` or `127.0.0.1:7878`; `--team-questions-per-day` to `RELEVO_TEAM_QUESTIONS_PER_DAY` or 5; `--tls` to `RELEVO_TLS` (`true` or `false`), `--tls-name` to `RELEVO_TLS_NAME` (comma-separated), and `--tls-cert` and `--tls-key` to `RELEVO_TLS_CERT` and `RELEVO_TLS_KEY`, so that a systemd drop-in can turn TLS on with `Environment=` lines. `--tls` and `--tls-cert` exclude each other, `--tls-cert` and `--tls-key` go together, and `--tls-name` needs `--tls`; `serve` refuses anything else before it opens the database (`RELEVO_TLS=false` counts as no `--tls`).

`serve` can speak HTTPS itself, with no proxy in front:

- `relevo serve --tls` uses a certificate of its own. The first start creates a self-signed one (ECDSA P-256, valid 10 years) for the names in `--tls-name` (repeatable: host names or IP addresses; by default `localhost`, `127.0.0.1`, `::1`, the machine's host name and its non-loopback addresses) and keeps it next to the database as `<db>.tls/cert.pem` and `key.pem` (the key mode 0600, the directory 0700). Later starts reuse it; `--tls-name` with names the stored certificate lacks makes a new one, and says so. At every start it prints the certificate's SHA-256 fingerprint; `relevo tls-fingerprint` prints it too. Without `--tls-name`, it logs a warning at every start when the certificate has none of the machine's non-loopback addresses, or when it finds none (they could not be read): clients that connect by address would be refused.
- `relevo serve --tls-cert <pem> --tls-key <pem>` uses a certificate you already have (one from Let's Encrypt or the office's own authority).
- Without TLS, `serve` speaks plain HTTP, as before: keep `--addr` on loopback, or behind a proxy or a VPN that encrypts. It logs a warning when `--addr` is not a loopback address and TLS is off.

Whatever the transport, the server limits what a client can hold: 10 seconds to send the request head, which may take at most 16 KiB (a longer one is refused with 431, so a connection holds little before its token is checked), 30 seconds to send the body, an idle connection closed after 75 seconds, a request answered within 90 seconds (a waiting `inbox` answers by 60), at most 1,024 connections at once (beyond that, new ones wait) and 64 from one address (beyond that, new ones are closed unanswered). Loopback is not counted against one address's share: behind a proxy on the same machine, every connection comes from there, the whole team's. Clients behind one NAT share one address's 64. These replace what the proxy used to enforce; a proxy in front stays possible.

`invite` on an existing handle reactivates the person, sets the new `--name`, and `--about` if given. `team` and `equipo` cannot be handles (they mean the whole team). `--label` defaults to `agent`. `deactivate` revokes every token of the person, for good: inviting them back issues a new token, and their old ones never work again.

## Privacy and logs

The server logs counts and ids, never note text or tokens. The database holds token hashes only, but note text in plain form, so the server creates it (and SQLite its `-wal` and `-shm` files) readable by the server's user only (mode 0600).

## Later (not v0)

Consent between teams, webhooks, OAuth for ChatGPT, A2A gateway, attachments.
