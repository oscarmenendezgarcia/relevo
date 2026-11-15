//! `relevo statusline` (docs/agents.md, "The status line (Claude Code)"): one line for Claude
//! Code's status line, saying how many threads wait on the person, as `/relevo` counts them, or
//! that the server is out of reach. It reads only: one `GET /api/timeline` with the token alone,
//! which delivers nothing and counts as no session (docs/protocol.md, "timeline"), at most once
//! every `FRESH` seconds per server and token, through a small cache file. And the setting that
//! makes Claude Code run it, which `relevo connect` offers to add.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::{Client, Error};
use crate::config::Config;
use crate::waiting;

/// How long a count is shown without asking the server again, in seconds.
pub const FRESH: i64 = 15;
/// How long the request may take before the line answers without it.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// How old a count may be to stand in for one that timed out, in seconds: past that, a server
/// that does not answer is offline.
pub const GRACE: i64 = 120;
/// How often Claude Code runs the line on its own while idle (`refreshInterval`, seconds), so a
/// note that arrives then shows: Claude Code's own triggers are its events (a message, `/compact`).
pub const REFRESH: u64 = 30;

/// Colours, as pi's footer has them: muted, warning (#f5c451) and error (#f85149), in the
/// 256-colour palette, which more terminals show than 24-bit colour.
const GREY: &str = "\x1b[38;5;245m";
const AMBER: &str = "\x1b[38;5;221m";
const RED: &str = "\x1b[38;5;203m";
const RESET: &str = "\x1b[0m";

/// What the line shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shown {
    /// N threads wait on the person (0: nothing does).
    Waiting(u64),
    /// The server out of reach since then (unix seconds).
    Offline(i64),
    /// The server refused the token.
    Refused,
    /// Any other answer than a count.
    Error,
    /// Relevo's config file is there but cannot be read.
    ConfigError,
}

/// The cache file: what was shown, when it was asked, and what a later answer needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    /// When the server was last asked (unix seconds).
    pub at: i64,
    pub shown: Shown,
    /// The last count the server gave, and when: it stands in for an answer that timed out.
    #[serde(default)]
    pub last_count: Option<(i64, u64)>,
    /// Since when the server has not answered, kept until it does.
    #[serde(default)]
    pub offline_since: Option<i64>,
}

/// The line for `shown` (ANSI colours, no newline).
pub fn render(shown: &Shown, now: i64) -> String {
    match shown {
        Shown::Waiting(0) => format!("{GREY}relevo{RESET}"),
        Shown::Waiting(n) => format!("{GREY}relevo{RESET} {AMBER}● {n}{RESET}"),
        Shown::Offline(since) => format!(
            "{RED}relevo ● offline {}{RESET}",
            waiting::clock(*since, now, waiting::local_offset(*since))
        ),
        Shown::Refused => format!("{RED}relevo ● token refused{RESET}"),
        Shown::Error => format!("{RED}relevo ● error{RESET}"),
        Shown::ConfigError => format!("{RED}relevo ● config error{RESET}"),
    }
}

/// The cache file for `config`'s server and token, in `$XDG_CACHE_HOME/relevo` or
/// `~/.cache/relevo`: named by a hash of both, so it says neither.
pub fn cache_path(env: impl Fn(&str) -> Option<String>, config: &Config) -> Option<PathBuf> {
    let set = |name| env(name).filter(|v: &String| !v.is_empty());
    let dir = match set("XDG_CACHE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(&set("HOME")?).join(".cache"),
    };
    let hash = Sha256::digest(format!("{}\n{}", config.url, config.token).as_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    Some(dir.join("relevo").join(format!("statusline-{name}.json")))
}

fn read_cache(path: &Path) -> Option<Cache> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Writes the cache in one step, readable by its owner only; a failure is ignored (the line is
/// shown all the same).
fn write_cache(path: &Path, cache: &Cache) {
    let Some(dir) = path.parent() else { return };
    let _ = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir);
    let tmp = dir.join(crate::db::random_id(&format!(
        ".statusline.{}.",
        std::process::id()
    )));
    let write = || -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&serde_json::to_vec(cache).unwrap_or_default())?;
        fs::rename(&tmp, path)
    };
    if write().is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// What the server said, as the line reads it.
enum Answer {
    Count(u64),
    TimedOut,
    Unreachable,
    Refused,
    Other,
}

/// One count, read only: the token alone, no session headers, `TIMEOUT` at most, setting up
/// the client (TLS roots) included. What is still running then (a name lookup on a blocking
/// thread) is left behind: `relevo statusline` exits without waiting for it.
async fn ask(config: &Config) -> Answer {
    let config = config.clone();
    let work = async move {
        let client = tokio::task::spawn_blocking(move || {
            Client::new(&config, Some(String::new()), "/".into())
        })
        .await
        .map_err(|_| None)?
        .map_err(|_| None)?;
        let path = "/api/timeline?tab=threads&show=waiting&limit=1";
        client
            .fetch(path, 1 << 20, TIMEOUT + Duration::from_secs(1))
            .await
            .map_err(Some)
    };
    match tokio::time::timeout(TIMEOUT, work).await {
        Err(_) => Answer::TimedOut,
        Ok(Ok(bytes)) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["counts"]["waiting"].as_u64())
            .map_or(Answer::Other, Answer::Count),
        Ok(Err(Some(e))) if e.unreachable() => Answer::Unreachable,
        Ok(Err(Some(Error::Api { status: 401, .. }))) => Answer::Refused,
        Ok(Err(_)) => Answer::Other,
    }
}

/// The line for `config` at `now`: from the cache at `cache` while it is fresh, else from the
/// server, which the cache then keeps (unless another run wrote a newer answer meanwhile). Empty
/// when Relevo is not configured.
pub async fn line(config: Option<&Config>, cache: Option<&Path>, now: i64) -> String {
    let Some(config) = config else {
        return String::new();
    };
    let started = std::time::Instant::now();
    let before = cache.and_then(read_cache);
    if let Some(c) = &before
        && (0..FRESH).contains(&(now - c.at))
    {
        return render(&c.shown, now);
    }
    let last_count = before.as_ref().and_then(|c| c.last_count);
    let answer = ask(config).await;
    // When the answer came, not when the run started.
    let now_then = now;
    let now = now + started.elapsed().as_secs() as i64;
    let since = before.as_ref().and_then(|c| c.offline_since).unwrap_or(now);
    let counted = matches!(answer, Answer::Count(_));
    let next = match answer {
        Answer::Count(n) => Cache {
            at: now,
            shown: Shown::Waiting(n),
            last_count: Some((now, n)),
            offline_since: None,
        },
        Answer::TimedOut => Cache {
            at: now,
            shown: match last_count {
                Some((at, n)) if now - at < GRACE => Shown::Waiting(n),
                _ => Shown::Offline(since),
            },
            last_count,
            offline_since: Some(since),
        },
        Answer::Unreachable => Cache {
            at: now,
            shown: Shown::Offline(since),
            last_count,
            offline_since: Some(since),
        },
        Answer::Refused => Cache {
            at: now,
            shown: Shown::Refused,
            last_count: None,
            offline_since: None,
        },
        Answer::Other => Cache {
            at: now,
            shown: Shown::Error,
            last_count,
            offline_since: None,
        },
    };
    if let Some(path) = cache {
        // Another run may have answered while this one waited: a newer answer, or a count got
        // since this run started when this one has none, stays, and is what is shown.
        let current = read_cache(path);
        if let Some(c) = current.filter(|c| {
            c.at > now
                || (!counted
                    && c.last_count
                        .is_some_and(|(at, _)| at >= now_then && at == c.at))
        }) {
            return render(&c.shown, now);
        }
        write_cache(path, &next);
    }
    render(&next.shown, now)
}

/// `relevo statusline`'s line: nothing when Relevo is not configured at all, `config error`
/// when its config is there but cannot be read.
pub async fn run() -> String {
    let config = match crate::config::load(crate::config::env) {
        Ok(config) => config,
        Err(e) if e.to_string().starts_with(crate::config::NOT_CONFIGURED) => {
            return String::new();
        }
        Err(_) => return render(&Shown::ConfigError, 0),
    };
    let cache = cache_path(crate::config::env, &config);
    line(Some(&config), cache.as_deref(), crate::db::now_unix()).await
}

// --- Claude Code's setting ---

/// Claude Code's user settings file in its folder `dir`.
pub fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

/// What the settings say of the status line.
#[derive(Debug, PartialEq)]
pub enum Setting {
    /// No settings file.
    NoFile,
    /// A settings file without `statusLine`.
    Absent,
    /// A status line that runs `relevo statusline`.
    Ours,
    /// Another status line.
    Foreign,
    /// A file that is not a JSON object: never written.
    Malformed(String),
    /// A symbolic link to nothing (its target): never written.
    Dangling(PathBuf),
}

/// Whether a status line command is Relevo's: its last word is `statusline`, and the one before
/// it a program called `relevo` (after any `VAR=value` before it).
pub fn is_ours(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    match words.as_slice() {
        [.., program, "statusline"] => {
            let program = program.trim_matches(['\'', '"']);
            Path::new(program)
                .file_name()
                .is_some_and(|n| n == "relevo")
        }
        _ => false,
    }
}

pub fn setting(path: &Path) -> Result<Setting> {
    if crate::claude_command::is_link(path) && fs::metadata(path).is_err() {
        let target = fs::read_link(path).unwrap_or_default();
        return Ok(Setting::Dangling(target));
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Setting::NoFile),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            return Ok(Setting::Malformed("not UTF-8".into()));
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(match serde_json::from_str::<Value>(&text) {
        Err(e) => Setting::Malformed(e.to_string()),
        Ok(Value::Object(map)) => match map.get("statusLine") {
            None => Setting::Absent,
            Some(s) if s["command"].as_str().is_some_and(is_ours) => Setting::Ours,
            Some(_) => Setting::Foreign,
        },
        Ok(_) => Setting::Malformed("not a JSON object".into()),
    })
}

/// `command` for a shell: as it is when it needs no quoting, else in single quotes.
fn quoted(command: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+,:=@%".contains(c);
    if !command.is_empty() && command.chars().all(safe) {
        command.to_string()
    } else {
        format!("'{}'", command.replace('\'', r"'\''"))
    }
}

/// The command for the setting: the binary `exe` by its full path (Claude Code started from a
/// desktop may not have `~/.local/bin` on its `PATH`), quoted for the shell when it must be.
pub fn command_for(exe: &Path) -> String {
    let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    format!("{} statusline", quoted(&exe.to_string_lossy()))
}

/// The setting's value, `{"type": "command", "command": …, "refreshInterval": …}`.
pub fn value(command: &str) -> Value {
    serde_json::json!({ "type": "command", "command": command, "refreshInterval": REFRESH })
}

/// `text` (a JSON object without `statusLine`) with `statusLine` added as its last key, and
/// every other byte as it was: the same indentation and line ending (`\r\n` or `\n`) on a file
/// of several lines, one line on a file of one.
pub fn with_status_line(text: &str, command: &str) -> Result<String> {
    let Value::Object(mut before) = serde_json::from_str::<Value>(text)? else {
        bail!("not a JSON object");
    };
    if before.contains_key("statusLine") {
        bail!("it has a statusLine already");
    }
    let close = text.trim_end().len() - 1; // the object's `}`: the JSON parsed
    let body = text[..close].trim_end();
    let empty = body.ends_with('{');
    let comma = if empty { "" } else { "," };
    let command = Value::String(command.to_string());
    let out = if text[..close].contains('\n') {
        let indent = text
            .lines()
            .skip(1)
            .find_map(|l| {
                let rest = l.trim_start_matches([' ', '\t']);
                rest.starts_with('"').then(|| &l[..l.len() - rest.len()])
            })
            .unwrap_or("  ");
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        // What was before the closing brace (its line break and indentation) stays before it.
        let rest = &text[body.len()..];
        let rest = if rest[..close - body.len()].contains('\n') {
            rest.to_string()
        } else {
            format!("{nl}{rest}")
        };
        format!(
            "{body}{comma}{nl}{indent}\"statusLine\": {{{nl}{indent}{indent}\"type\": \
             \"command\",{nl}{indent}{indent}\"command\": {command},{nl}{indent}{indent}\
             \"refreshInterval\": {REFRESH}{nl}{indent}}}{rest}"
        )
    } else {
        let space = if empty { "" } else { " " };
        format!(
            "{body}{comma}{space}\"statusLine\": {{\"type\": \"command\", \"command\": {command}, \
             \"refreshInterval\": {REFRESH}}}{}",
            &text[body.len()..]
        )
    };
    // What was there, and the status line: nothing else.
    before.insert("statusLine".into(), value(command.as_str().unwrap_or("")));
    if serde_json::from_str::<Value>(&out).ok() != Some(Value::Object(before)) {
        bail!("could not add statusLine to it as it is written");
    }
    Ok(out)
}

/// Adds the status line to the settings at `path` (made, with its folder, when there is none),
/// in one step, keeping the file's mode. A symbolic link is written through: the file it points
/// to changes, the link stays. The new text goes first to a temporary file of a name no one
/// else uses, made new (never through a link already there), and removed when anything fails.
pub fn install(path: &Path, command: &str) -> Result<()> {
    let path = if crate::claude_command::is_link(path) {
        fs::canonicalize(path).with_context(|| format!("following {}", path.display()))?
    } else {
        path.to_path_buf()
    };
    let (text, mode) = match fs::read_to_string(&path) {
        Ok(text) => {
            let mode = fs::metadata(&path)?.permissions().mode() & 0o7777;
            (with_status_line(&text, command)?, mode)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            format!(
                "{}\n",
                serde_json::to_string_pretty(&serde_json::json!({ "statusLine": value(command) }))?
            ),
            0o600,
        ),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let dir = path.parent().context("the settings' path has no folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(
        "{}.tmp",
        crate::db::random_id(&format!(".settings.json.relevo.{}.", std::process::id()))
    ));
    let write = || -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    };
    write()
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
        .with_context(|| format!("writing {}", path.display()))
}

/// How to add Relevo to a status line of one's own.
pub fn how_to_add(command: &str) -> String {
    format!(
        "to show Relevo in it too, have your status line command run `{command}` and print its \
         output after yours (for a script: echo \"$(your-line) $({command})\")"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "relevo statusline";

    #[test]
    fn the_line_in_each_state() {
        assert_eq!(render(&Shown::Waiting(0), 0), "\x1b[38;5;245mrelevo\x1b[0m");
        assert_eq!(
            render(&Shown::Waiting(2), 0),
            "\x1b[38;5;245mrelevo\x1b[0m \x1b[38;5;221m● 2\x1b[0m"
        );
        let at = 1_763_200_800; // 2025-11-15 10:00 UTC
        let offline = render(&Shown::Offline(at), at + 60);
        let hm = waiting::clock(at, at + 60, waiting::local_offset(at));
        assert_eq!(
            offline,
            format!("\x1b[38;5;203mrelevo ● offline {hm}\x1b[0m")
        );
    }

    #[test]
    fn whose_status_line() {
        for ours in [
            "relevo statusline",
            "/home/a/.local/bin/relevo statusline",
            "RELEVO_CONFIG=/tmp/x.json /opt/relevo statusline",
            "'/my dir/relevo' statusline",
        ] {
            assert!(is_ours(ours), "{ours}");
        }
        for theirs in [
            "~/.claude/statusline.sh",
            "relevo",
            "relevo-statusline",
            "myrelevo statusline",
            "relevo statusline | tr a b",
        ] {
            assert!(!is_ours(theirs), "{theirs}");
        }
    }

    #[test]
    fn adding_keeps_every_other_byte() {
        let text = "{\n    \"model\": \"opus\",\n    \"env\": {\"A\": \"1\"},\n    \"permissions\": {\n        \"allow\": [\"Bash(ls)\"]\n    }\n}\n";
        let out = with_status_line(text, CMD).unwrap();
        assert_eq!(
            out,
            "{\n    \"model\": \"opus\",\n    \"env\": {\"A\": \"1\"},\n    \"permissions\": {\n        \"allow\": [\"Bash(ls)\"]\n    },\n    \"statusLine\": {\n        \"type\": \"command\",\n        \"command\": \"relevo statusline\",\n        \"refreshInterval\": 30\n    }\n}\n"
        );
        // An empty object, one line, tabs, no newline at the end.
        assert_eq!(
            with_status_line("{}", CMD).unwrap(),
            "{\"statusLine\": {\"type\": \"command\", \"command\": \"relevo statusline\", \"refreshInterval\": 30}}"
        );
        assert_eq!(
            with_status_line("{\"a\": 1}  \n", CMD).unwrap(),
            "{\"a\": 1, \"statusLine\": {\"type\": \"command\", \"command\": \"relevo statusline\", \"refreshInterval\": 30}}  \n"
        );
        let out = with_status_line("{\n\t\"a\": 1\n}", "/x y/relevo statusline").unwrap();
        assert!(
            out.starts_with("{\n\t\"a\": 1,\n\t\"statusLine\": {\n\t\t\"type\""),
            "{out}"
        );
        assert!(
            out.contains("\"command\": \"/x y/relevo statusline\""),
            "{out}"
        );
        let out = with_status_line("{\n}\n", CMD).unwrap();
        assert!(out.starts_with("{\n  \"statusLine\": {"), "{out}");
        // Windows line endings stay; so does what is before the closing brace.
        assert_eq!(
            with_status_line("{\r\n  \"a\": 1\r\n}\r\n", CMD).unwrap(),
            "{\r\n  \"a\": 1,\r\n  \"statusLine\": {\r\n    \"type\": \"command\",\r\n    \"command\": \"relevo statusline\",\r\n    \"refreshInterval\": 30\r\n  }\r\n}\r\n"
        );
        assert_eq!(
            with_status_line("{\n  \"a\": 1\n  }", CMD).unwrap(),
            "{\n  \"a\": 1,\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"relevo statusline\",\n    \"refreshInterval\": 30\n  }\n  }"
        );
        // A closing brace on the last value's line goes on a line of its own.
        let out = with_status_line("{\n  \"a\": 1}", CMD).unwrap();
        assert!(out.ends_with("\"refreshInterval\": 30\n  }\n}"), "{out}");
        // Not an object, or not JSON: refused.
        assert!(with_status_line("[]", CMD).is_err());
        assert!(with_status_line("{\"a\": 1,}", CMD).is_err());
        assert!(with_status_line("", CMD).is_err());
        assert!(with_status_line("{\"statusLine\": 1}", CMD).is_err());
    }

    #[test]
    fn the_command_names_the_binary_by_its_full_path() {
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-sl-"));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("relevo"), "").unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        assert_eq!(
            command_for(&bin.join("relevo")),
            format!("{}/bin/relevo statusline", dir.display())
        );
        let other = dir.join("my bin");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("relevo"), "").unwrap();
        assert_eq!(
            command_for(&other.join("relevo")),
            format!("'{}/my bin/relevo' statusline", dir.display())
        );
        assert!(is_ours(&command_for(&other.join("relevo"))));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_adds_to_a_file_or_makes_one_and_keeps_its_mode() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-sl-"));
        let path = settings_path(&dir.join("claude"));
        assert_eq!(setting(&path).unwrap(), Setting::NoFile);
        install(&path, CMD).unwrap();
        assert_eq!(setting(&path).unwrap(), Setting::Ours);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A file there: kept, with its mode, through a link.
        let real = dir.join("dotfiles-settings.json");
        fs::write(&real, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&real, &path).unwrap();
        assert_eq!(setting(&path).unwrap(), Setting::Absent);
        install(&path, CMD).unwrap();
        assert!(crate::claude_command::is_link(&path));
        let v: Value = serde_json::from_str(&fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["statusLine"], value(CMD));
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // Someone else's line, a malformed file: told apart, and `install` refuses to touch them.
        fs::write(
            &real,
            "{\"statusLine\": {\"type\": \"command\", \"command\": \"x.sh\"}}",
        )
        .unwrap();
        assert_eq!(setting(&path).unwrap(), Setting::Foreign);
        assert!(install(&path, CMD).is_err());
        fs::write(&real, "{ \"theme\": ").unwrap();
        assert!(matches!(setting(&path).unwrap(), Setting::Malformed(_)));
        assert!(install(&path, CMD).is_err());
        assert_eq!(fs::read_to_string(&real).unwrap(), "{ \"theme\": ");
        // A link to nothing: told apart from no file.
        fs::remove_file(&real).unwrap();
        assert_eq!(setting(&path).unwrap(), Setting::Dangling(real.clone()));
        // Nothing is left behind.
        let left: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_cache_is_named_by_server_and_token_and_says_neither() {
        let config = |url: &str, token: &str| Config {
            url: url.into(),
            token: token.into(),
            cert: None,
            adapters: vec![],
        };
        let env = |name: &str| (name == "HOME").then(|| "/home/x".to_string());
        let a = cache_path(env, &config("https://r.example", "rlv_secret")).unwrap();
        let b = cache_path(env, &config("https://r.example", "rlv_other")).unwrap();
        assert_ne!(a, b);
        let name = a.to_string_lossy();
        assert!(
            name.starts_with("/home/x/.cache/relevo/statusline-"),
            "{name}"
        );
        assert!(
            !name.contains("secret") && !name.contains("example"),
            "{name}"
        );
        let env = |name: &str| (name == "XDG_CACHE_HOME").then(|| "/c".to_string());
        assert!(
            cache_path(env, &config("http://a", "t"))
                .unwrap()
                .starts_with("/c/relevo")
        );
    }
}
