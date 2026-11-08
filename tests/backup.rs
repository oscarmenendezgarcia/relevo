//! `relevo backup`, run as the real binary against a database another connection keeps open.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relevo::db;

fn relevo(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_relevo"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn failed(out: Output) -> String {
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8(out.stderr).unwrap()
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(db::random_id("relevo-backup-"));
    fs::create_dir(&dir).unwrap();
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A database the "server" holds open, with ana committed (still in the -wal file) and bob in a
/// write transaction that is not committed.
fn busy_database(dir: &Path) -> (rusqlite::Connection, rusqlite::Connection) {
    let path = dir.join("relevo.db");
    let server = db::open(path.to_str().unwrap()).unwrap();
    db::invite(&server, "ana", "Ana", None, "laptop", 0).unwrap();
    assert!(fs::metadata(dir.join("relevo.db-wal")).unwrap().len() > 0);
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO people (handle, name, created_at) VALUES ('bob', 'Bob', 0);",
        )
        .unwrap();
    (server, writer)
}

#[test]
fn a_backup_taken_while_the_server_writes_is_consistent_private_and_restorable() {
    let dir = temp_dir();
    let (server, writer) = busy_database(&dir);
    let dest = dir.join("copy.db");
    let out = ok(relevo(
        &dir.join("relevo.db"),
        &["backup", dest.to_str().unwrap()],
    ));
    assert!(out.contains("copy.db"), "{out}");
    assert_eq!(mode(&dest), 0o600);
    // Nothing half-written is left next to it.
    assert!(
        names(&dir).iter().all(|n| !n.contains("partial")),
        "{:?}",
        names(&dir)
    );
    drop(writer);
    drop(server);

    // Restoring is using the file: the committed person is there, the uncommitted one is not.
    let restored = dir.join("restored");
    fs::create_dir(&restored).unwrap();
    fs::copy(&dest, restored.join("relevo.db")).unwrap();
    let people = ok(relevo(&restored.join("relevo.db"), &["people"]));
    assert!(people.starts_with("ana\tAna\tactive"), "{people}");
    assert!(!people.contains("bob"), "{people}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_existing_file_is_never_overwritten() {
    let dir = temp_dir();
    let (_server, _writer) = busy_database(&dir);
    let dest = dir.join("copy.db");
    fs::write(&dest, "precious").unwrap();
    let err = failed(relevo(
        &dir.join("relevo.db"),
        &["backup", dest.to_str().unwrap()],
    ));
    assert!(err.contains("refusing to overwrite"), "{err}");
    assert_eq!(fs::read_to_string(&dest).unwrap(), "precious");
    assert_eq!(
        names(&dir),
        ["copy.db", "relevo.db", "relevo.db-shm", "relevo.db-wal"]
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn keep_writes_timestamped_backups_into_a_directory_and_removes_only_old_ones() {
    let dir = temp_dir();
    let (_server, _writer) = busy_database(&dir);
    let db = dir.join("relevo.db");
    let backups = dir.join("backups");

    // A missing directory is created, private.
    let out = ok(relevo(
        &db,
        &["backup", "--keep", "2", backups.to_str().unwrap()],
    ));
    assert_eq!(mode(&backups), 0o700);
    let first = names(&backups);
    assert_eq!(first.len(), 1, "{out}");
    assert_eq!(first[0].len(), "20261108-104100.db".len());
    assert!(first[0].starts_with("20") && first[0].ends_with(".db"));

    for old in [
        "20200101-000000.db",
        "20210101-000000.db",
        "20220101-000000.db",
    ] {
        fs::write(backups.join(old), "old").unwrap();
    }
    // Not backups by name, or not files: left alone.
    fs::write(backups.join("notes.txt"), "keep me").unwrap();
    fs::write(backups.join("20190101-000000.db-wal"), "").unwrap();
    fs::create_dir(backups.join("20180101-000000.db")).unwrap();
    // Two runs within a second would clash on the name; make sure this one is new.
    fs::remove_file(backups.join(&first[0])).unwrap();

    let out = ok(relevo(
        &db,
        &["backup", "--keep", "2", backups.to_str().unwrap()],
    ));
    let left = names(&backups);
    assert_eq!(left.len(), 5, "{left:?}");
    let new = left[3].clone();
    assert_eq!(
        [&left[..3], &left[4..]].concat(),
        [
            "20180101-000000.db",
            "20190101-000000.db-wal",
            "20220101-000000.db",
            "notes.txt"
        ]
    );
    assert!(new.as_str() > "2026", "{left:?}");
    assert_eq!(mode(&backups.join(&new)), 0o600);
    assert!(out.contains("removed old backup"), "{out}");
    assert!(!out.contains("20220101"), "{out}");

    // Without --keep, a directory gets a timestamped file and nothing is removed.
    fs::remove_file(backups.join(&new)).unwrap();
    ok(relevo(&db, &["backup", backups.to_str().unwrap()]));
    assert_eq!(names(&backups).len(), 5);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_sources_and_destinations_are_refused_and_create_nothing() {
    let dir = temp_dir();
    let missing = dir.join("missing.db");
    let dest = dir.join("copy.db");
    let err = failed(relevo(&missing, &["backup", dest.to_str().unwrap()]));
    assert!(err.contains("missing.db"), "{err}");
    assert!(names(&dir).is_empty(), "{:?}", names(&dir));

    // An empty file is no database to back up.
    fs::write(&missing, "").unwrap();
    let err = failed(relevo(&missing, &["backup", dest.to_str().unwrap()]));
    assert!(err.contains("nothing to back up"), "{err}");
    assert!(!dest.exists());
    fs::remove_file(&missing).unwrap();

    let (_server, _writer) = busy_database(&dir);
    let db = dir.join("relevo.db");
    let file = dir.join("file");
    fs::write(&file, "x").unwrap();
    let err = failed(relevo(
        &db,
        &["backup", "--keep", "3", file.to_str().unwrap()],
    ));
    assert!(err.contains("needs a directory"), "{err}");
    let err = failed(relevo(
        &db,
        &["backup", "--keep", "0", dir.to_str().unwrap()],
    ));
    assert!(err.contains("--keep"), "{err}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_database_from_another_version_can_still_be_backed_up() {
    // So that the owner can keep a copy before deleting it on an upgrade.
    let dir = temp_dir();
    let db = dir.join("old.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE people (id INTEGER PRIMARY KEY, handle TEXT);
             INSERT INTO people (handle) VALUES ('ana');",
        )
        .unwrap();
    let dest = dir.join("copy.db");
    ok(relevo(&db, &["backup", dest.to_str().unwrap()]));
    let copy = rusqlite::Connection::open(&dest).unwrap();
    let handle: String = copy
        .query_row("SELECT handle FROM people", [], |r| r.get(0))
        .unwrap();
    assert_eq!(handle, "ana");
    fs::remove_dir_all(&dir).unwrap();
}
