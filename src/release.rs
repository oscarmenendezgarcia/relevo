//! Release notes on the global board: when `relevo serve` starts on a version newer than the last
//! one the database announced, the person `relevo` posts that version's section of CHANGELOG.md
//! there, once (docs/protocol.md, "Release notes").

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{SYSTEM_PERSON_SQL, random_id};

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
    /// This build is not a release: the CHANGELOG's first section is another version's, or is
    /// marked "(unreleased)", or there is none (a build between releases). Nothing posted, and
    /// nothing recorded (the notes on or off), so the release itself still posts its notes.
    NotReleased,
    /// Turned off (`--no-release-notes`), on a release: recorded, not posted.
    Off,
    /// A person or a team had the name `relevo` before 0.5.0 reserved it, so the server has no
    /// person of its own: recorded, not posted.
    NameTaken,
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

/// The `## ` headings of `changelog` and the lines under each, outside fenced code (a `## ` line
/// inside a fence is part of the section). Line ends are `\n` or `\r\n`.
fn sections(changelog: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in changelog.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let marker = line.trim_start_matches(' ');
        let indent = line.len() - marker.len();
        let run = |c: char| marker.chars().take_while(|&m| m == c).count();
        match fence {
            Some((c, n)) => {
                if indent <= 3 && run(c) >= n && marker.trim_start_matches(c).trim().is_empty() {
                    fence = None;
                }
            }
            None if indent <= 3 && (run('`') >= 3 || run('~') >= 3) => {
                let c = marker.chars().next().unwrap_or('`');
                fence = Some((c, run(c)));
            }
            None => {
                if let Some(heading) = line.strip_prefix("## ") {
                    out.push((heading, Vec::new()));
                    continue;
                }
            }
        }
        if let Some((_, body)) = out.last_mut() {
            body.push(line);
        }
    }
    out
}

/// Whether a heading is `version`'s: the version alone, or followed by a space and more (a date).
fn heading_of(heading: &str, version: &str) -> bool {
    heading
        .strip_prefix(version)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// The section of `changelog` for `version`: what follows its `## <version>` heading, up to the
/// next `## ` heading (or the end), trimmed. `None` when there is none, or it is empty.
pub fn section(changelog: &str, version: &str) -> Option<String> {
    let (_, body) = sections(changelog)
        .into_iter()
        .find(|(heading, _)| heading_of(heading, version))?;
    let body = body.join("\n").trim().to_string();
    (!body.is_empty()).then_some(body)
}

/// The notes this build posts: its version's section, only when it is the CHANGELOG's first
/// section and is not marked "(unreleased)". A build between releases (its version's section is
/// below a newer one still being written) has none.
pub fn release_section(changelog: &str, version: &str) -> Option<String> {
    let (heading, _) = sections(changelog).into_iter().next()?;
    if !heading_of(heading, version) || heading.to_lowercase().contains("(unreleased)") {
        return None;
    }
    section(changelog, version)
}

/// What a new database records: the version that made it, when this build is a release (then it
/// posts no notes for the version it starts at). Made by a build between releases, it records
/// nothing, so the release itself, once it runs, posts its notes. Whether it recorded.
pub fn record_new(conn: &Connection, version: &str, changelog: &str) -> rusqlite::Result<bool> {
    if release_section(changelog, version).is_none() {
        return Ok(false);
    }
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('announced', ?1)",
        [version],
    )?;
    Ok(true)
}

/// A pre-release identifier: numeric ones come before the others, and compare as numbers.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Number(u64),
    Text(String),
}

/// A version as semver orders it: `major.minor.patch`, whether it is a release (no
/// pre-release: a release comes after its pre-releases), then its pre-release identifiers.
type Order = ((u64, u64, u64), bool, Vec<Ident>);

/// A version's `Order`; build metadata (`+…`) is left out, as semver says.
fn parse(version: &str) -> Option<Order> {
    let version = version.trim();
    let version = version.split_once('+').map_or(version, |(v, _)| v);
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    let idents = match pre {
        None => Vec::new(),
        Some(pre) => pre
            .split('.')
            .map(|i| {
                if i.is_empty() {
                    None
                } else if i.bytes().all(|b| b.is_ascii_digit()) {
                    i.parse().ok().map(Ident::Number)
                } else {
                    Some(Ident::Text(i.to_string()))
                }
            })
            .collect::<Option<_>>()?,
    };
    Some((v, pre.is_none(), idents))
}

/// Whether `version` can be read as a semver version.
pub fn readable(version: &str) -> bool {
    parse(version).is_some()
}

/// Whether `a` is a newer version than `b`, in semver order: a release after its pre-releases,
/// pre-releases by their identifiers, build metadata ignored. A version that cannot be read is
/// newer than nothing.
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
    match &announced {
        // Written by a version that cannot be read: taken as older, and written over.
        Some(last) if !readable(last) => tracing::info!(
            announced = %last,
            "the version the database announced last cannot be read: taken as older"
        ),
        Some(last) if !newer(version, last) => return Ok(Outcome::Announced),
        _ => {}
    }
    // A build between releases records nothing, the notes on or off: its version's release,
    // once it runs, still posts.
    let Some(section) = release_section(changelog, version) else {
        return Ok(Outcome::NotReleased);
    };
    let outcome = if !post {
        // Off: recorded, so turning them on later posts no older version's notes.
        Outcome::Off
    } else {
        match system_person(&tx)? {
            None => Outcome::NameTaken,
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

/// The server's own person and the token its notes record (`db::make_system_person`); `None`
/// when someone had the name `relevo` before 0.5.0 reserved it, so there is none.
fn system_person(tx: &Connection) -> Result<Option<(i64, String)>> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT p.id, t.id FROM people p JOIN tokens t ON t.person_id = p.id
                 WHERE p.id = {SYSTEM_PERSON_SQL} ORDER BY t.created_at LIMIT 1"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
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
        // Build metadata is left out; pre-release identifiers compare one by one, numbers as
        // numbers and before words.
        assert!(!newer("0.5.0+build.7", "0.5.0"));
        assert!(!newer("0.5.0", "0.5.0+other"));
        assert!(newer("1.0.0-rc.2", "1.0.0-rc.1"));
        assert!(newer("1.0.0-rc.10", "1.0.0-rc.9"));
        assert!(newer("1.0.0-rc.1", "1.0.0-beta.11"));
        assert!(newer("1.0.0-alpha.beta", "1.0.0-alpha.1"));
        assert!(newer("1.0.0-alpha.1", "1.0.0-alpha"));
        assert!(!readable("0.5"));
        assert!(!readable("1.0.0-"));
        assert!(!readable("garbage"));
        assert!(readable("1.0.0-rc.1+sha.5114f85"));
    }

    #[test]
    fn only_a_dated_first_section_is_a_release() {
        let dev =
            "# Changelog\n\n## 0.5.0 (unreleased)\n\nNext.\n\n## 0.4.4 (12-nov-2026)\n\nNow.\n";
        // A 0.4.4 build between releases: its section is not the first.
        assert_eq!(release_section(dev, "0.4.4"), None);
        // A build of 0.5.0 before its release: still "(unreleased)".
        assert_eq!(release_section(dev, "0.5.0"), None);
        let release = dev.replace("(unreleased)", "(14-nov-2026)");
        assert_eq!(release_section(&release, "0.5.0").unwrap(), "Next.");
        assert_eq!(release_section(&release, "0.4.4"), None);
        // A heading with the version alone is a release too.
        assert_eq!(release_section("## 0.6.0\n\nIt.", "0.6.0").unwrap(), "It.");
        assert_eq!(release_section("No sections.", "0.6.0"), None);
    }

    #[test]
    fn a_new_database_records_its_version_only_when_made_by_a_release() {
        let announced = |conn: &Connection| -> Option<String> {
            conn.query_row("SELECT value FROM meta WHERE key = 'announced'", [], |r| {
                r.get(0)
            })
            .optional()
            .unwrap()
        };
        // This build: recorded if and only if it is a release.
        let conn = crate::db::open(":memory:").unwrap();
        let version = env!("CARGO_PKG_VERSION");
        let released = release_section(CHANGELOG, version).is_some();
        assert_eq!(announced(&conn).is_some(), released);
        conn.execute("DELETE FROM meta WHERE key = 'announced'", [])
            .unwrap();
        let dev = "## 0.5.0 (unreleased)\n\nNext.\n\n## 0.4.4 (12-nov-2026)\n\nNow.\n";
        // A 0.5.0 build before its release, and a 0.4.4 one between releases: nothing, so the
        // dated 0.5.0 posts its notes on this database.
        assert!(!record_new(&conn, "0.5.0", dev).unwrap());
        assert!(!record_new(&conn, "0.4.4", dev).unwrap());
        assert_eq!(announced(&conn), None);
        let release = dev.replace("(unreleased)", "(14-nov-2026)");
        assert!(matches!(
            announce(&conn, "0.5.0", &release, true, 0).unwrap(),
            Outcome::Posted(_)
        ));
        // The release itself records its version.
        conn.execute("DELETE FROM meta WHERE key = 'announced'", [])
            .unwrap();
        assert!(record_new(&conn, "0.5.0", &release).unwrap());
        assert_eq!(announced(&conn).as_deref(), Some("0.5.0"));
    }

    #[test]
    fn a_heading_inside_fenced_code_is_text_and_crlf_lines_read_the_same() {
        let log = "## 0.6.0 (14-nov-2026)\n\nRun:\n\n```\n## not a heading\n```\n\n~~~~md\n## nor this\n~~~\n~~~~\nEnd.\n\n## 0.5.0 (1-nov-2026)\n\nOld.";
        let body = section(log, "0.6.0").unwrap();
        assert!(
            body.contains("## not a heading") && body.contains("## nor this"),
            "{body}"
        );
        assert!(body.ends_with("End."), "{body}");
        assert_eq!(section(log, "0.5.0").unwrap(), "Old.");
        let crlf = log.replace('\n', "\r\n");
        assert_eq!(section(&crlf, "0.6.0").unwrap(), body);
        assert_eq!(release_section(&crlf, "0.6.0").unwrap(), body);
        assert!(!body.contains('\r'));
        // The last section runs to the end.
        assert_eq!(
            section("## 0.6.0\n\nA.\n\nB.\n", "0.6.0").unwrap(),
            "A.\n\nB."
        );
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
