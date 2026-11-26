# Development

## Prerequisites

- Rust stable (1.85+), with `rustfmt` and `clippy`
- A C compiler for bundled SQLite (`cc`): MSVC Build Tools on Windows, Xcode CLT on macOS,
  `build-essential` on Linux. Linux also needs `pkg-config` and `libdbus-1-dev` for the Secret
  Service keyring backend.

## Everyday commands

```sh
cargo build
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run the CLI against an isolated environment so your real config, keyring and Claude settings
are untouched:

```sh
export AGENTPING_HOME=$(mktemp -d) AGENTPING_DISABLE_KEYRING=1 AGENTPING_DAEMON_PORT=38999
cargo run -p agentping-cli -- init --non-interactive --no-claude
cargo run -p agentping-cli -- hook claude < tests/fixtures/claude/notification_permission_prompt.json
cargo run -p agentping-cli -- events
cargo run -p agentping-cli -- daemon run -v
```

## Tests

| Where | What |
|---|---|
| `agentping-core` | event model, config validation/migration/`set`, redaction, paths, secrets (file backend), backoff |
| `agentping-storage` | migrations, dedup, queue lifecycle, lease recovery, cancellation, sessions, pruning |
| `agentping-rules` | rule ordering, filters, quiet hours, repeat suppression |
| `agentping-notifiers` | rendering/templates, Discord & Telegram request construction and error classification against a local mock HTTP server, secret non-leakage |
| `agentping-agents` | every Claude fixture in `tests/fixtures/claude`, settings merge/uninstall/inspect |
| `agentping-daemon` | engine, delivery with a mock provider, API auth / Host / Origin / limits |
| `agentping-cli/tests` | the real binary: hooks (silent, exit 0), dedup, queueing, question cancellation, settings install/remove, config, secrets |

No test needs real Discord, Telegram or WhatsApp credentials.

## Fixtures

`tests/fixtures/claude/*.json` mirror payloads from the Claude Code hooks documentation,
including malformed and unexpected inputs. Add a fixture whenever Claude Code adds a field or
event you handle.

## Releasing (manual for now)

1. Update `CHANGELOG.md` and the workspace version.
2. `cargo build --release` on each platform; the binary is `target/release/agentping[.exe]`.
3. Tag `vX.Y.Z`. (Automated release builds are on the roadmap.)
