use std::net::SocketAddr;

use anyhow::Result;
use clap::{Parser, Subcommand};
use relevo::{App, Clock, api, client, config, db, mcp};

/// Relevo: notes that your agent hands to a teammate's agent.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// SQLite database file.
    #[arg(long, global = true, env = "RELEVO_DB", default_value = "./relevo.db")]
    db: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve {
        #[arg(long, env = "RELEVO_ADDR", default_value = "127.0.0.1:7878")]
        addr: SocketAddr,
        /// Team threads one person may open in any rolling 24 hours.
        #[arg(long, env = "RELEVO_TEAM_QUESTIONS_PER_DAY", default_value_t = 5)]
        team_questions_per_day: u32,
    },
    /// Create a person (or reuse and reactivate one) and print a new token once.
    Invite {
        handle: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        about: Option<String>,
        #[arg(long, default_value = "agent")]
        label: String,
    },
    /// Manage agent tokens.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// List everyone, active or not.
    People,
    /// List sessions seen in the last 7 days: label, session id, context, last seen.
    Sessions { handle: Option<String> },
    /// Set the one line on what a person (or their agent) knows or looks after.
    About { handle: String, about: String },
    /// Stop a person from using Relevo (every token of theirs is revoked).
    Deactivate { handle: String },
    /// Check a token against a server and save both for `relevo mcp` (in
    /// ~/.config/relevo/config.json, or the file RELEVO_CONFIG names).
    Connect {
        url: String,
        /// The token. Without it (or with `-`) it is read from RELEVO_TOKEN, else from one line
        /// of stdin, so that it stays out of shell history and the process list.
        token: Option<String>,
    },
    /// Serve MCP on stdin and stdout, for Claude Code and any MCP client.
    Mcp {
        /// Push new notes as Claude Code channel events (keeps an inbox request waiting).
        #[arg(long)]
        channel: bool,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Issue another token for a person (another agent).
    Add {
        handle: String,
        #[arg(long)]
        label: String,
    },
    /// List tokens, of everyone or of one person.
    List { handle: Option<String> },
    /// Revoke a token by id.
    Revoke { id: String },
}

fn print_token(handle: &str, label: &str, token: &db::NewToken) {
    println!("token {} for {handle} ({label})", token.id);
    println!("{}", token.secret);
    println!("This secret is shown once; only its hash is stored.");
}

/// The token for `connect`: the argument, else `RELEVO_TOKEN`, else a line of stdin.
fn connect_token(arg: Option<String>) -> Result<String> {
    use std::io::IsTerminal;
    if let Some(token) = arg.filter(|t| t != "-") {
        return Ok(token);
    }
    if let Some(token) = config::env("RELEVO_TOKEN").filter(|t| !t.trim().is_empty()) {
        return Ok(token);
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("token: ");
    }
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    if line.trim().is_empty() {
        anyhow::bail!("no token: give it on stdin or in RELEVO_TOKEN");
    }
    Ok(line)
}

/// Checks the token with `GET /api/people` and saves the server and token.
async fn connect(url: &str, token: &str) -> Result<()> {
    let config = config::Config {
        url: config::normalize_url(url)?,
        token: token.trim().to_string(),
    };
    let client = client::Client::new(&config, Some(String::new()), "/".into())?;
    let people = client
        .people()
        .await
        .map_err(|e| anyhow::anyhow!("checking the token with {}: {e}", config.url))?;
    let path = config::path(config::env)?;
    config::save(&path, &config)?;
    let team = people["team"].as_array().map(Vec::len).unwrap_or(0);
    println!(
        "connected to {} (a team of {team}); saved to {}",
        config.url,
        path.display()
    );
    let loopback = ["http://127.", "http://localhost", "http://[::1]"];
    if config.url.starts_with("http://") && !loopback.iter().any(|l| config.url.starts_with(l)) {
        eprintln!("warning: plain http sends the token and notes in the clear; use https");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // The client commands never touch a database.
    match cli.command {
        Command::Connect { url, token } => return connect(&url, &connect_token(token)?).await,
        Command::Mcp { channel } => return mcp::run(channel).await,
        _ => {}
    }
    let conn = db::open(&cli.db)?;
    let now = db::now_unix();
    match cli.command {
        Command::Serve {
            addr,
            team_questions_per_day,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let app = App::new(conn, Clock::system(), team_questions_per_day);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            tracing::info!(%addr, db = %cli.db, "relevo listening");
            if !addr.ip().is_loopback() {
                tracing::warn!(
                    %addr,
                    "plain HTTP without timeouts on a non-loopback address; \
                     put a TLS-terminating reverse proxy in front (see docs/protocol.md)"
                );
            }
            axum::serve(listener, api::router(app))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Command::Invite {
            handle,
            name,
            about,
            label,
        } => {
            let token = db::invite(&conn, &handle, &name, about.as_deref(), &label, now)?;
            print_token(&handle, &label, &token);
        }
        Command::Token { command } => match command {
            TokenCommand::Add { handle, label } => {
                let token = db::add_token(&conn, &handle, &label, now)?;
                print_token(&handle, &label, &token);
            }
            TokenCommand::List { handle } => {
                for t in db::list_tokens(&conn, handle.as_deref())? {
                    let state = match t.revoked_at {
                        Some(at) => format!("revoked {}", db::rfc3339(at)),
                        None => "active".to_string(),
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{state}",
                        t.id,
                        t.handle,
                        t.label,
                        db::rfc3339(t.created_at)
                    );
                }
            }
            TokenCommand::Revoke { id } => {
                db::revoke_token(&conn, &id, now)?;
                println!("revoked {id}");
            }
        },
        Command::People => {
            for p in db::list_people(&conn)? {
                let state = if p.active { "active" } else { "inactive" };
                println!("{}\t{}\t{state}\t{}", p.handle, p.name, p.about);
            }
        }
        Command::Sessions { handle } => {
            for s in db::list_sessions(&conn, handle.as_deref(), now)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    s.handle,
                    s.label,
                    s.session.as_deref().unwrap_or("default"),
                    s.context.as_deref().unwrap_or("general"),
                    db::rfc3339(s.last_seen)
                );
            }
        }
        Command::About { handle, about } => {
            db::set_about(&conn, &handle, &about)?;
            println!("{handle}: {about}");
        }
        Command::Deactivate { handle } => {
            let revoked = db::deactivate(&conn, &handle, now)?;
            println!("deactivated {handle}, revoked {revoked} token(s)");
        }
        Command::Connect { .. } | Command::Mcp { .. } => unreachable!("handled above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_args(cli: &Cli) -> (SocketAddr, u32) {
        match cli.command {
            Command::Serve {
                addr,
                team_questions_per_day,
            } => (addr, team_questions_per_day),
            _ => panic!("not serve"),
        }
    }

    /// One test, because it changes the process environment.
    #[test]
    fn defaults_and_environment() {
        const VARS: [&str; 3] = ["RELEVO_DB", "RELEVO_ADDR", "RELEVO_TEAM_QUESTIONS_PER_DAY"];
        // SAFETY: no other test in this binary reads or writes the environment.
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };

        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(cli.db, "./relevo.db");
        assert_eq!(serve_args(&cli), ("127.0.0.1:7878".parse().unwrap(), 5));

        let cli = Cli::try_parse_from(["relevo", "invite", "ana", "--name", "Ana"]).unwrap();
        let Command::Invite { label, about, .. } = cli.command else {
            panic!("not invite")
        };
        assert_eq!(label, "agent");
        assert_eq!(about, None);

        unsafe {
            std::env::set_var("RELEVO_DB", "/srv/team.db");
            std::env::set_var("RELEVO_ADDR", "0.0.0.0:9000");
            std::env::set_var("RELEVO_TEAM_QUESTIONS_PER_DAY", "2");
        }
        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(cli.db, "/srv/team.db");
        assert_eq!(serve_args(&cli), ("0.0.0.0:9000".parse().unwrap(), 2));
        // Flags win over the environment, and --db works after the subcommand too.
        let cli =
            Cli::try_parse_from(["relevo", "serve", "--db", "x.db", "--addr", "[::1]:1"]).unwrap();
        assert_eq!(cli.db, "x.db");
        assert_eq!(serve_args(&cli).0, "[::1]:1".parse().unwrap());
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };
    }
}
