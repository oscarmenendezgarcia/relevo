//! SQLite storage: schema, ids, tokens, and the admin operations the CLI runs.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::crypto::{self, Key};

const SCHEMA: &str = "
-- `threads.status` is always 'open' since layout 8 (relevo 0.5): threads no longer close. The
-- column, its check and the `resolved` columns below stay, unused, so that layout 7 moves to 8
-- without making those tables again (see `MIGRATE_FROM_7`).
CREATE TABLE IF NOT EXISTS people (
    id         INTEGER PRIMARY KEY,
    handle     TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    about      TEXT NOT NULL DEFAULT '',
    active     INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
-- `threads.title` and `notes.text` hold, since layout 11, values encrypted for their row
-- (`crypto`): BLOBs, which a TEXT column keeps as they are. A database `serve` has not opened
-- since it was made may still hold them in plain text (see `server_key`).
CREATE TABLE IF NOT EXISTS threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team', 'global')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
-- A session is one running agent of a person, named by the `Relevo-Session` header under one
-- token (`client_id` is '' for the token's default session). `context` is the latest
-- `Relevo-Context` (NULL: general). `claims` is 1 while the session may hold claims: from its
-- first claim until they lapse, 15 minutes after it was last seen (see `session::lapse`).
-- Sessions not seen for 7 days are forgotten. Ids are never reused (AUTOINCREMENT): `send_keys` keeps the
-- id of a session that may be forgotten since, and a replay must not name a newer one for it.
-- `client` is what the latest request said of the agent's client, `<name>/<version>` (the
-- `Relevo-Client` header, or a `relevo/<version>` User-Agent read as `mcp/<version>`, then
-- `client_inferred` is 1); NULL when it said nothing that could be read. Layout 9 brings both.
-- `agent` is what the latest request said of the agent (`Relevo-Agent`: Claude Code, pi), the
-- session's name for everyone else (a note's `via`); NULL: its token's label. Layout 10 brings it.
CREATE TABLE IF NOT EXISTS sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id        TEXT NOT NULL REFERENCES tokens(id),
    client_id       TEXT NOT NULL,
    person_id       INTEGER NOT NULL REFERENCES people(id),
    context         TEXT,
    last_seen       INTEGER NOT NULL,
    claims          INTEGER NOT NULL DEFAULT 0,
    client          TEXT,
    client_inferred INTEGER NOT NULL DEFAULT 0,
    agent           TEXT,
    UNIQUE (token_id, client_id)
);
CREATE INDEX IF NOT EXISTS sessions_by_person ON sessions(person_id, last_seen);
CREATE INDEX IF NOT EXISTS sessions_by_last_seen ON sessions(last_seen);
CREATE INDEX IF NOT EXISTS sessions_claiming ON sessions(last_seen) WHERE claims = 1;
CREATE INDEX IF NOT EXISTS threads_by_opener ON threads(opener_id, kind, created_at);
-- One row per participant of a thread; rows are inserted in participant order, which rowid keeps.
-- `context` is the context the thread was addressed to for this person; `claimed_by` the session
-- of theirs that claimed it (the first to write, or to claim without a note, while nobody held
-- it), `claimed_context` its context then and `claimed_at` when. `delivered_seq` is the latest note that reached any of their sessions;
-- `delivered_at`, `_label` and `_context` say when the latest one by someone else first did, and
-- which session (its label and context then). `last_seq` is the thread's latest note, `last_other_seq` the
-- latest one not theirs; `has_new` (no session of theirs has that one),
-- `waiting` (pending: the person named, with a note not theirs after `handled_seq` and from the
-- one that named them on), `inbox` (`has_new` or `waiting`) and `fresh` (pending, with a note
-- past `handled_seq` that reached one of their sessions: a new session of theirs gets its own
-- row, starting at `handled_seq`) are kept by `api::refresh` after every change to the thread.
-- `resolved` is unused since layout 8 (it marked a closed `team` thread), and always 0.
-- `handled_seq` is how far the person has handled the thread (by writing in it, or `seen`; up to
-- 0.4.x by closing it too); `handled_at`, `_label` and `_context` say when they last handled a
-- note by someone else, and from which session (its label and context then). `named_seq` says the thread names
-- the person with their own `@`: 0 when it did from the start (every participant of a `direct`
-- thread, and the people named in the note that opened a `team` or `global` one), the note that
-- named them when a later one did, `NULL` while it does not (reached through a team or the global
-- board, or the opener of either): only then can it be pending for them, and only the notes by
-- others from that one on.
CREATE TABLE IF NOT EXISTS participants (
    thread_id         TEXT NOT NULL REFERENCES threads(id),
    person_id         INTEGER NOT NULL REFERENCES people(id),
    context           TEXT,
    claimed_by        INTEGER REFERENCES sessions(id),
    claimed_context   TEXT,
    claimed_at        INTEGER,
    delivered_seq     INTEGER,
    delivered_at      INTEGER,
    delivered_label   TEXT,
    delivered_context TEXT,
    last_seq          INTEGER NOT NULL DEFAULT 0,
    last_other_seq    INTEGER NOT NULL DEFAULT 0,
    has_new           INTEGER NOT NULL DEFAULT 0,
    waiting           INTEGER NOT NULL DEFAULT 0,
    inbox             INTEGER NOT NULL DEFAULT 0,
    resolved          INTEGER NOT NULL DEFAULT 0,
    handled_seq       INTEGER,
    handled_at        INTEGER,
    handled_label     TEXT,
    handled_context   TEXT,
    fresh             INTEGER NOT NULL DEFAULT 0,
    named_seq         INTEGER,
    PRIMARY KEY (thread_id, person_id)
);
-- What a session has of a thread, once it differs from its person's: a session without a row has
-- what its person has (`participants.delivered_seq`). Before that moves, every other session of
-- the person gets a row with the old value, so one session reading never delivers to another.
-- A new session gets a row for each `fresh` thread of its person, at what the person handled, so
-- a note that reached a session which died before handling it is new for the next one.
-- The other columns copy the participant row (kept by `api::refresh`) so that the inbox can read
-- a session's new threads, in order, from an index.
CREATE TABLE IF NOT EXISTS session_threads (
    session_id    INTEGER NOT NULL REFERENCES sessions(id),
    thread_id     TEXT NOT NULL REFERENCES threads(id),
    person_id     INTEGER NOT NULL REFERENCES people(id),
    delivered_seq INTEGER NOT NULL,
    has_new       INTEGER NOT NULL DEFAULT 0,
    waiting       INTEGER NOT NULL DEFAULT 0,
    last_seq      INTEGER NOT NULL DEFAULT 0,
    claimed_by    INTEGER,
    context       TEXT,
    person_inbox  INTEGER NOT NULL DEFAULT 0,
    resolved      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, thread_id)
);
CREATE INDEX IF NOT EXISTS session_threads_by_thread ON session_threads(thread_id);
CREATE INDEX IF NOT EXISTS session_threads_by_claim
    ON session_threads(claimed_by) WHERE claimed_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS participants_by_person ON participants(person_id);
CREATE INDEX IF NOT EXISTS participants_fresh ON participants(person_id) WHERE fresh = 1;
CREATE INDEX IF NOT EXISTS participants_by_claim
    ON participants(claimed_by) WHERE claimed_by IS NOT NULL;
-- The inbox of a session is the union of a few index ranges, each already in inbox order, so it
-- reads at most a page from each and stops, however many threads are flagged (see `api::page`):
-- every thread of the person (`all`), those claimed by the session or unclaimed (general), those
-- unclaimed with no context or the session's context.
CREATE INDEX IF NOT EXISTS participants_inbox
    ON participants(person_id, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX IF NOT EXISTS participants_inbox_claim
    ON participants(person_id, claimed_by, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX IF NOT EXISTS participants_inbox_context
    ON participants(person_id, claimed_by, context, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX IF NOT EXISTS session_new
    ON session_threads(session_id, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX IF NOT EXISTS session_new_claim
    ON session_threads(session_id, claimed_by, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX IF NOT EXISTS session_new_context
    ON session_threads(session_id, claimed_by, context, waiting, last_seq) WHERE has_new = 1;
-- For `more`: the threads new only for the session, by route (see `api::count_sql`).
CREATE INDEX IF NOT EXISTS session_count
    ON session_threads(session_id, claimed_by, context)
    WHERE has_new = 1 AND person_inbox = 0;
CREATE TABLE IF NOT EXISTS notes (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    thread_id  TEXT NOT NULL REFERENCES threads(id),
    author_id  INTEGER NOT NULL REFERENCES people(id),
    token_id   TEXT NOT NULL REFERENCES tokens(id),
    via        TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS notes_by_thread ON notes(thread_id, seq);
CREATE INDEX IF NOT EXISTS notes_by_author ON notes(author_id, created_at);
-- Idempotency keys: the first result of a send, per person and key. A key lives 24 hours from
-- `created_at`; `send` deletes expired ones, so the table holds about a day of keyed sends.
-- `claimed_by` is the session that held the thread for the person after that send: a replay names
-- it for the session that asks. `result` holds the rest of the first result (`kind`, `to`, `added`,
-- `reached`) as JSON; `NULL` for a key used before layout 7.
CREATE TABLE IF NOT EXISTS send_keys (
    person_id  INTEGER NOT NULL REFERENCES people(id),
    key        TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    note_id    TEXT,
    status     TEXT NOT NULL,
    claimed_by INTEGER,
    created_at INTEGER NOT NULL,
    result     TEXT,
    PRIMARY KEY (person_id, key)
);
CREATE INDEX IF NOT EXISTS send_keys_by_time ON send_keys(created_at);
-- Teams: named groups of people, kept by the admin. A name is never a person's handle.
CREATE TABLE IF NOT EXISTS teams (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS team_members (
    team_id   INTEGER NOT NULL REFERENCES teams(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    PRIMARY KEY (team_id, person_id)
);
CREATE INDEX IF NOT EXISTS team_members_by_person ON team_members(person_id);
-- The teams the note that opened a `team` thread named, by their names then: a team removed
-- later keeps its name here.
CREATE TABLE IF NOT EXISTS thread_teams (
    thread_id TEXT NOT NULL REFERENCES threads(id),
    name      TEXT NOT NULL,
    PRIMARY KEY (thread_id, name)
);
-- What the timeline (`relevo web`) shows that nothing else kept. Tables of their own, not
-- columns, so that layout 7 keeps its version: every layout-7 database gets them when it is
-- opened, with no migration. The context of the session that wrote a note, when it had one (a
-- note with no row was written by a general session, or before these tables); who closed a
-- thread under 0.4.x, and when (a thread closed before them has no row): read by nothing since
-- layout 8, when threads no longer close, except to say when its migration handled it.
CREATE TABLE IF NOT EXISTS note_contexts (
    seq     INTEGER PRIMARY KEY REFERENCES notes(seq),
    context TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS thread_closes (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    closed_at INTEGER NOT NULL
);
-- What the server keeps about itself, by key: `announced`, the latest version of relevo whose
-- release notes the server posted on the global board (or would have: see `release`),
-- `system_person`, the id of the person `relevo` who posts them, and what `serve` last said of
-- the address it serves at, for invitation links (`public_url`, `tls_pin`, `tls_fingerprint`,
-- `tls_port`: see `set_public`). Layout 8 brings it (a database of layout 7 gets it as it migrates).
-- `key_check` says which key encrypted the database (`crypto`; set by `serve`, see `server_key`),
-- and `scrub_pending` that it is still to be rewritten since (see `scrub`).
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- The timeline reads a person's threads by latest note, newest first, a page at a time, and
-- whether they wrote in a thread.
CREATE INDEX IF NOT EXISTS participants_timeline ON participants(person_id, last_seq);
-- How many threads a person handled since a time (the timeline's `handled_since`). An index
-- only: every database of this layout gets it when opened.
CREATE INDEX IF NOT EXISTS participants_handled ON participants(person_id, handled_at);
CREATE INDEX IF NOT EXISTS notes_by_thread_author ON notes(thread_id, author_id);
-- Invitations (`relevo invite`): a code that works once, until `expires_at`, to trade for a token
-- of its person (`POST /api/join`), kept by its SHA-256 only, with the server's address the link
-- gave (`url`) and, when that is `serve`'s own TLS address, its certificate's key pin and
-- fingerprint (`pin`, `fingerprint`; see `Address`). A person has at most one unused invitation: a new one replaces it. Added in 0.7.0
-- without a new layout, as the index above: every database gets it when opened, and nothing
-- before 0.7.0 reads it.
CREATE TABLE IF NOT EXISTS invites (
    hash       TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    url        TEXT NOT NULL,
    pin         TEXT,
    fingerprint TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);
CREATE INDEX IF NOT EXISTS invites_by_person ON invites(person_id);
-- The people marked as bots (`relevo invite --bot`, `relevo bot`). A table, not a column, as
-- `invites`: added in 0.8.0 without a new layout, and nothing before 0.8.0 reads it.
CREATE TABLE IF NOT EXISTS bots (
    person_id INTEGER PRIMARY KEY REFERENCES people(id)
);
";

/// Names no person nor team may take: 0.3's whole team, which the deprecated `to` still reads
/// as "everyone". In a note's text they name nobody (a team is always named).
pub const RESERVED_HANDLES: [&str; 2] = ["team", "equipo"];

/// The handle of the server's own person, who posts each version's release notes on the global
/// board (`release`). The server makes it when it first opens a database where nobody has the
/// name (`make_system_person`); it has no usable token, is in no people list, a mention of it
/// names nobody, and the admin commands refuse it, by its id: a person who had the handle before
/// 0.5.0 keeps it, and stays theirs to manage.
pub const SYSTEM_HANDLE: &str = "relevo";

/// The id of the person `relevo`, when the server made it: `NULL` otherwise.
pub const SYSTEM_PERSON_SQL: &str =
    "(SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'system_person')";

/// Refuses the server's own person (by its id, not its name) for anything an admin does to a
/// person or a team.
fn not_system(conn: &Connection, handle: &str) -> Result<()> {
    let system: bool = conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM people WHERE handle = ?1 AND id IS {SYSTEM_PERSON_SQL})"
        ),
        [handle],
        |r| r.get(0),
    )?;
    if system {
        bail!(
            "'{handle}' is the server's own name: it posts the release notes of each version on \
             the global board, and nobody can take it"
        );
    }
    Ok(())
}

/// Makes the server's own person (`SYSTEM_HANDLE`) the first time a database is opened where
/// nobody has the name, neither a person nor a team: inactive (so it is in no people list, in no
/// new thread, and names nobody), with one revoked token whose hash no secret has, which its
/// notes record. Its id goes in `meta`.
fn make_system_person(tx: &Connection, now: i64) -> Result<()> {
    let made: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = 'system_person')",
        [],
        |r| r.get(0),
    )?;
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM people WHERE handle = ?1)
             OR EXISTS (SELECT 1 FROM teams WHERE name = ?1)",
        [SYSTEM_HANDLE],
        |r| r.get(0),
    )?;
    if made || taken {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO people (handle, name, about, active, created_at)
         VALUES (?1, 'Relevo', 'the release notes of each version', 0, ?2)",
        params![SYSTEM_HANDLE, now],
    )?;
    let person = tx.last_insert_rowid();
    let token = random_id("t_");
    // A SHA-256 is 64 hex digits: this hash is no secret's.
    tx.execute(
        "INSERT INTO tokens (id, person_id, label, hash, created_at, revoked_at)
         VALUES (?1, ?2, 'release notes', ?3, ?4, ?4)",
        params![token, person, format!("system:{token}"), now],
    )?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('system_person', ?1)",
        [person.to_string()],
    )?;
    Ok(())
}

/// A session not seen for this long is forgotten, and so are its claims.
pub const SESSION_LIFETIME_SECS: i64 = 7 * 24 * 3600;

/// The claims of a session not seen for this long lapse.
pub const CLAIM_LAPSE_SECS: i64 = 15 * 60;

/// The layout of `SCHEMA`, kept in SQLite's `user_version`. A database of layout 5 (relevo 0.1.0
/// to 0.2.x), 6 (relevo 0.3.x), 7 (relevo 0.4.x), 8 (development builds of 0.5.0), 9 (relevo
/// 0.5.x) or 10 (relevo 0.6.0 to 0.8.x) is migrated in place (`MIGRATE_FROM_5` to
/// `MIGRATE_FROM_9`, then `OPEN_CLOSED`, then `server_key`), by `relevo serve` only; any other
/// layout is refused, never half-updated (`CREATE TABLE IF NOT EXISTS` would skip the old tables
/// and fail on their missing columns). Layout 11 changes no table: its note text and thread
/// titles are encrypted (`crypto`), which no version before it can read.
const SCHEMA_VERSION: i64 = 11;

/// Layout 5 to 6: the handled state of each participant, from what layout 5 recorded as handling.
/// A thread that was `waiting` there (open, direct, its latest note not theirs) waited for their
/// reply, whether or not it had reached a session: it is handled up to their own latest note in
/// it, so it stays pending. Any other thread is handled up to its latest note. Reaching a session
/// is never taken as handling: `handled_at` and its session are set only when the person wrote
/// after the latest note by someone else, as when they first did so (from that token; its
/// session's context then is not known), and stay `NULL` otherwise. Then the flags that depend
/// on it, as `api::refresh` keeps them, `fresh` included: a new session gets the threads still
/// pending once, like a note that reached a session which died before handling it. The new
/// columns go last, as in `SCHEMA`.
const MIGRATE_FROM_5: &str = "
ALTER TABLE participants ADD COLUMN handled_seq INTEGER;
ALTER TABLE participants ADD COLUMN handled_at INTEGER;
ALTER TABLE participants ADD COLUMN handled_label TEXT;
ALTER TABLE participants ADD COLUMN handled_context TEXT;
ALTER TABLE participants ADD COLUMN fresh INTEGER NOT NULL DEFAULT 0;
UPDATE participants AS p SET handled_seq = nullif(CASE WHEN p.waiting
    THEN coalesce((SELECT max(n.seq) FROM notes n
                   WHERE n.thread_id = p.thread_id AND n.author_id = p.person_id), 0)
    ELSE p.last_seq END, 0);
UPDATE participants AS p SET handled_at = w.created_at, handled_label = w.via
FROM notes AS w
WHERE p.last_other_seq > 0
  AND w.seq = (SELECT min(n.seq) FROM notes n
               WHERE n.thread_id = p.thread_id AND n.author_id = p.person_id
                 AND n.seq > p.last_other_seq);
UPDATE participants AS p SET
    waiting = t.kind = 'direct' AND t.status = 'open'
              AND coalesce(p.handled_seq, 0) < p.last_other_seq,
    inbox = coalesce(p.delivered_seq, 0) < p.last_other_seq
            OR (t.kind = 'direct' AND t.status = 'open'
                AND coalesce(p.handled_seq, 0) < p.last_other_seq),
    fresh = t.kind = 'direct' AND t.status = 'open'
            AND coalesce(p.handled_seq, 0) < p.last_other_seq
            AND coalesce(p.handled_seq, 0) < coalesce(p.delivered_seq, 0)
FROM threads t WHERE t.id = p.thread_id;
UPDATE session_threads AS s SET waiting = p.waiting, person_inbox = p.inbox
FROM participants p WHERE p.thread_id = s.thread_id AND p.person_id = s.person_id;
";

/// Layout 6 to 7: teams, the `global` kind and named participants. A `team` thread of layout 6
/// was a question to everyone: it becomes a `global` thread, which names none of its participants
/// (its `kind` is checked, so the table is made again, as SQLite says to change a constraint).
/// Every participant of a `direct` thread is named from the start. Delivery, handled and claims
/// stay as they were, and so do the flags `api::refresh` keeps: pending was already for `direct`
/// threads only, and a closed `global` thread that names nobody is resolved as a closed `team`
/// thread was. The new columns go last, as in `SCHEMA`; the new tables come from `SCHEMA`.
/// Runs with foreign keys off (see `open_with`).
const MIGRATE_FROM_6: &str = "
ALTER TABLE participants ADD COLUMN named_seq INTEGER;
UPDATE participants SET named_seq = 0
WHERE thread_id IN (SELECT id FROM threads WHERE kind = 'direct');
ALTER TABLE send_keys ADD COLUMN result TEXT;
CREATE TABLE threads_7 (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team', 'global')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads_7 (id, kind, title, opener_id, status, created_at)
SELECT id, CASE kind WHEN 'team' THEN 'global' ELSE kind END, title, opener_id, status, created_at
FROM threads;
DROP TABLE threads;
ALTER TABLE threads_7 RENAME TO threads;
";

/// Layout 7 to 8, before `SCHEMA` (which brings the `meta` table, empty: the release notes of
/// this version are then posted, as after any upgrade): threads no longer close, so the inbox's indexes no longer leave
/// out resolved threads (a closed `team` or `global` thread that does not name the person). They
/// go, and `SCHEMA` makes them again without that condition. The `resolved` columns and
/// `threads.status` stay, unused: dropping a column would make those tables again, for nothing.
const MIGRATE_FROM_7: &str = "
DROP INDEX IF EXISTS participants_inbox_claim;
DROP INDEX IF EXISTS participants_inbox_context;
DROP INDEX IF EXISTS session_new_claim;
DROP INDEX IF EXISTS session_new_context;
DROP INDEX IF EXISTS session_count;
";

/// Layout 8 to 9, before `SCHEMA`: each session keeps what its latest request said of its
/// client (see `sessions`). A session seen before has said nothing yet: unknown until its next
/// request. The new columns go last, as in `SCHEMA`.
const MIGRATE_FROM_8: &str = "
ALTER TABLE sessions ADD COLUMN client TEXT;
ALTER TABLE sessions ADD COLUMN client_inferred INTEGER NOT NULL DEFAULT 0;
";

/// Layout 9 to 10, before `SCHEMA`: each session keeps the agent its latest request said it is
/// (see `sessions`). A session seen before has said nothing yet: its token's label until its next
/// request.
const MIGRATE_FROM_9: &str = "ALTER TABLE sessions ADD COLUMN agent TEXT;";

/// Layout 7 to 8, after `SCHEMA` (which brings `thread_closes` to a database that did not have it
/// yet): every closed thread opens, handled by each of its named participants up to its latest
/// note, so nothing becomes pending. Where that covers a note by someone else they had not
/// handled, or one layout 6 counted as handled without saying when (`MIGRATE_FROM_5`),
/// `handled_at` is when the thread was closed (or, closed before `thread_closes` kept it, its
/// latest note), from no known session. Its latest note also reaches every participant, named or
/// not, and every session of theirs, as if each had fetched it: a closed thread asked nothing
/// more, so it must not come back to anyone's inbox as new (0.4.x left a closed `team` or
/// `global` thread out of the inbox of whom it did not name, notes unfetched or not). Where that
/// covers a note by someone else, `delivered_at` is likewise when it was closed, from no known
/// session. So none of these threads is new, waiting, or in an inbox, for anyone.
const OPEN_CLOSED: &str = "
UPDATE participants SET
    handled_at = CASE WHEN c.covers THEN coalesce(c.closed_at, c.last_at) ELSE handled_at END,
    handled_label = CASE WHEN c.covers THEN NULL ELSE handled_label END,
    handled_context = CASE WHEN c.covers THEN NULL ELSE handled_context END,
    handled_seq = c.last_seq
FROM (SELECT q.thread_id, q.person_id,
             EXISTS (SELECT 1 FROM notes n
                     WHERE n.thread_id = q.thread_id AND n.author_id != q.person_id
                       AND (n.seq > coalesce(q.handled_seq, 0) OR q.handled_at IS NULL))
                 AS covers,
             (SELECT tc.closed_at FROM thread_closes tc WHERE tc.thread_id = q.thread_id)
                 AS closed_at,
             (SELECT max(n.created_at) FROM notes n WHERE n.thread_id = q.thread_id) AS last_at,
             (SELECT max(n.seq) FROM notes n WHERE n.thread_id = q.thread_id) AS last_seq
      FROM participants q JOIN threads t ON t.id = q.thread_id
      WHERE t.status = 'closed' AND q.named_seq IS NOT NULL) AS c
WHERE participants.thread_id = c.thread_id AND participants.person_id = c.person_id;
UPDATE participants SET
    delivered_at = CASE WHEN c.by_others THEN coalesce(c.closed_at, c.last_at)
                        ELSE delivered_at END,
    delivered_label = CASE WHEN c.by_others THEN NULL ELSE delivered_label END,
    delivered_context = CASE WHEN c.by_others THEN NULL ELSE delivered_context END,
    delivered_seq = c.last_seq,
    has_new = 0, waiting = 0, inbox = 0, fresh = 0
FROM (SELECT q.thread_id, q.person_id,
             EXISTS (SELECT 1 FROM notes n
                     WHERE n.thread_id = q.thread_id AND n.author_id != q.person_id
                       AND n.seq > coalesce(q.delivered_seq, 0)) AS by_others,
             (SELECT tc.closed_at FROM thread_closes tc WHERE tc.thread_id = q.thread_id)
                 AS closed_at,
             (SELECT max(n.created_at) FROM notes n WHERE n.thread_id = q.thread_id) AS last_at,
             (SELECT max(n.seq) FROM notes n WHERE n.thread_id = q.thread_id) AS last_seq
      FROM participants q JOIN threads t ON t.id = q.thread_id
      WHERE t.status = 'closed') AS c
WHERE participants.thread_id = c.thread_id AND participants.person_id = c.person_id;
UPDATE session_threads AS s SET
    delivered_seq = p.delivered_seq, has_new = 0, waiting = 0, person_inbox = 0
FROM participants p JOIN threads t ON t.id = p.thread_id
WHERE t.status = 'closed' AND p.thread_id = s.thread_id AND p.person_id = s.person_id;
UPDATE participants SET resolved = 0 WHERE resolved != 0;
UPDATE session_threads SET resolved = 0 WHERE resolved != 0;
UPDATE threads SET status = 'open' WHERE status = 'closed';
";

/// Opens (or creates) the database and applies the schema. A database of layout 5 to 10 is
/// refused: only `relevo serve` migrates it (`open_migrating`), so that an admin command never
/// changes the layout behind the owner's back, before a backup. No key: the admin commands never
/// read note text nor thread titles. `":memory:"` works for tests.
pub fn open(path: &str) -> Result<Connection> {
    Ok(open_with(path, None, OPEN_PATIENCE)?.0)
}

/// `open`, migrating a database of layout 5 to 10 in place, with the key in `key_file`: for
/// `relevo serve`. A database encrypted already opens only with the key it was encrypted with,
/// which must be there; one never encrypted takes the key in `key_file` (made if missing), and
/// every note text and thread title in it is encrypted (see `server_key`).
pub fn open_migrating(path: &str, key_file: &Path) -> Result<(Connection, Key)> {
    let (conn, key) = open_with(path, Some(key_file), OPEN_PATIENCE)?;
    Ok((conn, key.expect("a key file gives a key")))
}

/// How long opening waits, in all, for other processes opening the same file at the same time
/// (two servers started at once on a new database, or on a copy not yet in WAL mode).
const OPEN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(15);

/// Migrates, and handles the key, when given a key file.
fn open_with(
    path: &str,
    key_file: Option<&Path>,
    patience: std::time::Duration,
) -> Result<(Connection, Option<Key>)> {
    create_private(path).with_context(|| format!("creating database {path}"))?;
    // SQLite answers "database is locked" at once, without waiting, to a switch to WAL mode while
    // another process reads the file, and in a few other moments of another one making it or
    // switching it: an attempt that met one is tried again (it wrote nothing: its transaction
    // was rolled back), with a new connection, for a while.
    // Every wait, the busy timeout's included, ends by the deadline.
    let deadline = std::time::Instant::now() + patience;
    let mut pause = std::time::Duration::from_millis(5);
    loop {
        match try_open(path, key_file, deadline) {
            Err(e) if busy(&e) && !left(deadline).is_zero() => {
                std::thread::sleep(pause.min(left(deadline)));
                pause = (pause * 2).min(std::time::Duration::from_millis(100));
            }
            done => return done,
        }
    }
}

/// The time left until `deadline`, zero past it.
fn left(deadline: std::time::Instant) -> std::time::Duration {
    deadline.saturating_duration_since(std::time::Instant::now())
}

/// Waits for another process's lock at most until `deadline` (and, then, not at all).
fn wait_until(conn: &Connection, deadline: std::time::Instant) -> rusqlite::Result<()> {
    conn.busy_timeout(left(deadline))
}

/// Whether `e` is SQLite's "database is locked".
fn busy(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code)
            == Some(rusqlite::ErrorCode::DatabaseBusy)
    })
}

/// One attempt of `open_with`, waiting for other processes' locks until `deadline` at most.
fn try_open(
    path: &str,
    key_file: Option<&Path>,
    deadline: std::time::Instant,
) -> Result<(Connection, Option<Key>)> {
    let migrate = key_file.is_some();
    let mut conn = Connection::open(path).with_context(|| format!("opening database {path}"))?;
    wait_until(&conn, deadline)?;
    // A layout it refuses is left as it is: found before anything is written, WAL mode included.
    let refuses = |layout| matches!(layout, Layout::Migrates(_)) && !migrate;
    let found = layout(&conn, path)?;
    if refuses(found) {
        bail!(no_migration(path, found));
    }
    wait_until(&conn, deadline)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Off while migrating: layout 7 makes the threads table again, which the others refer to.
    // (The pragma does nothing inside a transaction, so it is set around it.)
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // One transaction, so a process opening a new file at the same time sees it either empty or
    // with its version set, and one opening an old file sees it either old or migrated.
    wait_until(&conn, deadline)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Said here, logged once the transaction has committed: a migration that did not happen must
    // not be announced as if it had.
    let mut from = None;
    let now_found = layout(&tx, path)?;
    let new = now_found == Layout::New;
    match now_found {
        layout if refuses(layout) => bail!(no_migration(path, layout)),
        Layout::Migrates(old) => {
            if old == 5 {
                tx.execute_batch(MIGRATE_FROM_5)
                    .with_context(|| format!("migrating database {path} from layout 5 to 6"))?;
            }
            if old <= 6 {
                tx.execute_batch(MIGRATE_FROM_6)
                    .with_context(|| format!("migrating database {path} from layout 6 to 7"))?;
                let broken = broken_thread_refs(&tx)?;
                if !broken.is_empty() {
                    let rows: Vec<String> = broken
                        .iter()
                        .map(|(table, rowid, parent)| {
                            format!("{table} rowid {rowid} refers to {parent}")
                        })
                        .collect();
                    bail!(
                        "migrating database {path} from layout 6 to 7: {} broken references: {}",
                        broken.len(),
                        rows.join("; ")
                    );
                }
            }
            if old <= 7 {
                tx.execute_batch(MIGRATE_FROM_7)
                    .with_context(|| format!("migrating database {path} from layout 7 to 8"))?;
            }
            if old <= 8 {
                tx.execute_batch(MIGRATE_FROM_8)
                    .with_context(|| format!("migrating database {path} from layout 8 to 9"))?;
            }
            if old <= 9 {
                tx.execute_batch(MIGRATE_FROM_9)
                    .with_context(|| format!("migrating database {path} from layout 9 to 10"))?;
            }
            from = Some(old);
        }
        Layout::New | Layout::Current => {}
    }
    tx.execute_batch(SCHEMA)?;
    make_system_person(&tx, now_unix())?;
    // A new database made by a release has no release to announce: it starts at this version.
    // One made by a build between releases records nothing (see `release::record_new`).
    if new {
        crate::release::record_new(&tx, env!("CARGO_PKG_VERSION"), crate::release::CHANGELOG)?;
    }
    // How many closed threads layout 8 opens.
    let mut opened = 0;
    if from.is_some_and(|old| old <= 7) {
        opened = tx.query_row(
            "SELECT count(*) FROM threads WHERE status = 'closed'",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        tx.execute_batch(OPEN_CLOSED)
            .with_context(|| format!("migrating database {path} from layout 7 to 8"))?;
    }
    let key = key_file
        .map(|file| server_key(&tx, path, file))
        .transpose()?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    if let Some(old) = from {
        for line in migrated(old, opened) {
            tracing::info!(db = path, "{line}");
        }
    }
    if let (Some(file), Some((_, Some(sealed)))) = (key_file, &key) {
        let key_file = file.display();
        let which = if sealed.made { "a new" } else { "the existing" };
        tracing::info!(
            "encrypting with {which} key at {key_file}: keep a copy of it apart from the \
             database and its backups; without it, the notes cannot be read"
        );
        tracing::info!(
            db = path,
            key = %key_file,
            notes = sealed.notes,
            titles = sealed.titles,
            "encrypted the database's note text and thread titles"
        );
    }
    if migrate {
        let pending: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM meta WHERE key = 'scrub_pending')",
            [],
            |r| r.get(0),
        )?;
        if pending && let Err(e) = scrub(&conn) {
            tracing::warn!(
                db = path,
                error = %format!("{e:#}"),
                "the database could not be rewritten after its notes were encrypted: pages of \
                 it may still hold them in plain text; the next start tries again"
            );
        }
    }
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Open: from now on, each statement waits for another process's lock as long as it needs.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok((conn, key.map(|(key, _)| key)))
}

/// Rewrites the database once its notes were encrypted, so that no page of it, nor of its `-wal`,
/// still holds them in plain text: `VACUUM` writes every page again, and a truncating checkpoint
/// moves them into the file and empties the `-wal`. Until both are done, `meta.scrub_pending`
/// stays, and every start of `serve` tries again: a crash between the encryption and this loses
/// nothing.
fn scrub(conn: &Connection) -> Result<()> {
    conn.execute_batch("VACUUM")?;
    let blocked: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if blocked != 0 {
        bail!("another connection kept the checkpoint from finishing");
    }
    conn.execute("DELETE FROM meta WHERE key = 'scrub_pending'", [])?;
    tracing::info!("rewrote the database: no page of it holds the notes in plain text");
    Ok(())
}

/// What `server_key` did to a database never encrypted before.
struct Sealed {
    /// The key file was made now.
    made: bool,
    notes: usize,
    titles: usize,
}

/// The key of the database `tx` has open (in its migration transaction), from `file`. One
/// encrypted already (`meta.key_check`) opens only with its own key (`key_for`). One never
/// encrypted takes the key in `file`, made if missing: written and synced before this
/// transaction commits, so a start that dies before the commit leaves the database as it was and
/// the next one takes the same key. Every note text and thread title still in plain text is
/// sealed with it, and `key_check` records it.
fn server_key(tx: &Connection, path: &str, file: &Path) -> Result<(Key, Option<Sealed>)> {
    if let Some(check) = key_check(tx)? {
        return Ok((key_for(&check, file)?, None));
    }
    let (key, made) = crypto::load_or_create(file)?;
    let (notes, titles) =
        seal_plain(tx, &key).with_context(|| format!("encrypting database {path}"))?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('key_check', ?1)",
        [key.check()],
    )?;
    if notes + titles > 0 {
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('scrub_pending', '1')",
            [],
        )?;
    }
    let sealed = Sealed {
        made,
        notes,
        titles,
    };
    Ok((key, Some(sealed)))
}

/// Seals every note text and thread title still in plain text with `key`, as bytes (whatever
/// they hold), and says how many of each.
fn seal_plain(tx: &Connection, key: &Key) -> rusqlite::Result<(usize, usize)> {
    let mut notes = 0;
    let mut after = 0;
    loop {
        let batch: Vec<(i64, String, String, Vec<u8>)> = tx
            .prepare_cached(
                "SELECT seq, id, thread_id, CAST(text AS BLOB) FROM notes
                 WHERE seq > ?1 AND typeof(text) = 'text' ORDER BY seq LIMIT 500",
            )?
            .query_map([after], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let Some((last, ..)) = batch.last() else {
            break;
        };
        after = *last;
        for (seq, id, thread, text) in &batch {
            tx.prepare_cached("UPDATE notes SET text = ?2 WHERE seq = ?1")?
                .execute(params![seq, key.seal(&crypto::note_aad(thread, id), text)])?;
        }
        notes += batch.len();
    }
    let titles: Vec<(String, Vec<u8>)> = tx
        .prepare("SELECT id, CAST(title AS BLOB) FROM threads WHERE typeof(title) = 'text'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, title) in &titles {
        tx.prepare_cached("UPDATE threads SET title = ?2 WHERE id = ?1")?
            .execute(params![id, key.seal(&crypto::title_aad(id), title)])?;
    }
    Ok((notes, titles.len()))
}

/// `meta.key_check`: set once `serve` has encrypted the database.
pub fn key_check(conn: &Connection) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = 'key_check'", [], |r| {
            r.get(0)
        })
        .optional()?)
}

/// The key in `file`, which must be the one that made `check`.
fn key_for(check: &[u8], file: &Path) -> Result<Key> {
    let shown = file.display();
    let Some(key) = crypto::load(file)? else {
        bail!(
            "this database is encrypted but there is no key at {shown} (RELEVO_KEY_FILE). \
             Restore the key it was encrypted with; without it, its notes cannot be read."
        );
    };
    if !key.opens(check) {
        bail!(
            "this key does not open this database (RELEVO_KEY_FILE={shown}).\n\
             Use the key it was encrypted with. Without it, its notes cannot be read."
        );
    }
    Ok(key)
}

/// What a migration from layout `old` logs, a line per step, once it has committed.
fn migrated(old: i64, opened: i64) -> Vec<String> {
    let mut lines = Vec::new();
    if old == 5 {
        lines.push("migrated the database from layout 5 to 6".to_string());
    }
    if old <= 6 {
        lines.push(
            "migrated the database from layout 6 to 7: team threads are global ones now"
                .to_string(),
        );
    }
    if old <= 7 {
        lines.push(format!(
            "migrated the database from layout 7 to 8: threads no longer close; opened {opened} \
             closed threads, handled by everyone they name"
        ));
    }
    if old <= 8 {
        lines.push(
            "migrated the database from layout 8 to 9: sessions keep the client and version \
             their agent reports"
                .to_string(),
        );
    }
    if old <= 9 {
        lines.push(
            "migrated the database from layout 9 to 10: sessions keep the agent they say they are"
                .to_string(),
        );
    }
    lines.push(
        "migrated the database from layout 10 to 11: note text and thread titles are encrypted"
            .to_string(),
    );
    lines
}

/// The tables that refer to `threads`: the only references layout 6 to 7 can break, since it makes
/// the `threads` table again. A broken reference elsewhere was there with 0.3 too, and is no
/// reason to refuse to start.
const THREAD_REFERENCES: [&str; 4] = ["participants", "notes", "session_threads", "thread_teams"];

/// The references that no longer hold in those tables, each as (table, rowid, parent).
fn broken_thread_refs(tx: &Connection) -> Result<Vec<(String, i64, String)>> {
    let mut out = Vec::new();
    for table in THREAD_REFERENCES {
        // `thread_teams` is one of the tables layout 7 brings: in a database of layout 6 it is
        // not there yet, and has no reference to check.
        let there: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        if there == 0 {
            continue;
        }
        let mut stmt =
            tx.prepare_cached("SELECT \"table\", rowid, parent FROM pragma_foreign_key_check(?1)")?;
        let rows = stmt.query_map([table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// Why a command other than `serve` refuses a database of layout 5 to 10.
fn no_migration(path: &str, layout: Layout) -> String {
    format!(
        "database {path} has the layout of {}, which only `relevo serve` migrates (in place, \
         when it starts). Keep a copy first with `relevo backup <file>` (it copies this layout \
         too), then start the server, and run this again once it runs (see \"Upgrades\" in \
         docs/install.md)",
        layout.made_by()
    )
}

/// What a database's layout means to this version.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Layout {
    /// Nothing in it yet: it gets this version's layout.
    New,
    /// This version's layout.
    Current,
    /// Layout 5 (relevo 0.1.0 to 0.2.x), 6 (relevo 0.3.x), 7 (relevo 0.4.x), 8 (development
    /// builds of 0.5.0), 9 (relevo 0.5.x) or 10 (relevo 0.6.0 to 0.8.x): `relevo serve` migrates
    /// it.
    Migrates(i64),
}

impl Layout {
    /// The versions of relevo that made a layout `serve` migrates.
    pub fn made_by(self) -> &'static str {
        match self {
            Layout::Migrates(5) => "relevo 0.1.0 to 0.2.x",
            Layout::Migrates(6) => "relevo 0.3.x",
            Layout::Migrates(7) => "relevo 0.4.x",
            Layout::Migrates(9) => "relevo 0.5.x",
            Layout::Migrates(10) => "relevo 0.6.0 to 0.8.x",
            Layout::Migrates(_) => "a development build of relevo 0.5.0 (before its release)",
            Layout::New | Layout::Current => "this relevo",
        }
    }
}

/// The layout of the database `conn` has open, or why this version refuses it.
fn layout(conn: &Connection, path: &str) -> Result<Layout> {
    // The version and the tables in one statement, so of the same moment: read apart, a file
    // another process made in between would show its tables and not yet its version, as a file
    // of before 0.1.0 does.
    let (version, tables): (i64, i64) = conn.query_row(
        "SELECT (SELECT user_version FROM pragma_user_version),
                (SELECT count(*) FROM sqlite_master)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if version == SCHEMA_VERSION {
        return Ok(Layout::Current);
    }
    if version > SCHEMA_VERSION {
        bail!("database {path} was made by a newer version of relevo; use that one");
    }
    if (5..SCHEMA_VERSION).contains(&version) {
        return Ok(Layout::Migrates(version));
    }
    if tables > 0 {
        bail!(
            "database {path} was made by an older version of relevo (before 0.1.0), which \
             this one does not migrate: delete it (with its -wal and -shm files) and invite \
             the team again"
        );
    }
    Ok(Layout::New)
}

/// The layout of the existing database at `path`, or why this version refuses it, found without
/// writing anything (see `read_only`). An encrypted database must have its key in `key_file`
/// (read only: never made here); one not encrypted yet needs none, but a key file already there
/// must be one `serve` would take. `key_file` is only asked for then: where it cannot be said
/// (no `HOME`), a database that needs no key still checks.
pub fn check(path: &str, key_file: impl FnOnce() -> Result<PathBuf>) -> Result<Layout> {
    if !std::path::Path::new(path).is_file() {
        bail!("no database at {path}");
    }
    let conn = read_only(path).with_context(|| format!("opening database {path}"))?;
    let found = layout(&conn, path)?;
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
        [],
        |r| r.get(0),
    )?;
    match has_meta.then(|| key_check(&conn)).transpose()?.flatten() {
        Some(check) => {
            key_for(&check, &key_file()?)?;
        }
        None => {
            if let Ok(file) = key_file() {
                crypto::load(&file)?;
            }
        }
    }
    Ok(found)
}

/// Opens the existing database at `path` to read it only, writing nothing: not the database, and
/// no `-wal` or `-shm` file is left behind. A database without them (no server has it open) is
/// read as immutable; one with them, read-only, through the server's. Never creates it.
pub fn read_only(path: &str) -> rusqlite::Result<Connection> {
    let quiet = ["-wal", "-shm"]
        .iter()
        .all(|ext| !std::path::Path::new(&format!("{path}{ext}")).exists());
    // A URI, so that `immutable` can be asked for: `%`, `?` and `#` would end the path.
    let file = path
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    let uri = format!(
        "file:{file}?{}",
        if quiet { "immutable=1" } else { "mode=ro" }
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&uri, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(conn)
}

/// Creates a missing database file readable by the server user only: all but the note text and
/// thread titles is plain. SQLite gives the `-wal` and `-shm` files the same mode. Existing files
/// are left as they are.
fn create_private(path: &str) -> std::io::Result<()> {
    if path == ":memory:" {
        return Ok(());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).map(drop)
}

pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// `prefix` + 12 random base32 characters (lowercase RFC 4648 alphabet).
pub fn random_id(prefix: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let tail: String = (0..12)
        .map(|_| ALPHABET[rand::random_range(0..32)] as char)
        .collect();
    format!("{prefix}{tail}")
}

/// A new token secret: `rlv_` + base64url of 32 random bytes (43 characters).
fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    format!("rlv_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Hex SHA-256 of a token secret: the only form in which tokens are stored.
pub fn hash_token(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn valid_handle(handle: &str) -> bool {
    let b = handle.as_bytes();
    let first_ok = b
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = b[1.min(b.len())..]
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-');
    first_ok && rest_ok && (2..=32).contains(&b.len())
}

/// A `Relevo-Session` id: `^[A-Za-z0-9_-]{8,64}$`.
pub fn valid_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// What `valid_context` accepts, for error messages.
pub const CONTEXT_FORMAT: &str = "a repository ^[a-z0-9][a-z0-9._-]{0,63}$, optionally followed by @ and a branch ^[a-z0-9._/-]{1,100}$";

/// A context: a repository, `^[a-z0-9][a-z0-9._-]{0,63}$`, optionally followed by `@` and a
/// branch, `^[a-z0-9._/-]{1,100}$`.
pub fn valid_context(context: &str) -> bool {
    let (repo, branch) = match context.split_once('@') {
        Some((repo, branch)) => (repo, Some(branch)),
        None => (context, None),
    };
    let b = repo.as_bytes();
    let first_ok = b
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = b[1.min(b.len())..]
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'));
    let branch_ok = branch.is_none_or(|branch| {
        (1..=100).contains(&branch.len())
            && branch.bytes().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, b'.' | b'_' | b'/' | b'-')
            })
    });
    first_ok && rest_ok && b.len() <= 64 && branch_ok
}

/// The repository part of a context that names a branch (`pagos-api` for
/// `pagos-api@feat/refunds`); `None` for a context without one.
pub fn context_repo(context: &str) -> Option<&str> {
    context.split_once('@').map(|(repo, _)| repo)
}

fn one_line(what: &str, value: &str) -> Result<()> {
    if value.contains(['\n', '\r']) {
        bail!("{what} must be one line");
    }
    Ok(())
}

fn person_id(conn: &Connection, handle: &str) -> Result<i64> {
    not_system(conn, handle)?;
    conn.query_row("SELECT id FROM people WHERE handle = ?1", [handle], |r| {
        r.get(0)
    })
    .optional()?
    .with_context(|| format!("no person with handle '{handle}'"))
}

/// A token as just created: its id and the secret, which is shown once and never stored.
pub struct NewToken {
    pub id: String,
    pub secret: String,
}

/// Creates the person, or reuses (and reactivates) an existing one, then issues a token.
pub fn invite(
    conn: &Connection,
    handle: &str,
    name: &str,
    about: Option<&str>,
    label: &str,
    now: i64,
) -> Result<NewToken> {
    check_label(label)?;
    let tx = add_person(conn, handle, name, about, now)?;
    let token = add_token(&tx, handle, label, now)?;
    tx.commit()?;
    Ok(token)
}

/// The name of the person with this handle, for `invite` without `--name`.
pub fn person_name(conn: &Connection, handle: &str) -> Result<String> {
    let id =
        person_id(conn, handle).map_err(|e| anyhow::anyhow!("{e:#}: a new person needs --name"))?;
    Ok(conn.query_row("SELECT name FROM people WHERE id = ?1", [id], |r| r.get(0))?)
}

/// How long an invitation works.
pub const INVITE_LIFETIME_SECS: i64 = 24 * 3600;

/// The characters of an invitation code: digits and capitals, without 0, 1, I and O, which are
/// read for one another. 8 of them are 40 bits.
const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// An invitation as just made: its code, shown once (`XXXX-XXXX`), and never stored.
pub struct Invitation {
    pub code: String,
    pub expires_at: i64,
}

/// How an invitation's link reaches the server: its address, and, when that is `serve`'s own TLS
/// address (with a certificate of its own), the certificate's key pin (base64 SHA-256 of its
/// SubjectPublicKeyInfo, for `curl --pinnedpubkey`) and fingerprint (for `relevo connect
/// --fingerprint`). Never for another address, a proxy's say: its certificate is another.
#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    pub url: String,
    pub pin: Option<String>,
    pub fingerprint: Option<String>,
}

/// Creates the person, or reuses (and reactivates) an existing one, as `invite` does, then gives
/// them a new invitation to the server at `address`, in place of an unused one they had.
pub fn invite_link(
    conn: &Connection,
    handle: &str,
    name: &str,
    about: Option<&str>,
    address: &Address,
    now: i64,
) -> Result<Invitation> {
    let tx = add_person(conn, handle, name, about, now)?;
    let person = person_id(&tx, handle)?;
    tx.execute(
        "DELETE FROM invites WHERE person_id = ?1 AND used_at IS NULL",
        [person],
    )?;
    let raw: String = (0..8)
        .map(|_| CODE_ALPHABET[rand::random_range(0..32)] as char)
        .collect();
    let invitation = Invitation {
        code: invite_code(&raw).expect("a code of the alphabet"),
        expires_at: now + INVITE_LIFETIME_SECS,
    };
    tx.execute(
        "INSERT INTO invites (hash, person_id, url, pin, fingerprint, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            invite_hash(&raw).expect("a code of the alphabet"),
            person,
            address.url,
            address.pin,
            address.fingerprint,
            now,
            invitation.expires_at
        ],
    )?;
    tx.commit()?;
    Ok(invitation)
}

/// A code as `relevo invite` prints it (`XXXX-XXXX`), from one given in any case, with or
/// without its `-`; `None` when it cannot be a code.
pub fn invite_code(code: &str) -> Option<String> {
    let raw: String = code
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (raw.len() == 8 && raw.bytes().all(|c| CODE_ALPHABET.contains(&c)))
        .then(|| format!("{}-{}", &raw[..4], &raw[4..]))
}

/// The hash a code is stored by: of its 8 characters, without the `-`.
fn invite_hash(code: &str) -> Option<String> {
    invite_code(code).map(|code| hash_token(&code.replace('-', "")))
}

/// What an invitation code is now.
#[derive(Debug, PartialEq)]
pub enum Invite {
    /// It works: for this person, to the server at `address`, until `expires_at`.
    Valid {
        handle: String,
        name: String,
        /// Their person is marked as a bot.
        bot: bool,
        address: Address,
        expires_at: i64,
    },
    /// It worked once, for this handle.
    Used(String),
    /// It was not used in time, for this handle.
    Expired(String),
    /// No such code (never made, replaced, or of a person since deactivated).
    Unknown,
}

/// What `code` is at `now`. Reads only: the pages that show an invitation do not use it.
pub fn find_invite(conn: &Connection, code: &str, now: i64) -> rusqlite::Result<Invite> {
    let Some(hash) = invite_hash(code) else {
        return Ok(Invite::Unknown);
    };
    let row = conn
        .query_row(
            "SELECT p.handle, p.name, p.active, i.url, i.pin, i.fingerprint, i.expires_at,
                    i.used_at, EXISTS (SELECT 1 FROM bots WHERE person_id = p.id)
             FROM invites i JOIN people p ON p.id = i.person_id WHERE i.hash = ?1",
            [hash],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    Address {
                        url: r.get(3)?,
                        pin: r.get(4)?,
                        fingerprint: r.get(5)?,
                    },
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, bool>(8)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        None | Some((_, _, false, ..)) => Invite::Unknown,
        Some((handle, .., Some(_), _)) => Invite::Used(handle),
        Some((handle, _, _, _, expires_at, None, _)) if expires_at <= now => {
            Invite::Expired(handle)
        }
        Some((handle, name, _, address, expires_at, None, bot)) => Invite::Valid {
            handle,
            name,
            bot,
            address,
            expires_at,
        },
    })
}

/// An invitation used: for whom, to which server, and the token it was traded for.
pub struct Joined {
    pub handle: String,
    pub url: String,
    pub token: NewToken,
}

/// Uses `code`: when it works, marks it used and issues a new token for its person, labelled
/// `joined <date>`, in one step; otherwise says what it is, and changes nothing.
pub fn redeem_invite(
    conn: &Connection,
    code: &str,
    now: i64,
) -> Result<std::result::Result<Joined, Invite>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let (handle, url) = match find_invite(&tx, code, now)? {
        Invite::Valid {
            handle, address, ..
        } => (handle, address.url),
        other => return Ok(Err(other)),
    };
    let hash = invite_hash(code).expect("a valid invitation's code");
    tx.execute(
        "UPDATE invites SET used_at = ?2 WHERE hash = ?1",
        params![hash, now],
    )?;
    let date = rfc3339(now);
    let token = add_token(&tx, &handle, &format!("joined {}", &date[..10]), now)?;
    tx.commit()?;
    Ok(Ok(Joined { handle, url, token }))
}

/// What `serve` says of the address it serves at, for invitation links: the URL teammates reach
/// it at, and, when it speaks TLS with a certificate of its own, that certificate's key pin and
/// fingerprint (see `Address`) and the port it serves it on.
#[derive(Debug, Default, PartialEq)]
pub struct Public {
    pub url: Option<String>,
    pub pin: Option<String>,
    pub fingerprint: Option<String>,
    pub port: Option<u16>,
}

const PUBLIC_KEYS: [&str; 4] = ["public_url", "tls_pin", "tls_fingerprint", "tls_port"];

/// Records `public` in `meta`, in place of what an earlier start said.
pub fn set_public(conn: &Connection, public: &Public) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let port = public.port.map(|p| p.to_string());
    for (key, value) in
        PUBLIC_KEYS
            .iter()
            .zip([&public.url, &public.pin, &public.fingerprint, &port])
    {
        tx.execute("DELETE FROM meta WHERE key = ?1", [key])?;
        if let Some(value) = value {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// What `serve` last recorded with `set_public`.
pub fn public(conn: &Connection) -> rusqlite::Result<Public> {
    let get = |key: &str| {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
    };
    Ok(Public {
        url: get(PUBLIC_KEYS[0])?,
        pin: get(PUBLIC_KEYS[1])?,
        fingerprint: get(PUBLIC_KEYS[2])?,
        port: get(PUBLIC_KEYS[3])?.and_then(|p: String| p.parse().ok()),
    })
}

/// The person part of `invite` and `invite_link`: creates the person, or reuses (and
/// reactivates) an existing one, in a transaction the caller commits.
fn add_person<'c>(
    conn: &'c Connection,
    handle: &str,
    name: &str,
    about: Option<&str>,
    now: i64,
) -> Result<Transaction<'c>> {
    if !valid_handle(handle) {
        bail!("handle must match ^[a-z0-9][a-z0-9_-]{{1,31}}$");
    }
    if RESERVED_HANDLES.contains(&handle) {
        bail!("'{handle}' is reserved: it was 0.3's whole team, and @{handle} names nobody");
    }
    not_system(conn, handle)?;
    one_line("name", name)?;
    if let Some(about) = about {
        one_line("about", about)?;
    }
    // IMMEDIATE so the team-name check and the insert are one atomic step: a concurrent
    // `team add <handle>` sees the person and fails, not a silent duplicate @-name.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if team_id(&tx, handle)?.is_some() {
        bail!(
            "'{handle}' is the name of a team: a handle cannot be one, so that @{handle} means one thing"
        );
    }
    tx.execute(
        "INSERT INTO people (handle, name, about, created_at) VALUES (?1, ?2, coalesce(?3, ''), ?4)
         ON CONFLICT (handle) DO UPDATE SET name = ?2, about = coalesce(?3, about), active = 1",
        params![handle, name, about, now],
    )?;
    Ok(tx)
}

fn check_label(label: &str) -> Result<()> {
    if label.trim().is_empty() {
        bail!("label must not be empty");
    }
    one_line("label", label)
}

pub fn add_token(conn: &Connection, handle: &str, label: &str, now: i64) -> Result<NewToken> {
    check_label(label)?;
    let person = person_id(conn, handle)?;
    let active: bool =
        conn.query_row("SELECT active FROM people WHERE id = ?1", [person], |r| {
            r.get(0)
        })?;
    if !active {
        bail!("'{handle}' is deactivated: invite them back to give them a token");
    }
    let token = NewToken {
        id: random_id("t_"),
        secret: new_secret(),
    };
    conn.execute(
        "INSERT INTO tokens (id, person_id, label, hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![token.id, person, label, hash_token(&token.secret), now],
    )?;
    Ok(token)
}

pub struct TokenInfo {
    pub id: String,
    pub handle: String,
    pub label: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

pub fn list_tokens(conn: &Connection, handle: Option<&str>) -> Result<Vec<TokenInfo>> {
    if let Some(handle) = handle {
        person_id(conn, handle)?;
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, p.handle, t.label, t.created_at, t.revoked_at
         FROM tokens t JOIN people p ON p.id = t.person_id
         WHERE (?1 IS NULL OR p.handle = ?1) AND p.id IS NOT {SYSTEM_PERSON_SQL}
         ORDER BY p.handle, t.created_at, t.id",
    ))?;
    let rows = stmt.query_map([handle], |r| {
        Ok(TokenInfo {
            id: r.get(0)?,
            handle: r.get(1)?,
            label: r.get(2)?,
            created_at: r.get(3)?,
            revoked_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Revokes the token and forgets its sessions, with their claims: no request can be made as
/// them again, so they must not keep threads from the person's other sessions.
pub fn revoke_token(conn: &Connection, id: &str, now: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, now],
    )?;
    if changed == 0 {
        bail!("no active token with id '{id}'");
    }
    let sessions = select_ids(&tx, "SELECT id FROM sessions WHERE token_id = ?1", id)?;
    forget_sessions(&tx, &sessions)?;
    tx.commit()?;
    Ok(())
}

fn select_ids(conn: &Connection, sql: &str, param: impl rusqlite::ToSql) -> Result<Vec<i64>> {
    let ids = conn
        .prepare(sql)?
        .query_map([param], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Forgets these sessions and their claims: the threads they claimed go back to every session of
/// their person that sees them. Callers that can, wake those sessions' waiting requests.
pub fn forget_sessions(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    for id in ids {
        release_claims(conn, *id)?;
        conn.execute("DELETE FROM session_threads WHERE session_id = ?1", [id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    }
    Ok(())
}

/// Ends every claim of the session: those threads count as unclaimed for its person.
pub fn release_claims(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE participants SET claimed_by = NULL, claimed_context = NULL, claimed_at = NULL
         WHERE claimed_by = ?1",
    )?
    .execute([id])?;
    conn.prepare_cached("UPDATE session_threads SET claimed_by = NULL WHERE claimed_by = ?1")?
        .execute([id])?;
    conn.prepare_cached("UPDATE sessions SET claims = 0 WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

pub struct PersonInfo {
    pub handle: String,
    pub name: String,
    pub about: String,
    pub active: bool,
    pub bot: bool,
}

/// Whether the person `people.id` is marked as a bot, as SQL.
pub const IS_BOT_SQL: &str = "EXISTS (SELECT 1 FROM bots WHERE person_id = people.id)";

/// Everyone on the server but its own person (`SYSTEM_HANDLE`).
pub fn list_people(conn: &Connection) -> Result<Vec<PersonInfo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT handle, name, about, active, {IS_BOT_SQL} FROM people
         WHERE id IS NOT {SYSTEM_PERSON_SQL} ORDER BY handle"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(PersonInfo {
            handle: r.get(0)?,
            name: r.get(1)?,
            about: r.get(2)?,
            active: r.get(3)?,
            bot: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn set_about(conn: &Connection, handle: &str, about: &str) -> Result<()> {
    one_line("about", about)?;
    let id = person_id(conn, handle)?;
    conn.execute(
        "UPDATE people SET about = ?2 WHERE id = ?1",
        params![id, about],
    )?;
    Ok(())
}

/// Marks the person as a bot, or not.
pub fn set_bot(conn: &Connection, handle: &str, bot: bool) -> Result<()> {
    let id = person_id(conn, handle)?;
    let sql = if bot {
        "INSERT OR IGNORE INTO bots (person_id) VALUES (?1)"
    } else {
        "DELETE FROM bots WHERE person_id = ?1"
    };
    conn.execute(sql, [id])?;
    Ok(())
}

pub struct SessionInfo {
    pub handle: String,
    pub label: String,
    /// The `Relevo-Session` id, or `None` for a token's default session.
    pub session: Option<String>,
    pub context: Option<String>,
    pub last_seen: i64,
    /// What its latest request said of its client, `<name>/<version>`; `None`: nothing.
    pub client: Option<String>,
    /// Whether `client` was read from the User-Agent, not said.
    pub client_inferred: bool,
    /// The agent its latest request said it is; `None`: nothing.
    pub agent: Option<String>,
}

impl SessionInfo {
    /// The client's name and version, the version marked `?` when inferred; `None`: unknown.
    pub fn client_version(&self) -> Option<(&str, String)> {
        let (name, version) = self.client.as_deref()?.split_once('/')?;
        let mark = if self.client_inferred { "?" } else { "" };
        Some((name, format!("{version}{mark}")))
    }
}

/// Sessions seen in the last 7 days, of every active person or of one, most recently seen first.
pub fn list_sessions(
    conn: &Connection,
    handle: Option<&str>,
    now: i64,
) -> Result<Vec<SessionInfo>> {
    if let Some(handle) = handle {
        person_id(conn, handle)?;
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT p.handle, t.label, nullif(s.client_id, ''), s.context, s.last_seen, s.client,
                s.client_inferred, s.agent
         FROM sessions s JOIN tokens t ON t.id = s.token_id JOIN people p ON p.id = s.person_id
         WHERE s.last_seen > ?2 AND (?1 IS NULL OR p.handle = ?1) AND p.active = 1
           AND p.id IS NOT {SYSTEM_PERSON_SQL}
         ORDER BY p.handle, s.last_seen DESC, s.id"
    ))?;
    let rows = stmt.query_map(params![handle, now - SESSION_LIFETIME_SECS], |r| {
        Ok(SessionInfo {
            handle: r.get(0)?,
            label: r.get(1)?,
            session: r.get(2)?,
            context: r.get(3)?,
            last_seen: r.get(4)?,
            client: r.get(5)?,
            client_inferred: r.get(6)?,
            agent: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Deactivated people can no longer authenticate nor receive new threads. Every token of theirs
/// is revoked for good, and its sessions forgotten with their claims, and an unused invitation of
/// theirs is dropped: `invite` brings them back, and they find their threads again. Returns how
/// many tokens it revoked.
pub fn deactivate(conn: &Connection, handle: &str, now: i64) -> Result<usize> {
    let id = person_id(conn, handle)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE people SET active = 0 WHERE id = ?1", [id])?;
    tx.execute(
        "DELETE FROM invites WHERE person_id = ?1 AND used_at IS NULL",
        [id],
    )?;
    let revoked = tx.execute(
        "UPDATE tokens SET revoked_at = ?2 WHERE person_id = ?1 AND revoked_at IS NULL",
        params![id, now],
    )?;
    let sessions = select_ids(&tx, "SELECT id FROM sessions WHERE person_id = ?1", id)?;
    forget_sessions(&tx, &sessions)?;
    tx.commit()?;
    Ok(revoked)
}

fn team_id(conn: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM teams WHERE name = ?1", [name], |r| r.get(0))
        .optional()?)
}

fn existing_team(conn: &Connection, name: &str) -> Result<i64> {
    team_id(conn, name)?.with_context(|| format!("no team named '{name}' (see relevo teams)"))
}

/// Creates a team, with nobody in it. Its name follows the rule of handles, and is never the
/// handle of a person (active or not), nor `team`, `equipo` or the server's own `relevo`, so that
/// a mention means one thing.
pub fn add_team(conn: &Connection, name: &str, title: Option<&str>, now: i64) -> Result<()> {
    if !valid_handle(name) {
        bail!("a team name must match ^[a-z0-9][a-z0-9_-]{{1,31}}$");
    }
    if RESERVED_HANDLES.contains(&name) {
        bail!("'{name}' is reserved: it was 0.3's whole team, and @{name} names nobody");
    }
    not_system(conn, name)?;
    if let Some(title) = title {
        one_line("title", title)?;
    }
    // IMMEDIATE so the person-handle check and the insert are one atomic step: a concurrent
    // `invite <name>` sees the team and fails, not a silent duplicate @-name.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let person: Option<bool> = tx
        .query_row("SELECT active FROM people WHERE handle = ?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(active) = person {
        let state = if active { "" } else { " (inactive)" };
        bail!("'{name}' is the handle of a person{state}: a team cannot take it");
    }
    if team_id(&tx, name)?.is_some() {
        bail!("there is a team named '{name}' already");
    }
    tx.execute(
        "INSERT INTO teams (name, title, created_at) VALUES (?1, coalesce(?2, ''), ?3)",
        params![name, title, now],
    )?;
    tx.commit()?;
    Ok(())
}

/// What `team join` or `team leave` did for one person.
pub enum Membership {
    Changed,
    /// Joining someone already in the team, or leaving someone not in it.
    Already,
}

/// Puts active people in a team. An unknown or inactive handle fails, changing nothing.
pub fn join_team(conn: &Connection, name: &str, handles: &[String]) -> Result<Vec<Membership>> {
    let team = existing_team(conn, name)?;
    let tx = conn.unchecked_transaction()?;
    let mut done = Vec::new();
    for handle in handles {
        not_system(&tx, handle)?;
        let active: Option<bool> = tx
            .query_row(
                "SELECT active FROM people WHERE handle = ?1",
                [handle],
                |r| r.get(0),
            )
            .optional()?;
        let person = match active {
            Some(true) => person_id(&tx, handle)?,
            Some(false) => {
                bail!("'{handle}' is deactivated: invite them back first; nobody joined")
            }
            None => bail!("no person with handle '{handle}'; nobody joined"),
        };
        let added = tx.execute(
            "INSERT OR IGNORE INTO team_members (team_id, person_id) VALUES (?1, ?2)",
            params![team, person],
        )?;
        done.push(if added > 0 {
            Membership::Changed
        } else {
            Membership::Already
        });
    }
    tx.commit()?;
    Ok(done)
}

/// Takes people out of a team. An unknown handle fails, changing nothing.
pub fn leave_team(conn: &Connection, name: &str, handles: &[String]) -> Result<Vec<Membership>> {
    let team = existing_team(conn, name)?;
    let tx = conn.unchecked_transaction()?;
    let mut done = Vec::new();
    for handle in handles {
        let person = person_id(&tx, handle).context("nobody left")?;
        let removed = tx.execute(
            "DELETE FROM team_members WHERE team_id = ?1 AND person_id = ?2",
            params![team, person],
        )?;
        done.push(if removed > 0 {
            Membership::Changed
        } else {
            Membership::Already
        });
    }
    tx.commit()?;
    Ok(done)
}

/// Deletes a team and its membership. Its threads keep its name (`thread_teams`), and a later
/// team may take it.
pub fn remove_team(conn: &Connection, name: &str) -> Result<()> {
    let team = existing_team(conn, name)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM team_members WHERE team_id = ?1", [team])?;
    tx.execute("DELETE FROM teams WHERE id = ?1", [team])?;
    tx.commit()?;
    Ok(())
}

pub struct TeamInfo {
    pub name: String,
    pub title: String,
    /// (handle, active), by handle.
    pub members: Vec<(String, bool)>,
}

/// Every team, by name, with its members, active or not.
pub fn list_teams(conn: &Connection) -> Result<Vec<TeamInfo>> {
    let mut teams: Vec<(i64, TeamInfo)> = conn
        .prepare("SELECT id, name, title FROM teams ORDER BY name")?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                TeamInfo {
                    name: r.get(1)?,
                    title: r.get(2)?,
                    members: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "SELECT m.team_id, p.handle, p.active FROM team_members m
         JOIN people p ON p.id = m.person_id ORDER BY p.handle",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
    for row in rows {
        let (team, handle, active) = row?;
        if let Some((_, t)) = teams.iter_mut().find(|(id, _)| *id == team) {
            t.members.push((handle, active));
        }
    }
    Ok(teams.into_iter().map(|(_, t)| t).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles() {
        assert!(valid_handle("ana"));
        assert!(valid_handle("a1_-b"));
        assert!(!valid_handle("a"));
        assert!(!valid_handle("_ana"));
        assert!(!valid_handle("Ana"));
        assert!(!valid_handle(&"a".repeat(33)));
        assert!(valid_handle(&"a".repeat(32)));
    }

    #[test]
    fn session_ids_and_contexts() {
        assert!(valid_session_id("abcd1234"));
        assert!(valid_session_id(&"A_-z".repeat(16)));
        assert!(!valid_session_id("short"));
        assert!(!valid_session_id(&"a".repeat(65)));
        assert!(!valid_session_id("abcd 1234"));
        assert!(!valid_session_id("abcd.1234"));
        assert!(valid_context("pagos-api"));
        assert!(valid_context("a"));
        assert!(valid_context("v1.2_x-y"));
        assert!(valid_context(&"a".repeat(64)));
        assert!(!valid_context(&"a".repeat(65)));
        assert!(!valid_context(""));
        assert!(!valid_context("-x"));
        assert!(!valid_context(".x"));
        assert!(!valid_context("Pagos"));
        assert!(!valid_context("a/b"));
        assert!(valid_context("pagos-api@feat/refunds"));
        assert!(valid_context("pagos-api@v1.2_x-y/z"));
        assert!(valid_context(&format!("a@{}", "b".repeat(100))));
        assert!(!valid_context(&format!("a@{}", "b".repeat(101))));
        assert!(!valid_context("a@"));
        assert!(!valid_context("@main"));
        assert!(!valid_context("a@Main"));
        assert!(!valid_context("a@b@c"));
        assert!(!valid_context("a@b c"));
        assert!(!valid_context("a/b@main"));
        assert_eq!(context_repo("pagos-api@feat/refunds"), Some("pagos-api"));
        assert_eq!(context_repo("pagos-api"), None);
    }

    #[test]
    fn ids_and_secrets() {
        let id = random_id("r_");
        assert_eq!(id.len(), 14);
        assert!(id.starts_with("r_"));
        let secret = new_secret();
        assert_eq!(secret.len(), 4 + 43);
        assert_eq!(hash_token(&secret).len(), 64);
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn invite_reuses_and_reserves() {
        let conn = open(":memory:").unwrap();
        invite(&conn, "ana", "Ana", Some("payments"), "laptop", 0).unwrap();
        deactivate(&conn, "ana", 0).unwrap();
        invite(&conn, "ana", "Ana B", None, "desktop", 0).unwrap();
        let people = list_people(&conn).unwrap();
        assert_eq!(people.len(), 1);
        assert!(people[0].active);
        assert_eq!(people[0].about, "payments");
        assert_eq!(people[0].name, "Ana B");
        assert_eq!(list_tokens(&conn, Some("ana")).unwrap().len(), 2);
        assert!(invite(&conn, "team", "T", None, "x", 0).is_err());
    }

    #[test]
    fn invitation_codes_and_the_servers_address() {
        assert_eq!(invite_code("k7q29fpa").as_deref(), Some("K7Q2-9FPA"));
        assert_eq!(invite_code("K7Q2-9FPA").as_deref(), Some("K7Q2-9FPA"));
        for bad in [
            "K7Q2-9FP",
            "K7Q2-9FPA2",
            "K0Q2-9FPA",
            "K1Q2-9FPA",
            "KIQ2-9FPA",
            "KOQ2-9FPA",
        ] {
            assert_eq!(invite_code(bad), None, "{bad}");
        }
        let conn = open(":memory:").unwrap();
        let x = Address {
            url: "https://x".into(),
            pin: None,
            fingerprint: None,
        };
        // A new person needs a name; an existing one keeps theirs.
        assert!(person_name(&conn, "ana").is_err());
        invite_link(&conn, "ana", "Ana", None, &x, 0).unwrap();
        assert_eq!(person_name(&conn, "ana").unwrap(), "Ana");
        assert!(invite_link(&conn, "team", "T", None, &x, 0).is_err());
        assert!(invite_link(&conn, SYSTEM_HANDLE, "R", None, &x, 0).is_err());

        assert_eq!(public(&conn).unwrap(), Public::default());
        let full = Public {
            url: Some("https://a:1".into()),
            pin: Some("p".into()),
            fingerprint: Some("f".into()),
            port: Some(7878),
        };
        set_public(&conn, &full).unwrap();
        assert_eq!(public(&conn).unwrap(), full);
        let url_only = Public {
            url: Some("https://b".into()),
            ..Public::default()
        };
        set_public(&conn, &url_only).unwrap();
        assert_eq!(public(&conn).unwrap(), url_only);
    }

    #[test]
    fn teams_and_handles_never_share_a_name() {
        let conn = open(":memory:").unwrap();
        invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        invite(&conn, "old", "Old", None, "laptop", 0).unwrap();
        deactivate(&conn, "old", 0).unwrap();
        add_team(&conn, "backend", Some("Backend"), 0).unwrap();
        // A team takes no handle, active or not, nor `team` or `equipo`, nor a bad name.
        for name in ["ana", "old", "team", "equipo", "Backend", "b", "backend"] {
            assert!(add_team(&conn, name, None, 0).is_err(), "{name}");
        }
        assert!(add_team(&conn, "x", None, 0).is_err());
        assert!(add_team(&conn, "front", Some("a\nb"), 0).is_err());
        // And a person takes no team's name.
        let Err(err) = invite(&conn, "backend", "B", None, "laptop", 0) else {
            panic!("a handle took a team's name")
        };
        assert!(err.to_string().contains("name of a team"), "{err}");
        assert_eq!(list_people(&conn).unwrap().len(), 2);
    }

    #[test]
    fn joining_and_leaving_teams() {
        let conn = open(":memory:").unwrap();
        for h in ["ana", "bob", "old"] {
            invite(&conn, h, h, None, "laptop", 0).unwrap();
        }
        deactivate(&conn, "old", 0).unwrap();
        add_team(&conn, "backend", None, 0).unwrap();
        let h = |hs: &[&str]| hs.iter().map(|h| h.to_string()).collect::<Vec<_>>();
        let done = join_team(&conn, "backend", &h(&["ana", "bob"])).unwrap();
        assert!(done.iter().all(|d| matches!(d, Membership::Changed)));
        let done = join_team(&conn, "backend", &h(&["ana"])).unwrap();
        assert!(matches!(done[0], Membership::Already));
        // Unknown or inactive: refused, and nobody joins.
        add_team(&conn, "front", None, 0).unwrap();
        assert!(join_team(&conn, "front", &h(&["ana", "nobody"])).is_err());
        assert!(join_team(&conn, "front", &h(&["ana", "old"])).is_err());
        assert!(join_team(&conn, "nope", &h(&["ana"])).is_err());
        let teams = list_teams(&conn).unwrap();
        assert_eq!(teams[1].name, "front");
        assert!(teams[1].members.is_empty());
        // A member deactivated later stays in, marked.
        deactivate(&conn, "bob", 0).unwrap();
        let teams = list_teams(&conn).unwrap();
        assert_eq!(
            teams[0].members,
            vec![("ana".to_string(), true), ("bob".to_string(), false)]
        );
        let done = leave_team(&conn, "backend", &h(&["bob", "old"])).unwrap();
        assert!(matches!(done[0], Membership::Changed));
        assert!(matches!(done[1], Membership::Already));
        remove_team(&conn, "backend").unwrap();
        assert!(remove_team(&conn, "backend").is_err());
        assert_eq!(list_teams(&conn).unwrap().len(), 1);
        // The name is free again.
        add_team(&conn, "backend", None, 0).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn database_files_are_private_to_the_server_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(random_id("relevo-test-"));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("relevo.db");
        let conn = open(path.to_str().unwrap()).unwrap();
        invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        for file in ["relevo.db", "relevo.db-wal", "relevo.db-shm"] {
            let mode = std::fs::metadata(dir.join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Opens `path` from `n` threads at once, each with its own connection, as servers started
    /// together: every one must get it, at this layout.
    fn open_at_once(path: &std::path::Path, n: usize) {
        let gate = std::sync::Arc::new(std::sync::Barrier::new(n));
        let opens: Vec<_> = (0..n)
            .map(|_| {
                let gate = gate.clone();
                let path = path.to_str().unwrap().to_string();
                std::thread::spawn(move || {
                    gate.wait();
                    let key = std::path::Path::new(&path).with_extension("key");
                    open_migrating(&path, &key).map(drop)
                })
            })
            .collect();
        for open in opens {
            if let Err(e) = open.join().unwrap() {
                panic!("{}: {e:#}", path.display());
            }
        }
        assert_eq!(
            check(path.to_str().unwrap(), || Ok(path.with_extension("key"))).unwrap(),
            Layout::Current
        );
    }

    #[test]
    fn servers_starting_at_once_on_a_new_file_all_open_it() {
        let dir = std::env::temp_dir().join(random_id("relevo-test-"));
        std::fs::create_dir(&dir).unwrap();
        for round in 0..20 {
            open_at_once(&dir.join(format!("new-{round}.db")), 6);
        }
        // A copy not in WAL mode (as `relevo backup` writes one), opened at once.
        let source = dir.join("source.db");
        let conn = open(source.to_str().unwrap()).unwrap();
        invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        for round in 0..20 {
            let copy = dir.join(format!("copy-{round}.db"));
            conn.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
                .unwrap();
            open_at_once(&copy, 6);
        }
        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opening_waits_for_a_held_lock_no_longer_than_its_patience() {
        let dir = std::env::temp_dir().join(random_id("relevo-test-"));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("held.db");
        let path = path.to_str().unwrap();
        drop(open(path).unwrap());
        // Another process holds the database, for longer than the opener will wait.
        let holder = Connection::open(path).unwrap();
        holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let patience = std::time::Duration::from_millis(400);
        let start = std::time::Instant::now();
        let err = open_with(path, None, patience).unwrap_err();
        let took = start.elapsed();
        assert!(busy(&err), "{err:#}");
        assert!(took >= patience, "{took:?}");
        // Bounded, not exact: SQLite's busy handler sleeps in coarser steps on macOS (about
        // 1.1-1.3 s here for 400 ms on GitHub's runners), so allow 2 s over the patience.
        assert!(
            took < patience + std::time::Duration::from_secs(2),
            "{took:?}"
        );
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_migration_logs_each_step_once() {
        let steps = |old| -> Vec<String> {
            migrated(old, 2)
                .iter()
                .map(|l| l.split(':').next().unwrap().to_string())
                .collect()
        };
        let line = |from, to| format!("migrated the database from layout {from} to {to}");
        let from = |old: i64| -> Vec<String> { (old..11).map(|l| line(l, l + 1)).collect() };
        for old in 5..=10 {
            assert_eq!(steps(old), from(old), "{old}");
        }
        assert!(migrated(7, 2)[0].contains("opened 2 closed threads"));
    }

    #[test]
    fn failed_invite_changes_nothing() {
        let conn = open(":memory:").unwrap();
        invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        deactivate(&conn, "ana", 0).unwrap();
        assert!(invite(&conn, "ana", "Ana B", None, "  ", 0).is_err());
        assert!(invite(&conn, "zoe", "Zoe", None, "a\nb", 0).is_err());
        let people = list_people(&conn).unwrap();
        assert_eq!(people.len(), 1);
        assert!(!people[0].active);
        assert_eq!(people[0].name, "Ana");
        assert_eq!(list_tokens(&conn, None).unwrap().len(), 1);
    }

    #[test]
    fn deactivated_person_gets_no_new_token_until_invited_back() {
        let conn = open(":memory:").unwrap();
        invite(&conn, "ana", "Ana", None, "laptop", 0).unwrap();
        assert_eq!(deactivate(&conn, "ana", 0).unwrap(), 1);
        assert!(add_token(&conn, "ana", "desktop", 0).is_err());
        assert_eq!(list_tokens(&conn, Some("ana")).unwrap().len(), 1);
        invite(&conn, "ana", "Ana", None, "laptop 2", 0).unwrap();
        add_token(&conn, "ana", "desktop", 0).unwrap();
        assert_eq!(list_tokens(&conn, Some("ana")).unwrap().len(), 3);
    }
}
