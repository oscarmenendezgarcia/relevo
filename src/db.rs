//! SQLite storage: schema, ids, tokens, and the admin operations the CLI runs.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
-- A session is one running agent of a person, named by the `Relevo-Session` header under one
-- token (`client_id` is '' for the token's default session). `context` is the latest
-- `Relevo-Context` (NULL: general). `claims` is 1 while the session may hold claims: from its
-- first claim until they lapse, an hour after it was last seen (see `session::lapse`). Sessions
-- not seen for 7 days are forgotten. Ids are never reused (AUTOINCREMENT): `send_keys` keeps the
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
-- of theirs that claimed it (the first to write while nobody held it), and `claimed_context` its
-- context then. `delivered_seq` is the latest note that reached any of their sessions;
-- `delivered_at`, `_label` and `_context` say when the latest one by someone else first did, and
-- which session (its label and context then). `last_seq` is the thread's latest note, `last_other_seq` the
-- latest one not theirs; `has_new` (no session of theirs has that one),
-- `waiting` (open, direct, latest note not theirs), `inbox` (`has_new` or `waiting`) and
-- `resolved` (a closed `team` thread: it shows only with `all`) are kept by `api::refresh` after
-- every change to the thread.
CREATE TABLE IF NOT EXISTS participants (
    thread_id         TEXT NOT NULL REFERENCES threads(id),
    person_id         INTEGER NOT NULL REFERENCES people(id),
    context           TEXT,
    claimed_by        INTEGER REFERENCES sessions(id),
    claimed_context   TEXT,
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
    PRIMARY KEY (thread_id, person_id)
);
-- What a session has of a thread, once it differs from its person's: a session without a row has
-- what its person has (`participants.delivered_seq`). Before that moves, every other session of
-- the person gets a row with the old value, so one session reading never delivers to another.
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
    ON session_threads(session_id, claimed_by, context, resolved)
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
-- it for the session that asks.
CREATE TABLE IF NOT EXISTS send_keys (
    person_id  INTEGER NOT NULL REFERENCES people(id),
    key        TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    note_id    TEXT,
    status     TEXT NOT NULL,
    claimed_by INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (person_id, key)
);
CREATE INDEX IF NOT EXISTS send_keys_by_time ON send_keys(created_at);
";

/// Handles that mean "the whole team" in `to`, so no person may take them.
pub const RESERVED_HANDLES: [&str; 2] = ["team", "equipo"];

/// A session not seen for this long is forgotten, and so are its claims.
pub const SESSION_LIFETIME_SECS: i64 = 7 * 24 * 3600;

/// The claims of a session not seen for this long lapse.
pub const CLAIM_LAPSE_SECS: i64 = 3600;

/// The layout of `SCHEMA`, kept in SQLite's `user_version`. v0 does not migrate: a database with
/// another layout is refused, never half-updated (`CREATE TABLE IF NOT EXISTS` would skip the
/// old tables and fail on their missing columns).
const SCHEMA_VERSION: i64 = 3;

/// Opens (or creates) the database and applies the schema. `":memory:"` works for tests.
pub fn open(path: &str) -> Result<Connection> {
    create_private(path).with_context(|| format!("creating database {path}"))?;
    let mut conn = Connection::open(path).with_context(|| format!("opening database {path}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // One transaction, so a process opening a new file at the same time sees it either empty or
    // with its version set.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        let tables: i64 = tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("database {path} was made by a newer version of relevo; use that one");
        }
        if tables > 0 {
            bail!(
                "database {path} was made by an older version of relevo, and v0 does not \
                 migrate: delete it (with its -wal and -shm files) and invite the team again"
            );
        }
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
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
        bail!("'{handle}' is reserved: it means the whole team");
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
        "UPDATE participants SET claimed_by = NULL, claimed_context = NULL WHERE claimed_by = ?1",
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
