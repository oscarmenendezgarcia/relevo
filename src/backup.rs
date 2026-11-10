//! `relevo backup`: a consistent copy of the database, safe to take while the server runs.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use time::OffsetDateTime;

/// What a backup wrote and, with `keep`, which older backups it removed.
pub struct Backup {
    pub path: PathBuf,
    pub removed: Vec<PathBuf>,
}

/// Copies the database at `db` with `VACUUM INTO`, which writes one consistent snapshot of it
/// (a raw copy of a WAL database can miss what is still in the `-wal` file, or catch a page half
/// written). Readers never block the server in WAL mode, so it can keep running.
///
/// `dest` is a new file, or a directory to write `YYYYMMDD-HHMMSS.db` (UTC) into. An existing file
/// is never overwritten. With `keep`, `dest` must be a directory (created, 0700, if missing) and
/// only the newest `keep` backups named that way are left in it; other files are not touched.
/// The copy is written to a hidden file next to it, created 0600 (it holds every note), checked,
/// and only then given its name, so a failed or interrupted backup never looks like a good one.
pub fn backup(db: &str, dest: &Path, keep: Option<usize>, now: OffsetDateTime) -> Result<Backup> {
    if keep == Some(0) {
        bail!("--keep must be at least 1");
    }
    let target = if keep.is_some() || dest.is_dir() {
        if !dest.exists() {
            create_private_dir(dest).with_context(|| format!("creating {}", dest.display()))?;
        } else if !dest.is_dir() {
            bail!(
                "--keep needs a directory, and {} is not one",
                dest.display()
            );
        }
        dest.join(file_name(now))
    } else {
        dest.to_path_buf()
    };
    if fs::symlink_metadata(&target).is_ok() {
        bail!(
            "{} already exists; refusing to overwrite it",
            target.display()
        );
    }

    // Read-only, and never created: a missing database is an error, not an empty backup.
    let source = crate::db::read_only(db).with_context(|| format!("opening database {db}"))?;
    let tables: i64 = source
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .with_context(|| format!("reading database {db}"))?;
    if tables == 0 {
        bail!("database {db} is empty or missing; nothing to back up");
    }

    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = target
        .file_name()
        .with_context(|| format!("{} names no file", target.display()))?
        .to_string_lossy()
        .into_owned();
    let partial = dir.join(format!(".{name}.partial-{}", crate::db::random_id("")));
    let written = write_copy(&source, &partial, &target);
    // The partial file is gone either way: renamed, linked (then this removes the extra name), or
    // failed.
    let _ = fs::remove_file(&partial);
    written?;
    if let Ok(dir) = fs::File::open(&dir) {
        let _ = dir.sync_all();
    }

    let mut removed = Vec::new();
    if let Some(keep) = keep {
        let mut older: Vec<String> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_backup_name(n) && *n != name)
            .collect();
        // Names sort by time; newest first. The one just written always stays, even if the clock
        // went back and it sorts before older ones.
        older.sort_unstable_by(|a, b| b.cmp(a));
        for old in older.into_iter().skip(keep - 1) {
            let path = dir.join(old);
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            removed.push(path);
        }
    }
    Ok(Backup {
        path: target,
        removed,
    })
}

/// `VACUUM INTO` a fresh 0600 file at `partial`, check it, flush it, and give it the name
/// `target` without ever replacing a file there.
fn write_copy(source: &Connection, partial: &Path, target: &Path) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(partial)
        .with_context(|| format!("creating {}", partial.display()))?;
    let partial_str = partial
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", partial.display()))?;
    // VACUUM INTO writes into an empty existing file, keeping its mode.
    source
        .execute("VACUUM INTO ?1", [partial_str])
        .context("copying the database")?;

    let copy = Connection::open_with_flags(partial, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = copy.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    drop(copy);
    if check != "ok" {
        bail!("the copy failed its integrity check: {check}");
    }
    fs::File::open(partial)?.sync_all()?;

    // A hard link fails if `target` exists, so a file that appeared meanwhile is never replaced.
    // Some file systems (FAT, some network mounts) have no hard links: there, rename if `target`
    // is still free.
    match fs::hard_link(partial, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bail!(
                "{} already exists; refusing to overwrite it",
                target.display()
            )
        }
        Err(_) if fs::symlink_metadata(target).is_err() => {
            fs::rename(partial, target).with_context(|| format!("writing {}", target.display()))
        }
        Err(e) => Err(e).with_context(|| format!("writing {}", target.display())),
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// `YYYYMMDD-HHMMSS.db`, in UTC.
fn file_name(now: OffsetDateTime) -> String {
    let t = now.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}.db",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Whether `name` is `YYYYMMDD-HHMMSS.db`: the only files `--keep` removes.
fn is_backup_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 18
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && &b[15..] == b".db"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_utc_timestamps_and_only_those_count() {
        let t = OffsetDateTime::from_unix_timestamp(1_794_133_505).unwrap();
        assert_eq!(file_name(t), "20261108-102505.db");
        let east = t.to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
        assert_eq!(file_name(east), "20261108-102505.db");
        assert!(is_backup_name("20261108-090505.db"));
        for other in [
            "relevo.db",
            "20261108-090505.db-wal",
            ".20261108-090505.db.partial-abc",
            "2026110a-090505.db",
            "20261108_090505.db",
        ] {
            assert!(!is_backup_name(other), "{other}");
        }
    }
}
