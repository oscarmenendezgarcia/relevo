//! Encryption at rest (docs/protocol.md, "Encryption at rest"): note text and thread titles are
//! stored sealed with ChaCha20-Poly1305 under the server's key, 32 random bytes in a file kept
//! apart from the database (`$HOME/.config/relevo-server/server.key`, or the file
//! `RELEVO_KEY_FILE` names), in a directory of its own.
//!
//! A sealed value is one version byte (1), a random 12-byte nonce, then the ciphertext and its
//! 16-byte tag. Its associated data says where it belongs (`note:<thread>:<note>`,
//! `title:<thread>`), so a value moved to another row does not open there.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};

/// The first byte of every sealed value: how it was sealed.
const VERSION: u8 = 1;

/// A key is this many raw bytes, in its file as in memory.
const KEY_LEN: usize = 32;

/// What `meta.key_check` holds sealed, under this associated data: a key that opens it is the
/// key the database was encrypted with.
const CHECK_TEXT: &[u8] = b"relevo key check v1";
const CHECK_AAD: &str = "key_check";

/// The server's key.
pub struct Key(LessSafeKey);

/// Says nothing of the key.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key")
    }
}

/// The associated data of a note's text.
pub fn note_aad(thread: &str, note: &str) -> String {
    format!("note:{thread}:{note}")
}

/// The associated data of a thread's title.
pub fn title_aad(thread: &str) -> String {
    format!("title:{thread}")
}

impl Key {
    /// A new random key, kept nowhere: for a database that lives in memory.
    pub fn random() -> Key {
        let mut bytes = [0u8; KEY_LEN];
        rand::fill(&mut bytes);
        Key::from_bytes(&bytes)
    }

    fn from_bytes(bytes: &[u8; KEY_LEN]) -> Key {
        let key = UnboundKey::new(&CHACHA20_POLY1305, bytes).expect("32 bytes are a ChaCha20 key");
        Key(LessSafeKey::new(key))
    }

    /// `plain` sealed for the place `aad` names, under a new random nonce.
    pub fn seal(&self, aad: &str, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::fill(&mut nonce);
        let mut body = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad.as_bytes()),
                &mut body,
            )
            .expect("a note is far below ChaCha20-Poly1305's limit");
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + body.len());
        sealed.push(VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&body);
        sealed
    }

    /// What `seal` sealed for the place `aad` names, if this key sealed it, for that place, and
    /// nothing changed it since; `None` otherwise (another key's, another row's, or damaged).
    pub fn open(&self, aad: &str, sealed: &[u8]) -> Option<Vec<u8>> {
        let (&VERSION, rest) = sealed.split_first()? else {
            return None;
        };
        if rest.len() < NONCE_LEN {
            return None;
        }
        let (nonce, body) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut body = body.to_vec();
        let plain = self
            .0
            .open_in_place(nonce, Aad::from(aad.as_bytes()), &mut body)
            .ok()?
            .len();
        body.truncate(plain);
        Some(body)
    }

    /// Column `idx` of `row`, sealed text, opened: a value this key does not open is an error
    /// that says so, never the value.
    pub fn text(&self, row: &rusqlite::Row, idx: usize, aad: &str) -> rusqlite::Result<String> {
        let sealed: Vec<u8> = row.get(idx)?;
        self.open(aad, &sealed)
            .and_then(|plain| String::from_utf8(plain).ok())
            .ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    idx,
                    rusqlite::types::Type::Blob,
                    "a sealed value this server's key does not open (another key's, or damaged)"
                        .into(),
                )
            })
    }

    /// A new `meta.key_check` for this key: `CHECK_TEXT` sealed.
    pub fn check(&self) -> Vec<u8> {
        self.seal(CHECK_AAD, CHECK_TEXT)
    }

    /// Whether this key made `check` (a `meta.key_check`).
    pub fn opens(&self, check: &[u8]) -> bool {
        self.open(CHECK_AAD, check).is_some_and(|p| p == CHECK_TEXT)
    }
}

/// The key file: `RELEVO_KEY_FILE`, else `$HOME/.config/relevo-server/server.key`.
pub fn path(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    if let Some(p) = env("RELEVO_KEY_FILE").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let home = env("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("HOME is not set; set RELEVO_KEY_FILE"))?;
    Ok(Path::new(&home).join(".config/relevo-server/server.key"))
}

/// The key in `file`, or `None` when there is no file. Only reads. A file of another size than
/// 32 bytes, or that anyone but its owner may read or write, is refused.
pub fn load(file: &Path) -> Result<Option<Key>> {
    let shown = file.display();
    let mut f = match std::fs::File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading the key {shown}")),
    };
    let meta = f
        .metadata()
        .with_context(|| format!("reading the key {shown}"))?;
    if !meta.is_file() {
        bail!("the key {shown} is not a file");
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o177 != 0 {
        bail!(
            "the key {shown} has mode {mode:04o}: only its owner may read it (chmod 600 {shown})"
        );
    }
    if meta.len() != KEY_LEN as u64 {
        bail!(
            "the key {shown} holds {} bytes: a relevo key is exactly {KEY_LEN} raw bytes",
            meta.len()
        );
    }
    let mut bytes = [0u8; KEY_LEN];
    f.read_exact(&mut bytes)
        .with_context(|| format!("reading the key {shown}"))?;
    Ok(Some(Key::from_bytes(&bytes)))
}

/// The key in `file`, and whether it was made now: made (32 random bytes, 0600, in a directory
/// made 0700 if missing) when there is none. It is written to a file of its own and synced
/// before it takes the name, and the directory is synced after, so once this returns the key
/// outlives a crash. A key is never replaced: when another process gave the name a key first,
/// that one is returned.
pub fn load_or_create(file: &Path) -> Result<(Key, bool)> {
    if let Some(key) = load(file)? {
        return Ok((key, false));
    }
    let shown = file.display();
    let dir = file
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let mut bytes = [0u8; KEY_LEN];
    rand::fill(&mut bytes);
    let tmp = dir.join(crate::db::random_id(".server.key."));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        // A link, not a rename: it never takes the name from a key already there.
        std::fs::hard_link(&tmp, file)
    })();
    let _ = std::fs::remove_file(&tmp);
    match written {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let key = load(file)?.with_context(|| format!("the key {shown} went away"))?;
            return Ok((key, false));
        }
        Err(e) => return Err(e).with_context(|| format!("writing the key {shown}")),
    }
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("writing the key {shown}"))?;
    Ok((Key::from_bytes(&bytes), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-crypto-"));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_sealed_value_opens_with_its_key_for_its_place_only() {
        let key = Key::random();
        let aad = note_aad("r_aaaaaaaaaaaa", "n_bbbbbbbbbbbb");
        let sealed = key.seal(&aad, "Deploy tonight?".as_bytes());
        assert_eq!(sealed[0], 1);
        assert_eq!(sealed.len(), 1 + 12 + "Deploy tonight?".len() + 16);
        assert_eq!(key.open(&aad, &sealed).unwrap(), b"Deploy tonight?");
        // A new nonce every time.
        assert_ne!(key.seal(&aad, b"Deploy tonight?"), sealed);
        assert_eq!(key.open(&aad, &key.seal(&aad, b"")).unwrap(), b"");

        // Another key.
        assert!(Key::random().open(&aad, &sealed).is_none());
        // Another row: the same value moved to another note, or to a title.
        assert!(
            key.open(&note_aad("r_aaaaaaaaaaaa", "n_cccccccccccc"), &sealed)
                .is_none()
        );
        assert!(key.open(&title_aad("r_aaaaaaaaaaaa"), &sealed).is_none());
        // Any byte changed: the version, the nonce, the ciphertext or the tag.
        for i in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[i] ^= 0x01;
            assert!(key.open(&aad, &tampered).is_none(), "byte {i}");
        }
        // Cut short, or not sealed at all.
        assert!(key.open(&aad, &sealed[..sealed.len() - 1]).is_none());
        assert!(key.open(&aad, &sealed[..5]).is_none());
        assert!(key.open(&aad, b"").is_none());
        assert!(key.open(&aad, b"Deploy tonight?").is_none());
    }

    #[test]
    fn the_key_check_says_which_key_encrypted_the_database() {
        let key = Key::random();
        let check = key.check();
        assert!(key.opens(&check));
        assert!(!Key::random().opens(&check));
        assert!(!key.opens(&check[1..]));
        assert!(!key.opens(b""));
    }

    #[test]
    fn a_sealed_column_opens_as_text_or_fails_without_its_value() {
        let key = Key::random();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sealed = key.seal("note:r_x:n_y", "Lunch?".as_bytes());
        let read = |aad: &str| conn.query_row("SELECT ?1", [&sealed], |r| key.text(r, 0, aad));
        assert_eq!(read("note:r_x:n_y").unwrap(), "Lunch?");
        let err = read("note:r_x:n_z").unwrap_err().to_string();
        assert!(err.contains("does not open"), "{err}");
        assert!(!err.contains("Lunch"), "{err}");
        // A value stored in plain text is no sealed value.
        let plain = conn.query_row("SELECT 'Lunch?'", [], |r| key.text(r, 0, "note:r_x:n_y"));
        assert!(plain.is_err());
    }

    #[test]
    fn a_key_is_made_once_privately_and_read_back() {
        let dir = tempdir();
        let file = dir.join("nested/relevo/server.key");
        assert!(load(&file).unwrap().is_none());
        let (key, made) = load_or_create(&file).unwrap();
        assert!(made);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        assert_eq!(std::fs::read(&file).unwrap().len(), 32);
        // Nothing left beside it.
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["server.key"]);
        // Read back, it is the same key; asked again, it is not made again.
        let check = key.check();
        assert!(load(&file).unwrap().unwrap().opens(&check));
        let (again, made) = load_or_create(&file).unwrap();
        assert!(!made);
        assert!(again.opens(&check));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_file_of_another_size_or_open_to_others_is_refused() {
        let dir = tempdir();
        let file = dir.join("server.key");
        for (bytes, mode, says) in [
            (31, 0o600, "holds 31 bytes"),
            (33, 0o600, "holds 33 bytes"),
            (64, 0o600, "exactly 32 raw bytes"),
            (32, 0o644, "mode 0644"),
            (32, 0o640, "chmod 600"),
            (32, 0o700, "mode 0700"),
        ] {
            std::fs::write(&file, vec![7u8; bytes]).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = format!("{:#}", load(&file).expect_err("refused"));
            assert!(err.contains(says), "{bytes} {mode:o}: {err}");
            // Never made again over it.
            assert!(load_or_create(&file).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), vec![7u8; bytes]);
        }
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(load(&file).unwrap().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_key_file_is_relevo_key_file_or_under_home() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            path(env(&[("HOME", "/home/x")])).unwrap(),
            Path::new("/home/x/.config/relevo-server/server.key")
        );
        assert_eq!(
            path(env(&[("HOME", "/home/x"), ("RELEVO_KEY_FILE", "/srv/k")])).unwrap(),
            Path::new("/srv/k")
        );
        assert!(path(env(&[])).is_err());
    }
}
