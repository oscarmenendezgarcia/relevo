//! Where a client finds its server and token: `~/.config/relevo/config.json`, or the file
//! `RELEVO_CONFIG` names, with `RELEVO_URL` and `RELEVO_TOKEN` over it.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    pub token: String,
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

/// The file (if any), then `RELEVO_URL` and `RELEVO_TOKEN` over it.
pub fn load(env: impl Fn(&str) -> Option<String>) -> Result<Config> {
    let file = path(&env)?;
    let mut url = None;
    let mut token = None;
    match std::fs::read(&file) {
        Ok(bytes) => {
            let saved: Config = serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", file.display()))?;
            url = Some(saved.url);
            token = Some(saved.token);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    }
    let set = |name| env(name).filter(|v: &String| !v.is_empty());
    let url = set("RELEVO_URL").or(url);
    let token = set("RELEVO_TOKEN").or(token);
    match (url, token) {
        (Some(url), Some(token)) => Ok(Config {
            url: normalize_url(&url)?,
            token,
        }),
        _ => bail!(
            "Relevo is not configured: run `relevo connect <url> <token>` \
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
            },
        )
        .unwrap();
        let f = file.to_str().unwrap();
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_TOKEN", "rlv_b")]);
        let c = load(env).unwrap();
        assert_eq!(
            (c.url.as_str(), c.token.as_str()),
            ("https://a.example", "rlv_b")
        );
        let env = env_of(&[("RELEVO_CONFIG", f), ("RELEVO_URL", "http://127.0.0.1:1/")]);
        assert_eq!(load(env).unwrap().url, "http://127.0.0.1:1");

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
