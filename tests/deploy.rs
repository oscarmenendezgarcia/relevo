//! The files in `deploy/` agree with what the server needs and does.

use std::process::{Command, Stdio};

fn read(path: &str) -> String {
    std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The value of `key=` in a unit file (the last one, as systemd reads it).
fn setting<'a>(unit: &'a str, key: &str) -> Option<&'a str> {
    unit.lines()
        .filter_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
        .next_back()
}

/// The sandbox settings of the server's unit that need no namespaces, so `systemd-run --user`
/// can apply them anywhere.
const SANDBOX: &[&str] = &[
    "NoNewPrivileges",
    "RestrictAddressFamilies",
    "RestrictNamespaces",
    "RestrictRealtime",
    "LockPersonality",
    "MemoryDenyWriteExecute",
    "SystemCallArchitectures",
    "UMask",
];

/// glibc's getifaddrs(), which `serve --tls` uses for the machine's addresses, needs a netlink
/// socket.
#[test]
fn the_unit_lets_serve_read_the_machines_addresses() {
    let unit = read("deploy/systemd/relevo.service");
    let families = setting(&unit, "RestrictAddressFamilies").unwrap();
    assert!(
        families.split_whitespace().any(|f| f == "AF_NETLINK"),
        "{families}"
    );
}

/// The first `serve --tls` under the unit's sandbox makes a certificate for the same names as
/// without it: the machine's network addresses included. Skipped without a systemd user manager.
#[test]
fn under_the_units_sandbox_serve_tls_names_the_machines_addresses() {
    let usable = Command::new("systemd-run")
        .args(["--user", "--wait", "--pipe", "--quiet", "--collect", "true"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success());
    if !usable {
        eprintln!("skipped: systemd-run --user does not work here");
        return;
    }
    let unit = read("deploy/systemd/relevo.service");
    let dir = std::env::temp_dir().join(relevo::db::random_id("relevo-deploy-"));
    std::fs::create_dir(&dir).unwrap();
    let mut run = Command::new("systemd-run");
    run.args(["--user", "--wait", "--pipe", "--quiet", "--collect"])
        .args(["-p", "RuntimeMaxSec=3"]);
    for key in SANDBOX {
        let value = setting(&unit, key).unwrap_or_else(|| panic!("the unit has no {key}="));
        run.arg("-p").arg(format!("{key}={value}"));
    }
    let out = run
        .arg(env!("CARGO_BIN_EXE_relevo"))
        .args(["serve", "--tls", "--addr", "127.0.0.1:0", "--db"])
        .arg(dir.join("relevo.db"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let made = stdout
        .lines()
        .find_map(|l| l.strip_prefix("made a certificate for "))
        .unwrap_or_else(|| {
            panic!(
                "no certificate made: {stdout}{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let names: Vec<&str> = made.rsplit_once(" in ").unwrap().0.split(", ").collect();
    assert_eq!(names, relevo::tls::default_names(), "{stdout}");
    assert!(!stdout.contains("no network address"), "{stdout}");
}
