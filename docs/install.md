# Installing Relevo

One person, the *owner*, runs the server for the team on a machine that stays on. Then each teammate connects their own agent to it. Parts 1 to 6 are the owner's; part 7 is what every teammate does on their own machine.

What the owner needs:

- A Linux machine, x86_64 or ARM64 (a DGX Spark, an office server, a small VM), that stays on. The release binaries need glibc 2.35 or newer: Ubuntu 22.04, Debian 12, DGX OS or later.
- A user account on it to run Relevo. Your own is simplest; root is not needed, except once for "lingering" (part 2).
- For HTTPS, nothing more: `relevo serve --tls` speaks it itself, with a certificate of its own (part 3). A proxy in front is optional: a hostname such as `relevo.example.com` with Caddy (ports 80 and 443 reachable), or a Cloudflare account for a Cloudflare Tunnel.
- The deploy files of this repository (`deploy/`), from the same version as the binary.

## 1. Get the binary

**Download a release.** Each release has `relevo-x86_64-unknown-linux-gnu`, `relevo-aarch64-unknown-linux-gnu` and `SHA256SUMS`. `uname -m` says which one you need. With the GitHub CLI (it also works while the repository is private):

```
gh release download --repo oscarmenendezgarcia/relevo \
    --pattern "relevo-$(uname -m)-unknown-linux-gnu" --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
chmod +x relevo-$(uname -m)-unknown-linux-gnu
```

Get the deploy files of the same version, for example `git clone --branch v0.1.0 https://github.com/oscarmenendezgarcia/relevo.git` (any `v…` tag), or `gh release download v0.1.0 --repo oscarmenendezgarcia/relevo --archive tar.gz` and unpack it.

**Or build it** from a checkout, with Rust (`rustup`, stable) and a C compiler (`build-essential` on Debian and Ubuntu; SQLite is compiled in):

```
cargo build --release --locked
# the binary is target/release/relevo
```

## 2. Install the server

From the checkout, first see what would happen, then do it:

```
deploy/install.sh --dry-run ./relevo-aarch64-unknown-linux-gnu
deploy/install.sh --enable  ./relevo-aarch64-unknown-linux-gnu
```

It puts the binary in `~/.local/bin/relevo`, creates `~/.local/share/relevo` (the database lives there) and `~/.local/share/relevo/backups`, both readable by you only, and installs three systemd user units in `~/.config/systemd/user/`:

- `relevo.service`: `relevo serve` on `127.0.0.1:7878`, database `~/.local/share/relevo/relevo.db`, restarted if it fails, at most 128 MB of memory, allowed to write only in its data directory, logging to the journal.
- `relevo-backup.timer` and `relevo-backup.service`: a backup every day (part 5).

`--enable` then runs `systemctl --user daemon-reload` and `systemctl --user enable --now relevo.service relevo-backup.timer`. Without it, the script prints those commands for you to run. Running the script again is safe: it changes only what differs. `--no-systemd` installs the binary and the data directory only, for running the server some other way.

Check that it runs:

```
systemctl --user status relevo
curl -s http://127.0.0.1:7878/health          # {"ok":true}
journalctl --user -u relevo                   # its log: counts and ids, never note text or tokens
```

A user's units run while that user is logged in. For Relevo to start at boot and keep running after you log out, someone with admin rights enables lingering for your user, once. It is your call; the script never uses `sudo`:

```
sudo loginctl enable-linger $USER
```

The admin commands (`invite`, `people`, `backup`, …) open the database directly, so they must use the same file as the server. Put this in your `~/.profile` (and `~/.local/bin` in your `PATH` if it is not):

```
export RELEVO_DB=~/.local/share/relevo/relevo.db
```

`~/.profile` is read at the next login, so run the same `export` in the shell you are in now too, and check that it finds the server's database: `relevo people` runs without an error (and lists nobody yet). Without `RELEVO_DB`, the admin commands refuse to run rather than create a new `./relevo.db` the server never reads.

**Settings.** Change them with a drop-in, which upgrades leave alone: `systemctl --user edit relevo` and, for example,

```
[Service]
Environment=RELEVO_TEAM_QUESTIONS_PER_DAY=10
```

Saving the drop-in does not restart the running server: `systemctl --user restart relevo` to use it.

As installed, the server speaks plain HTTP on loopback only: nobody else can reach it yet. Part 3 turns on HTTPS. Whatever the transport, the server enforces its own timeouts and connection limits (see `docs/protocol.md`).

**Sandbox.** `ProtectSystem=`, `ReadWritePaths=` and `PrivateTmp=` need user namespaces in a user unit. Where the system restricts them (Ubuntu 24.04 does, with `kernel.apparmor_restrict_unprivileged_userns=1`), systemd cannot set that part up; it then normally starts the service without it, and the memory limit and the other restrictions still apply. If the service fails to start instead with status `226/NAMESPACE`, turn those three off with `systemctl --user edit relevo` (and the same for `relevo-backup`):

```
[Service]
ProtectSystem=no
ReadWritePaths=
PrivateTmp=no
```

Then start it again (after failed starts systemd may have stopped trying, so clear that first):

```
systemctl --user reset-failed relevo
systemctl --user start relevo
```

## 3. Expose it with HTTPS

Teammates reach Relevo over HTTPS only. Pick one way.

### HTTPS in Relevo itself (the simplest, for an office network or a VPN)

`relevo serve --tls` speaks HTTPS with standard TLS (rustls) and a certificate of its own: no proxy, no domain, no Cloudflare. Turn it on with a drop-in (`systemctl --user edit relevo`):

```
[Service]
Environment=RELEVO_TLS=true
Environment=RELEVO_ADDR=0.0.0.0:7878
```

and restart it: `systemctl --user restart relevo`. The first start makes a self-signed certificate (valid 10 years) for `localhost`, the loopback addresses, this machine's host name and its network addresses, and keeps it next to the database in `~/.local/share/relevo/relevo.db.tls/` (the key readable by you only). Later starts reuse it. (The unit allows `AF_NETLINK` sockets for this: reading the machine's addresses needs one.) When the certificate, made without `RELEVO_TLS_NAME`, has none of the machine's network addresses, or none could be read, every start logs a warning saying so; then set the names as in **Names** below. At every start the server prints the certificate's SHA-256 fingerprint; see it again with

```
relevo tls-fingerprint                             # with RELEVO_DB set, as in part 2
journalctl --user -u relevo | grep fingerprint
```

Check it from the machine itself: `curl --cacert ~/.local/share/relevo/relevo.db.tls/cert.pem https://localhost:7878/health`.

**Open the port to the office network or the VPN only**, never to the internet. With `ufw`, for an office network `192.168.1.0/24` (someone with admin rights runs it):

```
sudo ufw allow from 192.168.1.0/24 to any port 7878 proto tcp
```

(with `firewalld`: a rich rule for that source and port, in the zone of that network). A VPN such as Tailscale or WireGuard works the same way: allow its range, or its interface.

**Teammates behind one NAT** (a VPN subnet router, a shared office uplink in front of the server) reach it from one address, and share its 64 connections: each running agent keeps one waiting inbox request and sometimes a second connection, so that is room for about 30 agents at once. Beyond it, new connections are closed unanswered.

**Give teammates the address and the fingerprint**: `https://<this machine's address or name>:7878`, and the fingerprint from `relevo tls-fingerprint`, through a channel you trust (in person, or the team's chat). On their first `relevo connect` they compare it with what `connect` shows; from then on their machine trusts that certificate alone for Relevo (part 7).

**Names.** The address teammates use must be one the certificate has. If they use another one (a DNS name the office gives this machine, an address added later), add the names, comma-separated, in the same drop-in:

```
Environment=RELEVO_TLS_NAME=relevo.office,192.168.1.50
```

Names the stored certificate lacks make a new one at the next start (the log says so) with a new fingerprint, and every teammate runs `relevo connect` again with it. Give the machine a fixed address (or a name) so that this stays rare.

**A certificate you already have** (from Let's Encrypt, or the office's own authority) is used instead with `--tls-cert` and `--tls-key`; then teammates' machines trust it through their authorities and `connect` asks nothing. In the drop-in:

```
[Service]
Environment=RELEVO_ADDR=0.0.0.0:7878
ExecStart=
ExecStart=%h/.local/bin/relevo serve --tls-cert /path/to/fullchain.pem --tls-key /path/to/privkey.pem
```

The key must be readable by your user. Renewed files are read at the next restart.

### Or a proxy in front (optional)

A proxy that terminates TLS stays possible, and suits a public hostname. Then leave `relevo serve` as installed (plain HTTP on loopback, TLS off) and never open port 7878 itself. Relevo's cap of 64 connections from one address does not count loopback, where all the proxy's connections come from; its cap of 1,024 in all still applies.

**Caddy** (`deploy/proxy/Caddyfile`). Caddy gets and renews the certificate by itself. Install Caddy from your distribution, replace `relevo.example.com` in the example with your name, put it in Caddy's configuration (usually `/etc/caddy/Caddyfile`), and reload Caddy (`sudo systemctl reload caddy`). If that file is still the one the package installed (a `:80` site serving `/usr/share/caddy`), replace it with the example. If Caddy already serves other sites, do not append the whole example: its first block (`{ servers { timeouts … } }`) holds global options, which must come first in the file and can appear only once. Merge its `servers` part into the global block at the top (or put the block there if there is none), and append only the `relevo.example.com { … }` site. Those timeouts then apply to every site of that Caddy; they suit most sites, but check them against the others. The name's DNS must point at the machine and ports 80 and 443 must reach it. The example sets header, body, write and idle timeouts like Relevo's own, lets a waiting inbox request (up to 60 s) through, and refuses bodies over 400 KiB. For a name only the office network knows, the comment in the file explains `tls internal`.

**Cloudflare Tunnel** (`deploy/proxy/cloudflared.yml`). `cloudflared` connects out to Cloudflare, so no port is opened and the certificate is Cloudflare's. The comments in the file list the commands that create the tunnel and its DNS name; the configuration goes in `~/.cloudflared/relevo.yml`. Started by hand, `cloudflared` stops when you log out, so run it as a user service, like Relevo (it needs lingering too, part 2):

```
install -m 0644 deploy/proxy/relevo-tunnel.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now relevo-tunnel
journalctl --user -u relevo-tunnel                 # "Registered tunnel connection"
```

If the machine already runs `cloudflared` for other names, use that tunnel rather than starting a second one, and skip `relevo-tunnel.service`: add the Relevo rule to its configuration, before its final catch-all rule, give the name a DNS record pointing at that tunnel (`cloudflared tunnel route dns <that-tunnel> relevo.example.com`), and restart that `cloudflared` the way it is run there so that it reads the new rule. Cloudflare ends a request whose answer has not started within 100 s; Relevo's waits are at most 60 s, so they fit.

*Cloudflare Access* (optional) can put a login in front of the name, but agents cannot log in through a browser, and the adapters do not send Access service-token headers. A policy that asks for a login blocks every agent. Use it only with rules an agent passes without a browser (the office's IP ranges, devices enrolled in Cloudflare WARP), or let Relevo's own tokens be the gate.

Check it from another machine: `curl -s https://relevo.example.com/health`.

## 4. Invite people

Each person gets a token per agent. The token is printed once; only its hash is stored:

```
relevo invite ana --name "Ana" --about "payments backend" --label "Claude Code laptop"
relevo token add ana --label "pi desktop"        # another agent of hers
```

Send each token to its person privately (a password manager, a direct message you delete afterwards), with the server's address (`https://192.168.1.50:7878` with `relevo serve --tls`, or `https://relevo.example.com` behind a proxy) and, with `relevo serve --tls`, the certificate's fingerprint (`relevo tls-fingerprint`). `--about` is the one line other agents read to decide whom to ask; change it with `relevo about ana "…"`.

Day to day:

```
relevo people                     # everyone, active or not
relevo sessions [ana]             # agents seen in the last 7 days, with their context
relevo token list [ana]
relevo token revoke t_…           # a lost laptop
relevo deactivate ana             # someone leaves: every token of theirs stops working
```

## 5. Backups and restore

The database holds every note in plain text, so backups are as private as the database: `relevo backup` creates them readable by you only.

`relevo-backup.timer` takes one a day (at a random moment in the half hour after midnight, or at the next start if the machine was off) into `~/.local/share/relevo/backups/YYYYMMDD-HHMMSS.db` (UTC), and keeps the newest 14. To see it work:

```
systemctl --user list-timers relevo-backup.timer
systemctl --user start relevo-backup               # one now
journalctl --user -u relevo-backup
```

A backup by hand, at any time, with the server running:

```
relevo backup ~/relevo-before-change.db
```

`relevo backup` uses SQLite's `VACUUM INTO`: a consistent copy, without stopping the server. Do not copy `relevo.db` with `cp` or `rsync` while the server runs: the latest changes may still be in `relevo.db-wal`, and the copy can be missing them or broken. Copy the *backups* elsewhere instead (another disk, another machine), encrypted if they leave the office.

**Restore** a backup with the version of relevo that made it, or a later one whose server migrates its layout (a backup has the layout of its version; see part 6):

```
systemctl --user stop relevo
cd ~/.local/share/relevo
relevo backup ./before-restore.db                  # the current state, just in case
mkdir -p old && mv relevo.db relevo.db-wal relevo.db-shm old/ 2>/dev/null
install -m 0600 backups/20261108-001500.db relevo.db
systemctl --user start relevo
```

The `-wal` and `-shm` files of the old database must not stay next to the restored one. The certificate of `relevo serve --tls` is not in the backups: it stays in `relevo.db.tls/`. Restoring on another machine, copy that directory too (keeping it 0700 and the key 0600) to keep the same fingerprint, or let the server make a new one and have everyone run `relevo connect` again. Notes written after the backup are lost; tokens issued after it no longer work (invite those people again).

## 6. Upgrades

```
deploy/install.sh --enable ./relevo-<new>          # from the deploy files of the new version
```

Before it changes anything, the script checks that the new binary runs on this machine and accepts the existing database, as it is or once the server migrates it (`relevo check-db`, which only reads it); if not, it stops and says why. Then, when the binary differs from the installed one, it copies the database with `relevo backup` into `~/.local/share/relevo/backups/before-upgrade-<UTC date and time>.db` (the daily backups' `--keep` never removes it: it is not named like them), puts the new binary and units in place, and restarts the server if it was running. It never migrates the database itself.

**The database layout.** The database records the version of its layout (`SCHEMA_VERSION` in `src/db.rs`, kept in SQLite's `user_version`); the release notes say when a version changes it. Only `relevo serve` migrates an older layout it knows, in place, when it starts. Every other command (`invite`, `people`, …) refuses such a database, writing nothing, and says to start the server first; `relevo backup` copies a database of any version. A layout the binary does not know (a newer version's, or one from before 0.1.0) is refused by every command, `serve` included: if the server is started on it anyway, it stops at once, systemd tries 5 times and then leaves it failed, and `journalctl --user -u relevo` shows the reason.

**From 0.2.x to 0.3.0.** 0.3.0 moves the database from layout 5 to 6, in place: people, tokens, threads and notes stay, so there is nobody to invite again, and nobody runs `relevo connect` again.

1. Back up, with the server running: `relevo backup ~/relevo-before-0.3.0.db`. (The script takes one too; this one you name and keep where you like.) It stays readable by 0.2.x.
2. Stop the server: `systemctl --user stop relevo`.
3. Replace the binary, from the 0.3.0 deploy files: `deploy/install.sh --enable ./relevo-<0.3.0>`. It says the database has "the layout of relevo 0.1.0 to 0.2.x", backs it up, installs, and starts the server.
4. The server migrates the database as it starts. Check: `systemctl --user status relevo`, then `relevo people` (until the server has started, admin commands refuse the old layout).

After the migration, an open direct thread that 0.2.x showed as waiting for someone's reply stays pending for them, including notes their agents fetched and never answered under 0.2.x; each person's first new session gets those once as new notes. Everything else counts as handled. Teammates then update their own `relevo` binary and adapters (part 7): 0.2.x clients keep working against 0.3.0, but do not show what is pending and cannot mark a thread done.

To go back to 0.2.x, the backup first, then the binary (0.2.x refuses the migrated database, and 0.3.0 would migrate the backup again): `systemctl --user stop relevo`; in `~/.local/share/relevo`, move `relevo.db`, `relevo.db-wal` and `relevo.db-shm` aside and put the backup from step 1 in their place (`install -m 0600 ~/relevo-before-0.3.0.db relevo.db`); then run `deploy/install.sh --enable ./relevo-<0.2.x>` from the 0.2.x deploy files, which starts the server. Notes written since the backup are lost.

## 7. Each teammate: connect your agent

This part is for each person, on their own machine, with the token and server address the owner sent.

1. **Get `relevo`** on your machine, as in part 1 (the release binaries are for Linux; elsewhere, build it with `cargo build --release`). The same binary is the client: it connects your agents, it does not run a server for you.
2. **Connect** once:

   ```
   relevo connect https://192.168.1.50:7878       # the address the owner sent; then paste your token
   ```

   If the server uses its own certificate (`relevo serve --tls`), `connect` first shows that certificate's SHA-256 fingerprint and asks whether to trust it: say yes only if it is the fingerprint the owner sent. Compare it for real: if it differs, someone may be between you and the server, and yes would send them your token and make your agents trust them instead of the server. (`--fingerprint <sha256>` gives it without asking, for scripts.) It then checks the token with the server and saves the address, the token and that certificate to `~/.config/relevo/config.json`, readable by you only. Every adapter below reads that file and trusts that certificate alone for Relevo: if the server ever shows another one, they refuse it until you run `relevo connect` again (do so only when the owner says the certificate changed, with the new fingerprint). Behind a proxy with a public certificate, `connect` asks nothing.
3. **Add Relevo to your agent.** How each one connects, and the rules every agent follows (one session per running agent, the context from the git repository it works in, claiming a note before acting on it), are in [docs/agents.md](agents.md). Then, for your agent:
   - **Claude Code**, and any MCP client: register `relevo mcp` as an MCP server. The commands, and the option that brings notes into a running Claude Code session as they arrive, are in [docs/agents.md](agents.md).
   - **pi**: [adapters/pi/README.md](../adapters/pi/README.md).
   - **Hermes**: [adapters/hermes/README.md](../adapters/hermes/README.md).

Each running agent counts as one session of yours; the owner sees them with `relevo sessions`. If your token stops working (revoked, or the server started over after an upgrade), ask the owner for a new one and run `relevo connect` again.
