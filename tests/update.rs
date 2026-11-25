//! `relevo update` (docs/update.md), end to end: a server handing out a dist signed with a
//! throwaway key, and a copy of "the binary" and of the adapters in a temporary directory. The
//! real release key is only used to check that the real binary refuses everything else.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use relevo::config::{Adapter, Config};
use relevo::update::{self, Kind, Outcome, PublicKey, Request, Setup, Step};
use relevo::{App, Clock, api, db, serve};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A temporary directory, removed at the end.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Temp {
        let dir = std::env::temp_dir().join(db::random_id("relevo-update-"));
        std::fs::create_dir(&dir).unwrap();
        Temp(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        // Directories a test made read-only first.
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&self.0)
            .status();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key() -> Ed25519KeyPair {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

fn public(key: &Ed25519KeyPair) -> PublicKey {
    PublicKey::from_bytes(key.public_key().as_ref().try_into().unwrap())
}

/// "A relevo": a script that says its version as `relevo --version` does.
fn fake_binary(version: &str) -> String {
    format!("#!/bin/sh\necho 'relevo {version}'\n")
}

fn write_exe(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The folder of an adapter at `version`, as its release has it.
fn adapter_dir(dir: &Path, kind: Kind, version: &str) {
    std::fs::create_dir_all(dir).unwrap();
    match kind {
        Kind::Pi => {
            let package = json!({ "name": "relevo-pi", "version": version, "type": "module" });
            std::fs::write(dir.join("package.json"), package.to_string()).unwrap();
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/index.ts"), format!("// pi {version}\n")).unwrap();
        }
        Kind::Hermes => {
            std::fs::write(
                dir.join("plugin.yaml"),
                format!("name: relevo\nlabel: Relevo\nversion: {version}\n"),
            )
            .unwrap();
            std::fs::write(dir.join("__init__.py"), format!("# hermes {version}\n")).unwrap();
        }
    }
}

fn tar_gz(src: &Path, out: &Path) {
    let ok = std::process::Command::new("tar")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(src)
        .arg(".")
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes a dist of `version` into `dir`: a binary for `TARGET`, both adapters, the manifest and
/// its signature by `key`.
fn make_dist(dir: &Path, version: &str, key: &Ed25519KeyPair) {
    std::fs::create_dir_all(dir).unwrap();
    let work = dir.join("../work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::write(dir.join(format!("relevo-{TARGET}")), fake_binary(version)).unwrap();
    for kind in [Kind::Pi, Kind::Hermes] {
        let src = work.join(kind.name());
        adapter_dir(&src, kind, version);
        tar_gz(
            &src,
            &dir.join(format!("relevo-{}-{version}.tar.gz", kind.name())),
        );
    }
    std::fs::remove_dir_all(&work).unwrap();
    let mut files = vec![];
    for (name, kind) in [
        (format!("relevo-{TARGET}"), "binary"),
        (format!("relevo-pi-{version}.tar.gz"), "pi"),
        (format!("relevo-hermes-{version}.tar.gz"), "hermes"),
    ] {
        let bytes = std::fs::read(dir.join(&name)).unwrap();
        let mut file =
            json!({ "name": name, "kind": kind, "size": bytes.len(), "sha256": sha256(&bytes) });
        if kind == "binary" {
            file["target"] = TARGET.into();
        }
        files.push(file);
    }
    let manifest =
        serde_json::to_vec_pretty(&json!({ "version": version, "files": files })).unwrap();
    std::fs::write(dir.join("manifest.json"), &manifest).unwrap();
    sign(dir, key);
}

fn sign(dir: &Path, key: &Ed25519KeyPair) {
    let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
    std::fs::write(dir.join("manifest.sig"), key.sign(&manifest).as_ref()).unwrap();
}

/// A server for `ana` handing out the dist in `dist`, on a random port of loopback.
async fn server(dist: &Path) -> (String, String) {
    let app = App::with_dist(
        db::open(":memory:").unwrap(),
        Clock::system(),
        5,
        Some(dist.to_path_buf()),
        relevo::crypto::Key::random(),
    );
    let token = db::invite(&app.db(), "ana", "Ana", None, "laptop", db::now_unix())
        .unwrap()
        .secret;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve::serve(
        listener,
        api::router(app),
        None,
        serve::Limits::default(),
    ));
    (url, token)
}

/// A machine with relevo 0.3.0 in `bin/` and both adapters at 0.3.0, and a server handing out
/// `dist/` (empty until a test makes it).
struct Machine {
    temp: Temp,
    key: Ed25519KeyPair,
    setup: Setup,
    config: Config,
}

impl Machine {
    async fn new() -> Machine {
        let temp = Temp::new();
        let root = temp.0.clone();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        write_exe(&root.join("bin/relevo"), &fake_binary("0.3.0"));
        adapter_dir(&root.join("pi/relevo"), Kind::Pi, "0.3.0");
        // Something only the old folder has: gone after the swap.
        std::fs::write(root.join("pi/relevo/old-only.txt"), "old").unwrap();
        adapter_dir(&root.join("hermes/relevo"), Kind::Hermes, "0.3.0");
        let key = key();
        let (url, token) = server(&root.join("dist")).await;
        let setup = Setup {
            key: public(&key),
            version: "0.3.0".into(),
            target: TARGET.into(),
            exe: root.join("bin/relevo"),
        };
        let config = Config {
            url,
            token,
            cert: None,
            adapters: vec![
                Adapter {
                    kind: "pi".into(),
                    path: root.join("pi/relevo"),
                },
                Adapter {
                    kind: "hermes".into(),
                    path: root.join("hermes/relevo"),
                },
            ],
        };
        Machine {
            temp,
            key,
            setup,
            config,
        }
    }

    fn root(&self) -> &Path {
        &self.temp.0
    }

    fn dist(&self) -> PathBuf {
        self.root().join("dist")
    }

    async fn update(&self, check: bool) -> anyhow::Result<Vec<Step>> {
        update::run(&self.config, &self.setup, check).await
    }

    /// The versions installed: the binary's, pi's and Hermes'.
    fn versions(&self) -> (String, String, String) {
        let out = std::process::Command::new(&self.setup.exe)
            .arg("--version")
            .output()
            .unwrap();
        (
            String::from_utf8(out.stdout).unwrap().trim().to_string(),
            Kind::Pi.version_in(&self.root().join("pi/relevo")).unwrap(),
            Kind::Hermes
                .version_in(&self.root().join("hermes/relevo"))
                .unwrap(),
        )
    }

    fn untouched(&self) {
        assert_eq!(
            self.versions(),
            ("relevo 0.3.0".into(), "0.3.0".into(), "0.3.0".into())
        );
        assert!(self.root().join("pi/relevo/old-only.txt").exists());
        self.no_leftovers();
    }

    /// Nothing half-done next to what is installed.
    fn no_leftovers(&self) {
        for dir in ["bin", "pi", "hermes"] {
            let names: Vec<String> = std::fs::read_dir(self.root().join(dir))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
            assert!(
                names.iter().all(|n| !n.starts_with('.')),
                "{dir}: {names:?}"
            );
        }
    }
}

fn outcomes(steps: &[Step]) -> Vec<Outcome> {
    steps.iter().map(|s| s.outcome.clone()).collect()
}

#[tokio::test]
async fn a_newer_dist_replaces_the_binary_and_swaps_the_adapters() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);

    // --check says what it would do, and does nothing.
    let steps = m.update(true).await.unwrap();
    let would = Outcome::WouldInstall {
        from: "0.3.0".into(),
        to: "0.4.0".into(),
    };
    assert_eq!(outcomes(&steps), vec![would.clone(), would.clone(), would]);
    m.untouched();

    let mode_before = std::fs::metadata(&m.setup.exe)
        .unwrap()
        .permissions()
        .mode();
    let steps = m.update(false).await.unwrap();
    assert!(steps.iter().all(Step::installed), "{steps:?}");
    assert_eq!(steps.len(), 3);
    let said: Vec<String> = steps.iter().map(ToString::to_string).collect();
    assert!(
        said[0].contains("installed 0.4.0 over 0.3.0; it takes effect at the next start"),
        "{said:?}"
    );
    assert!(
        said[1].contains("the pi extension") && said[1].contains("next start of pi"),
        "{said:?}"
    );
    assert!(
        said[2].contains("the Hermes plugin") && said[2].contains("restart the Hermes gateway"),
        "{said:?}"
    );
    assert_eq!(
        m.versions(),
        ("relevo 0.4.0".into(), "0.4.0".into(), "0.4.0".into())
    );
    assert_eq!(
        std::fs::read_to_string(&m.setup.exe).unwrap(),
        fake_binary("0.4.0")
    );
    assert_eq!(
        std::fs::metadata(&m.setup.exe)
            .unwrap()
            .permissions()
            .mode(),
        mode_before
    );
    // The folder was swapped whole: what only the old one had is gone.
    assert!(!m.root().join("pi/relevo/old-only.txt").exists());
    assert_eq!(
        std::fs::read_to_string(m.root().join("pi/relevo/src/index.ts")).unwrap(),
        "// pi 0.4.0\n"
    );
    m.no_leftovers();

    // Once more, as the new binary: everything is up to date.
    let setup = Setup {
        version: "0.4.0".into(),
        ..m.setup.clone()
    };
    let steps = update::run(&m.config, &setup, false).await.unwrap();
    assert_eq!(outcomes(&steps), vec![Outcome::UpToDate("0.4.0".into()); 3]);
}

#[tokio::test]
async fn the_same_version_is_up_to_date_and_changes_nothing() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.3.0", &m.key);
    let steps = m.update(false).await.unwrap();
    assert_eq!(outcomes(&steps), vec![Outcome::UpToDate("0.3.0".into()); 3]);
    m.untouched();
}

#[tokio::test]
async fn an_older_dist_is_refused() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.2.0", &m.key);
    let steps = m.update(false).await.unwrap();
    assert_eq!(steps.len(), 3);
    for step in &steps {
        let said = step.to_string();
        assert!(
            said.contains("0.2.0, older than 0.3.0") && said.contains("no downgrades"),
            "{said}"
        );
        assert!(!step.installed() && !step.failed());
    }
    m.untouched();

    // Only what is older than the dist is updated: a newer adapter is left alone.
    make_dist(&m.dist(), "0.4.0", &m.key);
    adapter_dir(&m.root().join("hermes/relevo"), Kind::Hermes, "0.5.0");
    let steps = m.update(false).await.unwrap();
    assert!(steps[0].installed() && steps[1].installed(), "{steps:?}");
    assert!(
        steps[2].to_string().contains("older than 0.5.0"),
        "{}",
        steps[2]
    );
}

#[tokio::test]
async fn a_bad_or_missing_signature_installs_nothing() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);

    // Signed by another key.
    sign(&m.dist(), &key());
    let err = m.update(false).await.unwrap_err().to_string();
    assert!(err.contains("not signed by the release key"), "{err}");
    m.untouched();

    // The manifest changed after it was signed.
    sign(&m.dist(), &m.key);
    let manifest = std::fs::read_to_string(m.dist().join("manifest.json")).unwrap();
    std::fs::write(
        m.dist().join("manifest.json"),
        manifest.replace("0.4.0", "0.4.1"),
    )
    .unwrap();
    let err = m.update(false).await.unwrap_err().to_string();
    assert!(err.contains("not signed by the release key"), "{err}");
    m.untouched();

    // No signature at all, or one of the wrong size.
    std::fs::write(m.dist().join("manifest.json"), manifest).unwrap();
    std::fs::remove_file(m.dist().join("manifest.sig")).unwrap();
    let err = m.update(false).await.unwrap_err().to_string();
    assert!(err.contains("not signed (no manifest.sig)"), "{err}");
    std::fs::write(m.dist().join("manifest.sig"), [0u8; 65]).unwrap();
    assert!(m.update(false).await.is_err());
    m.untouched();

    // --check checks the signature too.
    sign(&m.dist(), &key());
    assert!(m.update(true).await.is_err());
}

#[tokio::test]
async fn a_tampered_file_installs_nothing() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    // The same size, other bytes: the binary would still be fine, the pi archive is not.
    let pi = m.dist().join("relevo-pi-0.4.0.tar.gz");
    let mut bytes = std::fs::read(&pi).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&pi, &bytes).unwrap();
    let err = format!("{:#}", m.update(false).await.unwrap_err());
    assert!(err.contains("does not match the signed manifest"), "{err}");
    assert!(err.contains("nothing was installed"), "{err}");
    m.untouched();

    // A binary of another size.
    make_dist(&m.dist(), "0.4.0", &m.key);
    let bin = m.dist().join(format!("relevo-{TARGET}"));
    std::fs::write(&bin, fake_binary("0.4.0") + "# more\n").unwrap();
    let err = format!("{:#}", m.update(false).await.unwrap_err());
    assert!(
        err.contains("bigger than the 30 bytes of the signed manifest"),
        "{err}"
    );
    // Or a shorter one.
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    let err = format!("{:#}", m.update(false).await.unwrap_err());
    assert!(
        err.contains("is 10 bytes, not the 30 of the signed manifest"),
        "{err}"
    );
    m.untouched();
}

#[tokio::test]
async fn a_binary_that_does_not_run_here_is_never_put_in_place() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    // Signed and matching, but it says another version (a broken build, another machine's).
    let bin = m.dist().join(format!("relevo-{TARGET}"));
    std::fs::write(&bin, fake_binary("9.9.9")).unwrap();
    let manifest = std::fs::read_to_string(m.dist().join("manifest.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let bytes = std::fs::read(&bin).unwrap();
    v["files"][0]["size"] = bytes.len().into();
    v["files"][0]["sha256"] = sha256(&bytes).into();
    std::fs::write(m.dist().join("manifest.json"), v.to_string()).unwrap();
    sign(&m.dist(), &m.key);
    let steps = m.update(false).await.unwrap();
    assert!(steps[0].failed(), "{steps:?}");
    assert!(
        steps[0].to_string().contains("does not run here"),
        "{}",
        steps[0]
    );
    assert_eq!(m.versions().0, "relevo 0.3.0");
    // The adapters are independent of it.
    assert!(steps[1].installed() && steps[2].installed(), "{steps:?}");
    m.no_leftovers();
}

#[tokio::test]
async fn where_it_cannot_write_it_says_so_and_installs_nothing() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root writes anywhere");
        return;
    }
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    let bin = m.root().join("bin");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
    let err = format!("{:#}", m.update(false).await.unwrap_err());
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        err.contains(&format!("cannot write in {}", bin.display())),
        "{err}"
    );
    assert!(err.contains("nothing was installed"), "{err}");
    m.untouched();
}

#[tokio::test]
async fn only_an_adapters_own_folder_is_ever_replaced() {
    let mut m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    let other = m.root().join("not-pi");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(
        other.join("package.json"),
        r#"{"name":"something-else","version":"0.1.0"}"#,
    )
    .unwrap();
    m.config.adapters = vec![Adapter {
        kind: "pi".into(),
        path: other.clone(),
    }];
    // Left as it is, and the rest still updated: one broken folder does not hold back the binary.
    let steps = m.update(false).await.unwrap();
    assert!(steps[0].installed(), "{steps:?}");
    assert!(
        matches!(&steps[1].outcome, Outcome::Kept(why) if why.contains("is not the pi extension")),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("package.json")).unwrap(),
        r#"{"name":"something-else","version":"0.1.0"}"#
    );
    assert_eq!(m.versions().0, "relevo 0.4.0");

    // A folder that went away is skipped, not an error.
    m.config.adapters = vec![Adapter {
        kind: "hermes".into(),
        path: m.root().join("gone"),
    }];
    let mut setup = m.setup.clone();
    setup.version = "0.4.0".into();
    make_dist(&m.dist(), "0.5.0", &m.key);
    let steps = update::run(&m.config, &setup, false).await.unwrap();
    assert!(steps[0].installed());
    assert_eq!(steps[1].outcome, Outcome::Kept("no longer there".into()));
}

/// A clone of the repository with the pi extension in it, at 0.3.0, and something not committed.
fn clone_with_pi(root: &Path) -> PathBuf {
    let clone = root.join("src/relevo");
    std::fs::create_dir_all(clone.join(".git")).unwrap();
    let pi = clone.join("adapters/pi");
    adapter_dir(&pi, Kind::Pi, "0.3.0");
    std::fs::write(pi.join("uncommitted.ts"), "work in progress").unwrap();
    pi
}

#[tokio::test]
async fn a_folder_in_a_git_checkout_is_never_replaced() {
    let mut m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    let pi = clone_with_pi(m.root());
    // Registered before, or by hand: left as it is, the rest updated.
    m.config.adapters.push(Adapter {
        kind: "pi".into(),
        path: pi.clone(),
    });
    let steps = m.update(false).await.unwrap();
    assert!(steps[..3].iter().all(Step::installed), "{steps:?}");
    assert_eq!(
        steps[3].outcome,
        Outcome::Kept("in a git checkout: update it with git".into())
    );
    assert_eq!(Kind::Pi.version_in(&pi).unwrap(), "0.3.0");
    assert!(pi.join("uncommitted.ts").exists());

    // A `.git` file (a worktree) counts as well, and so does one in the folder itself.
    let worktree = m.root().join("worktree");
    adapter_dir(&worktree, Kind::Hermes, "0.3.0");
    std::fs::write(worktree.join(".git"), "gitdir: /elsewhere\n").unwrap();
    assert!(update::in_git_checkout(&worktree));
    assert!(!update::in_git_checkout(&m.root().join("hermes/relevo")));

    // `relevo update --adapter` refuses to register it, and replaces nothing.
    let file = saved_config(&m);
    let env = env_of(vec![
        ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
        ("HOME", m.root().to_str().unwrap().to_string()),
    ]);
    let req = Request {
        adapter: Some((Kind::Pi, pi.clone())),
        ..Request::default()
    };
    let err = format!(
        "{:#}",
        update::command(&req, &m.setup, env, 0).await.unwrap_err()
    );
    assert!(
        err.contains("in a git checkout") && err.contains("not registered"),
        "{err}"
    );
    let saved = relevo::config::saved(&file).unwrap().unwrap();
    assert!(saved.adapters.is_empty(), "{:?}", saved.adapters);
    assert_eq!(Kind::Pi.version_in(&pi).unwrap(), "0.3.0");
    assert!(pi.join("uncommitted.ts").exists());
}

#[tokio::test]
async fn a_link_to_a_folder_stays_a_link() {
    let mut m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    // Hermes reaches the plugin through a link (~/.hermes/plugins/relevo -> a folder elsewhere).
    let plugins = m.root().join("plugins");
    std::fs::create_dir(&plugins).unwrap();
    let link = plugins.join("relevo");
    std::os::unix::fs::symlink(m.root().join("hermes/relevo"), &link).unwrap();
    m.config.adapters = vec![Adapter {
        kind: "hermes".into(),
        path: link.clone(),
    }];
    let steps = m.update(false).await.unwrap();
    assert!(steps.iter().all(Step::installed), "{steps:?}");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(Kind::Hermes.version_in(&link).unwrap(), "0.4.0");
    m.no_leftovers();
    let names: Vec<_> = std::fs::read_dir(&plugins).unwrap().collect();
    assert_eq!(names.len(), 1, "{names:?}");

    // A link into a clone is the clone's: left as it is.
    let pi = clone_with_pi(m.root());
    let pi_link = plugins.join("pi");
    std::os::unix::fs::symlink(&pi, &pi_link).unwrap();
    m.config.adapters = vec![Adapter {
        kind: "pi".into(),
        path: pi_link.clone(),
    }];
    let steps = m.update(true).await.unwrap();
    assert_eq!(
        steps[1].outcome,
        Outcome::Kept("in a git checkout: update it with git".into())
    );

    // Registered by hand through a link, it is the folder that is saved.
    make_dist(&m.dist(), "0.5.0", &m.key);
    let file = saved_config(&m);
    let env = env_of(vec![
        ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
        ("HOME", m.root().to_str().unwrap().to_string()),
    ]);
    let req = Request {
        adapter: Some((Kind::Hermes, link.clone())),
        ..Request::default()
    };
    let steps = update::command(&req, &m.setup, env, 0).await.unwrap();
    assert!(steps.iter().all(Step::installed), "{steps:?}");
    let saved = relevo::config::saved(&file).unwrap().unwrap();
    assert_eq!(
        saved.adapters,
        vec![Adapter {
            kind: "hermes".into(),
            path: std::fs::canonicalize(m.root().join("hermes/relevo")).unwrap(),
        }]
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(Kind::Hermes.version_in(&link).unwrap(), "0.5.0");
}

#[tokio::test]
async fn a_server_without_a_dist_hands_out_nothing() {
    let m = Machine::new().await;
    let steps = m.update(false).await.unwrap();
    assert_eq!(steps.len(), 1);
    assert!(
        steps[0].to_string().contains("hands out no clients"),
        "{}",
        steps[0]
    );
    m.untouched();
}

// ---------- the command: registration, the hourly throttle, the opt-out ----------

/// An environment for `update::command`: a config file, a cache, and nothing of the real ones.
fn env_of(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |name| {
        vars.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    }
}

fn saved_config(m: &Machine) -> PathBuf {
    let file = m.root().join("config.json");
    let config = Config {
        adapters: vec![],
        ..m.config.clone()
    };
    relevo::config::save(&file, &config).unwrap();
    file
}

#[tokio::test]
async fn an_adapter_registers_itself_and_is_updated_from_then_on() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    let file = saved_config(&m);
    let env = || {
        env_of(vec![
            ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
            ("HOME", m.root().to_str().unwrap().to_string()),
        ])
    };
    let pi = m.root().join("pi/relevo");
    let req = |check| Request {
        check,
        auto: false,
        adapter: Some((Kind::Pi, pi.clone())),
    };
    // --check registers nothing.
    let steps = update::command(&req(true), &m.setup, env(), 0)
        .await
        .unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(
        relevo::config::saved(&file)
            .unwrap()
            .unwrap()
            .adapters
            .is_empty()
    );

    let steps = update::command(&req(false), &m.setup, env(), 0)
        .await
        .unwrap();
    assert!(steps.iter().all(Step::installed), "{steps:?}");
    let saved = relevo::config::saved(&file).unwrap().unwrap();
    assert_eq!(
        saved.adapters,
        vec![Adapter {
            kind: "pi".into(),
            // macOS resolves /var → /private/var; canonicalize both sides.
            path: pi.canonicalize().unwrap_or_else(|_| pi.clone())
        }]
    );
    // The token and the rest of the file stay as they were.
    assert_eq!(saved.token, m.config.token);

    // Not an adapter: refused, and not registered.
    let req = Request {
        adapter: Some((Kind::Hermes, m.root().join("bin"))),
        ..Request::default()
    };
    let err = update::command(&req, &m.setup, env(), 0).await.unwrap_err();
    assert!(format!("{err:#}").contains("not registered"), "{err:#}");
    assert_eq!(
        relevo::config::saved(&file)
            .unwrap()
            .unwrap()
            .adapters
            .len(),
        1
    );

    // A later plain `relevo update` (or the one `relevo mcp` runs) updates it too.
    make_dist(&m.dist(), "0.5.0", &m.key);
    let mut setup = m.setup.clone();
    setup.version = "0.5.0".into();
    let steps = update::command(&Request::default(), &setup, env(), 0)
        .await
        .unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(steps[1].installed(), "{steps:?}");
    assert_eq!(Kind::Pi.version_in(&pi).unwrap(), "0.5.0");
}

#[tokio::test]
async fn auto_checks_at_most_once_an_hour_and_says_only_what_it_did() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.3.0", &m.key);
    let file = saved_config(&m);
    let cache = m.root().join("cache");
    let env = || {
        env_of(vec![
            ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
            ("HOME", m.root().to_str().unwrap().to_string()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap().to_string()),
        ])
    };
    let auto = Request {
        auto: true,
        ..Request::default()
    };
    let t0 = 1_800_000_000;
    // Up to date: nothing to say.
    let steps = update::command(&auto, &m.setup, env(), t0).await.unwrap();
    assert!(steps.is_empty(), "{steps:?}");
    assert_eq!(
        std::fs::read_to_string(cache.join("relevo/update-checked")).unwrap(),
        format!("{t0}\n")
    );
    // Within the hour, no check at all: a newer dist waits.
    make_dist(&m.dist(), "0.4.0", &m.key);
    let steps = update::command(&auto, &m.setup, env(), t0 + 3599)
        .await
        .unwrap();
    assert!(steps.is_empty(), "{steps:?}");
    m.untouched();
    // An hour later it checks, and says what it installed.
    let steps = update::command(&auto, &m.setup, env(), t0 + 3600)
        .await
        .unwrap();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].installed());
    assert_eq!(m.versions().0, "relevo 0.4.0");

    // A clock set back does not stop checks for good.
    assert!(update::due(&cache.join("relevo"), t0 - 10_000).unwrap());
    assert!(!update::due(&cache.join("relevo"), t0 - 10_000 + 60).unwrap());
}

#[tokio::test]
async fn auto_is_off_with_relevo_auto_update_0_and_quiet_without_a_config() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.4.0", &m.key);
    let file = saved_config(&m);
    let cache = m.root().join("cache");
    let vars = |off: bool| {
        let mut vars = vec![
            ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
            ("HOME", m.root().to_str().unwrap().to_string()),
            ("XDG_CACHE_HOME", cache.to_str().unwrap().to_string()),
        ];
        if off {
            vars.push(("RELEVO_AUTO_UPDATE", "0".to_string()));
        }
        env_of(vars)
    };
    let auto = Request {
        auto: true,
        ..Request::default()
    };
    let steps = update::command(&auto, &m.setup, vars(true), 0)
        .await
        .unwrap();
    assert!(steps.is_empty());
    m.untouched();
    assert!(!cache.exists(), "it looked at the cache");
    // By hand, it still works.
    let steps = update::command(&Request::default(), &m.setup, vars(true), 0)
        .await
        .unwrap();
    assert!(steps[0].installed(), "{steps:?}");

    // No config: nothing to update from, and nothing to say.
    let env = env_of(vec![
        (
            "RELEVO_CONFIG",
            m.root().join("none.json").to_str().unwrap().to_string(),
        ),
        ("HOME", m.root().to_str().unwrap().to_string()),
    ]);
    assert!(
        update::command(&auto, &m.setup, &env, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        update::command(&Request::default(), &m.setup, &env, 0)
            .await
            .is_err()
    );
}

#[test]
fn one_update_at_a_time_per_machine() {
    let temp = Temp::new();
    let first = update::lock(&temp.0).unwrap();
    assert!(first.is_some());
    assert!(update::lock(&temp.0).unwrap().is_none());
    drop(first);
    // A child that another test forks at that moment holds it until it execs: a moment.
    let again = (0..100).find_map(|_| {
        let lock = update::lock(&temp.0).unwrap();
        if lock.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        lock
    });
    assert!(again.is_some());
}

// ---------- the release workflow's script, and the real binary ----------

fn have(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// deploy/make-dist.sh with a throwaway key made by openssl: what it signs, a client checks.
/// macOS ships bash 3.2 and BSD find/stat, but make-dist.sh needs GNU tools: skip there.
#[tokio::test]
#[cfg_attr(target_os = "macos", ignore)]
async fn the_dist_the_release_makes_is_what_clients_install() {
    if !have("openssl") {
        eprintln!("skipped: no openssl");
        return;
    }
    let m = Machine::new().await;
    let keys = m.root().join("keys");
    std::fs::create_dir(&keys).unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(args[0])
            .args(&args[1..])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    let k = keys.join("key.pem");
    let p = keys.join("pub.pem");
    run(&[
        "openssl",
        "genpkey",
        "-algorithm",
        "ed25519",
        "-out",
        k.to_str().unwrap(),
    ]);
    run(&[
        "openssl",
        "pkey",
        "-in",
        k.to_str().unwrap(),
        "-pubout",
        "-out",
        p.to_str().unwrap(),
    ]);

    // The binaries of a release, as the build jobs leave them.
    let out = m.root().join("out");
    std::fs::create_dir(&out).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    write_exe(&out.join(format!("relevo-{TARGET}")), &fake_binary(version));
    write_exe(
        &out.join("relevo-aarch64-unknown-linux-gnu"),
        &fake_binary(version),
    );
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/make-dist.sh");
    let made = run(&[
        "bash",
        script,
        "--key",
        k.to_str().unwrap(),
        "--pub",
        p.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    let said = String::from_utf8_lossy(&made.stdout);
    assert!(
        said.contains(&format!("relevo-dist-{version}.tar.gz")),
        "{said}"
    );
    // Another public key: the script refuses to leave a dist the clients would refuse.
    let other = keys.join("other.pem");
    let other_pub = keys.join("other-pub.pem");
    run(&[
        "openssl",
        "genpkey",
        "-algorithm",
        "ed25519",
        "-out",
        other.to_str().unwrap(),
    ]);
    run(&[
        "openssl",
        "pkey",
        "-in",
        other.to_str().unwrap(),
        "-pubout",
        "-out",
        other_pub.to_str().unwrap(),
    ]);
    let refused = std::process::Command::new("bash")
        .args([script, "--key", k.to_str().unwrap(), "--pub"])
        .arg(&other_pub)
        .arg(m.root().join("nowhere"))
        .output()
        .unwrap();
    assert!(!refused.status.success());

    // The server's dist is that tarball, unpacked.
    let tarball = out.join(format!("relevo-dist-{version}.tar.gz"));
    std::fs::create_dir(m.dist()).unwrap();
    run(&[
        "tar",
        "-xzf",
        tarball.to_str().unwrap(),
        "-C",
        m.dist().to_str().unwrap(),
    ]);
    let manifest =
        relevo::dist::Manifest::parse(&std::fs::read(m.dist().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest.version, version);
    let kinds: Vec<&str> = manifest.files.iter().map(|f| f.kind.as_str()).collect();
    assert_eq!(kinds, ["binary", "binary", "pi", "hermes"]);

    // A client of an older version, trusting that key, installs it all: the real adapters.
    let setup = Setup {
        key: PublicKey::from_pem(&std::fs::read_to_string(&p).unwrap()).unwrap(),
        version: "0.0.1".into(),
        ..m.setup.clone()
    };
    adapter_dir(&m.root().join("pi/relevo"), Kind::Pi, "0.0.1");
    adapter_dir(&m.root().join("hermes/relevo"), Kind::Hermes, "0.0.1");
    let steps = update::run(&m.config, &setup, false).await.unwrap();
    assert!(steps.iter().all(Step::installed), "{steps:?}");
    assert_eq!(
        m.versions(),
        (format!("relevo {version}"), version.into(), version.into())
    );
    let pi = m.root().join("pi/relevo");
    assert!(pi.join("src/index.ts").exists());
    assert!(m.root().join("hermes/relevo/adapter.py").exists());
    assert!(!m.root().join("hermes/relevo/__pycache__").exists());
}

/// The real binary, in a HOME of its own.
fn relevo(home: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_relevo"));
    cmd.env("HOME", home)
        .env("RELEVO_CONFIG", home.join("config.json"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("RELEVO_URL")
        .env_remove("RELEVO_TOKEN")
        .env_remove("RELEVO_CERT")
        .env_remove("RELEVO_AUTO_UPDATE")
        .env("RELEVO_CONTEXT", "")
        .kill_on_drop(true);
    cmd
}

#[tokio::test]
async fn the_binary_trusts_the_release_key_alone() {
    let m = Machine::new().await;
    // Signed by a throwaway key, and newer: the real binary must refuse it.
    make_dist(&m.dist(), "99.0.0", &m.key);
    saved_config(&m);
    let out = relevo(m.root()).arg("update").output().await.unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not signed by the release key"), "{err}");
    let out = relevo(m.root())
        .args(["update", "--check"])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());

    // --auto says so too (a failure), once; then it waits an hour.
    let out = relevo(m.root())
        .args(["update", "--auto"])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not signed by the release key"));
    assert!(m.root().join("cache/relevo/update-checked").exists());
    let out = relevo(m.root())
        .args(["update", "--auto"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

#[tokio::test]
async fn relevo_auto_update_0_turns_auto_off_in_the_binary() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "99.0.0", &m.key);
    saved_config(&m);
    let out = relevo(m.root())
        .args(["update", "--auto"])
        .env("RELEVO_AUTO_UPDATE", "0")
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(!m.root().join("cache").exists());
    // Bad arguments.
    for args in [
        &["update", "--adapter", "pi"][..],
        &["update", "--path", "/tmp"],
        &["update", "--adapter", "vim", "--path", "/tmp"],
        &["update", "--check", "--auto"],
    ] {
        let out = relevo(m.root()).args(args).output().await.unwrap();
        assert!(!out.status.success(), "{args:?}");
    }
}

/// `relevo mcp` runs the automatic update in the background: MCP answers on stdout as ever,
/// and what the update says goes to stderr.
#[tokio::test(flavor = "multi_thread")]
async fn relevo_mcp_updates_in_the_background_and_keeps_stdout_for_mcp() {
    let m = Machine::new().await;
    make_dist(&m.dist(), "99.0.0", &m.key);
    saved_config(&m);
    let mut child = relevo(m.root())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": { "name": "test", "version": "0" } } });
    stdin
        .write_all(format!("{init}\n").as_bytes())
        .await
        .unwrap();
    let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["serverInfo"]["name"], "relevo");
    let said = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stderr.next_line().await.unwrap() {
                Some(line) if line.contains("update") => return line,
                Some(_) => continue,
                None => panic!("stderr closed"),
            }
        }
    })
    .await
    .unwrap();
    assert!(said.starts_with("relevo mcp: update: "), "{said}");
    assert!(said.contains("not signed by the release key"), "{said}");
    assert!(m.root().join("cache/relevo/update-checked").exists());
    // Still serving MCP, and nothing else came on stdout.
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    stdin
        .write_all(format!("{note}\n{list}\n").as_bytes())
        .await
        .unwrap();
    let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], 2, "{line}");
    let _ = child.kill().await;
}

/// The /relevo command for Claude Code: refreshed when Relevo wrote it and its copy differs,
/// never when someone else wrote or edited it, and not installed by an update.
#[tokio::test]
async fn the_relevo_command_is_refreshed_only_when_relevo_wrote_it() {
    use relevo::claude_command::{self as command, contents};
    let m = Machine::new().await;
    make_dist(&m.dist(), "0.3.0", &m.key);
    let file = saved_config(&m);
    let claude = m.root().join("claude");
    let env = || {
        env_of(vec![
            ("RELEVO_CONFIG", file.to_str().unwrap().to_string()),
            ("HOME", m.root().to_str().unwrap().to_string()),
            ("CLAUDE_CONFIG_DIR", claude.to_str().unwrap().to_string()),
        ])
    };
    let path = command::path_in(&claude);
    let step = |steps: Vec<Step>| -> Option<Outcome> {
        steps
            .into_iter()
            .find(|s| s.what.starts_with("the /relevo command"))
            .map(|s| s.outcome)
    };
    let update = |check| {
        let req = Request {
            check,
            ..Request::default()
        };
        let env = env();
        let setup = m.setup.clone();
        async move { update::command(&req, &setup, env, 0).await.unwrap() }
    };
    let version = env!("CARGO_PKG_VERSION").to_string();
    // Not installed: an update does not install it.
    assert_eq!(step(update(false).await), None);
    assert!(!path.exists());
    // A newer one of Relevo's: never downgraded.
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents("99.0.0")).unwrap();
    for check in [true, false] {
        assert!(matches!(step(update(check).await), Some(Outcome::Kept(_))));
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents("99.0.0"));
    // An older one of Relevo's: --check says so, the update replaces it.
    std::fs::write(&path, contents("0.4.9")).unwrap();
    assert_eq!(
        step(update(true).await),
        Some(Outcome::WouldInstall {
            from: "0.4.9".into(),
            to: version.clone()
        })
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents("0.4.9"));
    // An update that fails (clients signed by another key) leaves the command as it was: it is
    // refreshed after the binary's step, not before.
    sign(&m.dist(), &key());
    let req = Request::default();
    assert!(update::command(&req, &m.setup, env(), 0).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents("0.4.9"));
    sign(&m.dist(), &m.key);
    let outcome = step(update(false).await).unwrap();
    assert!(
        matches!(&outcome, Outcome::Installed { from, to, .. } if from == "0.4.9" && *to == version),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents(&version));
    assert_eq!(
        step(update(false).await),
        Some(Outcome::UpToDate(version.clone()))
    );
    // Edited, or someone else's: left alone.
    let edited = contents("0.5.9").replace("first person", "my own words");
    std::fs::write(&path, &edited).unwrap();
    assert!(matches!(step(update(false).await), Some(Outcome::Kept(_))));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}
