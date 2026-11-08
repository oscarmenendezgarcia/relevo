//! A client's context, worked out from git as `docs/agents.md` says.

use std::path::Path;

use tokio::process::Command;

/// The context for a client running in `dir`: `repo` or `repo@branch` inside a git repository,
/// `None` (general) outside one or when git is missing.
pub async fn detect(dir: &Path) -> Option<String> {
    let top = git(dir, &["rev-parse", "--show-toplevel"]).await?;
    let repo = repo_part(Path::new(&top).file_name()?.to_str()?)?;
    match git(dir, &["symbolic-ref", "-q", "HEAD"]).await {
        Some(head) => match head.strip_prefix("refs/heads/").and_then(branch_part) {
            Some(branch) => Some(format!("{repo}@{branch}")),
            None => Some(repo),
        },
        // A detached HEAD is on no branch.
        None => Some(repo),
    }
}

/// The context to send: `RELEVO_CONTEXT` when set (empty means general), else what git says.
pub async fn current(dir: &Path, fixed: Option<&str>) -> Option<String> {
    match fixed {
        Some(fixed) => Some(fixed.trim().to_string()).filter(|c| !c.is_empty()),
        None => detect(dir).await,
    }
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim_end_matches(['\n', '\r']);
    (!s.is_empty()).then(|| s.to_string())
}

/// Lowercase, characters outside `allowed` as `-`.
fn clean(s: &str, allowed: impl Fn(char) -> bool) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if allowed(c) { c } else { '-' })
        .collect()
}

/// The repository part (`^[a-z0-9][a-z0-9._-]{0,63}$`), or `None` when nothing of the name fits.
fn repo_part(name: &str) -> Option<String> {
    let s = clean(name, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)
    });
    let s = s.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let s: String = s.chars().take(64).collect();
    (!s.is_empty()).then_some(s)
}

/// The branch part (`^[a-z0-9._/-]{1,100}$`).
fn branch_part(name: &str) -> Option<String> {
    let s = clean(name, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || "._/-".contains(c)
    });
    let s: String = s.chars().take(100).collect();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(crate::db::random_id("relevo-ctx-"))
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo(name: &str, branch: &str) -> PathBuf {
        let dir = tempdir(name);
        run(&dir, &["init", "-q", "-b", branch]);
        dir
    }

    #[tokio::test]
    async fn repository_and_branch() {
        let dir = repo("Pagos-API", "feat/Refunds");
        assert_eq!(
            detect(&dir).await.as_deref(),
            Some("pagos-api@feat/refunds")
        );
        // From a subdirectory too.
        let sub = dir.join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            detect(&sub).await.as_deref(),
            Some("pagos-api@feat/refunds")
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn odd_characters_become_dashes() {
        let dir = repo("Mi Repo+ñ", "fix/año#1_ok.x");
        assert_eq!(
            detect(&dir).await.as_deref(),
            Some("mi-repo--@fix/a-o-1_ok.x")
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn detached_head_is_the_repository_alone() {
        let dir = repo("app", "main");
        let commit = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ];
        run(&dir, &commit);
        run(&dir, &["checkout", "-q", "--detach"]);
        assert_eq!(detect(&dir).await.as_deref(), Some("app"));
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn outside_git_is_general() {
        let dir = tempdir("plain");
        // GIT_CEILING_DIRECTORIES is not needed: the temp dir is not inside a repository.
        assert_eq!(detect(&dir).await, None);
        assert_eq!(detect(Path::new("/nonexistent/dir")).await, None);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn the_environment_overrides_git() {
        let dir = repo("app", "main");
        assert_eq!(current(&dir, None).await.as_deref(), Some("app@main"));
        assert_eq!(current(&dir, Some("other")).await.as_deref(), Some("other"));
        assert_eq!(current(&dir, Some("")).await, None);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn names_fit_the_protocol() {
        assert_eq!(repo_part(".dotfiles").as_deref(), Some("dotfiles"));
        assert_eq!(repo_part("---"), None);
        assert_eq!(repo_part("ÄÖ"), None);
        assert_eq!(repo_part(&"a".repeat(80)).unwrap().len(), 64);
        assert_eq!(branch_part(&"b/".repeat(80)).unwrap().len(), 100);
        assert_eq!(branch_part("Feature/X").as_deref(), Some("feature/x"));
    }
}
