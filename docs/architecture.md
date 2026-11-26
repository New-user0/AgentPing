# Architecture

AgentPing is a Cargo workspace that compiles into a single `agentping` executable. The binary
plays three roles: **hook** (short-lived, launched by an agent), **daemon** (long-lived local
service) and **CLI** (setup and inspection).

```text
                ┌────────────────────────── agentping (one binary) ──────────────────────────┐
 Claude Code    │                                                                            │
 hook (stdin) ─▶│ hook ─▶ AgentAdapter ─▶ Event ──POST /v1/events (loopback + token)──▶ daemon│
                │                          │                                         │       │
                │                          └─(daemon offline)─▶ Engine.ingest ─┐      ▼       │
                │                                               (same code)    │  Engine     │
                │                                                              ▼   │         │
                │                                    SQLite (WAL): events, sessions,│         │
                │                                    notifications queue, attempts ◀┘         │
                │                                              │                              │
                │               Deliverer (daemon worker or detached `agentping deliver`)     │
                │                                              │                              │
                │                     NotificationProvider: Discord │ Telegram                 │
                └──────────────────────────────────────────────┼─────────────────────────────┘
                                                               ▼
                                                    provider APIs → phone
```

## Crates

| Crate | Depends on | Responsibility |
|---|---|---|
| `agentping-core` | — | `Event` (schema v1), enums, config schema/validation/migration/`set`, platform `Paths`, `SecretStore`, redaction, `RetryPolicy`, atomic file writes |
| `agentping-storage` | core | rusqlite database, embedded migrations (`PRAGMA user_version`), repositories, durable queue |
| `agentping-rules` | core | Pure, deterministic `RulesEngine` |
| `agentping-notifiers` | core | `NotificationProvider` trait, rendering/templates, Discord, Telegram, `build_providers` |
| `agentping-agents` | core | `AgentAdapter` trait, Claude Code normalization, settings installer, detection |
| `agentping-daemon` | core, storage, rules, notifiers | `Engine` (pipeline), `Deliverer`, axum API, client, process spawning, OS services |
| `agentping-cli` | all | Commands, onboarding, output, logging |

The engine never names a provider or an agent: providers are looked up by name from a
`ProviderSet`, and agents only produce `Event`s.

## Event pipeline

`Engine::ingest` runs in **one SQLite write transaction**:

1. **Dedup**: skip if an event with the same fingerprint (`Event::dedup_key`: agent, session,
   type, native event/subtype, title, message, tool_use_id) was received in the last 30 s.
   This absorbs the same hook being registered in several settings files.
2. **Session tracking**: upsert `(agent, session_id)` with a derived status.
3. **Cancellation**: an event with `delivery.cancels` cancels pending notifications registered
   under that key (used to drop question pings answered at the keyboard).
4. **Rules**: `RulesEngine::evaluate` returns a `Decision` (notify?, providers, severity, reason).
5. **Enqueue**: one `notifications` row per provider, due at `now + delivery.delay_secs`.
6. **Mark processed**: the decision summary is stored on the event (visible in `agentping events`).

Because the engine is synchronous and storage-backed, the hook can run the very same pipeline
when the daemon is offline; the daemon also processes any unprocessed backlog at startup.

## Durable queue

- `claim_due` atomically moves due `pending` rows to `sending` with a 60 s **lease**
  (`UPDATE … RETURNING`). Expired leases are reclaimed on every claim, so a crash mid-send leads
  to a retry, not a lost notification.
- `record_attempt` writes a `delivery_attempts` row and moves the notification to `delivered`,
  `pending` (retry at a backoff time) or `failed`.
- Backoff: `5 s × 2^(n-1)`, ±20 % jitter, capped at 15 min, 8 attempts. Provider `retry_after`
  (Discord/Telegram 429) is honoured.
- Graceful shutdown lets the current pass finish; unclaimed work stays in SQLite.
- Several processes (daemon, detached senders) can deliver concurrently; leases prevent double
  sends.

## IPC decision: loopback HTTP

| Option | Pros | Cons |
|---|---|---|
| Unix domain socket | file-permission auth | not on Windows (AF_UNIX support is partial), second code path |
| Named pipe | native on Windows | Windows only, second code path |
| **Loopback HTTP (chosen)** | one implementation everywhere; same API serves a future dashboard; easy to debug | any local process can connect → needs auth |

Mitigations: bind only to loopback (non-loopback config is rejected), 256-bit token in a
user-only file (`daemon.token`), constant-time comparison, `Host` allow-list (DNS rebinding),
reject any `Origin` header (browser CSRF), 256 KiB body limit, no command execution endpoints.
The API is versioned under `/v1`.

## Hook latency and the offline path

The hook loads config, parses stdin, and POSTs to the daemon with a 300 ms connect / 1.5 s total
timeout (typically ~30–60 ms end to end). If the daemon is unreachable it ingests directly into
SQLite and spawns `agentping deliver` **detached**:

- stdio is redirected to null; on Windows the hook's own std handles are first marked
  non-inheritable (otherwise the child would hold the agent's stdout pipe open and the agent
  would wait for it), the child gets `CREATE_NO_WINDOW` and tries to break away from the
  caller's job object; on Unix it gets its own process group.
- `deliver` drains the queue, waits for items due within 2 minutes (e.g. delayed questions),
  and exits. Longer retries are picked up by the next run or the daemon.

## Storage choice: rusqlite

rusqlite (with bundled SQLite) was chosen over SQLx because the workload is small, local and
single-writer: synchronous calls are simple, need no compile-time database or offline query
metadata, and are trivially shared between the async daemon (`spawn_blocking` around a mutex)
and the synchronous hook. WAL mode and a 5 s busy timeout make concurrent processes safe.

## Configuration

TOML with `deny_unknown_fields` (typos are errors with line numbers), defaults for everything,
semantic validation that reports all problems at once, a `config_version` migration hook,
environment overrides (`AGENTPING_LOG_LEVEL`, `AGENTPING_DAEMON_PORT`,
`AGENTPING_NOTIFICATIONS_ENABLED`) and comment-preserving edits through `toml_edit`. The daemon
reads configuration at start; restart it after changes.

## Platform specifics

| Concern | Linux | macOS | Windows |
|---|---|---|---|
| Config dir | `~/.config/agentping` | `~/Library/Application Support/agentping` | `%APPDATA%\agentping\config` |
| Data/logs | `~/.local/share/agentping` | `~/Library/Application Support/agentping` | `%LOCALAPPDATA%\agentping\data` |
| Secrets | Secret Service | Keychain | Credential Manager |
| Start at login | systemd user unit | launchd agent | per-user Run registry entry (no admin) |
| Hook shell | sh | sh | Git Bash if installed, else PowerShell |

`AGENTPING_HOME` relocates everything (tests, portable installs). On Windows the daemon starts
from a per-user login item (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`), not a Windows
service: services run as another account and could not read the user's credential store, and the
Run entry needs no administrator rights. A console window may flash briefly at logon.

## Extending

- **New provider**: implement `NotificationProvider` (classify errors as retryable/permanent,
  never include credentials in messages), add a `SecretKey` if needed, add config and wire it in
  `build_provider`.
- **New agent**: implement `AgentAdapter` to produce `Event`s, add a `hook <agent>` dispatch and
  an installer for that agent's hook system.
