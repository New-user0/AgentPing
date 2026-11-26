# AgentPing

**Stay connected to your AI coding agents, wherever you are.**

AgentPing is a local-first notification system for AI coding agents. Step away from your
desk while Claude Code works and get a ping on your phone (via Discord or Telegram) when the
agent:

- 🔐 needs **permission** to run a tool
- 💬 **asks you a question**
- ✅ **finishes** its task
- ⚠️ **stops with an error**
- ⏳ has been **idle**, waiting for your input

It is a single Rust binary with no cloud service of its own. Events, history and configuration
stay on your machine; only the notifications you configure leave it.

```text
Claude Code ──hook──▶ agentping hook ──loopback──▶ agentping daemon ──▶ Discord / Telegram ──▶ 📱
                         (exits in ms)                (SQLite queue,
                                                       rules, retries)
```

**Two-way (opt-in):** approve or deny permissions, answer Claude's questions, and reply to a
finished session with new instructions, right from Discord. See
[Two-way control](#two-way-control-from-discord).

> Status: **v0.2.0.** Claude Code + Discord + Telegram work end to end on Windows, macOS and
> Linux, plus two-way control through a Discord bot. WhatsApp and a dashboard are **not
> implemented** yet; see the [roadmap](#roadmap).

---

## Contents

- [Features](#features)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Discord setup](#discord-setup)
- [Telegram setup](#telegram-setup)
- [Claude Code integration](#claude-code-integration)
- [Two-way control from Discord](#two-way-control-from-discord)
- [CLI usage](#cli-usage)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Security & privacy](#security--privacy)
- [Architecture](#architecture)
- [Roadmap](#roadmap)
- [Contributing](#contributing)

## Features

| Area | What works today |
|---|---|
| Agents | Claude Code via native hooks (`Notification`, `PreToolUse`/`PostToolUse` for `AskUserQuestion`, `Stop`, `StopFailure`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`) |
| Providers | Discord webhooks (rich embeds, rate-limit aware), Telegram bots (HTML messages, chat discovery) |
| Reliability | SQLite (WAL) event store, durable lease-based queue, exponential backoff, crash recovery, dedup of repeated hook firings |
| Rules | Per-event toggles, project include/exclude, ordered `[[rules]]` (agent / project / path glob / severity → mute, route to providers, override severity), quiet hours, repeat suppression |
| Smart questions | Question notifications wait 15 s and are cancelled automatically if you answer at the keyboard |
| Sessions | Concurrent sessions tracked per project with derived status (working, waiting for permission, …) |
| Security | Secrets in the OS credential store, loopback-only API with token auth + Host/Origin checks, webhook host allow-list, redaction everywhere, no remote actions |
| Diagnostics | `doctor` with actionable fixes, structured rotating logs, `events show` with per-attempt delivery history |
| Platforms | Windows, macOS, Linux; optional start-at-login (Windows login item / launchd / systemd user unit) |

## Installation

Prebuilt binaries are not published yet. Install from source with a stable Rust toolchain
(1.85+); on Windows the MSVC build tools are required (bundled SQLite is compiled from C):

```sh
git clone https://github.com/New-user0/AgentPing.git
cd AgentPing
cargo install --path crates/agentping-cli --locked
agentping --version
```

This installs a single `agentping` executable into `~/.cargo/bin`.

## Quick start

```sh
agentping init
```

The wizard detects your OS and Claude Code, configures Discord and/or Telegram (secrets go to
the OS credential store), sends a test notification, installs the Claude Code hooks (your
existing settings are preserved and backed up), optionally starts the daemon at login, and runs
diagnostics.

Open Claude Code sessions pick up the hooks automatically. Walk away. 🚶

Scripted setup:

```sh
export DISCORD_WEBHOOK='https://discord.com/api/webhooks/…'
agentping init --non-interactive --providers discord --discord-webhook-env DISCORD_WEBHOOK --install-daemon
```

## Discord setup

1. In Discord: **Server Settings → Integrations → Webhooks → New Webhook**, choose a channel,
   **Copy Webhook URL**. A private server with just you works well; enable mobile notifications
   for that channel.
2. Run one of:

   ```sh
   agentping setup discord                         # hidden interactive prompt
   agentping setup discord --from-env DISCORD_WEBHOOK
   echo "$URL" | agentping setup discord --stdin
   ```

3. AgentPing validates the URL (only `https://discord.com/api/webhooks/<id>/<token>` and the
   official Discord hosts are accepted), stores it in the OS credential store, enables the
   provider, checks connectivity and sends a test message.

Disable with `agentping setup discord --disable` (also deletes the stored URL).

## Telegram setup

1. Message **@BotFather**, send `/newbot`, copy the bot token.
2. Open your new bot and send it any message.
3. Run `agentping setup telegram` (or `--from-env VAR` / `--stdin`). AgentPing finds your chat
   via `getUpdates`; pass `--chat-id` to set it explicitly (groups and channels work too).

## Claude Code integration

```sh
agentping setup claude                  # ~/.claude/settings.json (all projects)
agentping setup claude --scope local    # ./.claude/settings.local.json
agentping setup claude --dry-run        # show the exact changes
agentping setup claude --remove         # remove only AgentPing's entries
```

AgentPing adds `command` hooks that run `agentping hook claude`. The hook reads the JSON payload
from stdin, never writes to stdout/stderr, always exits 0, and typically finishes in 30–60 ms.

| Claude Code hook | AgentPing event | Notifies by default |
|---|---|---|
| `Notification` · `permission_prompt` (after ~6 s unanswered) | `permission_request` | ✅ |
| `PreToolUse` · `AskUserQuestion` | `user_question` (delayed 15 s, cancelled if answered) | ✅ |
| `Notification` · `elicitation_dialog` / `agent_needs_input` | `user_question` | ✅ |
| `Notification` · `idle_prompt` | `agent_idle` | ✅ (repeat-suppressed) |
| `Stop` | `task_completed` (with Claude's final message) | ✅ |
| `StopFailure` | `task_failed` | ✅ |
| `SessionStart` / `SessionEnd` | `agent_started` / `agent_stopped` | ❌ (session tracking) |
| `UserPromptSubmit` | `activity` (status only; prompt text is **never** read) | ❌ |
| other `Notification` types | `notification` | ✅ |

Notes:

- On Windows the hook is pinned to Git Bash (`"shell": "bash"`) when it is installed, otherwise
  PowerShell. Running sessions reload settings automatically (verify with `/hooks`).
- AgentPing **never approves or answers anything**. It only observes. See
  [docs/integrations/claude-code.md](docs/integrations/claude-code.md).

## Two-way control from Discord

```text
🔐 Claude Code wants to use Bash · backend-api        [✅ Approve] [❌ Deny]
   Run the test suite
   npm test -- --watch=false
```

| From Discord | Effect in Claude Code |
|---|---|
| Tap **Approve** / **Deny**, or reply `deny: use npm ci` | permission decision (the reason is shown to Claude) |
| Tap an option / pick from a menu / reply with your own answer | answers Claude's question |
| Reply to a ✅ finished or ⏳ waiting ping | Claude wakes up and continues with your instructions |
| `status` · `away` · `back` · `help` | sessions overview · route prompts to Discord or not |

```sh
agentping setup discord-bot     # bot token + pairing with a one-time code
agentping away                  # send permission requests & questions to Discord now
agentping back                  # keep them in the terminal again
```

Off by default; only your paired Discord account can answer; every prompt is single-use and
expires; no shell access. Details and setup steps: [docs/remote-interaction.md](docs/remote-interaction.md).

## CLI usage

```text
agentping                    overview and next steps
agentping init               guided setup
agentping setup <claude|discord|discord-bot|telegram|whatsapp>
agentping away | back          route prompts to Discord while you're away
agentping test [discord|telegram]
agentping status [--json]    daemon, active sessions, delivery counters
agentping doctor [--offline] diagnostics with fixes
agentping logs [-f] [-n 100]
agentping events [--today|--since 2h] [--agent claude] [--project api] [--type permission_request] [--limit 50] [--page 2] [--json]
agentping events show <id>   event details + delivery attempts
agentping events clear       delete history (asks first)
agentping providers [--check]
agentping config [path|show|validate|set <key> <value>]
agentping rules test --type permission_request --project api [--time 23:30]
agentping privacy
agentping daemon <start|stop|restart|status|run|install|uninstall>
agentping uninstall [--purge]
```

Global flags: `--json`, `--no-color` (and `NO_COLOR`), `-v/-vv/-vvv` (logs to stderr).

Example:

```text
$ agentping status
AgentPing v0.1.0

  Daemon: Running
  Uptime: 2h 14m
  Providers: discord
  Claude hooks: installed

Active sessions: 1

Claude Code
    Project: api-server
    Status: Waiting for permission
    Last event: 12s ago

Notifications
  Delivered: 18
  Pending: 0
  Failed: 0
```

## Configuration

`agentping config path` shows the file (`~/.config/agentping/config.toml` on Linux,
`~/Library/Application Support/agentping/config.toml` on macOS,
`%APPDATA%\agentping\config\config.toml` on Windows). A commented file is created by
`agentping init`. Highlights:

```toml
[notifications]
task_completed = true
include_message = true        # set false to send only titles/project names
include_project_path = false

[quiet_hours]
enabled = true
start = "22:00"
end = "07:00"
min_severity = "urgent"

[[rules]]
name = "permissions are urgent"
event_types = ["permission_request"]
severity = "urgent"           # bypasses quiet hours

[[rules]]
name = "mute sandbox"
projects = ["sandbox"]
notify = false
```

Full reference: [docs/configuration.md](docs/configuration.md).

## Troubleshooting

Start with `agentping doctor`. Common issues:

| Symptom | Fix |
|---|---|
| No notifications at all | `agentping test`; check `agentping events`, where the *decision* column says why something was muted |
| Hooks don't fire | Restart Claude Code; `agentping doctor` checks hook registration and the binary path |
| "development build" warning | Install with `cargo install --path crates/agentping-cli`, then re-run `agentping setup claude` |
| Failed deliveries | `agentping events show <id>` lists every attempt with HTTP status and (redacted) error |
| Daemon not running | `agentping daemon start` or `agentping daemon install`. Without it, hooks spawn a short-lived sender, so notifications still go out |
| Linux headless (no Secret Service) | Use `AGENTPING_DISCORD_WEBHOOK_URL` in the service environment, or `--insecure-store` |

Logs: `agentping logs -f`.

## Security & privacy

- No telemetry, analytics or update checks. Network traffic goes only to providers you enable.
- Secrets live in Windows Credential Manager / macOS Keychain / Secret Service, never in the
  config, database or logs. A plaintext fallback exists only with explicit `--insecure-store`.
- The daemon binds to loopback only and requires a 256-bit token. Requests with a non-loopback
  `Host` or any browser `Origin` are rejected. There are no command-execution endpoints.
- Notification text is redacted for common secret formats and truncated. Paths are hidden by
  default. Prompts you type are never captured.
- Two-way control is opt-in, limited to paired Discord accounts, single-use and expiring, and
  only uses the hooks API (permission decisions, question answers, messages to Claude).

Details: [SECURITY.md](SECURITY.md) and `agentping privacy`.

## Architecture

A Cargo workspace compiled into one binary:

| Crate | Responsibility |
|---|---|
| `agentping-core` | Versioned event model, config, paths, secret store, redaction, retry policy |
| `agentping-storage` | SQLite schema/migrations, dedup, durable queue, sessions, history |
| `agentping-rules` | Deterministic notification rules |
| `agentping-notifiers` | `NotificationProvider` trait, rendering/templates, Discord, Telegram |
| `agentping-agents` | Agent adapters (Claude Code hooks) and hook installer |
| `agentping-remote` | Discord bot: Gateway client, interactive prompts, reply parsing |
| `agentping-daemon` | Pipeline engine, delivery worker, remote service, loopback API, service registration |
| `agentping-cli` | The `agentping` binary |

See [docs/architecture.md](docs/architecture.md) for decisions and trade-offs.

## Roadmap

- **Next:** prebuilt release binaries and install scripts, Homebrew tap, more agent adapters
  (via the `AgentAdapter` trait), richer per-provider routing.
- **Planned:** WhatsApp via the official Cloud API ([requirements](docs/integrations/whatsapp.md)),
  a lightweight local dashboard on the existing `/v1` API, two-way control over Telegram.

See [CHANGELOG.md](CHANGELOG.md) for what shipped.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/development.md](docs/development.md).

## License

MIT. See [LICENSE](LICENSE).
