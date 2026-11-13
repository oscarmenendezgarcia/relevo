//! Release notes on the global board: when `relevo serve` starts on a version newer than the last
//! one the database announced, the person `relevo` posts that version's section of CHANGELOG.md
//! there, once (docs/protocol.md, "Release notes").

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{SYSTEM_HANDLE, random_id};

/// This build's CHANGELOG, whose section for its version is posted.
pub const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// A note holds at most this many bytes (as `send` allows).
const MAX_TEXT: usize = 64 * 1024;

/// What a start did about the release notes.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Posted in this new `global` thread.
    Posted(String),
    /// This version, or a newer one, was announced already (a restart, a downgrade, or a
    /// database made by this version).
    Announced,
    /// The CHANGELOG has no section for this version (a build between releases): nothing posted
    /// and nothing recorded, so the release that has one still posts it.
    NoSection,
    /// Turned off (`--no-release-notes`): recorded, not posted.
    Off,
    /// A person took the handle `relevo` before it was reserved: recorded, not posted.
    HandleTaken,
}

/// Whether the release notes are on: not with `--no-release-notes`, nor with
/// `RELEVO_RELEASE_NOTES` set to `0`, `false`, `no` or `off`.
pub fn enabled(no_flag: bool, env: Option<&str>) -> bool {
    let off = env.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    });
    !no_flag && !off
}

/// The section of `changelog` for `version`: what follows its `## <version>` heading (alone, or
/// followed by a space and a date), up to the next `## ` heading, trimmed. `None` when there is
/// none, or it is empty.
pub fn section(changelog: &str, version: &str) -> Option<String> {
    let mut lines = changelog.lines();
    lines.find(|line| {
        line.strip_prefix("## ")
            .and_then(|rest| rest.strip_prefix(version))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })?;
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    let body = body.join("\n").trim().to_string();
    (!body.is_empty()).then_some(body)
}

/// `major.minor.patch`, with an optional `-pre` that comes before the release itself.
fn parse(version: &str) -> Option<(u64, u64, u64, bool)> {
    let (core, pre) = match version.trim().split_once('-') {
        Some((core, _)) => (core, true),
        None => (version.trim(), false),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    Some((v.0, v.1, v.2, !pre))
}

/// Whether `a` is a newer version than `b` (semver order, without build metadata). A version that
/// cannot be read is newer than nothing.
pub fn newer(a: &str, b: &str) -> bool {
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The note: `Relevo <version>`, a blank line, then the section, cut at a line to fit a note,
/// with a last line saying where the rest is.
pub fn note_text(version: &str, section: &str) -> String {
    let head = format!("Relevo {version}\n\n");
    if head.len() + section.len() <= MAX_TEXT {
        return head + section;
    }
    let tail = format!("\n\n… The rest is in the CHANGELOG of relevo {version}.");
    let room = MAX_TEXT - head.len() - tail.len();
    let mut kept = String::new();
    for line in section.lines() {
        if kept.len() + line.len() + 1 > room {
            if kept.is_empty() {
                // One line longer than a note: cut it at a character.
                let mut end = room;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                kept.push_str(&line[..end]);
            }
            break;
        }
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str(line);
    }
    head + kept.trim_end() + &tail
}

/// Posts the release notes of `version` (from `changelog`) on the global board if the database
/// has not announced it, or a newer one, yet; with `post` off, only records it. One transaction,
/// so several processes starting at once post them once.
pub fn announce(
    conn: &Connection,
    version: &str,
    changelog: &str,
    post: bool,
    now: i64,
) -> Result<Outcome> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let announced: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if announced.is_some_and(|last| !newer(version, &last)) {
        return Ok(Outcome::Announced);
    }
    let Some(section) = section(changelog, version) else {
        return Ok(Outcome::NoSection);
    };
    let outcome = if !post {
        Outcome::Off
    } else {
        match system_person(&tx, now)? {
            None => Outcome::HandleTaken,
            Some((person, token)) => {
                Outcome::Posted(post_note(&tx, person, &token, version, &section, now)?)
            }
        }
    };
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('announced', ?1)
         ON CONFLICT (key) DO UPDATE SET value = ?1",
        [version],
    )?;
    tx.commit()?;
    Ok(outcome)
}

/// The person `relevo` and the token its notes record, made the first time: inactive (so it is in
/// no people list, no new thread, and names nobody), with one revoked token whose hash no secret
/// has. `None` when a person took the handle before it was reserved.
fn system_person(tx: &Connection, now: i64) -> Result<Option<(i64, String)>> {
    let made: Option<i64> = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'system_person'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(person) = made {
        let token: String = tx.query_row(
            "SELECT id FROM tokens WHERE person_id = ?1 ORDER BY created_at LIMIT 1",
            [person],
            |r| r.get(0),
        )?;
        return Ok(Some((person, token)));
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM people WHERE handle = ?1)",
        [SYSTEM_HANDLE],
        |r| r.get(0),
    )?;
    if taken {
        return Ok(None);
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
    Ok(Some((person, token)))
}

/// Opens a `global` thread by `person` with the note, as `send` opens one with no mention:
/// everyone active takes part, nobody is named, so it is pending for nobody.
fn post_note(
    tx: &Connection,
    person: i64,
    token: &str,
    version: &str,
    section: &str,
    now: i64,
) -> Result<String> {
    let id = random_id("r_");
    tx.execute(
        "INSERT INTO threads (id, kind, title, opener_id, status, created_at)
         VALUES (?1, 'global', ?2, ?3, 'open', ?4)",
        params![id, format!("Relevo {version}"), person, now],
    )?;
    tx.execute(
        "INSERT INTO participants (thread_id, person_id) VALUES (?1, ?2)",
        params![id, person],
    )?;
    tx.execute(
        "INSERT INTO participants (thread_id, person_id)
         SELECT ?1, id FROM people WHERE active = 1 AND id != ?2 ORDER BY handle",
        params![id, person],
    )?;
    tx.execute(
        "INSERT INTO notes (id, thread_id, author_id, token_id, via, text, created_at)
         VALUES (?1, ?2, ?3, ?4, 'release notes', ?5, ?6)",
        params![
            random_id("n_"),
            id,
            person,
            token,
            note_text(version, section),
            now
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "UPDATE participants SET last_other_seq = ?2 WHERE thread_id = ?1 AND person_id != ?3",
        params![id, seq, person],
    )?;
    tx.execute(
        "UPDATE participants SET delivered_seq = ?2 WHERE thread_id = ?1 AND person_id = ?3",
        params![id, seq, person],
    )?;
    crate::api::refresh(tx, &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_semver() {
        assert!(newer("0.5.0", "0.4.4"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("0.4.4", "0.4.4"));
        assert!(!newer("0.4.3", "0.4.4"));
        assert!(!newer("x", "0.4.4"));
    }

    #[test]
    fn a_section_runs_from_its_heading_to_the_next() {
        let log = "# Changelog\n\n## 0.5.0 (unreleased)\n\nFirst.\n\nSecond.\n\n## 0.4.4 (12-nov)\n\nOld.\n\n## 0.2.1\n\nOlder.\n";
        assert_eq!(section(log, "0.5.0").unwrap(), "First.\n\nSecond.");
        assert_eq!(section(log, "0.2.1").unwrap(), "Older.");
        assert_eq!(section(log, "0.4.4").unwrap(), "Old.");
        // Not a prefix of another version.
        assert_eq!(section(log, "0.5"), None);
        assert_eq!(section(log, "0.6.0"), None);
        // This build's own CHANGELOG has its version.
        assert!(section(CHANGELOG, env!("CARGO_PKG_VERSION")).is_some());
    }

    #[test]
    fn a_note_too_long_is_cut_at_a_line_and_says_where_the_rest_is() {
        let line = "x".repeat(99);
        let long: Vec<&str> = std::iter::repeat_n(line.as_str(), 1000).collect();
        let text = note_text("0.5.0", &long.join("\n"));
        assert!(text.len() <= MAX_TEXT, "{}", text.len());
        assert!(text.starts_with("Relevo 0.5.0\n\nxxx"));
        assert!(text.ends_with("\n\n… The rest is in the CHANGELOG of relevo 0.5.0."));
        assert!(text.lines().all(|l| l.len() == 99 || !l.starts_with('x')));
        // One line longer than a note: cut within it.
        let text = note_text("0.5.0", &"é".repeat(MAX_TEXT));
        assert!(text.len() <= MAX_TEXT);
        // A short one is whole.
        assert_eq!(note_text("0.5.0", "Hi."), "Relevo 0.5.0\n\nHi.");
    }

    #[test]
    fn the_switch_reads_the_flag_and_the_environment() {
        assert!(enabled(false, None));
        assert!(enabled(false, Some("1")));
        assert!(!enabled(true, None));
        for off in ["0", "false", "No", "off"] {
            assert!(!enabled(false, Some(off)), "{off}");
        }
    }
}
