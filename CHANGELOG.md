# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- Two-way prompts follow notification policy: muted projects/event types (filters, toggles,
  `notify = false` rules) keep their prompts in the terminal.
- Setup no longer asks to restart Claude Code sessions: settings are reloaded automatically.

- Windows start-at-login uses a per-user Run registry entry instead of Task Scheduler, which
  required administrator rights.

## [0.2.0] - 2026-11-27

### Added

- Two-way control through a Discord bot (opt-in): approve/deny permissions, answer questions
  (buttons, menus or text), reply to a finished/idle ping to continue (asyncRewake), `status` /
  `away` / `back` / `help` commands; `agentping setup discord-bot` with one-time-code pairing;
  `agentping away|back`; Discord Gateway client with resume/reconnect.
- Permission pings include the exact command, file or diff (from the `PermissionRequest` hook).
- Notifications record provider message ids so replies map to sessions.

### Changed

- Full message text by default: long notifications are split into several messages
  ("part 1/3"), never truncated (`notifications.max_message_chars = 0`).

## [0.1.0] - 2026-11-27

First milestone: Claude Code → AgentPing → Discord/Telegram → phone, end to end.

### Added

- Unified, versioned event model (`schema_version = 1`) with event types, agents and severities.
- Claude Code integration via native hooks (`Notification` with subtype mapping,
  `PreToolUse`/`PostToolUse` for `AskUserQuestion`, `Stop`, `StopFailure`, `SessionStart`,
  `SessionEnd`, `UserPromptSubmit`), with a silent, always-exit-0 hook command.
- `agentping setup claude` merge-safe installer with backups, dry run and removal.
- Discord webhook provider (embeds, push preview, mention blocking, limits, 429 handling).
- Telegram bot provider (HTML messages, chat discovery, rate limits).
- SQLite storage (WAL) with migrations, deduplication, durable lease-based queue, exponential
  backoff, crash recovery, delivery history, sessions and retention.
- Rules engine: per-type toggles, project filters, ordered `[[rules]]`, quiet hours, repeat
  suppression; delayed and cancellable question notifications.
- Local daemon with a versioned loopback API (`/v1`), token auth, Host/Origin checks;
  offline fallback with detached one-shot delivery.
- CLI: `init`, `setup`, `test`, `status`, `doctor`, `logs`, `events`, `providers`, `config`,
  `rules test`, `privacy`, `daemon`, `uninstall`, JSON output and `--no-color`.
- OS credential store for secrets; redaction of logs, errors and notification text.
- Start-at-login registration: Task Scheduler (Windows), launchd (macOS), systemd user unit (Linux).
- Documentation: README, architecture, configuration, security, integrations, development.

### Not yet implemented

- WhatsApp delivery, dashboard UI, remote approvals/replies, prebuilt binaries.
