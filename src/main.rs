use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use relevo::{App, Clock, api, backup, client, config, db, mcp, serve, tls};

/// Relevo: notes that your agent hands to a teammate's agent.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// SQLite database file [default: ./relevo.db, which only `serve` creates].
    #[arg(long, global = true, env = "RELEVO_DB")]
    db: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server: plain HTTP, or HTTPS with --tls or --tls-cert and --tls-key.
    Serve {
        #[arg(long, env = "RELEVO_ADDR", default_value = "127.0.0.1:7878")]
        addr: SocketAddr,
        /// Team threads one person may open in any rolling 24 hours.
        #[arg(long, env = "RELEVO_TEAM_QUESTIONS_PER_DAY", default_value_t = 5)]
        team_questions_per_day: u32,
        /// Speak HTTPS with a certificate of its own, kept in <db>.tls/ (self-signed, made on
        /// the first start; its fingerprint is printed at every start).
        #[arg(long, env = "RELEVO_TLS", conflicts_with = "tls_cert")]
        tls: bool,
        /// A host name or IP address the certificate of --tls is for (repeatable, or
        /// comma-separated in RELEVO_TLS_NAME). Names the stored certificate lacks make a new
        /// one. [default: localhost, loopback, this machine's host name and addresses]
        #[arg(
            long,
            value_name = "NAME",
            env = "RELEVO_TLS_NAME",
            value_delimiter = ',',
            requires = "tls"
        )]
        tls_name: Vec<String>,
        /// Speak HTTPS with this certificate (PEM, the chain), one you already have.
        #[arg(long, value_name = "PEM", requires = "tls_key")]
        tls_cert: Option<PathBuf>,
        /// The private key (PEM) of --tls-cert.
        #[arg(long, value_name = "PEM", requires = "tls_cert")]
        tls_key: Option<PathBuf>,
    },
    /// Print the SHA-256 fingerprint of the certificate `serve --tls` uses, for teammates to
    /// compare with what `relevo connect` shows.
    TlsFingerprint,
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
        /// For an https:// server whose certificate no authority of this machine vouches for:
        /// its SHA-256 fingerprint (what the server printed), to trust it without being asked.
        #[arg(long, value_name = "SHA256")]
        fingerprint: Option<String>,
    },
    /// Serve MCP on stdin and stdout, for Claude Code and any MCP client.
    Mcp {
        /// Push new notes as Claude Code channel events (keeps an inbox request waiting).
        #[arg(long)]
        channel: bool,
    },
    /// Copy the database consistently (SQLite VACUUM INTO), safe while the server runs.
    Backup {
        /// A new file (never overwritten), or a directory to write YYYYMMDD-HHMMSS.db (UTC) into.
        dest: PathBuf,
        /// Write into the directory DEST (created if missing) and keep only the newest N
        /// backups there.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        keep: Option<u32>,
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

/// The database when neither `--db` nor `RELEVO_DB` names one.
const DEFAULT_DB: &str = "./relevo.db";

/// The database to open. Without `--db` or `RELEVO_DB`, only `serve` may create `./relevo.db`:
/// an admin command run where `RELEVO_DB` is not set yet would otherwise invite people into a new
/// database the server never reads.
fn db_path(db: Option<String>, command: &Command) -> Result<String> {
    match db {
        Some(db) => Ok(db),
        None if matches!(command, Command::Serve { .. }) || Path::new(DEFAULT_DB).exists() => {
            Ok(DEFAULT_DB.to_string())
        }
        None => bail!(
            "no database: RELEVO_DB is not set and there is no {DEFAULT_DB} here. Set RELEVO_DB \
             to the server's database (deploy/install.sh puts it in \
             ~/.local/share/relevo/relevo.db), or pass --db"
        ),
    }
}

fn print_token(handle: &str, label: &str, token: &db::NewToken) {
    println!("token {} for {handle} ({label})", token.id);
    println!("{}", token.secret);
    println!("This secret is shown once; only its hash is stored.");
}

/// The token for `connect`: the argument, else `RELEVO_TOKEN`, else a line of stdin.
fn connect_token(arg: Option<String>) -> Result<String> {
    if let Some(token) = arg.filter(|t| t != "-") {
        return Ok(token);
    }
    if let Some(token) = config::env("RELEVO_TOKEN").filter(|t| !t.trim().is_empty()) {
        return Ok(token);
    }
    let line = read_line("token: ")?;
    if line.trim().is_empty() {
        bail!("no token: give it on stdin or in RELEVO_TOKEN");
    }
    Ok(line)
}

/// A line of stdin, after `prompt` on stderr when stdin is a terminal.
fn read_line(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("{prompt}");
    }
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(line)
}

/// The certificate to pin for the `https://` server of `url`: none when this machine's
/// authorities vouch for it, else the one it showed, once its fingerprint is `given` or the
/// person at the terminal says yes. Runs before the token is read, so it never goes out to a
/// server that is not trusted.
async fn trust(url: &str, given: Option<&str>) -> Result<Option<String>> {
    use std::io::IsTerminal;
    let seen = tls::look(url).await?;
    let fingerprint = tls::fingerprint(&seen.cert);
    if let Some(given) = given
        && !tls::same_fingerprint(given, &fingerprint)
    {
        bail!(
            "the server's certificate has the SHA-256 fingerprint {fingerprint}, not the one \
             --fingerprint gave: not trusted, nothing was saved"
        );
    }
    if seen.trusted {
        return Ok(None);
    }
    if !seen.names_host {
        bail!(
            "the server's certificate is not for the host in {url}: use a name or address it \
             has, or have the server add it (relevo serve --tls --tls-name <host>)"
        );
    }
    if given.is_none() {
        eprintln!(
            "No authority this machine trusts vouches for the server's certificate.\n\
             Its SHA-256 fingerprint is\n  {fingerprint}\n\
             Compare it with the one the server printed (relevo tls-fingerprint on the server)."
        );
        if !std::io::stdin().is_terminal() {
            bail!(
                "not trusted, nothing was saved: run again on a terminal, or with \
                 --fingerprint <sha256> once you have checked it"
            );
        }
        let answer = read_line("Trust this certificate? [y/N] ")?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            bail!("not trusted, nothing was saved");
        }
    }
    Ok(Some(tls::pem(&seen.cert)))
}

/// Trusts the server (https), checks the token with `GET /api/people` and saves the server, the
/// token and the certificate to pin, if any.
async fn connect(url: &str, token: Option<String>, fingerprint: Option<String>) -> Result<()> {
    let url = config::normalize_url(url)?;
    let cert = if url.starts_with("https://") {
        trust(&url, fingerprint.as_deref()).await?
    } else if fingerprint.is_some() {
        bail!("--fingerprint is for https:// servers");
    } else {
        None
    };
    let config = config::Config {
        url,
        token: connect_token(token)?.trim().to_string(),
        cert,
    };
    // Before the token goes out, not after.
    if let Some(warning) = config::plain_http_warning(&config.url) {
        eprintln!("{warning}");
    }
    let client = client::Client::new(&config, Some(String::new()), "/".into())?;
    let people = client
        .people()
        .await
        .map_err(|e| anyhow::anyhow!("checking the token with {}: {e}", config.url))?;
    // The check is a session of its own: end it, or it stays listed until it is forgotten.
    let _ = client.end(std::time::Duration::from_secs(2)).await;
    let path = config::path(config::env)?;
    config::save(&path, &config)?;
    let team = people["team"].as_array().map(Vec::len).unwrap_or(0);
    let pinned = if config.cert.is_some() {
        " with its certificate"
    } else {
        ""
    };
    println!(
        "connected to {} (a team of {team}); saved{pinned} to {}",
        config.url,
        path.display()
    );
    Ok(())
}

/// What `serve` speaks TLS with, if anything, and the fingerprint it prints.
fn server_tls(
    db_file: &str,
    tls: bool,
    names: &[String],
    cert: Option<&Path>,
    key: Option<&Path>,
) -> Result<Option<std::sync::Arc<rustls::ServerConfig>>> {
    let cert = match (tls, cert, key) {
        (_, Some(cert), Some(key)) => tls::load(cert, key)?,
        (true, _, _) => {
            let dir = tls::dir_for(db_file);
            let (cert, made) = tls::stored_or_new(&dir, names, tls::default_names)?;
            match made {
                tls::Made::Kept => {}
                tls::Made::New(names) => {
                    println!(
                        "made a certificate for {} in {}",
                        names.join(", "),
                        dir.display()
                    );
                }
                tls::Made::Replaced { names, missing } => println!(
                    "made a new certificate for {} in {}, because the stored one lacked {}: \
                     every client must run `relevo connect` again",
                    names.join(", "),
                    dir.display(),
                    missing.join(", ")
                ),
            }
            cert
        }
        _ => return Ok(None),
    };
    println!(
        "TLS certificate SHA-256 fingerprint: {}",
        cert.fingerprint()
    );
    Ok(Some(cert.server_config()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // The client commands never touch a database.
    match cli.command {
        Command::Connect {
            url,
            token,
            fingerprint,
        } => return connect(&url, token, fingerprint).await,
        Command::Mcp { channel } => return mcp::run(channel).await,
        _ => {}
    }
    let db_file = db_path(cli.db, &cli.command)?;
    // A backup only reads the database: it must not create one, nor refuse another version's.
    if let Command::Backup { dest, keep } = &cli.command {
        let done = backup::backup(
            &db_file,
            dest,
            keep.map(|k| k as usize),
            time::OffsetDateTime::now_utc(),
        )?;
        println!("backed up {db_file} to {}", done.path.display());
        for old in done.removed {
            println!("removed old backup {}", old.display());
        }
        return Ok(());
    }
    // Nor does reading the certificate's fingerprint.
    if let Command::TlsFingerprint = &cli.command {
        let dir = tls::dir_for(&db_file);
        let cert = tls::stored(&dir)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no certificate in {}: `relevo serve --tls` makes it",
                dir.display()
            )
        })?;
        println!("{}", cert.fingerprint());
        return Ok(());
    }
    let conn = db::open(&db_file)?;
    let now = db::now_unix();
    match cli.command {
        Command::Serve {
            addr,
            team_questions_per_day,
            tls,
            tls_name,
            tls_cert,
            tls_key,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let tls = server_tls(
                &db_file,
                tls,
                &tls_name,
                tls_cert.as_deref(),
                tls_key.as_deref(),
            )?;
            let app = App::new(conn, Clock::system(), team_questions_per_day);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            let scheme = if tls.is_some() { "https" } else { "http" };
            tracing::info!(%addr, db = %db_file, %scheme, "relevo listening");
            if tls.is_none() && !addr.ip().is_loopback() {
                tracing::warn!(
                    %addr,
                    "plain HTTP on a non-loopback address: tokens and notes cross the network \
                     in the clear; use --tls, or a proxy or VPN that encrypts (see \
                     docs/install.md)"
                );
            }
            serve::raise_open_files(serve::Limits::default().connections);
            tokio::select! {
                _ = serve::serve(listener, api::router(app), tls, serve::Limits::default()) => {}
                _ = tokio::signal::ctrl_c() => {}
            }
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
        Command::Connect { .. }
        | Command::Mcp { .. }
        | Command::Backup { .. }
        | Command::TlsFingerprint => {
            unreachable!("handled above")
        }
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
                ..
            } => (addr, team_questions_per_day),
            _ => panic!("not serve"),
        }
    }

    fn tls_args(cli: &Cli) -> (bool, Vec<String>) {
        match &cli.command {
            Command::Serve { tls, tls_name, .. } => (*tls, tls_name.clone()),
            _ => panic!("not serve"),
        }
    }

    /// One test, because it changes the process environment.
    #[test]
    fn defaults_and_environment() {
        const VARS: [&str; 5] = [
            "RELEVO_DB",
            "RELEVO_ADDR",
            "RELEVO_TEAM_QUESTIONS_PER_DAY",
            "RELEVO_TLS",
            "RELEVO_TLS_NAME",
        ];
        // SAFETY: no other test in this binary reads or writes the environment.
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };

        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(cli.db, None);
        assert_eq!(serve_args(&cli), ("127.0.0.1:7878".parse().unwrap(), 5));
        assert_eq!(tls_args(&cli), (false, vec![]));

        // TLS: names only with --tls, a certificate of yours only with its key, not both.
        let parse = |args: &[&str]| Cli::try_parse_from([&["relevo", "serve"], args].concat());
        let cli = parse(&["--tls", "--tls-name", "a.office", "--tls-name", "10.0.0.5"]).unwrap();
        assert_eq!(
            tls_args(&cli),
            (true, vec!["a.office".into(), "10.0.0.5".into()])
        );
        assert!(parse(&["--tls-name", "a.office"]).is_err());
        assert!(parse(&["--tls-cert", "c.pem"]).is_err());
        assert!(parse(&["--tls-key", "k.pem"]).is_err());
        assert!(parse(&["--tls", "--tls-cert", "c.pem", "--tls-key", "k.pem"]).is_err());
        assert!(parse(&["--tls-cert", "c.pem", "--tls-key", "k.pem"]).is_ok());

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
        assert_eq!(cli.db.as_deref(), Some("/srv/team.db"));
        assert_eq!(serve_args(&cli), ("0.0.0.0:9000".parse().unwrap(), 2));
        assert_eq!(tls_args(&cli), (false, vec![]));
        unsafe {
            std::env::set_var("RELEVO_TLS", "true");
            std::env::set_var("RELEVO_TLS_NAME", "relevo.office,192.168.1.5");
        }
        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(
            tls_args(&cli),
            (true, vec!["relevo.office".into(), "192.168.1.5".into()])
        );
        unsafe {
            std::env::set_var("RELEVO_TLS", "false");
            std::env::remove_var("RELEVO_TLS_NAME");
        }
        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(tls_args(&cli), (false, vec![]));
        // Flags win over the environment, and --db works after the subcommand too.
        let cli =
            Cli::try_parse_from(["relevo", "serve", "--db", "x.db", "--addr", "[::1]:1"]).unwrap();
        assert_eq!(cli.db.as_deref(), Some("x.db"));
        assert_eq!(serve_args(&cli).0, "[::1]:1".parse().unwrap());
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };
    }
}
