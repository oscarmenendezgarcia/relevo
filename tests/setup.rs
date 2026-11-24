//! `relevo setup` and `relevo connect --setup` (docs/agents.md): Claude Code, pi and Hermes set
//! up with the server's dist, run as the real binary in a throwaway HOME, against stand-ins for
//! `claude`, `pi` and `hermes` that record how they were run. Never the real ones.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relevo::config::Config;
use relevo::{App, Clock, api, db, serve};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A temporary directory, removed at the end.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn tar_gz(src: &Path, out: &Path) {
    let ok = Command::new("tar")
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

fn key() -> Ed25519KeyPair {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

/// `key`'s public half as a PEM, for `RELEVO_TEST_RELEASE_KEY` (a debug build only).
fn pem(key: &Ed25519KeyPair) -> String {
    use base64::Engine;
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(key.public_key().as_ref());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(der)
    )
}

/// A dist of 0.7.0 with both adapters (no binary), its manifest signed by `key`.
fn make_dist(root: &Path, key: &Ed25519KeyPair) -> PathBuf {
    let dist = root.join("dist");
    let work = root.join("work");
    std::fs::create_dir_all(work.join("pi/src")).unwrap();
    std::fs::create_dir_all(work.join("hermes")).unwrap();
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(
        work.join("pi/package.json"),
        json!({ "name": "relevo-pi", "version": "0.7.0" }).to_string(),
    )
    .unwrap();
    std::fs::write(work.join("pi/src/index.ts"), "// pi\n").unwrap();
    std::fs::write(
        work.join("hermes/plugin.yaml"),
        "name: relevo\nversion: 0.7.0\n",
    )
    .unwrap();
    let mut files = vec![];
    for kind in ["pi", "hermes"] {
        let name = format!("relevo-{kind}-0.7.0.tar.gz");
        tar_gz(&work.join(kind), &dist.join(&name));
        let bytes = std::fs::read(dist.join(&name)).unwrap();
        files.push(json!({ "name": name, "kind": kind, "size": bytes.len(),
                           "sha256": sha256(&bytes) }));
    }
    let manifest = json!({ "version": "0.7.0", "files": files });
    std::fs::write(dist.join("manifest.json"), manifest.to_string()).unwrap();
    let sig = key.sign(manifest.to_string().as_bytes());
    std::fs::write(dist.join("manifest.sig"), sig.as_ref()).unwrap();
    dist
}

/// A stand-in for `program`: it appends how it was run to `log`, and fails when `fail` exists
/// (`claude mcp get` fails unless `has` exists, as for a server it does not have).
fn fake(bin: &Path, program: &str, log: &Path) {
    let script = format!(
        "#!/bin/sh\necho \"{program} $*\" >> '{log}'\n\
         [ -e '{bin}/{program}.fail' ] && {{ echo \"{program} broke\" >&2; exit 3; }}\n\
         [ \"$1 $2\" = 'mcp get' ] && [ ! -e '{bin}/claude.has' ] && exit 1\nexit 0\n",
        log = log.display(),
        bin = bin.display()
    );
    let file = bin.join(program);
    std::fs::write(&file, script).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Machine {
    temp: Temp,
    url: String,
    /// The PEM of the key the dist is signed with, which the binary is told to trust.
    key: String,
}

impl Machine {
    /// A server with a dist and `ana`, and a machine connected to it as `ana`, with stand-ins
    /// for every agent.
    async fn new() -> Machine {
        let temp = Temp(std::env::temp_dir().join(db::random_id("relevo-setup-")));
        std::fs::create_dir_all(temp.0.join("home")).unwrap();
        std::fs::create_dir_all(temp.0.join("bin")).unwrap();
        let key = key();
        let dist = make_dist(&temp.0, &key);
        let app = App::with_dist(
            db::open(":memory:").unwrap(),
            Clock::system(),
            5,
            Some(dist),
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
        let m = Machine {
            temp,
            url,
            key: pem(&key),
        };
        let config = Config {
            url: m.url.clone(),
            token,
            cert: None,
            adapters: vec![],
        };
        relevo::config::save(&m.config(), &config).unwrap();
        for program in ["claude", "pi", "hermes"] {
            fake(&m.bin(), program, &m.log());
        }
        m
    }

    fn home(&self) -> PathBuf {
        self.temp.0.join("home")
    }

    fn bin(&self) -> PathBuf {
        self.temp.0.join("bin")
    }

    fn log(&self) -> PathBuf {
        self.temp.0.join("ran.log")
    }

    fn config(&self) -> PathBuf {
        self.home().join(".config/relevo/config.json")
    }

    fn ran(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// `relevo <args>` here, without a terminal.
    fn relevo(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_relevo"))
            .args(args)
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin().display()))
            .env("HERMES_HOME", self.home().join("hermes-home"))
            .env("RELEVO_AUTO_UPDATE", "0")
            .env("RELEVO_TEST_RELEASE_KEY", &self.key)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
}

fn output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[tokio::test(flavor = "multi_thread")]
// Only a debug build trusts `RELEVO_TEST_RELEASE_KEY`, so a release build (the release workflow
// tests one) installs no adapter from these dists.
#[cfg_attr(
    not(debug_assertions),
    ignore = "a release build trusts only the release key"
)]
async fn named_agents_are_set_up_without_asking() {
    let m = Machine::new().await;
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "claude-code", "pi", "hermes"]));
    let said = output(&out);
    assert!(out.status.success(), "{said}");
    let exe = env!("CARGO_BIN_EXE_relevo");
    let pi = m.home().join(".local/share/relevo/pi");
    let hermes = m.home().join("hermes-home/plugins/relevo");
    assert_eq!(
        m.ran(),
        vec![
            "claude mcp get relevo".to_string(),
            format!("claude mcp add --scope user relevo -- {exe} mcp"),
            format!("pi install {}", pi.display()),
            "hermes plugins enable relevo".into(),
            "hermes config set plugins.entries.relevo.allow_gateway_injection true".into(),
            "hermes config set platforms.relevo.enabled true".into(),
            "hermes config set RELEVO_ALLOW_ALL_USERS true".into(),
            "hermes tools enable relevo --platform relevo".into(),
            "hermes tools enable relevo --platform cli".into(),
        ]
    );
    // Claude Code's command and status line, in this HOME.
    assert!(
        m.home().join(".claude/commands/relevo.md").exists(),
        "{said}"
    );
    let settings = std::fs::read_to_string(m.home().join(".claude/settings.json")).unwrap();
    assert!(settings.contains("statusline"), "{settings}");
    // The adapters, from the dist, registered for updates.
    assert!(
        std::fs::read_to_string(pi.join("package.json"))
            .unwrap()
            .contains("0.7.0")
    );
    assert!(
        std::fs::read_to_string(hermes.join("plugin.yaml"))
            .unwrap()
            .contains("0.7.0")
    );
    let config: Value = serde_json::from_slice(&std::fs::read(m.config()).unwrap()).unwrap();
    let kinds: Vec<&str> = config["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["pi", "hermes"]);
    assert_eq!(
        said,
        "Added Relevo to Claude Code, pi and Hermes.\n\nDone. Restart Claude Code, pi and the \
         Hermes gateway to load Relevo.\n"
    );

    // Again: the MCP server Claude Code has is left alone, the adapters are installed again.
    std::fs::write(m.bin().join("claude.has"), "").unwrap();
    std::fs::remove_file(m.log()).unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "claude-code,pi"]));
    assert!(out.status.success(), "{}", output(&out));
    assert_eq!(
        m.ran(),
        vec![
            "claude mcp get relevo".to_string(),
            format!("pi install {}", pi.display())
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_terminal_nothing_changes_and_it_says_what_to_run() {
    let m = Machine::new().await;
    let out = tokio::task::block_in_place(|| m.relevo(&["setup"]));
    let said = output(&out);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains(&format!(
            "Found Claude Code, pi and Hermes (no terminal to ask). To add Relevo to them, run\n  \
             {} setup claude-code pi hermes",
            env!("CARGO_BIN_EXE_relevo")
        )),
        "{said}"
    );
    assert!(m.ran().is_empty());
    assert!(!m.home().join(".claude").exists());
    assert!(!m.home().join(".local").exists());
}

#[tokio::test(flavor = "multi_thread")]
// Only a debug build trusts `RELEVO_TEST_RELEASE_KEY`, so a release build (the release workflow
// tests one) installs no adapter from these dists.
#[cfg_attr(
    not(debug_assertions),
    ignore = "a release build trusts only the release key"
)]
async fn one_agent_that_fails_leaves_the_others_set_up() {
    let m = Machine::new().await;
    std::fs::write(m.bin().join("hermes.fail"), "").unwrap();
    // A folder there that is not the plugin is never replaced.
    let pi = m.home().join(".local/share/relevo/pi");
    std::fs::create_dir_all(&pi).unwrap();
    std::fs::write(pi.join("mine.txt"), "mine").unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "pi", "hermes", "claude-code"]));
    let said = output(&out);
    // Claude Code was set up, but not every agent named: a script is told with exit 1.
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(said.contains("Could not add Relevo to pi:"), "{said}");
    assert!(said.contains("left as it is"), "{said}");
    assert_eq!(
        std::fs::read_to_string(pi.join("mine.txt")).unwrap(),
        "mine"
    );
    assert!(
        said.contains(
            "Could not add Relevo to Hermes: `hermes plugins enable relevo` failed: hermes broke"
        ),
        "{said}"
    );
    assert!(
        said.contains("Done. Restart Claude Code to load Relevo."),
        "{said}"
    );
    assert!(!said.contains("Restart pi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_adapter_is_installed_only_from_a_signed_manifest() {
    let m = Machine::new().await;
    let dist = m.temp.0.join("dist");
    let pi = m.home().join(".local/share/relevo/pi");

    // Signed by another key: the server may have been changed. Nothing is installed.
    let other = key().sign(&std::fs::read(dist.join("manifest.json")).unwrap());
    let good = std::fs::read(dist.join("manifest.sig")).unwrap();
    std::fs::write(dist.join("manifest.sig"), other.as_ref()).unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "pi"]));
    let said = output(&out);
    assert!(
        said.contains("are not signed by the release key: nothing was installed"),
        "{said}"
    );
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(!pi.join("package.json").exists(), "{said}");
    assert!(m.ran().is_empty(), "{said}");

    // A file changed with its manifest, the old signature kept: refused too.
    std::fs::write(dist.join("manifest.sig"), &good).unwrap();
    let manifest = std::fs::read_to_string(dist.join("manifest.json")).unwrap();
    std::fs::write(
        dist.join("manifest.json"),
        manifest.replace("0.7.0", "9.9.9"),
    )
    .unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "pi"]));
    assert!(
        output(&out).contains("not signed by the release key"),
        "{}",
        output(&out)
    );
    assert!(!pi.join("package.json").exists());

    // No signature at all.
    std::fs::write(dist.join("manifest.json"), manifest).unwrap();
    std::fs::remove_file(dist.join("manifest.sig")).unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "pi"]));
    assert!(
        output(&out).contains("are not signed (no manifest.sig): nothing was installed"),
        "{}",
        output(&out)
    );
    assert!(!pi.join("package.json").exists());
    assert!(m.ran().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(debug_assertions),
    ignore = "a release build trusts only the release key"
)]
async fn setup_never_replaces_an_adapter_with_an_older_one() {
    let m = Machine::new().await;
    // pi's extension of a newer release is there; the server hands out 0.7.0, signed. As
    // `relevo update`: the newer one stays, and is set up as it is.
    let pi = m.home().join(".local/share/relevo/pi");
    std::fs::create_dir_all(&pi).unwrap();
    let newer = json!({ "name": "relevo-pi", "version": "9.0.0" }).to_string();
    std::fs::write(pi.join("package.json"), &newer).unwrap();
    let out = tokio::task::block_in_place(|| m.relevo(&["setup", "pi"]));
    let said = output(&out);
    assert!(
        said.contains("has 9.0.0, newer than the 0.7.0") && said.contains("(no downgrades)"),
        "{said}"
    );
    assert!(out.status.success(), "{said}");
    assert!(said.contains("Added Relevo to pi."), "{said}");
    assert_eq!(
        std::fs::read_to_string(pi.join("package.json")).unwrap(),
        newer
    );
    let pi = std::fs::canonicalize(&pi).unwrap();
    assert_eq!(m.ran(), [format!("pi install {}", pi.display())]);
}
