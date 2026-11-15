//! The `/relevo` command for Claude Code (docs/agents.md, "The /relevo command"): a custom
//! command file, built into the binary, that `relevo connect` offers to install and `relevo
//! update` keeps current.
//!
//! The file Relevo writes carries a line in its front matter, `# relevo <version>
//! sha256:<hash>`, with the hash of the rest of the file. A file without it, or whose hash no
//! longer matches (someone edited it), is not Relevo's: it is never replaced without asking.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// The command as the repository has it, without the line that marks it as Relevo's.
pub const TEMPLATE: &str = include_str!("../adapters/claude-code/commands/relevo.md");

const MARK: &str = "# relevo ";

/// Claude Code's folder: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name| env(name).filter(|v: &String| !v.is_empty());
    match set("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => set("HOME").map(|h| Path::new(&h).join(".claude")),
    }
}

/// Where the command goes in Claude Code's folder `dir`.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("commands").join("relevo.md")
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The file as Relevo `version` writes it: the template with its mark after the front matter's
/// opening line.
pub fn contents(version: &str) -> String {
    let mark = format!("{MARK}{version} sha256:{}\n", sha256(TEMPLATE));
    match TEMPLATE.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{mark}{rest}"),
        None => format!("{TEMPLATE}\n{mark}"),
    }
}

/// What is at the command's place.
#[derive(Debug, PartialEq)]
pub enum Installed {
    Missing,
    /// Written by Relevo `version` and not edited since; `current` when it is this binary's.
    Ours {
        version: String,
        current: bool,
    },
    /// Not written by Relevo, or edited since.
    Foreign,
}

/// Who wrote `text`: Relevo (with its version) when its mark is there and the hash of the rest
/// matches.
fn ours(text: &str) -> Option<String> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    let mark = lines.next()?;
    let (version, hash) = mark.strip_prefix(MARK)?.trim_end().split_once(" sha256:")?;
    let rest = &text[first.len() + mark.len()..];
    (sha256(&format!("{first}{rest}")) == hash).then(|| version.to_string())
}

pub fn state(path: &Path) -> Result<Installed> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Installed::Missing),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Ok(Installed::Foreign),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(match ours(&text) {
        Some(version) => Installed::Ours {
            current: text == contents(env!("CARGO_PKG_VERSION")),
            version,
        },
        None => Installed::Foreign,
    })
}

/// Writes this binary's command at `path` (its folders made as needed), in one step: a reader
/// sees the old file or the new one.
pub fn install(path: &Path) -> Result<()> {
    let dir = path.parent().context("the command's path has no folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(".relevo.md.tmp");
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents(env!("CARGO_PKG_VERSION")).as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_a_valid_custom_command() {
        let text = contents("9.9.9");
        let (front, body) = text
            .strip_prefix("---\n")
            .and_then(|t| t.split_once("\n---\n"))
            .expect("front matter");
        let fields: Vec<&str> = front
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| l.split_once(": ").expect(l).0)
            .collect();
        assert_eq!(fields, ["description", "argument-hint", "allowed-tools"]);
        assert!(
            front.contains(
                "allowed-tools: mcp__relevo__waiting mcp__relevo__thread mcp__relevo__send"
            )
        );
        assert!(body.contains("$ARGUMENTS"), "{body}");
        for tool in [
            "`waiting`",
            "send(thread: \"N\"",
            "thread(id: \"N\")",
            "seen: true",
        ] {
            assert!(body.contains(tool), "{tool}");
        }
        // Short, as a prompt that runs on every /relevo should be.
        assert!(body.len() < 2000, "{}", body.len());
        assert_eq!(ours(&text).as_deref(), Some("9.9.9"));
        // Edited, or another file: not Relevo's.
        assert_eq!(ours(&text.replace("first person", "third person")), None);
        assert_eq!(ours(TEMPLATE), None);
        assert_eq!(ours(""), None);
    }

    #[test]
    fn installed_states() {
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-command-"));
        let path = path_in(&dir);
        assert_eq!(state(&path).unwrap(), Installed::Missing);
        install(&path).unwrap();
        let version = env!("CARGO_PKG_VERSION").to_string();
        assert_eq!(
            state(&path).unwrap(),
            Installed::Ours {
                version: version.clone(),
                current: true
            }
        );
        fs::write(&path, contents("0.5.9")).unwrap();
        assert_eq!(
            state(&path).unwrap(),
            Installed::Ours {
                version: "0.5.9".into(),
                current: false
            }
        );
        fs::write(&path, "my own /relevo\n").unwrap();
        assert_eq!(state(&path).unwrap(), Installed::Foreign);
        fs::remove_dir_all(&dir).unwrap();
        let env = |name: &str| match name {
            "HOME" => Some("/home/x".to_string()),
            _ => None,
        };
        assert_eq!(claude_dir(&env).unwrap(), Path::new("/home/x/.claude"));
        let env = |name: &str| Some(format!("/{name}"));
        assert_eq!(claude_dir(&env).unwrap(), Path::new("/CLAUDE_CONFIG_DIR"));
    }
}
