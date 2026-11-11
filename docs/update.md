# Updating the clients by themselves

From 0.4.0, the team's server hands out the clients of its own version, and every client updates itself from it. To update the team, the owner updates the server; the teammates' `relevo` binary, pi extension and Hermes plugin follow on their own. Nobody needs access to the GitHub repository.

## What is handed out

Every release builds, besides the server binaries, a **dist**: the `relevo` binary for each target (`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`), the pi extension (`relevo-pi-<version>.tar.gz`, the contents of `adapters/pi`) and the Hermes plugin (`relevo-hermes-<version>.tar.gz`, the contents of `adapters/hermes/relevo`), and a **manifest** that lists them:

```json
{ "version": "0.4.0",
  "files": [ { "name": "relevo-aarch64-unknown-linux-gnu", "kind": "binary", "target": "aarch64-unknown-linux-gnu", "size": 9036312, "sha256": "…" },
             { "name": "relevo-pi-0.4.0.tar.gz", "kind": "pi", "size": 51200, "sha256": "…" },
             { "name": "relevo-hermes-0.4.0.tar.gz", "kind": "hermes", "size": 40960, "sha256": "…" } ] }
```

**The manifest is signed.** The release workflow signs `manifest.json` with an Ed25519 key kept as the repository secret `RELEVO_SIGNING_KEY` and publishes the 64-byte signature as `manifest.sig`. The public key is compiled into every `relevo` binary. A client installs nothing whose manifest does not carry a valid signature by that key, and nothing whose bytes do not match the manifest's `sha256` and `size`. So whoever gets into the team's server can change the files there, but cannot make a client install them.

The release also publishes the whole dist as one file, `relevo-dist-<version>.tar.gz`, for the server.

## The server

`deploy/install.sh` puts the dist next to the database, in `~/.local/share/relevo/dist/` (given `--dist relevo-dist-<version>.tar.gz`, or found next to the binary it installs). `relevo serve` hands it out to anyone with a valid token:

```
GET /api/dist/manifest.json
GET /api/dist/manifest.sig
GET /api/dist/<name>          a file the manifest lists, as is
```

They answer `NOT_FOUND` when there is no dist, or no such file. The server logs a warning at start when the dist's version is not its own. It never signs anything and holds no key.

## The clients

`relevo update` (and the same check that the clients run by themselves):

1. reads the server and token from the config (`relevo connect`), trusting the pinned certificate as every client does;
2. fetches `manifest.json` and `manifest.sig` and checks the signature with the compiled-in key; a bad or missing signature stops it;
3. compares the manifest's version with its own: newer is installed, the same is up to date, older is refused (no downgrades through this path);
4. downloads its own target's binary, checks size and `sha256`, writes it next to the running binary and renames it over it in one step, so a failure leaves the old one whole. It takes effect at the next start: a running `relevo mcp` keeps running the old one;
5. updates the adapters registered on this machine the same way: their archive is checked, unpacked next to the installed folder, and swapped in.

`relevo update --check` only says what it would do. Adapters are registered in the config by the adapters themselves (`relevo update --adapter pi --path <dir>` records the path), or by hand with the same flags.

**By themselves.** `relevo mcp`, at start, runs the update in the background; it is silent unless it installed something or failed, and at most one check an hour per machine runs. The pi extension, at `session_start`, runs `relevo update --auto --adapter pi --path <its folder>`; when something was installed, it tells the person in pi that it applies at the next start of pi. The Hermes plugin, at connect, does the same for its folder; when it installed something, it tells the person in Hermes' main chat that the gateway must be restarted (the plugin never restarts it). `RELEVO_AUTO_UPDATE=0` turns the automatic update off; `relevo update` still works by hand.

A client that cannot write where it is installed (a binary in `/usr/local/bin` owned by root, say) says so and installs nothing.

**From 0.3.x.** Clients before 0.4.0 have no updater: each teammate updates once by hand to 0.4.0 (the binary, and the adapters through `relevo update --adapter …`), and after that they follow the server.

## If the key is lost or leaked

A new key means a client release with the new public key, which the clients cannot trust by themselves: each teammate updates once by hand, as from 0.3.x.
