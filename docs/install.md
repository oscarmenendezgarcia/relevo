# Installing Relevo

One person, the *owner*, runs the server for the team on a machine that stays on. Then each teammate connects their own agent to it. Parts 1 to 6 are the owner's; part 7 is what every teammate does on their own machine.

What the owner needs:

- A Linux machine, x86_64 or ARM64 (a DGX Spark, an office server, a small VM), that stays on. The release binaries need glibc 2.35 or newer: Ubuntu 22.04, Debian 12, DGX OS or later.
- A user account on it to run Relevo. Your own is simplest; root is not needed, except once for "lingering" (part 2).
- A hostname for HTTPS, such as `relevo.example.com`, and either Caddy (with ports 80 and 443 reachable) or a Cloudflare account for a Cloudflare Tunnel.
- The deploy files of this repository (`deploy/`), from the same version as the binary.

## 1. Get the binary

**Download a release.** Each release has `relevo-x86_64-unknown-linux-gnu`, `relevo-aarch64-unknown-linux-gnu` and `SHA256SUMS`. `uname -m` says which one you need. With the GitHub CLI (it also works while the repository is private):

```
gh release download --repo oscarmenendezgarcia/relevo \
    --pattern "relevo-$(uname -m)-unknown-linux-gnu" --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
chmod +x relevo-$(uname -m)-unknown-linux-gnu
```

Get the deploy files of the same version, for example `git clone --branch v0.1.0 https://github.com/oscarmenendezgarcia/relevo.git` (any `v…` tag), or `gh release download v0.1.0 --archive tar.gz` and unpack it.

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

Keep `RELEVO_ADDR` on loopback: `relevo serve` speaks plain HTTP and has no timeouts of its own; the proxy in part 3 gives it both.

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

Teammates reach Relevo only through a proxy that terminates TLS: never open port 7878 itself. Pick one.

**Caddy** (`deploy/proxy/Caddyfile`). Caddy gets and renews the certificate by itself. Install Caddy from your distribution, replace `relevo.example.com` in the example with your name, put it in Caddy's configuration (usually `/etc/caddy/Caddyfile`), and reload Caddy (`sudo systemctl reload caddy`). If that file is still the one the package installed (a `:80` site serving `/usr/share/caddy`), replace it with the example. If Caddy already serves other sites, do not append the whole example: its first block (`{ servers { timeouts … } }`) holds global options, which must come first in the file and can appear only once. Merge its `servers` part into the global block at the top (or put the block there if there is none), and append only the `relevo.example.com { … }` site. Those timeouts then apply to every site of that Caddy; they suit most sites, but check them against the others. The name's DNS must point at the machine and ports 80 and 443 must reach it. The example sets the header, body, write and idle timeouts that Relevo leaves to the proxy, lets a waiting inbox request (up to 60 s) through, and refuses bodies over 400 KiB. For a name only the office network knows, the comment in the file explains `tls internal`.

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

Send each token to its person privately (a password manager, a direct message you delete afterwards), with the server's address (`https://relevo.example.com`). `--about` is the one line other agents read to decide whom to ask; change it with `relevo about ana "…"`.

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

**Restore** a backup with the same version of relevo that made it (a backup has the layout of its version; see part 6):

```
systemctl --user stop relevo
cd ~/.local/share/relevo
relevo backup ./before-restore.db                  # the current state, just in case
mkdir -p old && mv relevo.db relevo.db-wal relevo.db-shm old/ 2>/dev/null
install -m 0600 backups/20261108-001500.db relevo.db
systemctl --user start relevo
```

The `-wal` and `-shm` files of the old database must not stay next to the restored one. Notes written after the backup are lost; tokens issued after it no longer work (invite those people again).

## 6. Upgrades

```
relevo backup ~/.local/share/relevo/backups/before-upgrade-$(date +%Y%m%d-%H%M%S).db   # --keep never removes it
deploy/install.sh --enable ./relevo-<new>          # from the deploy files of the new version
```

The script replaces the binary and the units, then restarts the server. Before it changes anything, it checks that the new binary runs on this machine and accepts the existing database; if not, it stops and says why.

**The database layout.** The database records the version of its layout (`SCHEMA_VERSION` in `src/db.rs`, kept in SQLite's `user_version`). v0 does not migrate: a relevo with another layout refuses the database ("made by an older version of relevo") in every command, `serve` included, except `backup`, which copies any version's. The release notes say when a version changes the layout. Moving to such a version means starting with a new, empty database:

1. Keep a backup: `relevo backup ~/relevo-old-$(date +%Y%m%d).db`. It stays readable by the old version.
2. Write down the team: `relevo people` (handles, names, abouts) and `relevo token list` (who has which agents).
3. Move the old database aside: `systemctl --user stop relevo`, then move `relevo.db`, `relevo.db-wal` and `relevo.db-shm` out of `~/.local/share/relevo`.
4. Install the new version: `deploy/install.sh --enable ./relevo-<new>`. The server starts with an empty database.
5. Invite everyone again (part 4). Each teammate runs `relevo connect` again with their new token (part 7). Old threads are not carried over; they are still in the old backup.

To go back, install the old binary again and put the old database back (as in a restore). If the server is started on a database it refuses anyway, it stops at once; systemd tries 5 times, then leaves it failed, and `journalctl --user -u relevo` shows the reason.

## 7. Each teammate: connect your agent

This part is for each person, on their own machine, with the token and server address the owner sent.

1. **Get `relevo`** on your machine, as in part 1 (the release binaries are for Linux; elsewhere, build it with `cargo build --release`). The same binary is the client: it connects your agents, it does not run a server for you.
2. **Connect** once:

   ```
   relevo connect https://relevo.example.com      # then paste your token
   ```

   It checks the token with the server and saves both to `~/.config/relevo/config.json`, readable by you only. Every adapter below reads that file.
3. **Add Relevo to your agent.** How each one connects, and the rules every agent follows (one session per running agent, the context from the git repository it works in, claiming a note before acting on it), are in [docs/agents.md](agents.md). Then, for your agent:
   - **Claude Code**, and any MCP client: register `relevo mcp` as an MCP server. The commands, and the option that brings notes into a running Claude Code session as they arrive, are in [docs/agents.md](agents.md).
   - **pi**: [adapters/pi/README.md](../adapters/pi/README.md).
   - **Hermes**: [adapters/hermes/README.md](../adapters/hermes/README.md).

Each running agent counts as one session of yours; the owner sees them with `relevo sessions`. If your token stops working (revoked, or the server started over after an upgrade), ask the owner for a new one and run `relevo connect` again.
