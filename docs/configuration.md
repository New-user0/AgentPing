# Configuration reference

Location: `agentping config path`. Missing file = built-in defaults. Edit by hand or with
`agentping config set <dotted.key> <value>` (comments are preserved, the result is validated
before saving). Validate with `agentping config validate`. Restart the daemon after changes:
`agentping daemon restart`.

Unknown keys are errors (typos are caught with line/column). Secrets are **never** stored here.

```toml
config_version = 1
```

## `[general]`

| Key | Default | Description |
|---|---|---|
| `log_level` | `"info"` | `error`, `warn`, `info`, `debug`, `trace` |
| `retention_days` | `30` | Days of event history kept (pruned at daemon start and daily) |
| `notifications_enabled` | `true` | Master switch |
| `database_path` | unset | Override the SQLite path (`~` expanded) |

## `[daemon]`

| Key | Default | Description |
|---|---|---|
| `host` | `"127.0.0.1"` | Must be loopback (`127.0.0.1`, `::1`, `localhost`) |
| `port` | `38471` | Local API port |
| `spawn_delivery_when_offline` | `true` | Hooks start a short-lived background sender when the daemon is not running |

## `[agents.claude]`

| Key | Default | Description |
|---|---|---|
| `enabled` | `true` | Accept Claude Code events (when false, events are recorded but muted) |
| `question_delay_secs` | `15` | Delay before notifying about an `AskUserQuestion` prompt; cancelled if answered first (0–600) |

## `[providers.*]`

```toml
[providers.discord]
enabled = false
username = "AgentPing"        # webhook display name

[providers.telegram]
enabled = false
chat_id = "123456789"         # numeric id (negative for groups) or "@channel"
silent_low_severity = false   # no sound for low-severity events

[providers.whatsapp]
enabled = false               # not implemented; true is a validation error
```

Credentials come from (in order): environment variables `AGENTPING_DISCORD_WEBHOOK_URL` /
`AGENTPING_TELEGRAM_BOT_TOKEN`, the OS credential store, or `secrets.toml` (only after
`--insecure-store`). Set `AGENTPING_DISABLE_KEYRING=1` to skip the credential store.

## `[notifications]`

| Key | Default | Description |
|---|---|---|
| `permission_requests` | `true` | `permission_request` events |
| `user_questions` | `true` | `user_question` events |
| `task_completed` | `true` | `task_completed` events |
| `task_failed` | `true` | `task_failed` events |
| `agent_idle` | `true` | `agent_idle` events |
| `session_events` | `false` | `agent_started` / `agent_stopped` |
| `other_notifications` | `true` | generic `notification` events |
| `include_message` | `true` | Include agent text (redacted, truncated) |
| `include_project_path` | `false` | Include full project paths |
| `max_message_chars` | `0` | `0` = send the full text (long messages are split into several messages, "part 1/3"…); otherwise truncate to this many characters |
| `repeat_window_secs` | `60` | Suppress repeated `agent_idle`, `task_completed`, `notification`, session events for the same session within this window (0 disables) |

```toml
[notifications.filters]
include_projects = []         # if non-empty, only these project names notify
exclude_projects = ["sandbox"]
```

Project names are the folder name of the git root (or of `CLAUDE_PROJECT_DIR`), compared
case-insensitively.

## `[quiet_hours]`

```toml
[quiet_hours]
enabled = false
start = "22:00"          # local time
end = "07:00"            # may wrap past midnight
min_severity = "urgent"  # events at or above still notify
```

Severities: `low` < `normal` < `high` < `urgent`. Defaults: permission requests, questions and
failures are `high`; completions and notifications `normal`; idle and session events `low`.

## `[[rules]]`

Rules are evaluated **in order; the first matching rule decides**. Every match field is
optional (empty = match all):

| Field | Meaning |
|---|---|
| `name` | Label shown in decisions |
| `agents` | e.g. `["claude"]` |
| `event_types` | e.g. `["permission_request", "task_failed"]` |
| `projects` | project names |
| `project_paths` | glob patterns, forward slashes, e.g. `["D:/work/**", "/home/me/oss/**"]` |
| `min_severity` | match only at/above this severity |
| `notify` | `true` (default) or `false` to mute |
| `providers` | restrict delivery, e.g. `["telegram"]` |
| `severity` | override severity (e.g. `"urgent"` to bypass quiet hours) |

Evaluation order:

1. `activity` events never notify.
2. `general.notifications_enabled` and `agents.<agent>.enabled`.
3. First matching rule; if none matches, the per-type toggles and project filters apply.
4. Delivery to enabled providers (restricted by the rule's `providers`).
5. Quiet hours (using the possibly overridden severity).
6. Repeat suppression.

Examples:

```toml
[[rules]]
name = "permissions everywhere, loudly"
event_types = ["permission_request"]
providers = ["discord", "telegram"]
severity = "urgent"

[[rules]]
name = "work repos only to Telegram"
project_paths = ["D:/work/**"]
providers = ["telegram"]

[[rules]]
name = "quiet completions in sandbox"
projects = ["sandbox"]
event_types = ["task_completed"]
notify = false
```

Check a rule set without waiting for a real event:

```sh
agentping rules test --type permission_request --project api --time 23:30
```

## `[templates.<event_type>]`

Override titles/bodies per event type (or `default`):

```toml
[templates.task_completed]
title = "{emoji} {agent} finished in {project}"
body = "{message}"
```

Placeholders: `{project}` `{agent}` `{event}` `{message}` `{title}` `{session}` `{severity}`
`{time}` `{emoji}`. Unknown placeholders are validation errors. `{message}` is empty when
`include_message = false`.

## Environment variables

| Variable | Effect |
|---|---|
| `AGENTPING_HOME` | Put config, data and logs under one directory |
| `AGENTPING_LOG_LEVEL` | Override `general.log_level` |
| `AGENTPING_DAEMON_PORT` | Override `daemon.port` |
| `AGENTPING_NOTIFICATIONS_ENABLED` | `true`/`false` master switch |
| `AGENTPING_DISCORD_WEBHOOK_URL` | Discord webhook (takes precedence over stored secret) |
| `AGENTPING_TELEGRAM_BOT_TOKEN` | Telegram token (takes precedence over stored secret) |
| `AGENTPING_DISABLE_KEYRING` | `1` to bypass the OS credential store |
| `NO_COLOR` | Disable colors |
