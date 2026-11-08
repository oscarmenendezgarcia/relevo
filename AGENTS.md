# Relevo — rules for agents working on this repo

- The protocol in `docs/protocol.md` is the contract. Code follows it; if the code needs something the spec doesn't say, change the spec in the same commit and say why.
- Rust, one binary. `axum` for HTTP, `rusqlite` (bundled) for SQLite, `clap` for the CLI. MCP (`rmcp`) arrives in block 2.
- Before saying something works: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass. Run them in the foreground.
- Never log note text or tokens. Store token hashes only.
- Keep it small. No new dependency without a reason; no abstraction for a single use.
- Tests go through the HTTP router (in-memory database) for every rule of the spec: permissions, limits, delivery state, idempotency, errors.
- Commits: small, clear messages. Work on a branch; never push to `main` without the owner's OK.
