//! `relevo update` (docs/update.md): this binary and the adapters registered on this machine
//! update themselves from the clients the team's server hands out (`crate::dist`).
//!
//! Nothing is installed whose manifest does not carry a valid signature by the release key
//! compiled into this binary, or whose bytes do not match the manifest. The binary only ever
//! uses that key; tests give `run` another one.

use std::cmp::Ordering;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::client::{self, Client};
use crate::config::{self, Adapter, Config};
use crate::dist::{DistFile, MANIFEST, Manifest, SIGNATURE};

/// The public half of the key the release workflow signs `manifest.json` with.
pub const RELEASE_KEY: &str = include_str!("../keys/release-signing-pub.pem");

/// At most one `--auto` check an hour per machine.
pub const AUTO_EVERY: i64 = 3600;

const MAX_MANIFEST: u64 = 1024 * 1024;
const SIGNATURE_LEN: u64 = 64;
const SMALL_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_TIMEOUT: Duration = Duration::from_secs(300);

/// An Ed25519 public key.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicKey([u8; 32]);

/// The DER of an Ed25519 SubjectPublicKeyInfo before its 32 key bytes.
const ED25519_SPKI: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

impl PublicKey {
    /// From a PEM `PUBLIC KEY`, as `openssl pkey -pubout` writes it.
    pub fn from_pem(pem: &str) -> Result<PublicKey> {
        use base64::Engine;
        let b64: String = pem
            .lines()
            .map(str::trim)
            .skip_while(|l| *l != "-----BEGIN PUBLIC KEY-----")
            .skip(1)
            .take_while(|l| *l != "-----END PUBLIC KEY-----")
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| anyhow!("not a PEM public key"))?;
        match der.strip_prefix(&ED25519_SPKI[..]) {
            Some(key) if key.len() == 32 => Ok(PublicKey(key.try_into().expect("32 bytes"))),
            _ => bail!("not an Ed25519 public key"),
        }
    }

    pub fn from_bytes(bytes: [u8; 32]) -> PublicKey {
        PublicKey(bytes)
    }

    /// The release key compiled into this binary.
    pub fn release() -> PublicKey {
        PublicKey::from_pem(RELEASE_KEY).expect("keys/release-signing-pub.pem is an Ed25519 key")
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &self.0)
            .verify(message, signature)
            .is_ok()
    }
}

/// A version `major.minor.patch`, with an optional pre-release (`-rc.1`), which comes before
/// the release itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    numbers: [u64; 3],
    pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Result<Version> {
        let s = s.trim();
        let bad = || anyhow!("not a version: {s:?}");
        let core = s.split('+').next().unwrap_or("");
        let (numbers, pre) = match core.split_once('-') {
            Some((n, pre)) if !pre.is_empty() => (n, Some(pre.to_string())),
            Some(_) => return Err(bad()),
            None => (core, None),
        };
        let parts: Vec<u64> = numbers
            .split('.')
            .map(|p| p.parse().map_err(|_| bad()))
            .collect::<Result<_>>()?;
        let numbers: [u64; 3] = parts.try_into().map_err(|_| bad())?;
        // SemVer identifiers: never empty, and a number has no leading zero (so that two
        // pre-releases that compare equal are the same string).
        let identifier = |p: &str| {
            !p.is_empty()
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !(p.len() > 1 && p.starts_with('0') && p.chars().all(|c| c.is_ascii_digit()))
        };
        if pre
            .as_deref()
            .is_some_and(|p| !p.split('.').all(identifier))
        {
            return Err(bad());
        }
        Ok(Version { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers
            .cmp(&other.numbers)
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => pre_cmp(a, b),
            })
    }
}

/// Two pre-releases, as SemVer orders them: identifier by identifier, numbers as numbers and
/// before words, and a shorter list first when all else is equal (`rc.2` < `rc.10` < `rc.10.1`).
fn pre_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        let order = match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An adapter that `relevo update` can update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Kind {
    Pi,
    Hermes,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "pi" => Some(Kind::Pi),
            "hermes" => Some(Kind::Hermes),
            _ => None,
        }
    }

    /// As the config and the manifest name it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Pi => "pi",
            Kind::Hermes => "hermes",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Pi => "the pi extension",
            Kind::Hermes => "the Hermes plugin",
        }
    }

    fn applies(self) -> &'static str {
        match self {
            Kind::Pi => "it applies at the next start of pi",
            Kind::Hermes => "restart the Hermes gateway to use it",
        }
    }

    /// The version of this adapter in `dir`, from its own files; an error when `dir` is not
    /// this adapter, so that nothing else is ever replaced.
    pub fn version_in(self, dir: &Path) -> Result<String> {
        let not = || anyhow!("{} is not {}", dir.display(), self.label());
        match self {
            Kind::Pi => {
                let bytes = fs::read(dir.join("package.json")).map_err(|_| not())?;
                let package: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| not())?;
                if package["name"] != "relevo-pi" {
                    return Err(not());
                }
                package["version"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(not)
            }
            Kind::Hermes => {
                let text = fs::read_to_string(dir.join("plugin.yaml")).map_err(|_| not())?;
                // Top-level `key: value` lines only.
                let field = |key: &str| {
                    text.lines().find_map(|l| {
                        let v = l.strip_prefix(key)?.strip_prefix(':')?.trim();
                        Some(v.trim_matches(|c| c == '"' || c == '\'').to_string())
                    })
                };
                if field("name").as_deref() != Some("relevo") {
                    return Err(not());
                }
                field("version").ok_or_else(not)
            }
        }
    }
}

/// What this binary is, for `run`: the key it trusts, its version, its target and its file.
#[derive(Debug, Clone)]
pub struct Setup {
    pub key: PublicKey,
    pub version: String,
    pub target: String,
    pub exe: PathBuf,
}

impl Setup {
    pub fn this_binary() -> Result<Setup> {
        let exe = std::env::current_exe().context("finding this relevo's own file")?;
        Ok(Setup {
            key: PublicKey::release(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            target: own_target(),
            exe,
        })
    }
}

/// The target triple of this binary, as the release names its binaries.
pub fn own_target() -> String {
    let rest = match std::env::consts::OS {
        "linux" if cfg!(target_env = "musl") => "unknown-linux-musl",
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        other => other,
    };
    format!("{}-{rest}", std::env::consts::ARCH)
}

/// What happened to one thing `relevo update` looks after.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// `relevo (/path/to/relevo)`, or `the pi extension (/path)`.
    pub what: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    UpToDate(String),
    WouldInstall {
        from: String,
        to: String,
    },
    Installed {
        from: String,
        to: String,
        then: &'static str,
    },
    /// Left as it is, and why: not a failure.
    Kept(String),
    /// Installing it failed; it was left as it was.
    Failed(String),
}

impl Step {
    pub fn installed(&self) -> bool {
        matches!(self.outcome, Outcome::Installed { .. })
    }

    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed(_))
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = &self.what;
        match &self.outcome {
            Outcome::UpToDate(v) => write!(f, "{what}: {v}, up to date"),
            Outcome::WouldInstall { from, to } => {
                write!(f, "{what}: would install {to} over {from}")
            }
            Outcome::Installed { from, to, then } => {
                write!(f, "{what}: installed {to} over {from}; {then}")
            }
            Outcome::Kept(why) => write!(f, "{what}: {why}"),
            Outcome::Failed(why) => write!(f, "{what}: not installed: {why}"),
        }
    }
}

/// For one thing: its outcome already, or a job to do over the version it has.
type Plan = Result<Outcome, (String, Job)>;

/// One thing to install.
enum Job {
    Binary(DistFile),
    Adapter(Kind, PathBuf, DistFile),
}

/// Checks the server's dist and, unless `check`, installs what is newer: this binary
/// (`setup.exe`) and the adapters of `config.adapters`. `Err` means nothing was installed.
pub async fn run(config: &Config, setup: &Setup, check: bool) -> Result<Vec<Step>> {
    let client = Client::new(config, Some(String::new()), "/".into())?;
    let from = |what: &str, e: client::Error| anyhow!("fetching {what} from {}: {e}", config.url);
    let manifest_bytes = match client
        .fetch(
            &format!("/api/dist/{MANIFEST}"),
            MAX_MANIFEST,
            SMALL_TIMEOUT,
        )
        .await
    {
        Ok(bytes) => bytes,
        Err(client::Error::Api { status: 404, .. }) => {
            return Ok(vec![Step {
                what: "relevo".into(),
                outcome: Outcome::Kept(format!("{} hands out no clients", config.url)),
            }]);
        }
        Err(e) => return Err(from("the manifest", e)),
    };
    let signature = client
        .fetch(
            &format!("/api/dist/{SIGNATURE}"),
            SIGNATURE_LEN,
            SMALL_TIMEOUT,
        )
        .await
        .map_err(|e| match e {
            client::Error::Api { status: 404, .. } => anyhow!(
                "the clients {} hands out are not signed (no {SIGNATURE}): nothing was installed",
                config.url
            ),
            e => from("the signature", e),
        })?;
    if !setup.key.verify(&manifest_bytes, &signature) {
        bail!(
            "the clients {} hands out are not signed by the release key: nothing was installed",
            config.url
        );
    }
    let manifest = Manifest::parse(&manifest_bytes)?;
    let offered = Version::parse(&manifest.version)?;
    let to = manifest.version.clone();

    // What to do with each thing, in order: a step already, or a job.
    let mut plan: Vec<(String, Plan)> = vec![];
    let own = Version::parse(&setup.version)?;
    let what = format!("relevo ({})", setup.exe.display());
    let binary = manifest
        .files
        .iter()
        .find(|f| f.kind == "binary" && f.target.as_deref() == Some(&setup.target));
    plan.push((
        what,
        match (offered.cmp(&own), binary) {
            (Ordering::Equal, _) => Ok(Outcome::UpToDate(setup.version.clone())),
            (Ordering::Less, _) => Ok(older(&to, &setup.version)),
            (Ordering::Greater, None) => Ok(Outcome::Kept(format!(
                "the server hands out {to}, but no binary for {}",
                setup.target
            ))),
            (Ordering::Greater, Some(f)) => Err((setup.version.clone(), Job::Binary(f.clone()))),
        },
    ));
    let mut seen = vec![];
    for adapter in &config.adapters {
        let Some(kind) = Kind::parse(&adapter.kind) else {
            continue;
        };
        // The folder itself: a link to it stays a link, and what it points to is updated.
        let real = fs::canonicalize(&adapter.path);
        let path = real.as_ref().unwrap_or(&adapter.path).clone();
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());
        let what = format!("{} ({})", kind.label(), adapter.path.display());
        if real.is_err() {
            plan.push((what, Ok(Outcome::Kept("no longer there".into()))));
            continue;
        }
        if in_git_checkout(&path) {
            plan.push((what, Ok(Outcome::Kept(IN_GIT.into()))));
            continue;
        }
        // A folder that is no longer that adapter is left as it is; the rest is still updated.
        let have = match kind.version_in(&path) {
            Ok(have) => have,
            Err(e) => {
                plan.push((what, Ok(Outcome::Kept(format!("{e:#}: left as it is")))));
                continue;
            }
        };
        let file = manifest.files.iter().find(|f| f.kind == kind.name());
        let outcome = match (Version::parse(&have).map(|v| offered.cmp(&v)), file) {
            (Err(e), _) => Ok(Outcome::Kept(format!("{e:#}"))),
            (Ok(Ordering::Equal), _) => Ok(Outcome::UpToDate(have)),
            (Ok(Ordering::Less), _) => Ok(older(&to, &have)),
            (Ok(Ordering::Greater), None) => Ok(Outcome::Kept(format!(
                "the server hands out {to}, but not {}",
                kind.label()
            ))),
            (Ok(Ordering::Greater), Some(f)) => {
                Err((have, Job::Adapter(kind, path.clone(), f.clone())))
            }
        };
        plan.push((what, outcome));
    }

    if check {
        return Ok(plan
            .into_iter()
            .map(|(what, outcome)| Step {
                what,
                outcome: outcome.unwrap_or_else(|(from, _)| Outcome::WouldInstall {
                    from,
                    to: to.clone(),
                }),
            })
            .collect());
    }

    // Everything checked and writable before anything is installed.
    let mut fetched = vec![];
    for (what, outcome) in &plan {
        let Err((_, job)) = outcome else { continue };
        let file = match job {
            Job::Binary(f) | Job::Adapter(_, _, f) => f,
        };
        let bytes = client
            .fetch(&format!("/api/dist/{}", file.name), file.size, FILE_TIMEOUT)
            .await
            .map_err(|e| match e {
                client::Error::BadResponse(_) => anyhow!(
                    "{} is bigger than the {} bytes of the signed manifest",
                    file.name,
                    file.size
                ),
                e => from(&file.name, e),
            })
            .and_then(|bytes| matches(file, &bytes).map(|()| bytes))
            .with_context(|| format!("{what}: nothing was installed"))?;
        let dir = match job {
            Job::Binary(_) => setup.exe.parent(),
            Job::Adapter(_, path, _) => path.parent(),
        };
        let dir = dir
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !writable(dir) {
            bail!(
                "{what}: cannot write in {}: nothing was installed. Update it by hand, or run \
                 `relevo update` as the user who can write there",
                dir.display()
            );
        }
        fetched.push(bytes);
    }

    let mut fetched = fetched.into_iter();
    let mut steps = vec![];
    for (what, outcome) in plan {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err((from, job)) => {
                let bytes = fetched.next().expect("one per job");
                let (version, exe) = (to.clone(), setup.exe.clone());
                let done = tokio::task::spawn_blocking(move || match job {
                    Job::Binary(_) => install_binary(&exe, &bytes, &version)
                        .map(|()| "it takes effect at the next start of relevo"),
                    Job::Adapter(kind, dir, _) => {
                        install_adapter(kind, &dir, &bytes, &version).map(|()| kind.applies())
                    }
                })
                .await
                .unwrap_or_else(|e| Err(anyhow!("{e}")));
                match done {
                    Ok(then) => Outcome::Installed {
                        from,
                        to: to.clone(),
                        then,
                    },
                    Err(e) => Outcome::Failed(format!("{e:#}")),
                }
            }
        };
        steps.push(Step { what, outcome });
    }
    Ok(steps)
}

const IN_GIT: &str = "in a git checkout: update it with git";

/// Whether `dir` is in a git checkout (a `.git`, folder or file, in it or above it). Such a folder
/// is updated with git, never replaced: the swap would throw away what git tracks there,
/// uncommitted work included.
pub fn in_git_checkout(dir: &Path) -> bool {
    dir.ancestors()
        .any(|d| fs::symlink_metadata(d.join(".git")).is_ok())
}

fn older(offered: &str, have: &str) -> Outcome {
    Outcome::Kept(format!(
        "the server hands out {offered}, older than {have}: left as it is (no downgrades)"
    ))
}

/// Whether `bytes` are the file the manifest lists, by size and SHA-256.
fn matches(file: &DistFile, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != file.size {
        bail!(
            "{} is {} bytes, not the {} of the signed manifest",
            file.name,
            bytes.len(),
            file.size
        );
    }
    let sha: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !sha.eq_ignore_ascii_case(&file.sha256) {
        bail!("{} does not match the signed manifest (SHA-256)", file.name);
    }
    Ok(())
}

/// Whether this process may create and rename files in `dir`.
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a valid C string; access() only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// `.<name>.<suffix>` next to `path`.
fn beside(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
    Ok(path.with_file_name(format!(".{}.{suffix}", name.to_string_lossy())))
}

/// Puts `bytes` in place of the binary `exe` in one step: written next to it, synced, checked to
/// run here as relevo `version`, and renamed over it. A failure leaves the old one whole.
fn install_binary(exe: &Path, bytes: &[u8], version: &str) -> Result<()> {
    let tmp = beside(exe, "update")?;
    let _ = fs::remove_file(&tmp);
    let mode = fs::metadata(exe)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o755);
    let written = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.set_permissions(fs::Permissions::from_mode(mode | 0o100))?;
        f.sync_all()?;
        drop(f);
        // A binary for another machine, or one that does not start here, is never put in place.
        let out = run_version(&tmp).context("the new relevo does not run here")?;
        let says = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || says != format!("relevo {version}") {
            bail!("the new relevo does not run here (it says {says:?})");
        }
        fs::rename(&tmp, exe).with_context(|| format!("replacing {}", exe.display()))?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    sync_dir(exe.parent());
    Ok(())
}

/// `exe --version`. A process forked elsewhere in this one (a git or tar child) may still hold
/// the file open for writing for a moment, until it execs: "text file busy" is tried again.
fn run_version(exe: &Path) -> std::io::Result<std::process::Output> {
    let mut tries = 0;
    loop {
        let out = std::process::Command::new(exe)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output();
        match out {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            out => return out,
        }
    }
}

fn sync_dir(dir: Option<&Path>) {
    if let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty())
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
}

/// Puts the adapter archive `bytes` in place of the folder `dir`: unpacked next to it, checked
/// to be `kind` at `version`, and swapped with it in one step where the system can (Linux),
/// else with two renames.
fn install_adapter(kind: Kind, dir: &Path, bytes: &[u8], version: &str) -> Result<()> {
    let new = beside(dir, "update-new")?;
    let old = beside(dir, "update-old")?;
    for stale in [&new, &old] {
        remove_any(stale);
    }
    let mode = fs::metadata(dir)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o755);
    let unpacked = (|| -> Result<()> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&new)
            .with_context(|| format!("creating {}", new.display()))?;
        unpack(bytes, &new)?;
        let got = kind.version_in(&new)?;
        if got != version {
            bail!("the archive holds {} {got}, not {version}", kind.label());
        }
        sync_tree(&new)?;
        fs::set_permissions(&new, fs::Permissions::from_mode(mode))?;
        Ok(())
    })();
    if let Err(e) = unpacked {
        remove_any(&new);
        return Err(e);
    }
    match exchange(&new, dir) {
        // `new` now holds the old folder.
        Ok(()) => remove_any(&new),
        Err(_) => {
            fs::rename(dir, &old).with_context(|| format!("moving {} aside", dir.display()))?;
            if let Err(e) = fs::rename(&new, dir) {
                let _ = fs::rename(&old, dir);
                remove_any(&new);
                return Err(e).with_context(|| format!("putting {} in place", dir.display()));
            }
            remove_any(&old);
        }
    }
    sync_dir(dir.parent());
    Ok(())
}

/// Removes a file, a link or a whole folder, if there.
fn remove_any(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// Unpacks a `.tar.gz` into `dir` with the system's tar.
fn unpack(bytes: &[u8], dir: &Path) -> Result<()> {
    use std::process::{Command, Stdio};
    let mut tar = Command::new("tar")
        .args(["-xzf", "-", "--no-same-owner"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running tar")?;
    let mut stdin = tar.stdin.take().expect("piped");
    let wrote = stdin.write_all(bytes);
    drop(stdin);
    let out = tar.wait_with_output().context("running tar")?;
    if !out.status.success() || wrote.is_err() {
        bail!(
            "tar could not unpack it: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Syncs every file and folder under `dir`, so the swap never shows half-written files after a
/// crash.
fn sync_tree(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Swaps two paths in one step (Linux's `renameat2` with `RENAME_EXCHANGE`).
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes());
    let (a, b) = (c(a)?, c(b)?);
    // SAFETY: two valid C strings, only read.
    let r = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn exchange(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

// ---------- the command ----------

/// What `relevo update` was asked.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Only say what it would do.
    pub check: bool,
    /// As the clients run it by themselves: off with `RELEVO_AUTO_UPDATE=0`, at most once an
    /// hour, and quiet when there is nothing to update with.
    pub auto: bool,
    /// An adapter to register (unless `check`) and update.
    pub adapter: Option<(Kind, PathBuf)>,
}

/// Whether `RELEVO_AUTO_UPDATE` turns the automatic update off.
pub fn auto_disabled(env: &impl Fn(&str) -> Option<String>) -> bool {
    env("RELEVO_AUTO_UPDATE").is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// Where `--auto` keeps when it last checked: `$XDG_CACHE_HOME/relevo`, else
/// `$HOME/.cache/relevo`.
pub fn cache_dir(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name| env(name).filter(|v: &String| !v.is_empty());
    match set("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Some(dir.join("relevo")),
        _ => set("HOME").map(|h| Path::new(&h).join(".cache/relevo")),
    }
}

/// The machine's update lock, held while the returned file is open; `None` when another
/// `relevo update` holds it.
pub fn lock(cache: &Path) -> Result<Option<File>> {
    use std::os::fd::AsRawFd;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(cache)
        .with_context(|| format!("creating {}", cache.display()))?;
    let path = cache.join("update.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: a file descriptor this function owns.
    let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok((r == 0).then_some(file))
}

/// Whether an `--auto` check is due at `now` (none in the last hour); when it is, records `now`.
/// Call it with the lock held.
pub fn due(cache: &Path, now: i64) -> Result<bool> {
    let stamp = cache.join("update-checked");
    let last = fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    // A time in the future (a clock set back) does not stop checks for good.
    if let Some(last) = last
        && (0..AUTO_EVERY).contains(&(now - last))
    {
        return Ok(false);
    }
    let tmp = cache.join("update-checked.tmp");
    fs::write(&tmp, format!("{now}\n"))
        .and_then(|()| fs::rename(&tmp, &stamp))
        .with_context(|| format!("writing {}", stamp.display()))?;
    Ok(true)
}

/// Records `adapter` in the config file, if there is one. `Ok(false)` when there is none (the
/// server and token come from the environment): it is updated this time only.
fn register(env: &impl Fn(&str) -> Option<String>, kind: Kind, dir: &Path) -> Result<bool> {
    let file = config::path(env)?;
    let Some(mut saved) = config::saved(&file)? else {
        return Ok(false);
    };
    let entry = Adapter {
        kind: kind.name().into(),
        path: dir.to_path_buf(),
    };
    if saved.adapters.contains(&entry) {
        return Ok(true);
    }
    saved.adapters.retain(|a| a.path != dir);
    saved.adapters.push(entry);
    config::save(&file, &saved)?;
    Ok(true)
}

/// `relevo update` as asked, with the configuration of `env`, at `now`. With `auto`, every reason
/// not to check (turned off, checked within the hour, another update running, not configured)
/// gives no steps and no error.
pub async fn command(
    req: &Request,
    setup: &Setup,
    env: impl Fn(&str) -> Option<String>,
    now: i64,
) -> Result<Vec<Step>> {
    if req.auto && auto_disabled(&env) {
        return Ok(vec![]);
    }
    let mut config = match config::load(&env) {
        Ok(config) => config,
        Err(_) if req.auto => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut steps = vec![];
    let adapter = match &req.adapter {
        Some((kind, dir)) => {
            // The folder itself, so that a link to it is never replaced by a copy.
            let dir = fs::canonicalize(dir)
                .with_context(|| format!("finding {}: not registered", dir.display()))?;
            // Never register, or replace, anything but that adapter, nor a folder git keeps.
            if in_git_checkout(&dir) {
                bail!("{} is {IN_GIT}: not registered", dir.display());
            }
            kind.version_in(&dir).context("not registered")?;
            Some((*kind, dir))
        }
        None => None,
    };
    // The lock covers the config file and the check; `--check` changes nothing.
    let _lock = if req.check {
        None
    } else {
        let cache = cache_dir(&env).ok_or_else(|| anyhow!("HOME is not set"))?;
        match lock(&cache)? {
            Some(lock) => Some((lock, cache)),
            None if req.auto => return Ok(vec![]),
            None => bail!("another relevo update is running on this machine; try again later"),
        }
    };
    if let Some((kind, dir)) = adapter {
        if !req.check && !register(&env, kind, &dir)? {
            steps.push(Step {
                what: format!("{} ({})", kind.label(), dir.display()),
                outcome: Outcome::Kept(
                    "not registered, as there is no config file (`relevo connect` saves one): \
                     updated this time only"
                        .into(),
                ),
            });
        }
        let entry = Adapter {
            kind: kind.name().into(),
            path: dir.clone(),
        };
        config.adapters.retain(|a| a.path != dir);
        config.adapters.insert(0, entry);
    }
    if let Some((_, cache)) = &_lock
        && req.auto
        && !due(cache, now)?
    {
        return Ok(vec![]);
    }
    steps.extend(run(&config, setup, req.check).await?);
    if req.auto {
        // Quiet unless something was installed or failed.
        steps.retain(|s| s.installed() || s.failed());
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_key_is_compiled_in() {
        let key = PublicKey::release();
        assert_eq!(key, PublicKey::from_pem(RELEASE_KEY).unwrap());
        assert!(!key.verify(b"{}", &[0u8; 64]));
        assert!(
            PublicKey::from_pem("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----")
                .is_err()
        );
    }

    #[test]
    fn versions_compare_as_releases() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.4.0") > v("0.3.0"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.4.0") > v("0.4.0-rc.1"));
        assert!(v("0.4.0-rc.2") > v("0.4.0-rc.1"));
        // Numbers compare as numbers: rc.10 is after rc.9, so rc.9 is never an upgrade over it.
        assert!(v("0.4.0-rc.10") > v("0.4.0-rc.9"));
        assert!(v("0.5.0-rc.2") < v("0.5.0-rc.10"));
        assert!(v("0.4.0-rc.1") < v("0.4.0-rc.1.1"));
        assert!(v("0.4.0-1") < v("0.4.0-alpha"));
        assert!(v("0.4.0-alpha") < v("0.4.0-beta"));
        assert_eq!(v("0.4.0"), v(" 0.4.0+build.5 "));
        for bad in [
            "",
            "0.4",
            "0.4.0.1",
            "x.y.z",
            "0.4.0-",
            "v0.4.0",
            "0.4.0-rc..1",
            "0.4.0-rc.01",
            "0.4.0-rc_1",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_target_is_named_as_the_release_names_it() {
        let t = own_target();
        assert!(t.starts_with(std::env::consts::ARCH), "{t}");
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        assert!(t.ends_with("-unknown-linux-gnu"), "{t}");
    }

    #[test]
    fn auto_update_is_off_only_when_asked() {
        let env = |v: &'static str| {
            move |name: &str| (name == "RELEVO_AUTO_UPDATE").then(|| v.to_string())
        };
        for off in ["0", "false", "no", "OFF"] {
            assert!(auto_disabled(&env(off)), "{off}");
        }
        for on in ["1", "", "yes"] {
            assert!(!auto_disabled(&env(on)), "{on}");
        }
        assert!(!auto_disabled(&|_: &str| None));
    }

    #[test]
    fn the_cache_follows_xdg_then_home() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            cache_dir(&env(&[("HOME", "/home/x"), ("XDG_CACHE_HOME", "/c")])),
            Some(PathBuf::from("/c/relevo"))
        );
        // A relative XDG_CACHE_HOME is ignored, as the spec says.
        assert_eq!(
            cache_dir(&env(&[("HOME", "/home/x"), ("XDG_CACHE_HOME", "rel")])),
            Some(PathBuf::from("/home/x/.cache/relevo"))
        );
        assert_eq!(cache_dir(&env(&[])), None);
    }
}
