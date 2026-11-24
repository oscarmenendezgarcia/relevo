//! Setting agents up with Relevo once it is connected (`relevo connect --setup`, `relevo setup`):
//! Claude Code (its MCP server, the /relevo command and the status line), pi (its extension) and
//! Hermes (its plugin), the last two from the server's dist. With the agents named, it asks
//! nothing; otherwise, on a terminal, it asks once for each agent whose program is on `PATH`;
//! without a terminal it changes nothing and says what to run. Never fails: what goes wrong is
//! said, and the rest goes on.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::update::{self, Kind};

/// An agent Relevo can be set up for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    ClaudeCode,
    Pi,
    Hermes,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::ClaudeCode, Agent::Pi, Agent::Hermes];

    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Pi => "pi",
            Agent::Hermes => "Hermes",
        }
    }

    /// Its program, as found on `PATH`.
    fn program(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude",
            Agent::Pi => "pi",
            Agent::Hermes => "hermes",
        }
    }

    /// As `--setup` takes it.
    fn arg(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Pi => "pi",
            Agent::Hermes => "hermes",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Agent::ClaudeCode => " (MCP server, /relevo, status line)",
            Agent::Pi | Agent::Hermes => "",
        }
    }

    /// What to restart for it to load Relevo.
    fn restart(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Pi => "pi",
            Agent::Hermes => "the Hermes gateway",
        }
    }
}

/// A line of stdin, after `prompt` on stderr when stdin is a terminal.
pub fn read_line(prompt: &str) -> Result<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("{prompt}");
    }
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(line)
}

/// Whether `program` is a file anyone may run in one of the directories of `path`.
fn on_path(program: &str, path: Option<&OsString>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The agents whose program is on `PATH`.
pub fn found() -> Vec<Agent> {
    let path = std::env::var_os("PATH");
    Agent::ALL
        .into_iter()
        .filter(|a| on_path(a.program(), path.as_ref()))
        .collect()
}

/// Sets Relevo up for `named` without asking or, with none named, for the agents found: asked
/// on a terminal, else only told how. `command` and `statusline` are Claude Code's /relevo
/// command and status line: `Some(true)` without asking, `Some(false)` never, `None` as the
/// rest. When Claude Code is not set up here, nor declined, they are offered on their own, as
/// before 0.7.0 (on a terminal, when Claude Code's folder is there). Returns whether every agent
/// chosen was set up.
pub async fn run(named: &[Agent], command: Option<bool>, statusline: Option<bool>) -> bool {
    let found = found();
    let mut chosen: Vec<Agent> = vec![];
    // Whether the person said no to Claude Code.
    let mut declined = false;
    if !named.is_empty() {
        for agent in named {
            if !chosen.contains(agent) {
                chosen.push(*agent);
            }
        }
    } else if !found.is_empty() && std::io::stdin().is_terminal() {
        let labels: Vec<&str> = found.iter().map(|a| a.label()).collect();
        println!("\nFound {}.", and(&labels));
        for agent in &found {
            let question = format!("  Add Relevo to {}{}? [Y/n] ", agent.label(), agent.what());
            let yes = read_line(&question)
                .is_ok_and(|a| matches!(a.trim().to_lowercase().as_str(), "" | "y" | "yes"));
            if yes {
                chosen.push(*agent);
            } else {
                declined |= *agent == Agent::ClaudeCode;
            }
        }
    } else if !found.is_empty() {
        let labels: Vec<&str> = found.iter().map(|a| a.label()).collect();
        let args: Vec<&str> = found.iter().map(|a| a.arg()).collect();
        println!(
            "\nFound {} (no terminal to ask). To add Relevo to {}, run\n  {} setup {}",
            and(&labels),
            if found.len() == 1 { "it" } else { "them" },
            relevo().display(),
            args.join(" ")
        );
    }
    if !chosen.contains(&Agent::ClaudeCode) && !declined {
        if command != Some(false) {
            offer_command(command == Some(true), false);
        }
        if statusline != Some(false) {
            offer_statusline(statusline == Some(true), false);
        }
    }
    let mut done = vec![];
    let mut failed = false;
    for agent in chosen {
        match set_up(agent, command, statusline).await {
            Ok(()) => done.push(agent),
            Err(e) => {
                eprintln!("Could not add Relevo to {}: {e:#}", agent.label());
                failed = true;
            }
        }
    }
    if !done.is_empty() {
        let labels: Vec<&str> = done.iter().map(|a| a.label()).collect();
        let restart: Vec<&str> = done.iter().map(|a| a.restart()).collect();
        println!("Added Relevo to {}.", and(&labels));
        println!("\nDone. Restart {} to load Relevo.", and(&restart));
    }
    !failed
}

/// "a", "a and b", "a, b and c".
fn and(items: &[&str]) -> String {
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// This relevo's own file (`relevo` when it cannot be found).
fn relevo() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("relevo"))
}

/// Runs `program` with `args`; its output is kept, and said only when it fails.
fn run_program(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        let said = [out.stderr, out.stdout]
            .iter()
            .map(|o| String::from_utf8_lossy(o).trim().to_string())
            .find(|o| !o.is_empty())
            .unwrap_or_default();
        bail!("`{program} {}` failed: {said}", args.join(" "));
    }
    Ok(())
}

/// A directory of the environment, else one under the home directory.
fn dir_of(var: &str, under_home: &str) -> Result<PathBuf> {
    if let Some(dir) = config::env(var).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = config::env("HOME").filter(|h| !h.is_empty());
    Ok(Path::new(&home.context("HOME is not set")?).join(under_home))
}

/// Where the pi extension goes: `$XDG_DATA_HOME/relevo/pi`, or `~/.local/share/relevo/pi`.
pub fn pi_dir() -> Result<PathBuf> {
    Ok(dir_of("XDG_DATA_HOME", ".local/share")?.join("relevo/pi"))
}

/// Where the Hermes plugin goes: `plugins/relevo` in `$HERMES_HOME`, or in `~/.hermes`.
pub fn hermes_dir() -> Result<PathBuf> {
    Ok(dir_of("HERMES_HOME", ".hermes")?.join("plugins/relevo"))
}

async fn set_up(agent: Agent, command: Option<bool>, statusline: Option<bool>) -> Result<()> {
    match agent {
        Agent::ClaudeCode => {
            // `claude mcp get` fails for a server it does not have.
            let mcp = if run_program("claude", &["mcp", "get", "relevo"]).is_ok() {
                Ok(())
            } else {
                let exe = relevo();
                let exe = exe.to_string_lossy();
                run_program(
                    "claude",
                    &["mcp", "add", "--scope", "user", "relevo", "--", &exe, "mcp"],
                )
            };
            // The command and the status line work with an MCP server added by hand, too.
            if command != Some(false) {
                offer_command(true, true);
            }
            if statusline != Some(false) {
                offer_statusline(true, true);
            }
            mcp?;
        }
        Agent::Pi => {
            let dir = pi_dir()?;
            install(Kind::Pi, &dir).await?;
            run_program("pi", &["install", &dir.to_string_lossy()])?;
        }
        Agent::Hermes => {
            let dir = hermes_dir()?;
            install(Kind::Hermes, &dir).await?;
            // As adapters/hermes/README.md says: the plugin, its turns in chats, the gateway's
            // relevo platform (the server and token from the config `connect` saved), whom it
            // answers there (the Relevo team is the trust boundary), and its tools.
            for args in [
                &["plugins", "enable", "relevo"][..],
                &[
                    "config",
                    "set",
                    "plugins.entries.relevo.allow_gateway_injection",
                    "true",
                ],
                &["config", "set", "platforms.relevo.enabled", "true"],
                &["config", "set", "RELEVO_ALLOW_ALL_USERS", "true"],
                &["tools", "enable", "relevo", "--platform", "relevo"],
                &["tools", "enable", "relevo", "--platform", "cli"],
            ] {
                run_program("hermes", args)?;
            }
        }
    }
    Ok(())
}

/// Installs the adapter `kind` in `dir` from the server Relevo is connected to.
async fn install(kind: Kind, dir: &Path) -> Result<String> {
    let config = config::load(config::env)?;
    let key = update::PublicKey::for_setup(&config::env)?;
    update::install_from_server(&config, &config::env, &key, kind, dir).await
}

/// After `connect`: installs the /relevo command for Claude Code when Claude Code's folder is
/// here and the person says yes (`force`: without asking). A file Relevo did not write, or that
/// was edited, is replaced only when the person at the terminal says so. Never fails. `quiet`
/// leaves out what went as expected (installed, up to date).
pub fn offer_command(force: bool, quiet: bool) {
    use crate::claude_command::{self as command, Installed};
    use std::io::IsTerminal;
    let Some(dir) = command::claude_dir(&config::env) else {
        return;
    };
    if !force && !dir.is_dir() {
        return;
    }
    let path = command::path_in(&dir);
    let terminal = std::io::stdin().is_terminal();
    let ask = |question: &str, default: bool| -> bool {
        match read_line(question) {
            Ok(answer) => match answer.trim().to_lowercase().as_str() {
                "" => default,
                a => matches!(a, "y" | "yes"),
            },
            Err(_) => false,
        }
    };
    let install = |done: &str| match command::install(&path) {
        Ok(()) if quiet => {}
        Ok(()) => println!(
            "{done} the /relevo command for Claude Code in {}",
            path.display()
        ),
        Err(e) => eprintln!("the /relevo command for Claude Code: not installed: {e:#}"),
    };
    match command::state(&path) {
        Ok(Installed::Ours { current: true, .. }) if quiet => {}
        Ok(Installed::Ours { current: true, .. }) => println!(
            "the /relevo command for Claude Code is up to date in {}",
            path.display()
        ),
        Ok(Installed::Ours { version, .. }) if command::newer(&version) => println!(
            "left {} as it is: Relevo {version} wrote it, newer than this one ({})",
            path.display(),
            env!("CARGO_PKG_VERSION")
        ),
        Ok(Installed::Ours { .. }) => install("updated"),
        Ok(Installed::Missing) => {
            let question = format!(
                "Add the /relevo command to Claude Code ({})? [Y/n] ",
                path.display()
            );
            if force || (terminal && ask(&question, true)) {
                install("installed");
            } else if !terminal {
                println!(
                    "skipped the /relevo command for Claude Code (no terminal to ask): run \
                     relevo setup claude-code to install it"
                );
            }
        }
        Ok(Installed::Foreign) => {
            let question = format!(
                "{} exists, and Relevo did not write it (or it was edited). Replace it with \
                 Relevo's /relevo command? [y/N] ",
                path.display()
            );
            // A link is written through only to Relevo's own file (`command::install`).
            if terminal && !command::is_link(&path) && ask(&question, false) {
                install("installed");
            } else {
                println!(
                    "left {} as it is: Relevo did not write it, or it was edited",
                    path.display()
                );
            }
        }
        Err(e) => eprintln!("the /relevo command for Claude Code: {e:#}"),
    }
}

/// After `connect`: sets Claude Code's status line to `relevo statusline` when Claude Code's
/// folder is here, its settings have no status line, and the person says yes (`force`: without
/// asking). A status line that is not Relevo's, or settings that are not valid JSON, are never
/// changed: it says how to add Relevo by hand. Never fails. `quiet` leaves out what went as
/// expected (set, already Relevo's).
pub fn offer_statusline(force: bool, quiet: bool) {
    use crate::statusline::{self, Setting};
    use std::io::IsTerminal;
    let Some(dir) = crate::claude_command::claude_dir(&config::env) else {
        return;
    };
    if !force && !dir.is_dir() {
        return;
    }
    let path = statusline::settings_path(&dir);
    let command = match std::env::current_exe() {
        Ok(exe) => statusline::command_for(&exe),
        Err(_) => "relevo statusline".to_string(),
    };
    match statusline::setting(&path) {
        Ok(Setting::Ours) if quiet => {}
        Ok(Setting::Ours) => println!(
            "Claude Code's status line already shows Relevo ({})",
            path.display()
        ),
        Ok(Setting::Foreign) => println!(
            "Claude Code already has a status line ({}): left as it is; {}",
            path.display(),
            statusline::how_to_add(&command)
        ),
        Ok(Setting::Malformed(e)) => println!(
            "left {} as it is: it is not valid JSON ({e}); to show Relevo in Claude Code's status \
             line, add \"statusLine\": {} to it",
            path.display(),
            statusline::value(&command)
        ),
        Ok(Setting::Dangling(target)) => println!(
            "left {} as it is: it is a link to {}, which does not exist; to show Relevo in Claude \
             Code's status line, add \"statusLine\": {} to the settings it should point to",
            path.display(),
            target.display(),
            statusline::value(&command)
        ),
        Ok(Setting::Absent | Setting::NoFile) => {
            let terminal = std::io::stdin().is_terminal();
            let question = format!(
                "Show what waits on you in Claude Code's status line ({})? [Y/n] ",
                path.display()
            );
            let yes = || {
                read_line(&question)
                    .is_ok_and(|a| matches!(a.trim().to_lowercase().as_str(), "" | "y" | "yes"))
            };
            if force || (terminal && yes()) {
                match statusline::install(&path, &command) {
                    Ok(()) if quiet => {}
                    Ok(()) => println!(
                        "set Claude Code's status line to `{command}` in {}",
                        path.display()
                    ),
                    Err(e) => eprintln!("Claude Code's status line: not set: {e:#}"),
                }
            } else if !terminal {
                println!(
                    "skipped Claude Code's status line (no terminal to ask): run relevo setup \
                     claude-code to set it"
                );
            }
        }
        Err(e) => eprintln!("Claude Code's status line: {e:#}"),
    }
}
