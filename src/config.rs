//! Where a client finds its server, token and pinned certificate: `~/.config/relevo/config.json`,
//! or the file `RELEVO_CONFIG` names, with `RELEVO_URL`, `RELEVO_TOKEN` and `RELEVO_CERT` over it.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    pub token: String,
    /// The server's certificate (PEM), trusted alone for it: saved by `connect` when the system's
    /// authorities do not vouch for the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// The adapters installed on this machine, which `relevo update` updates too
    /// (docs/update.md). Only the file has them, never the environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapters: Vec<Adapter>,
}

/// An adapter's folder, as `relevo update --adapter <kind> --path <dir>` registers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Adapter {
    /// `pi` or `hermes`.
    pub kind: String,
    pub path: PathBuf,
}

/// The config file: `RELEVO_CONFIG`, else `$HOME/.config/relevo/config.json`.
pub fn path(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    if let Some(p) = env("RELEVO_CONFIG").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let home = env("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("HOME is not set; set RELEVO_CONFIG"))?;
    Ok(Path::new(&home).join(".config/relevo/config.json"))
}

/// The process environment, as `load` and `path` take it.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The config file as it is, or `None` when there is none.
pub fn saved(file: &Path) -> Result<Option<Config>> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("reading {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// The file (if any), then `RELEVO_URL`, `RELEVO_TOKEN` and `RELEVO_CERT` (a PEM file) over it.
/// The saved certificate goes with the saved URL: it is not used for another `RELEVO_URL`.
pub fn load(env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let saved = saved(&path(&env)?)?;
    let adapters = saved
        .as_ref()
        .map(|s| s.adapters.clone())
        .unwrap_or_default();
    let set = |name| env(name).filter(|v: &String| !v.is_empty());
    let url = match (set("RELEVO_URL"), &saved) {
        (Some(url), _) => Some(normalize_url(&url)?),
        (None, Some(saved)) => Some(normalize_url(&saved.url)?),
        (None, None) => None,
    };
    let token = set("RELEVO_TOKEN").or_else(|| saved.as_ref().map(|s| s.token.clone()));
    let cert = match set("RELEVO_CERT") {
        Some(pem) => Some(
            std::fs::read_to_string(&pem).with_context(|| format!("reading RELEVO_CERT {pem}"))?,
        ),
        None => saved.and_then(|s| {
            let same = url.as_deref() == normalize_url(&s.url).ok().as_deref();
            s.cert.filter(|_| same)
        }),
    };
    match (url, token) {
        (Some(url), Some(token)) => Ok(Config {
            url,
            token,
            cert,
            adapters,
        }),
        _ => bail!(
            "Relevo is not configured: run `relevo connect <url>` and give it the token \
             (or set RELEVO_URL and RELEVO_TOKEN)"
        ),
    }
}

/// `url` without trailing slashes; it must be http or https.
pub fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.contains(char::is_whitespace)
    {
        bail!("the server URL must start with https:// or http://");
    }
    Ok(url.to_string())
}

/// The warning for a server reached over plain `http://` on anything but loopback (127.0.0.0/8,
/// `::1` or `localhost`), where the token and notes cross the network in the clear.
pub fn plain_http_warning(url: &str) -> Option<&'static str> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    (!loopback).then_some("warning: plain http sends the token and notes in the clear; use https")
}

/// Writes `config` to `file`, readable by its user only (directory 0700, file 0600).
pub fn save(file: &Path, config: &Config) -> Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    // Write a fresh file next to it and move it in, so the secret never sits in a file with wider
    // permissions, whatever the old one had.
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(&serde_json::to_vec_pretty(config)?)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    std::fs::rename(&tmp, file).with_context(|| format!("writing {}", file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_is_fine_on_loopback_only() {
        for url in [
            "http://127.0.0.1:7878",
            "http://127.1.2.3",
            "http://localhost:7878/x",
            "http://LOCALHOST",
            "http://[::1]:7878",
            "https://relevo.example.com",
        ] {
            assert_eq!(plain_http_warning(url), None, "{url}");
        }
        for url in [
            "http://localhost.corp.example",
            "http://127.internal.example:80",
            "http://[::2]",
            "http://10.0.0.5:7878",
            "http://user@relevo.example.com",
            "http://localhost@evil.example",
        ] {
            assert!(plain_http_warning(url).is_some(), "{url}");
        }
    }
    use std::collections::HashMap;

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-config-"));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn saved_privately_and_read_back() {
        let dir = tempdir();
        let file = dir.join("nested/relevo/config.json");
        let config = Config {
            url: "https://relevo.example.com".into(),
            token: "rlv_secret".into(),
            cert: Some("-----BEGIN CERTIFICATE-----\n...".into()),
            adapters: vec![Adapter {
                kind: "pi".into(),
                path: "/home/x/.pi/relevo".into(),
            }],
        };
        save(&file, &config).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);

        // Overwriting a file that had wider permissions leaves it private.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&file, &config).unwrap();
        assert_eq!(mode(&file), 0o600);

        let env = env_of(&[("RELEVO_CONFIG", file.to_str().unwrap())]);
        assert_eq!(load(&env).unwrap(), config);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let dir = tempdir();
        let file = dir.join("config.json");
        save(
            &file,
            &Config {
                url: "https://a.example".into(),
                token: "rlv_a".into(),
                cert: Some("saved pem".into()),
                adapters: vec![],
            },
        )
        .unwrap();
        let f = file.to_str().unwrap();
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_TOKEN", "rlv_b")]);
        let c = load(env).unwrap();
        assert_eq!(
            (c.url.as_str(), c.token.as_str(), c.cert.as_deref()),
            ("https://a.example", "rlv_b", Some("saved pem"))
        );
        // The saved certificate is for the saved server only.
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_URL", "http://127.0.0.1:1/")]);
        let c = load(env).unwrap();
        assert_eq!((c.url.as_str(), c.cert), ("http://127.0.0.1:1", None));
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_URL", "https://a.example/")]);
        assert_eq!(load(env).unwrap().cert.as_deref(), Some("saved pem"));
        // RELEVO_CERT names a PEM file that wins over it, whatever the server.
        let pem = dir.join("other.pem");
        std::fs::write(&pem, "other pem").unwrap();
        let p = pem.to_str().unwrap();
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_CERT", p)]);
        assert_eq!(load(env).unwrap().cert.as_deref(), Some("other pem"));
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_CERT", "/nonexistent.pem")]);
        assert!(load(env).unwrap_err().to_string().contains("RELEVO_CERT"));

        // No file: the environment alone is enough, and nothing alone is an error.
        let missing = dir.join("missing.json");
        let m = missing.to_str().unwrap();
        let env = env_of(&[
            ("RELEVO_CONFIG", m),
            ("RELEVO_URL", "http://x"),
            ("RELEVO_TOKEN", "t"),
        ]);
        assert_eq!(load(env).unwrap().token, "t");
        let err = load(env_of(&[("RELEVO_CONFIG", m)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("relevo connect"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_path_is_under_home() {
        let p = path(env_of(&[("HOME", "/home/x")])).unwrap();
        assert_eq!(p, Path::new("/home/x/.config/relevo/config.json"));
        assert!(path(env_of(&[])).is_err());
    }

    #[test]
    fn urls_are_checked() {
        assert_eq!(normalize_url(" https://a.b/ ").unwrap(), "https://a.b");
        assert!(normalize_url("ftp://a.b").is_err());
        assert!(normalize_url("a.b").is_err());
    }
}
