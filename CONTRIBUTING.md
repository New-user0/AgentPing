# Contributing to AgentPing

Thanks for helping! AgentPing aims to be small, reliable and trustworthy, and contributions
should keep it that way.

## Ground rules

- **Security first.** Never log or print secrets; route errors through redaction; don't add
  remote-control capabilities without following [docs/remote-interaction.md](docs/remote-interaction.md).
- **Don't break agents.** The `hook` command must stay silent on stdout/stderr, exit 0, and be fast.
- **Honest docs.** Document only what works; mark planned features as planned.
- **No provider or agent specifics in the core.** Use `NotificationProvider` / `AgentAdapter`.

## Workflow

1. Open an issue for anything non-trivial.
2. Branch from `main`, keep commits focused.
3. Before pushing:
   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace
   ```
4. Add tests: unit tests next to the code, CLI behaviour in `crates/agentping-cli/tests`,
   new agent payloads as fixtures in `tests/fixtures/<agent>/`.
5. Update `CHANGELOG.md` (Unreleased) and the relevant docs.

## Code style

Idiomatic Rust, small modules, typed errors (`thiserror`) in libraries and `anyhow` with context
in the CLI, no `unwrap`/`expect` outside tests, no `unsafe` (see SECURITY.md for the single
documented exception). See [docs/development.md](docs/development.md) and
[docs/architecture.md](docs/architecture.md).
