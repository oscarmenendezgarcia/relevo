# Updating the clients by themselves

From 0.4.0, the team's server hands out the clients of its own version, and every client updates itself from it. To update the team, the owner updates the server; the teammates' `relevo` binary, pi extension and Hermes plugin follow on their own. Nobody needs access to the GitHub repository.

## What is handed out

Every release builds, besides the server binaries, a **dist**: the `relevo` binary for each target (`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, and `aarch64-apple-darwin`, macOS on Apple Silicon; there is none for Intel Macs), the pi extension (`relevo-pi-<version>.tar.gz`, the contents of `adapters/pi`) and the Hermes plugin (`relevo-hermes-<version>.tar.gz`, the contents of `adapters/hermes/relevo`), and a **manifest** that lists them:

```json
{ "version": "0.4.0",
  "files": [ { "name": "relevo-aarch64-unknown-linux-gnu", "kind": "binary", "target": "aarch64-unknown-linux-gnu", "size": 9036312, "sha256": "…" },
             { "name": "relevo-pi-0.4.0.tar.gz", "kind": "pi", "size": 51200, "sha256": "…" },
             { "name": "relevo-hermes-0.4.0.tar.gz", "kind": "hermes", "size": 40960, "sha256": "…" } ] }
```

**The manifest is signed.** The release workflow signs `manifest.json` with an Ed25519 key kept as the secret `RELEVO_SIGNING_KEY` of the GitHub environment `release-signing` (see [The signing key](#the-signing-key)) and publishes the 64-byte signature as `manifest.sig`. The public key is compiled into every `relevo` binary. A client installs nothing whose manifest does not carry a valid signature by that key, and nothing whose bytes do not match the manifest's `sha256` and `size`. So whoever gets into the team's server can change the files there, but cannot make a client install them.

The release also publishes the whole dist as one file, `relevo-dist-<version>.tar.gz`, for the server.

## The server

`deploy/install.sh` puts the dist next to the database, in `~/.local/share/relevo/dist/` (given `--dist relevo-dist-<version>.tar.gz`, or found next to the binary it installs). It refuses a dist that is not of the binary's version, or whose files do not match its manifest, before it changes anything; `dist` is a link to a directory of its own, replaced in one rename, so the server hands out the old dist or the new one, never half of each. `relevo serve` takes the dist next to its database (`--dist DIR` or `RELEVO_DIST` names another) and hands it out to anyone with a valid token:

```
GET /api/dist/manifest.json
GET /api/dist/manifest.sig
GET /api/dist/<name>          a file the manifest lists, as is
```

They answer `NOT_FOUND` when there is no dist, or no such file. `relevo setup pi` and `relevo setup hermes` (and `relevo connect --setup`) install an adapter for the first time from the same files, checked as `relevo update` checks them: the manifest's signature by the release key, then its size and SHA-256. A server without `manifest.sig`, or with one the key did not make, gets nothing installed, and an installed adapter is never replaced with an older version (no downgrades). That holds for a `relevo` you already have. The invitation link's script installs `relevo` itself from the server, checked only against the SHA-256s the server put in the script, and that `relevo` then checks the adapters: they are trusted as far as the script was. The binaries are also handed out, without a token, to whoever holds an invitation that still works (`GET /join/<code>/relevo?target=<triple>`, see `docs/protocol.md`, "Invitations"), for the install script of the invitation link. The server logs a warning at start when the dist's version is not its own. It never signs anything and holds no key.

## The clients

`relevo update` (and the same check that the clients run by themselves):

1. reads the server and token from the config (`relevo connect`), trusting the pinned certificate as every client does;
2. fetches `manifest.json` and `manifest.sig` and checks the signature with the compiled-in key; a bad or missing signature stops it;
3. compares the manifest's version with its own: newer is installed, the same is up to date, older is refused (no downgrades through this path);
4. downloads its own target's binary, checks size and `sha256`, writes it next to the running binary, checks that it runs here and says the manifest's version (`--version`), and renames it over the running one in one step, so a failure leaves the old one whole. It takes effect at the next start: a running `relevo mcp` keeps running the old one;
5. updates the adapters registered on this machine the same way: their archive is checked, unpacked next to the installed folder, checked to be that adapter at that version, and swapped in (in one step on Linux, `renameat2` with `RENAME_EXCHANGE`). Each adapter's version is read from its own folder (`package.json` of `relevo-pi`, `plugin.yaml` of `relevo`); a folder that is not that adapter is never registered nor replaced (a registered one that no longer is, a broken `package.json` say, is left as it is and the rest is still updated), and a registered folder that is gone is skipped. A path given through a link is resolved first: the folder it points to is registered and updated, and the link stays a link. A folder in a git checkout (a `.git`, folder or file, in it or above it) is never registered nor replaced, whoever runs the update: git updates that folder, and swapping it would throw away what git tracks there, uncommitted work included. `relevo update` says it is left as it is.

6. refreshes the `/relevo` command for Claude Code (`~/.claude/commands/relevo.md`, or under `$CLAUDE_CONFIG_DIR`) when Relevo wrote it, nobody edited it since (its `# relevo <version> sha256:<hash>` line still matches), and the running binary's copy differs, after the steps above (an update that fails before leaves it as it was); it never installs it where it is not, never replaces the file of a newer Relevo, and leaves a file Relevo did not write, or that was edited, as it is (see [docs/agents.md](agents.md), "The /relevo command"). It writes the copy built into the binary that runs it, so after a binary update the new command comes at the next run.

Every file is downloaded and checked, and every place it writes to is checked to be writable, before anything is installed. `relevo update --check` only says what it would do, and changes nothing. Adapters are registered in the config (`adapters` in `~/.config/relevo/config.json`, kept by `relevo connect`) by the adapters themselves (`relevo update --adapter pi --path <dir>` records the path), or by hand with the same flags; with the server and token in the environment and no config file, the adapter given is updated that time only. A server without a dist (or older than 0.4.0) hands out nothing, which is not an error.

`relevo update` prints a line for each thing it looks after (up to date, installed, left as it is and why), and what failed on stderr, with exit status 1. With `--auto` it prints only what it installed (stdout) and what failed (stderr, exit 1).

**By themselves.** `relevo mcp`, at start, runs the update in the background; it is silent unless it installed something or failed, and at most one check an hour per machine runs. The pi extension, at `session_start`, runs `relevo update --auto --adapter pi --path <its folder>` in the background (the `relevo` of `RELEVO_BIN`, else of `PATH`; none, nothing), except in print and JSON runs, which answer one prompt and exit; when something was installed, it tells the person in pi that it applies at the next start of pi, and what failed, as a warning (as `relevo mcp` is not silent about a failure). The Hermes plugin, at connect, does the same for its folder (`--adapter hermes`), never holding the connection; as any client of the machine may have updated the plugin's folder (a `relevo mcp`, usually first), the plugin compares, at connect and when its own run ends, the version in its folder with the one the gateway loaded; when they differ it tells the person once for that version, in the first home channel of the profile that takes the message (Relevo's own left out: its chats are teammates' threads), that the gateway must be restarted to use it (the plugin never restarts it), and logs it; when no home channel takes it, it tries again a few times, and at the next connect; what failed goes to the gateway log, as a warning. `RELEVO_AUTO_UPDATE=0` turns the automatic update off; `relevo update` still works by hand. The hour is kept in `update-checked` in `$XDG_CACHE_HOME/relevo` (or `~/.cache/relevo`), next to `update.lock`, which lets one update run at a time on the machine (`--auto` skips when another runs; by hand it says so). A client that is not configured has nothing to update from: `--auto` says nothing. An adapter whose folder is in a git checkout does not even start the update (and `relevo update` would leave that folder alone anyway, as above).

A client that cannot write where it is installed (a binary in `/usr/local/bin` owned by root, say) says so and installs nothing.

**From 0.3.x.** Clients before 0.4.0 have no updater: each teammate updates once by hand to 0.4.0 (the binary, and the adapters through `relevo update --adapter …`), and after that they follow the server.

## The signing key

Every client installs what the key signs, within the hour and with nobody approving it, so the release workflow never signs a build by itself:

- The signing job runs in the environment `release-signing` and the key is a secret of that environment only, never a repository secret: a workflow that leaves the environment out (a changed `release.yml` in a tagged commit, say) does not get it. In the repository's Settings → Environments, `release-signing` has **required reviewers** (the owner) and a **deployment rule** that allows only tags `v*`; each release waits there until a reviewer approves it, after the binaries are built and tested.
- A **tag ruleset** for `v*` lets only the owner create, move or delete release tags.
- Before signing, the workflow checks that the tag is `v` and the version of `Cargo.toml`, and that the tagged commit is on `main`; it refuses a key that is not the one `keys/release-signing-pub.pem` holds.
- Third-party actions are pinned to a commit (`dtolnay/rust-toolchain`, `actions/*`): what they build is signed. Update a pin on purpose, reading what changed.

The environment, its reviewers and the ruleset are repository settings: the owner sets them once, before the first release that carries the dist. On a private repository under GitHub Free, required reviewers and rulesets are not available (they need GitHub Pro, or a public repository): the environment then keeps only its rule that allows tags `v*`, so anyone with write access to the repository can push a release tag, and with it a commit on `main`, and the workflow signs it. Then only people trusted to release get write access; the owner adds the reviewers and the ruleset as soon as the plan allows.

## If the key is lost

Nobody can sign a release any more, so the clients stay where they are; nothing is at risk. Make a new key (`openssl genpkey -algorithm ed25519`), put its public half in `keys/release-signing-pub.pem` and the private half in the environment `release-signing`, and release. The clients cannot trust that release by themselves, as it is signed by a key they do not know: each teammate updates once by hand (the binary, and the adapters through `relevo update --adapter …`), as from 0.3.x, and they follow the server after that.

## If the key is leaked

This is urgent. Nothing revokes the key and a signed manifest never expires, so anyone who holds it and can answer a client (whoever gets into the team's server, or onto the network path with the server's certificate) can make every client install whatever they like, at its next check, within the hour. They can also sign a very high version (`999.0.0`, say): a client that installs it refuses every real release after it as older. So:

1. **At once**, every teammate sets `RELEVO_AUTO_UPDATE=0` where their clients run (the shell profile, the Hermes profile's env, the environment of the editor that starts `relevo mcp`) and restarts them. `relevo update` by hand still works, and is not needed now.
2. The owner removes the old key from the environment `release-signing`, makes a new one, and puts its public half in `keys/release-signing-pub.pem`.
3. While nobody may have been served something else, the switch can follow the server like any update: the clients still trust only the old key, so the dist of that release (whose binaries trust only the new key) is signed once with the **old** key, by hand and not by the workflow, which refuses a key other than `keys/release-signing-pub.pem`: `deploy/make-dist.sh --key <old private key> --pub <old public key> <dir with the release's binaries>`, from the release's commit. The owner puts that dist on the server, then destroys the old key. Each teammate turns the automatic update back on, takes that release (`relevo update`), and from then on follows releases signed by the new key.
4. Anyone who may already have been served a release nobody made (`relevo --version`, or the version in the adapters' folders, is not one of the team's releases, or is newer than the server's) reinstalls by hand from the GitHub release, checking `SHA256SUMS`: the binary, and the adapters through `relevo update --adapter …`. A client that took a very high version follows nothing until then.
