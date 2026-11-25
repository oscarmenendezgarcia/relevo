use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use relevo::setup::{Agent, read_line};
use relevo::{App, Clock, api, backup, client, config, db, mcp, serve, tls, update};

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
        /// Team and global-board threads one person may open in any rolling 24 hours.
        #[arg(long, env = "RELEVO_TEAM_QUESTIONS_PER_DAY", default_value_t = 5)]
        team_questions_per_day: u32,
        /// Speak HTTPS with a certificate of its own, kept in <db>.tls/ (self-signed, made on
        /// the first start; its fingerprint is printed at every start). Not with --tls-cert.
        // Checked in `check_tls`, not by clap, which counts RELEVO_TLS=false as given.
        #[arg(long, env = "RELEVO_TLS")]
        tls: bool,
        /// A host name or IP address the certificate of --tls is for (repeatable, or
        /// comma-separated in RELEVO_TLS_NAME). Names the stored certificate lacks make a new
        /// one. Only with --tls. [default: localhost, loopback, this machine's host name and
        /// addresses]
        #[arg(
            long,
            value_name = "NAME",
            env = "RELEVO_TLS_NAME",
            value_delimiter = ','
        )]
        tls_name: Vec<String>,
        /// Speak HTTPS with this certificate (PEM, the chain), one you already have.
        #[arg(
            long,
            value_name = "PEM",
            env = "RELEVO_TLS_CERT",
            requires = "tls_key"
        )]
        tls_cert: Option<PathBuf>,
        /// The private key (PEM) of --tls-cert.
        #[arg(
            long,
            value_name = "PEM",
            env = "RELEVO_TLS_KEY",
            requires = "tls_cert"
        )]
        tls_key: Option<PathBuf>,
        /// The clients to hand out to the team (docs/update.md). [default: dist/ next to the
        /// database]
        #[arg(long, value_name = "DIR", env = "RELEVO_DIST")]
        dist: Option<PathBuf>,
        /// Do not post a new version's release notes on the global board (it is still recorded
        /// as announced). RELEVO_RELEASE_NOTES=0 does the same.
        #[arg(long)]
        no_release_notes: bool,
        /// The address teammates reach this server at, for invitation links
        /// (https://relevo.example.com behind a proxy). [default: the scheme, the first
        /// --tls-name or the address it listens on, and its port]
        #[arg(long, value_name = "URL", env = "RELEVO_PUBLIC_URL")]
        public_url: Option<String>,
    },
    /// Print the SHA-256 fingerprint of the certificate `serve --tls` uses, for teammates to
    /// compare with what `relevo connect` shows.
    TlsFingerprint,
    /// Create a person (or reuse and reactivate one) and print a link that invites them: it
    /// works once, for 24 hours, and gets their agent a token of its own.
    Invite {
        handle: String,
        /// Their name; needed for a new person [default: the name they have].
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        about: Option<String>,
        /// Print a token now, once, instead of a link (for scripts, and the way before 0.7.0).
        #[arg(long)]
        token: bool,
        /// The label of the token of --token [default: agent].
        #[arg(long, requires = "token")]
        label: Option<String>,
        /// The server's address for the link [default: the one `relevo serve` recorded].
        #[arg(long, value_name = "URL", conflicts_with = "token")]
        url: Option<String>,
        /// Mark them as a bot: an agent that runs on its own, with a token of its own (without
        /// it, an existing person keeps the mark they have).
        #[arg(long)]
        bot: bool,
    },
    /// Manage agent tokens.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// List everyone, active or not.
    People,
    /// List sessions seen in the last 7 days: label, session id, context, last seen, client, agent.
    Sessions { handle: Option<String> },
    /// List the client and version each session seen in the last 7 days runs, and whether it is
    /// older than this server (outdated first), with a summary line.
    Clients {
        handle: Option<String>,
        /// Only the sessions that are outdated or unknown (no version, or one that cannot be read).
        #[arg(long)]
        outdated: bool,
    },
    /// Set the one line on what a person (or their agent) knows or looks after.
    About { handle: String, about: String },
    /// Mark a person as a bot (on) or not (off).
    Bot {
        handle: String,
        #[arg(value_parser = ["on", "off"])]
        mark: String,
    },
    /// Stop a person from using Relevo (every token of theirs is revoked).
    Deactivate { handle: String },
    /// Manage teams: named groups of people, whom a note reaches with @<name>.
    Team {
        #[command(subcommand)]
        command: TeamCommand,
    },
    /// List every team: name, title and members (inactive ones marked).
    Teams,
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
        /// Install the /relevo command for Claude Code (~/.claude/commands/relevo.md) without
        /// asking. A file there that Relevo did not write is still never replaced unasked.
        #[arg(long, conflicts_with = "no_command")]
        command: bool,
        /// Do not offer the /relevo command for Claude Code.
        #[arg(long)]
        no_command: bool,
        /// Show Relevo in Claude Code's status line (~/.claude/settings.json) without asking.
        /// A status line that is not Relevo's is never changed.
        #[arg(long, conflicts_with = "no_statusline")]
        statusline: bool,
        /// Do not offer Claude Code's status line.
        #[arg(long)]
        no_statusline: bool,
        /// Join with an invitation's code (from `relevo invite`'s link) instead of a token: the
        /// server trades it, once, for a token of your own.
        #[arg(long, value_name = "CODE", conflicts_with = "token")]
        join: Option<String>,
        /// Set Relevo up for these agents once connected, without asking (repeatable, or
        /// comma-separated). Without it, a terminal is asked about each agent found on PATH.
        #[arg(long, value_enum, value_name = "AGENT", value_delimiter = ',')]
        setup: Vec<Agent>,
    },
    /// Set Relevo up for agents on this machine, with the server `relevo connect` saved: Claude
    /// Code (MCP server, /relevo, status line), pi (its extension) and Hermes (its plugin), from
    /// the server's clients. Without agents, a terminal is asked about each found on PATH.
    Setup {
        #[arg(value_enum, value_delimiter = ',')]
        agents: Vec<Agent>,
    },
    /// Open your timeline in the browser: the board and your threads, read only (it delivers,
    /// handles and claims nothing). Serves it on 127.0.0.1 until Ctrl-C.
    Web {
        /// The local port [default: a free one].
        #[arg(long)]
        port: Option<u16>,
        /// Only print the address; do not open a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Write a note from a script, a cron job or CI, without an agent: open a thread for whom its
    /// @mentions name (none: the global board), or answer one with --reply. Prints the thread's
    /// id on stdout.
    Send {
        /// The note, or - to read it from stdin.
        #[arg(allow_hyphen_values = true)]
        text: String,
        /// Answer in this thread (its id) instead of opening one.
        #[arg(long, value_name = "THREAD")]
        reply: Option<String>,
        /// The name teammates see the note came "via": 1 to 40 letters, digits, spaces or
        /// .-_+()/
        #[arg(long, value_name = "NAME", default_value = "CLI", value_parser = via_name)]
        via: String,
    },
    /// Print one line for Claude Code's status line: how many threads wait on you, or that the
    /// server is offline; nothing when Relevo is not configured. Reads only, at most once every
    /// 15 seconds (a cache in ~/.cache/relevo).
    Statusline,
    /// Serve MCP on stdin and stdout, for Claude Code and any MCP client.
    Mcp {
        /// Push new notes as Claude Code channel events (keeps an inbox request waiting).
        #[arg(long)]
        channel: bool,
    },
    /// Update this relevo, and the adapters registered on this machine, from the clients the
    /// server hands out, signed by the release key (docs/update.md).
    Update {
        /// Only say what it would do; change nothing.
        #[arg(long, conflicts_with = "auto")]
        check: bool,
        /// As the clients run it by themselves: at most one check an hour, silent unless it
        /// installed something or failed. RELEVO_AUTO_UPDATE=0 turns it off.
        #[arg(long)]
        auto: bool,
        /// Register this adapter's folder (with --path), and update it too.
        #[arg(long, value_enum, requires = "path")]
        adapter: Option<relevo::update::Kind>,
        /// The folder of --adapter.
        #[arg(long, value_name = "DIR", requires = "adapter")]
        path: Option<PathBuf>,
    },
    /// Say whether this relevo can use the database, without changing it: as it is, or once
    /// `serve` migrates it (exit 0), or not (exit 1). deploy/install.sh asks before an upgrade.
    CheckDb,
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
enum TeamCommand {
    /// Create a team, with nobody in it. Its name cannot be a person's handle, `team`, `equipo`
    /// or `relevo` (the server's own person).
    Add {
        name: String,
        /// What the team is, in a few words ("Backend").
        #[arg(long)]
        title: Option<String>,
    },
    /// Put active people in a team (threads already open keep their participants).
    Join {
        name: String,
        #[arg(required = true)]
        handles: Vec<String>,
    },
    /// Take people out of a team (they stay in its threads already open).
    Leave {
        name: String,
        #[arg(required = true)]
        handles: Vec<String>,
    },
    /// Delete a team and its membership; its threads keep its name.
    Remove { name: String },
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

/// What may keep an invitation link from working, to say after it: no dist next to the
/// database (or in `RELEVO_DIST`), so the link has no `relevo` to install; or an address only
/// this machine reaches.
fn invite_notes(env: &impl Fn(&str) -> Option<String>, db_file: &str, url: &str) -> Vec<String> {
    let mut notes = vec![];
    let dist = env("RELEVO_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| relevo::dist::dir_for(db_file));
    if !dist.join("manifest.json").is_file() {
        notes.push(format!(
            "no dist in {} (unless the server is started with --dist elsewhere), so the link \
             cannot install relevo. Put the release's relevo-dist-<version>.tar.gz there with \
             deploy/install.sh (docs/install.md, part 2), or invite with --token.",
            dist.display()
        ));
    }
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()));
    let loopback = host.is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if loopback {
        notes.push(format!(
            "{url} is reached from this machine only. Behind a proxy, start the server with \
             --public-url https://… (RELEVO_PUBLIC_URL), or give this link's address with --url."
        ));
    }
    notes
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
             Compare it with the one the server's owner sent you (relevo tls-fingerprint on the \
             server) through a channel you trust. If it differs, someone may be between you and \
             the server: trusting it would send them your token, and from then on your agents \
             would trust them alone."
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

/// Trusts the server (https), gets the token (given, or traded for the invitation code `join`),
/// checks it with `GET /api/people` and saves the server, the token and the certificate to pin,
/// if any.
async fn connect(
    url: &str,
    token: Option<String>,
    fingerprint: Option<String>,
    join: Option<String>,
) -> Result<()> {
    let url = config::normalize_url(url)?;
    let cert = if url.starts_with("https://") {
        trust(&url, fingerprint.as_deref()).await?
    } else if fingerprint.is_some() {
        bail!("--fingerprint is for https:// servers");
    } else {
        None
    };
    let path = config::path(config::env)?;
    // The adapters registered on this machine stay registered with the new server.
    let adapters = config::saved(&path)
        .ok()
        .flatten()
        .map(|c| c.adapters)
        .unwrap_or_default();
    // Before the token goes out (or comes back), not after.
    if let Some(warning) = config::plain_http_warning(&url) {
        eprintln!("{warning}");
    }
    let mut config = config::Config {
        url,
        token: String::new(),
        cert,
        adapters,
    };
    let joined = join.is_some();
    config.token = match join {
        Some(code) => {
            let joining = client::Client::new(&config, Some(String::new()), "/".into())?;
            let joined = joining
                .join(code.trim())
                .await
                .map_err(|e| anyhow::anyhow!("joining {} with the invitation: {e}", config.url))?;
            let token = joined["token"].as_str().unwrap_or_default().to_string();
            // Saved at once: the invitation is used, and its token must not be lost to a failure
            // that comes later.
            config.token = token.clone();
            config::save(&path, &config)?;
            token
        }
        None => connect_token(token)?.trim().to_string(),
    };
    let client = client::Client::new(&config, Some(String::new()), "/".into())?;
    let people = client.people().await.map_err(|e| match joined {
        true => anyhow::anyhow!(
            "joined {}, and saved its token to {}; but checking it failed: {e}. Relevo is set up \
             with it: if this goes on, tell whoever invited you",
            config.url,
            path.display()
        ),
        false => anyhow::anyhow!("checking the token with {}: {e}", config.url),
    })?;
    // The check is a session of its own: end it, or it stays listed until it is forgotten.
    let _ = client.end(std::time::Duration::from_secs(2)).await;
    config::save(&path, &config)?;
    // `team` lists everyone on the server, whatever teams there are.
    let you = people["you"].as_str().unwrap_or_default().to_string();
    let people = people["team"].as_array().map(Vec::len).unwrap_or(0);
    if joined {
        let people = match people {
            1 => "1 person".to_string(),
            n => format!("{n} people"),
        };
        println!("Connected to {} as @{you} ({people})", config.url);
        return Ok(());
    }
    let pinned = if config.cert.is_some() {
        " with its certificate"
    } else {
        ""
    };
    println!(
        "connected to {} as @{you} ({people} people on it); saved{pinned} to {}",
        config.url,
        path.display()
    );
    Ok(())
}

/// `relevo update`: what it did on stdout, what failed on stderr (and exit 1).
async fn update_command(req: &update::Request) -> Result<()> {
    let setup = update::Setup::this_binary()?;
    let steps = update::command(req, &setup, config::env, db::now_unix()).await?;
    for step in &steps {
        if step.failed() {
            eprintln!("{step}");
        } else {
            println!("{step}");
        }
    }
    if steps.iter().any(update::Step::failed) {
        std::process::exit(1);
    }
    Ok(())
}

/// `--via`: what the server takes as `Relevo-Agent` (docs/protocol.md); it ignores anything else.
fn via_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    match relevo::auth::valid_agent(name) {
        true => Ok(name.to_string()),
        false => Err("1 to 40 letters, digits, spaces or .-_+()/".into()),
    }
}

/// `relevo send`: one note, in a session that ends with it. A token keeps at most 20 sessions,
/// so one left behind by each run would push out its person's agents; and a reply holds its
/// thread for the session that wrote it, away from their agents.
async fn send(text: String, reply: Option<String>, via: String) -> Result<()> {
    let c = config::load(config::env)?;
    if let Some(warning) = config::plain_http_warning(&c.url) {
        eprintln!("{warning}");
    }
    let dir = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let client = client::Client::new(&c, std::env::var("RELEVO_CONTEXT").ok(), dir)?;
    let _ = client.agent.set(via);
    let sent = async {
        let mut body = serde_json::Map::new();
        if let Some(thread) = &reply {
            // Read first: a reply handles only the notes that have reached its session.
            client.thread(thread).await?;
            body.insert("thread".into(), thread.as_str().into());
        }
        body.insert("text".into(), text.into());
        client.send(body, "`relevo web`").await
    }
    .await;
    let end = client.end(std::time::Duration::from_secs(2));
    match &sent {
        Ok(_) => {
            if let Err(e) = end.await {
                eprintln!("ending the session: {e}");
            }
        }
        // Refused: no session was recorded. Unreached: ending would most likely fail too, after
        // a wait (a request that timed out may have left its session, which lapses by itself).
        Err(e) if e.unreachable() => {}
        Err(client::Error::Api { code, .. }) if code == "UNAUTHORIZED" => {}
        Err(_) => {
            let _ = end.await;
        }
    }
    let v = sent.map_err(|e| {
        let mut out = e.to_string();
        if let client::Error::Api { code, body, .. } = &e
            && code == "UNKNOWN_PERSON"
        {
            out.push_str("\n\n");
            out.push_str(&relevo::render::team(&body["error"]["valid"], None));
            out.push_str(&relevo::render::teams(&body["error"]["teams"], None));
        }
        anyhow::anyhow!(out.trim_end().to_string())
    })?;
    use std::io::Write;
    let id = v["thread"].as_str().unwrap_or_default();
    // The note is written: a reader gone already is no error, or a script that retries on one
    // would write it twice.
    match writeln!(std::io::stdout(), "{id}") {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
        _ => {}
    }
    let whom = relevo::render::whom(&v, reply.is_none());
    let line = match reply {
        None => whom,
        Some(_) => format!("Replied in thread {id}. {whom}"),
    };
    let _ = writeln!(std::io::stderr(), "{}", line.trim_end());
    Ok(())
}

/// `--tls` (a certificate of its own) and `--tls-cert` (one you have) exclude each other, and
/// `--tls-name` is for the first only. Checked here rather than by clap, which takes
/// `RELEVO_TLS=false` from the environment as `--tls` given.
fn check_tls(tls: bool, names: &[String], cert: Option<&Path>) -> Result<()> {
    if tls && cert.is_some() {
        bail!(
            "--tls (RELEVO_TLS) makes a certificate of its own, and --tls-cert (RELEVO_TLS_CERT) \
             uses yours: give one of them, not both"
        );
    }
    if !tls && !names.is_empty() {
        bail!(
            "--tls-name (RELEVO_TLS_NAME) names the certificate of --tls (RELEVO_TLS=true): give that too"
        );
    }
    Ok(())
}

/// What `serve` speaks TLS with, if anything, and the fingerprint it prints. With a certificate
/// of its own (`--tls`), also that certificate's key pin and fingerprint, for invitation links
/// (the rest of `Public` is left to the caller); one of yours is taken as one the teammates'
/// machines trust.
fn server_tls(
    db_file: &str,
    tls: bool,
    names: &[String],
    cert: Option<&Path>,
    key: Option<&Path>,
) -> Result<(Option<std::sync::Arc<rustls::ServerConfig>>, db::Public)> {
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
            // Only for the names it chose itself: --tls-name says which the owner wants.
            if names.is_empty()
                && let Some(warning) =
                    tls::address_warning(&cert.chain[0], &tls::network_addresses())
            {
                tracing::warn!("{warning}");
            }
            cert
        }
        _ => return Ok((None, db::Public::default())),
    };
    println!(
        "TLS certificate SHA-256 fingerprint: {}",
        cert.fingerprint()
    );
    let mut public = db::Public::default();
    if tls {
        public.pin = Some(tls::key_pin(&cert.chain[0])?);
        public.fingerprint = Some(cert.fingerprint());
    }
    Ok((Some(cert.server_config()?), public))
}

/// How an invitation's link reaches the server at `url`: with the key pin and fingerprint of
/// `serve`'s own certificate (`public`, as `serve` recorded it) only when `url` is `serve`'s own
/// TLS address: https, the port `serve` speaks TLS on, and a host its certificate names (the one
/// stored next to the database `db_file`, if it is still the one `serve` uses). Any other address
/// (a proxy's, say) gets neither: curl checks its certificate as usual.
fn link_address(db_file: &str, public: &db::Public, url: String) -> Result<db::Address> {
    let mut address = db::Address {
        url,
        pin: None,
        fingerprint: None,
    };
    let (Some(pin), Some(fingerprint), Some(port)) =
        (&public.pin, &public.fingerprint, public.port)
    else {
        return Ok(address);
    };
    let parsed = reqwest::Url::parse(&address.url)?;
    let cert = tls::stored(&tls::dir_for(db_file))?;
    let own = parsed.scheme() == "https"
        && parsed.port_or_known_default() == Some(port)
        && cert.is_some_and(|cert| {
            tls::key_pin(&cert.chain[0]).is_ok_and(|p| &p == pin)
                && tls::names_host(&cert.chain[0], parsed.host_str().unwrap_or(""))
        });
    if own {
        address.pin = Some(pin.clone());
        address.fingerprint = Some(fingerprint.clone());
    }
    Ok(address)
}

/// The address teammates reach the server at, for invitation links: `given`, or else the
/// scheme, the first of `names` (`--tls-name`) or the address it listens on (`local`; one of
/// this machine's when it listens on all of them), and its port.
fn public_url(
    given: Option<&str>,
    scheme: &str,
    names: &[String],
    local: SocketAddr,
) -> Result<String> {
    if let Some(given) = given {
        return config::normalize_url(given);
    }
    let host = match names.first() {
        Some(name) => name.clone(),
        None if local.ip().is_unspecified() => tls::network_addresses()
            .first()
            .map_or_else(|| "localhost".to_string(), |ip| ip.to_string()),
        None => local.ip().to_string(),
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    let port = local.port();
    Ok(match (scheme, port) {
        ("https", 443) | ("http", 80) => format!("{scheme}://{host}"),
        _ => format!("{scheme}://{host}:{port}"),
    })
}

/// Posts this version's release notes on the global board, once (`relevo::release`). A failure is
/// logged, and the server starts all the same.
fn release_notes(app: &App, no_release_notes: bool, now: i64) {
    use relevo::release::{self, Outcome};
    let version = env!("CARGO_PKG_VERSION");
    let env = std::env::var("RELEVO_RELEASE_NOTES").ok();
    let post = release::enabled(no_release_notes, env.as_deref());
    match release::announce(&app.db(), app.key(), version, release::CHANGELOG, post, now) {
        Ok(Outcome::Posted(thread)) => {
            tracing::info!(%thread, "posted the release notes of relevo {version} on the global board")
        }
        Ok(Outcome::Announced) => {}
        Ok(Outcome::NotReleased) => tracing::info!(
            "relevo {version} is not a release (the CHANGELOG's first section is not its own, \
             dated): no release notes posted"
        ),
        Ok(Outcome::Off) => tracing::info!(
            "release notes are off: relevo {version} is recorded as announced, nothing posted"
        ),
        Ok(Outcome::NameTaken) => tracing::warn!(
            "a person or a team has the name 'relevo', which this version keeps for the \
             server's own release notes: relevo {version} is recorded as announced, nothing \
             posted"
        ),
        Err(e) => tracing::warn!("could not post the release notes of relevo {version}: {e:#}"),
    }
}

/// `relevo clients`: a tab-separated line per session (handle, label, session, context, client,
/// version, last seen, standing, agent), outdated first, and a summary of them all. With `outdated`,
/// only the outdated and unknown ones are listed.
fn clients(sessions: Vec<db::SessionInfo>, server: &str, outdated: bool) -> (Vec<String>, String) {
    use relevo::release::Standing;
    let mut rows: Vec<(Standing, db::SessionInfo)> = sessions
        .into_iter()
        .map(|s| {
            let version = s.client.as_deref().and_then(|c| c.split_once('/'));
            (Standing::of(version.map(|(_, v)| v), server), s)
        })
        .collect();
    // Stable: within a standing, by handle, most recently seen first.
    rows.sort_by_key(|(standing, _)| *standing);
    let count = |which| rows.iter().filter(|(s, _)| *s == which).count();
    let plural = |n| if n == 1 { "" } else { "s" };
    let current = count(Standing::Current);
    let mut summary = format!("{current} session{} on {server}", plural(current));
    for which in [Standing::Outdated, Standing::Unknown, Standing::Newer] {
        let n = count(which);
        if n > 0 || which != Standing::Newer {
            summary.push_str(&format!(", {n} {}", which.as_str()));
        }
    }
    let lines = rows
        .iter()
        .filter(|(standing, _)| {
            !outdated || matches!(standing, Standing::Outdated | Standing::Unknown)
        })
        .map(|(standing, s)| {
            let (name, version) = s.client_version().unwrap_or(("unknown", "-".to_string()));
            format!(
                "{}\t{}\t{}\t{}\t{name}\t{version}\t{}\t{}\t{}",
                s.handle,
                s.label,
                s.session.as_deref().unwrap_or("default"),
                s.context.as_deref().unwrap_or("general"),
                db::rfc3339(s.last_seen),
                standing.as_str(),
                s.agent.as_deref().unwrap_or("-")
            )
        })
        .collect();
    (lines, summary)
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
            command,
            no_command,
            statusline,
            no_statusline,
            join,
            setup,
        } => {
            connect(&url, token, fingerprint, join).await?;
            let choice = |yes: bool, no: bool| match (yes, no) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            let ok = relevo::setup::run(
                &setup,
                choice(command, no_command),
                choice(statusline, no_statusline),
            )
            .await;
            // An agent that could not be set up: exit 1, so a script (the invitation's) says so.
            if !ok {
                std::process::exit(1);
            }
            return Ok(());
        }
        Command::Setup { agents } => {
            config::load(config::env)?;
            if !relevo::setup::run(&agents, None, None).await {
                std::process::exit(1);
            }
            return Ok(());
        }
        Command::Statusline => {
            // Never fails (a failing command blanks Claude Code's status line), even with stdout
            // closed; and exits at once, not waiting for a name lookup the timeout left behind.
            use std::io::Write;
            let line = relevo::statusline::run().await;
            let mut out = std::io::stdout().lock();
            if !line.is_empty() {
                let _ = writeln!(out, "{line}");
            }
            let _ = out.flush();
            std::process::exit(0);
        }
        Command::Mcp { channel } => return mcp::run(channel).await,
        Command::Send { text, reply, via } => {
            let text = match text.as_str() {
                "-" => std::io::read_to_string(std::io::stdin())?,
                _ => text,
            };
            if text.trim().is_empty() {
                use clap::CommandFactory;
                let mut cli = Cli::command();
                cli.build();
                cli.find_subcommand_mut("send")
                    .expect("send is a subcommand")
                    .error(clap::error::ErrorKind::InvalidValue, "the note is empty")
                    .exit();
            }
            return send(text, reply, via).await;
        }
        Command::Web { port, no_open } => return relevo::web::run(port, no_open).await,
        Command::Update {
            check,
            auto,
            adapter,
            path,
        } => {
            let req = update::Request {
                check,
                auto,
                adapter: adapter.zip(path),
            };
            return update_command(&req).await;
        }
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
    // Nor does checking it.
    if let Command::CheckDb = &cli.command {
        let key_file = relevo::crypto::path(config::env)?;
        let says = match db::check(&db_file, &key_file)? {
            db::Layout::Current => "this version's layout",
            db::Layout::New => "empty: `relevo serve` sets it up",
            layout @ db::Layout::Migrates(_) => &format!(
                "the layout of {}: `relevo serve` migrates it in place when it starts (keep a \
                 `relevo backup` first)",
                layout.made_by()
            ),
        };
        println!("{db_file}: {says}");
        return Ok(());
    }
    // Settings that cannot work are refused before the database is opened, or migrated.
    if let Command::Serve {
        tls,
        tls_name,
        tls_cert,
        ..
    } = &cli.command
    {
        check_tls(*tls, tls_name, tls_cert.as_deref())?;
    }
    // Only the server migrates an older layout, and handles the key: an admin command refuses
    // the one, and needs not the other (it reads no note text nor thread title).
    let (conn, key) = if matches!(cli.command, Command::Serve { .. }) {
        // Logging starts before the database opens, so a migration is logged.
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .init();
        let key_file = relevo::crypto::path(config::env)?;
        let (conn, key) = db::open_migrating(&db_file, &key_file)?;
        (conn, Some(key))
    } else {
        (db::open(&db_file)?, None)
    };
    let now = db::now_unix();
    match cli.command {
        Command::Serve {
            addr,
            team_questions_per_day,
            tls,
            tls_name,
            tls_cert,
            tls_key,
            dist,
            no_release_notes,
            public_url: given_url,
        } => {
            let (tls, mut public) = server_tls(
                &db_file,
                tls,
                &tls_name,
                tls_cert.as_deref(),
                tls_key.as_deref(),
            )?;
            let dist = dist.unwrap_or_else(|| relevo::dist::dir_for(&db_file));
            match relevo::dist::start_note(&dist, env!("CARGO_PKG_VERSION")) {
                Ok(note) => tracing::info!("{note}"),
                Err(warning) => tracing::warn!("{warning}"),
            }
            let key = key.expect("serve opens the database with its key");
            let app = App::with_dist(
                conn,
                Clock::system(),
                team_questions_per_day,
                Some(dist),
                key,
            );
            let listener = tokio::net::TcpListener::bind(addr).await?;
            let scheme = if tls.is_some() { "https" } else { "http" };
            let url = public_url(
                given_url.as_deref(),
                scheme,
                &tls_name,
                listener.local_addr()?,
            )?;
            public.url = Some(url.clone());
            if public.pin.is_some() {
                public.port = Some(listener.local_addr()?.port());
            }
            db::set_public(&app.db(), &public)?;
            tracing::info!(%addr, db = %db_file, %scheme, "relevo listening");
            tracing::info!(
                %url,
                "invitation links point at this address (--public-url changes it)"
            );
            // Posted once the server can serve: bound, with its TLS set up.
            release_notes(&app, no_release_notes, db::now_unix());
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
            token,
            label,
            url,
            bot,
        } => {
            let name = match name {
                Some(name) => name,
                None => db::person_name(&conn, &handle)?,
            };
            if token {
                let label = label.as_deref().unwrap_or("agent");
                let token = db::invite(&conn, &handle, &name, about.as_deref(), label, now)?;
                if bot {
                    db::set_bot(&conn, &handle, true)?;
                }
                print_token(&handle, label, &token);
            } else {
                let url = match url {
                    Some(url) => config::normalize_url(&url)?,
                    None => db::public(&conn)?.url.ok_or_else(|| {
                        anyhow::anyhow!(
                            "the server has not said its address yet: start `relevo serve` (0.7.0 \
                             or later) once, or give it with --url https://…"
                        )
                    })?,
                };
                let address = link_address(&db_file, &db::public(&conn)?, url)?;
                let invitation =
                    db::invite_link(&conn, &handle, &name, about.as_deref(), &address, now)?;
                if bot {
                    db::set_bot(&conn, &handle, true)?;
                }
                println!(
                    "Invited @{handle} ({name}). Send them this link — it works once, for 24 \
                     hours:\n\n  {}/join/{}\n\nThey can open it, or just give it to their \
                     agent (Claude Code, pi, Hermes).",
                    address.url, invitation.code
                );
                for note in invite_notes(&config::env, &db_file, &address.url) {
                    println!("\nNote: {note}");
                }
            }
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
                let bot = if p.bot { ", bot" } else { "" };
                println!("{}\t{}\t{state}{bot}\t{}", p.handle, p.name, p.about);
            }
        }
        Command::Sessions { handle } => {
            for s in db::list_sessions(&conn, handle.as_deref(), now)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    s.handle,
                    s.label,
                    s.session.as_deref().unwrap_or("default"),
                    s.context.as_deref().unwrap_or("general"),
                    db::rfc3339(s.last_seen),
                    s.client_version()
                        .map_or("unknown".to_string(), |(name, v)| format!("{name}/{v}")),
                    s.agent.as_deref().unwrap_or("-")
                );
            }
        }
        Command::Clients { handle, outdated } => {
            let (lines, summary) = clients(
                db::list_sessions(&conn, handle.as_deref(), now)?,
                env!("CARGO_PKG_VERSION"),
                outdated,
            );
            for line in lines {
                println!("{line}");
            }
            println!("{summary}");
        }
        Command::About { handle, about } => {
            db::set_about(&conn, &handle, &about)?;
            println!("{handle}: {about}");
        }
        Command::Bot { handle, mark } => {
            let bot = mark == "on";
            db::set_bot(&conn, &handle, bot)?;
            println!("{handle}: {}", if bot { "bot" } else { "not a bot" });
        }
        Command::Deactivate { handle } => {
            let revoked = db::deactivate(&conn, &handle, now)?;
            println!("deactivated {handle}, revoked {revoked} token(s)");
        }
        Command::Team { command } => match command {
            TeamCommand::Add { name, title } => {
                db::add_team(&conn, &name, title.as_deref(), now)?;
                println!(
                    "added team {name}, with nobody in it yet: relevo team join {name} <handle>…"
                );
            }
            TeamCommand::Join { name, handles } => {
                let done = db::join_team(&conn, &name, &handles)?;
                for (handle, done) in handles.iter().zip(done) {
                    match done {
                        db::Membership::Changed => println!("{handle} joined {name}"),
                        db::Membership::Already => println!("{handle} is in {name} already"),
                    }
                }
            }
            TeamCommand::Leave { name, handles } => {
                let done = db::leave_team(&conn, &name, &handles)?;
                for (handle, done) in handles.iter().zip(done) {
                    match done {
                        db::Membership::Changed => println!("{handle} left {name}"),
                        db::Membership::Already => println!("{handle} was not in {name}"),
                    }
                }
            }
            TeamCommand::Remove { name } => {
                db::remove_team(&conn, &name)?;
                println!("removed team {name}; its threads keep its name");
            }
        },
        Command::Teams => {
            for t in db::list_teams(&conn)? {
                let members: Vec<String> = t
                    .members
                    .iter()
                    .map(|(handle, active)| {
                        if *active {
                            handle.clone()
                        } else {
                            format!("{handle} (inactive)")
                        }
                    })
                    .collect();
                println!("{}\t{}\t{}", t.name, t.title, members.join(", "));
            }
        }
        Command::Connect { .. }
        | Command::Setup { .. }
        | Command::Statusline
        | Command::Mcp { .. }
        | Command::Send { .. }
        | Command::Web { .. }
        | Command::Update { .. }
        | Command::Backup { .. }
        | Command::CheckDb
        | Command::TlsFingerprint => {
            unreachable!("handled above")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_says_when_the_link_cannot_work() {
        let dir = std::env::temp_dir().join(db::random_id("relevo-invite-"));
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        let db_file = dir.join("relevo.db").to_string_lossy().to_string();
        let none = |_: &str| None;
        // No dist, and an address only this machine reaches.
        let notes = invite_notes(&none, &db_file, "http://127.0.0.1:7878");
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("cannot install relevo"), "{notes:?}");
        assert!(notes[1].contains("--public-url"), "{notes:?}");
        assert_eq!(invite_notes(&none, &db_file, "https://[::1]:7878").len(), 2);
        assert_eq!(invite_notes(&none, &db_file, "https://localhost").len(), 2);
        // A dist, and an address teammates reach: nothing to say.
        std::fs::write(dir.join("dist/manifest.json"), "{}").unwrap();
        assert!(invite_notes(&none, &db_file, "https://192.168.1.50:7878").is_empty());
        assert!(invite_notes(&none, &db_file, "https://[2001:db8::1]:7878").is_empty());
        assert!(invite_notes(&none, &db_file, "https://relevo.example.com").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

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

    fn tls_files(cli: &Cli) -> (Option<PathBuf>, Option<PathBuf>) {
        match &cli.command {
            Command::Serve {
                tls_cert, tls_key, ..
            } => (tls_cert.clone(), tls_key.clone()),
            _ => panic!("not serve"),
        }
    }

    #[test]
    fn send_takes_the_text_a_thread_and_a_via() {
        let send = |args: &[&str]| match Cli::try_parse_from([&["relevo", "send"], args].concat()) {
            Ok(Cli {
                command: Command::Send { text, reply, via },
                ..
            }) => Ok((text, reply, via)),
            Ok(_) => panic!("not send"),
            Err(e) => Err(e.exit_code()),
        };
        assert_eq!(
            send(&["@ana hi"]),
            Ok(("@ana hi".into(), None, "CLI".into()))
        );
        assert_eq!(
            send(&["--reply", "r_1", "--via", " GitHub Actions ", "-"]),
            Ok(("-".into(), Some("r_1".into()), "GitHub Actions".into()))
        );
        // A note may start with a hyphen; - alone is stdin.
        assert_eq!(
            send(&["--via", "CI", "-5 tests fail"]),
            Ok(("-5 tests fail".into(), None, "CI".into()))
        );
        // A name the server would ignore, and a missing note, are misuse.
        assert_eq!(send(&["--via", "a@b", "hi"]), Err(2));
        assert_eq!(send(&["--via", " ", "hi"]), Err(2));
        assert_eq!(send(&["--via", &"x".repeat(41), "hi"]), Err(2));
        assert_eq!(send(&[]), Err(2));
    }

    /// One test, because it changes the process environment.
    #[test]
    fn defaults_and_environment() {
        const VARS: [&str; 8] = [
            "RELEVO_DB",
            "RELEVO_ADDR",
            "RELEVO_TEAM_QUESTIONS_PER_DAY",
            "RELEVO_TLS",
            "RELEVO_TLS_NAME",
            "RELEVO_TLS_CERT",
            "RELEVO_TLS_KEY",
            "RELEVO_DIST",
        ];
        // SAFETY: no other test in this binary reads or writes the environment.
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };

        let cli = Cli::try_parse_from(["relevo", "serve"]).unwrap();
        assert_eq!(cli.db, None);
        assert_eq!(serve_args(&cli), ("127.0.0.1:7878".parse().unwrap(), 5));
        assert_eq!(tls_args(&cli), (false, vec![]));

        // TLS: names only with --tls, a certificate of yours only with its key, not both.
        let parse = |args: &[&str]| {
            let cli = Cli::try_parse_from([&["relevo", "serve"], args].concat()).ok()?;
            let ok = match &cli.command {
                Command::Serve {
                    tls,
                    tls_name,
                    tls_cert,
                    ..
                } => check_tls(*tls, tls_name, tls_cert.as_deref()).is_ok(),
                _ => panic!("not serve"),
            };
            ok.then_some(cli)
        };
        let cli = parse(&["--tls", "--tls-name", "a.office", "--tls-name", "10.0.0.5"]).unwrap();
        assert_eq!(
            tls_args(&cli),
            (true, vec!["a.office".into(), "10.0.0.5".into()])
        );
        assert!(parse(&["--tls-name", "a.office"]).is_none());
        assert!(parse(&["--tls-cert", "c.pem"]).is_none());
        assert!(parse(&["--tls-key", "k.pem"]).is_none());
        assert!(parse(&["--tls", "--tls-cert", "c.pem", "--tls-key", "k.pem"]).is_none());
        assert!(parse(&["--tls-cert", "c.pem", "--tls-key", "k.pem"]).is_some());

        let cli = Cli::try_parse_from(["relevo", "invite", "ana", "--name", "Ana"]).unwrap();
        let Command::Invite {
            label,
            about,
            token,
            ..
        } = cli.command
        else {
            panic!("not invite")
        };
        assert_eq!((label, about, token), (None, None, false));
        // A label is for the token of --token, and a link's address is not.
        assert!(Cli::try_parse_from(["relevo", "invite", "ana", "--label", "x"]).is_err());
        assert!(
            Cli::try_parse_from(["relevo", "invite", "ana", "--token", "--url", "https://x"])
                .is_err()
        );

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
        assert_eq!(tls_files(&cli), (None, None));
        // A certificate of yours from the environment, both files or neither, and not with
        // RELEVO_TLS on.
        unsafe { std::env::set_var("RELEVO_TLS_CERT", "/etc/relevo/fullchain.pem") };
        assert!(parse(&[]).is_none());
        unsafe { std::env::set_var("RELEVO_TLS_KEY", "/etc/relevo/privkey.pem") };
        // RELEVO_TLS=false, as the drop-in may still say.
        let Some(cli) = parse(&[]) else {
            panic!("RELEVO_TLS_CERT and RELEVO_TLS_KEY refused")
        };
        assert_eq!(
            tls_files(&cli),
            (
                Some("/etc/relevo/fullchain.pem".into()),
                Some("/etc/relevo/privkey.pem".into())
            )
        );
        assert!(!tls_args(&cli).0);
        unsafe { std::env::set_var("RELEVO_TLS", "true") };
        assert!(parse(&[]).is_none());
        unsafe {
            std::env::set_var("RELEVO_TLS", "false");
            std::env::remove_var("RELEVO_TLS_CERT");
            std::env::remove_var("RELEVO_TLS_KEY");
        }
        // Flags win over the environment, and --db works after the subcommand too.
        let cli =
            Cli::try_parse_from(["relevo", "serve", "--db", "x.db", "--addr", "[::1]:1"]).unwrap();
        assert_eq!(cli.db.as_deref(), Some("x.db"));
        assert_eq!(serve_args(&cli).0, "[::1]:1".parse().unwrap());
        unsafe { VARS.iter().for_each(|v| std::env::remove_var(v)) };
    }
}
