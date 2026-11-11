//! SQLite storage: schema, ids, tokens, and the admin operations the CLI runs.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const SCHEMA: &str = "
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
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id   TEXT NOT NULL REFERENCES tokens(id),
    client_id  TEXT NOT NULL,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    context    TEXT,
    last_seen  INTEGER NOT NULL,
    claims     INTEGER NOT NULL DEFAULT 0,
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
-- `waiting` (pending: open, the person named, with a note not theirs after `handled_seq` and from
-- the one that named them on), `inbox` (`has_new` or `waiting`), `resolved` (a closed `team` or
-- `global` thread that does not name them: it shows only with `all`) and `fresh` (pending,
-- with a note past `handled_seq` that reached one of their sessions: a new session of theirs gets
-- its own row, starting at `handled_seq`) are kept by `api::refresh` after every change to the
-- thread. `handled_seq` is how far the person has handled the thread (by writing in it, closing it
-- or `done`); `handled_at`, `_label` and `_context` say when they last handled a note by someone
-- else, and from which session (its label and context then). `named_seq` says the thread names
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
-- unclaimed with no context or the session's context. Only `all` reads resolved threads.
CREATE INDEX IF NOT EXISTS participants_inbox
    ON participants(person_id, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX IF NOT EXISTS participants_inbox_claim
    ON participants(person_id, claimed_by, has_new, waiting, last_seq)
    WHERE inbox = 1 AND resolved = 0;
CREATE INDEX IF NOT EXISTS participants_inbox_context
    ON participants(person_id, claimed_by, context, has_new, waiting, last_seq)
    WHERE inbox = 1 AND resolved = 0;
CREATE INDEX IF NOT EXISTS session_new
    ON session_threads(session_id, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX IF NOT EXISTS session_new_claim
    ON session_threads(session_id, claimed_by, waiting, last_seq)
    WHERE has_new = 1 AND resolved = 0;
CREATE INDEX IF NOT EXISTS session_new_context
    ON session_threads(session_id, claimed_by, context, waiting, last_seq)
    WHERE has_new = 1 AND resolved = 0;
-- For `more`: the threads new only for the session, by route (see `api::count_sql`).
CREATE INDEX IF NOT EXISTS session_count
    ON session_threads(session_id, resolved, claimed_by, context)
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
-- thread, and when (a thread closed before them has no row).
CREATE TABLE IF NOT EXISTS note_contexts (
    seq     INTEGER PRIMARY KEY REFERENCES notes(seq),
    context TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS thread_closes (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    closed_at INTEGER NOT NULL
);
-- The timeline reads a person's threads by latest note, newest first, a page at a time, and
-- whether they wrote in a thread.
CREATE INDEX IF NOT EXISTS participants_timeline ON participants(person_id, last_seq);
CREATE INDEX IF NOT EXISTS notes_by_thread_author ON notes(thread_id, author_id);
";

/// Names no person nor team may take: 0.3's whole team, which the deprecated `to` still reads
/// as "everyone". In a note's text they name nobody (a team is always named).
pub const RESERVED_HANDLES: [&str; 2] = ["team", "equipo"];

/// A session not seen for this long is forgotten, and so are its claims.
pub const SESSION_LIFETIME_SECS: i64 = 7 * 24 * 3600;

/// The claims of a session not seen for this long lapse.
pub const CLAIM_LAPSE_SECS: i64 = 15 * 60;

/// The layout of `SCHEMA`, kept in SQLite's `user_version`. A database of layout 5 (relevo 0.1.0
/// to 0.2.x) or 6 (relevo 0.3.x) is migrated in place (`MIGRATE_FROM_5`, then `MIGRATE_FROM_6`),
/// by `relevo serve` only; any other layout is refused, never half-updated (`CREATE TABLE IF NOT
/// EXISTS` would skip the old tables and fail on their missing columns).
const SCHEMA_VERSION: i64 = 7;

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

/// Opens (or creates) the database and applies the schema. A database of layout 5 or 6 is
/// refused: only `relevo serve` migrates it (`open_migrating`), so that an admin command never
/// changes the layout behind the owner's back, before a backup. `":memory:"` works for tests.
pub fn open(path: &str) -> Result<Connection> {
    open_with(path, false)
}

/// `open`, migrating a database of layout 5 or 6 in place: for `relevo serve`.
pub fn open_migrating(path: &str) -> Result<Connection> {
    open_with(path, true)
}

fn open_with(path: &str, migrate: bool) -> Result<Connection> {
    create_private(path).with_context(|| format!("creating database {path}"))?;
    let mut conn = Connection::open(path).with_context(|| format!("opening database {path}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // A layout it refuses is left as it is: found before anything is written, WAL mode included.
    let refuses = |layout| matches!(layout, Layout::Migrates(_)) && !migrate;
    let found = layout(&conn, path)?;
    if refuses(found) {
        bail!(no_migration(path, found));
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Off while migrating: layout 7 makes the threads table again, which the others refer to.
    // (The pragma does nothing inside a transaction, so it is set around it.)
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    // One transaction, so a process opening a new file at the same time sees it either empty or
    // with its version set, and one opening an old file sees it either old or migrated.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match layout(&tx, path)? {
        layout if refuses(layout) => bail!(no_migration(path, layout)),
        Layout::Migrates(from) => {
            if from == 5 {
                tx.execute_batch(MIGRATE_FROM_5)
                    .with_context(|| format!("migrating database {path} from layout 5"))?;
                tracing::info!(db = path, "migrated the database from layout 5 to 6");
            }
            tx.execute_batch(MIGRATE_FROM_6)
                .with_context(|| format!("migrating database {path} from layout 6"))?;
            let broken: i64 =
                tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })?;
            if broken > 0 {
                bail!("migrating database {path} from layout 6: {broken} broken references");
            }
            tracing::info!(
                db = path,
                "migrated the database from layout 6 to {SCHEMA_VERSION}: team threads are \
                 global ones now"
            );
        }
        Layout::New | Layout::Current => {}
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Why a command other than `serve` refuses a database of layout 5 or 6.
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
    /// Layout 5 (relevo 0.1.0 to 0.2.x) or 6 (relevo 0.3.x): `relevo serve` migrates it.
    Migrates(i64),
}

impl Layout {
    /// The versions of relevo that made a layout `serve` migrates.
    pub fn made_by(self) -> &'static str {
        match self {
            Layout::Migrates(5) => "relevo 0.1.0 to 0.2.x",
            Layout::Migrates(_) => "relevo 0.3.x",
            Layout::New | Layout::Current => "this relevo",
        }
    }
}

/// The layout of the database `conn` has open, or why this version refuses it.
fn layout(conn: &Connection, path: &str) -> Result<Layout> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(Layout::Current);
    }
    if version > SCHEMA_VERSION {
        bail!("database {path} was made by a newer version of relevo; use that one");
    }
    if version == 5 || version == 6 {
        return Ok(Layout::Migrates(version));
    }
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
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
/// writing anything (see `read_only`).
pub fn check(path: &str) -> Result<Layout> {
    if !std::path::Path::new(path).is_file() {
        bail!("no database at {path}");
    }
    let conn = read_only(path).with_context(|| format!("opening database {path}"))?;
    layout(&conn, path)
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

/// Creates a missing database file readable by the server user only: it holds every note in
/// plain text. SQLite gives the `-wal` and `-shm` files the same mode. Existing files are left as
/// they are.
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
    if !valid_handle(handle) {
        bail!("handle must match ^[a-z0-9][a-z0-9_-]{{1,31}}$");
    }
    if RESERVED_HANDLES.contains(&handle) {
        bail!("'{handle}' is reserved: it was 0.3's whole team, and @{handle} names nobody");
    }
    if team_id(conn, handle)?.is_some() {
        bail!(
            "'{handle}' is the name of a team: a handle cannot be one, so that @{handle} means one thing"
        );
    }
    one_line("name", name)?;
    if let Some(about) = about {
        one_line("about", about)?;
    }
    check_label(label)?;
    // A failed invite must not leave the person created or reactivated.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO people (handle, name, about, created_at) VALUES (?1, ?2, coalesce(?3, ''), ?4)
         ON CONFLICT (handle) DO UPDATE SET name = ?2, about = coalesce(?3, about), active = 1",
        params![handle, name, about, now],
    )?;
    let token = add_token(&tx, handle, label, now)?;
    tx.commit()?;
    Ok(token)
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
    let mut stmt = conn.prepare(
        "SELECT t.id, p.handle, t.label, t.created_at, t.revoked_at
         FROM tokens t JOIN people p ON p.id = t.person_id
         WHERE ?1 IS NULL OR p.handle = ?1
         ORDER BY p.handle, t.created_at, t.id",
    )?;
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
}

pub fn list_people(conn: &Connection) -> Result<Vec<PersonInfo>> {
    let mut stmt =
        conn.prepare("SELECT handle, name, about, active FROM people ORDER BY handle")?;
    let rows = stmt.query_map([], |r| {
        Ok(PersonInfo {
            handle: r.get(0)?,
            name: r.get(1)?,
            about: r.get(2)?,
            active: r.get(3)?,
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

pub struct SessionInfo {
    pub handle: String,
    pub label: String,
    /// The `Relevo-Session` id, or `None` for a token's default session.
    pub session: Option<String>,
    pub context: Option<String>,
    pub last_seen: i64,
}

/// Sessions seen in the last 7 days, of everyone or of one person, most recently seen first.
pub fn list_sessions(
    conn: &Connection,
    handle: Option<&str>,
    now: i64,
) -> Result<Vec<SessionInfo>> {
    if let Some(handle) = handle {
        person_id(conn, handle)?;
    }
    let mut stmt = conn.prepare(
        "SELECT p.handle, t.label, nullif(s.client_id, ''), s.context, s.last_seen
         FROM sessions s JOIN tokens t ON t.id = s.token_id JOIN people p ON p.id = s.person_id
         WHERE s.last_seen > ?2 AND (?1 IS NULL OR p.handle = ?1)
         ORDER BY p.handle, s.last_seen DESC, s.id",
    )?;
    let rows = stmt.query_map(params![handle, now - SESSION_LIFETIME_SECS], |r| {
        Ok(SessionInfo {
            handle: r.get(0)?,
            label: r.get(1)?,
            session: r.get(2)?,
            context: r.get(3)?,
            last_seen: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Deactivated people can no longer authenticate nor receive new threads. Every token of theirs
/// is revoked for good, and its sessions forgotten with their claims: `invite` brings them back
/// with a new token, and they find their threads again. Returns how many tokens it revoked.
pub fn deactivate(conn: &Connection, handle: &str, now: i64) -> Result<usize> {
    let id = person_id(conn, handle)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE people SET active = 0 WHERE id = ?1", [id])?;
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
/// handle of a person (active or not), nor `team` or `equipo`, so that a mention means one thing.
pub fn add_team(conn: &Connection, name: &str, title: Option<&str>, now: i64) -> Result<()> {
    if !valid_handle(name) {
        bail!("a team name must match ^[a-z0-9][a-z0-9_-]{{1,31}}$");
    }
    if RESERVED_HANDLES.contains(&name) {
        bail!("'{name}' is reserved: it was 0.3's whole team, and @{name} names nobody");
    }
    let person: Option<bool> = conn
        .query_row("SELECT active FROM people WHERE handle = ?1", [name], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(active) = person {
        let state = if active { "" } else { " (inactive)" };
        bail!("'{name}' is the handle of a person{state}: a team cannot take it");
    }
    if team_id(conn, name)?.is_some() {
        bail!("there is a team named '{name}' already");
    }
    if let Some(title) = title {
        one_line("title", title)?;
    }
    conn.execute(
        "INSERT INTO teams (name, title, created_at) VALUES (?1, coalesce(?2, ''), ?3)",
        params![name, title, now],
    )?;
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
