# Security

AgentPing observes AI coding agents that run with access to your code and shell, so it is
designed to add as little attack surface as possible.

## Reporting a vulnerability

Please report vulnerabilities privately via GitHub's "Report a vulnerability" (Security
Advisories) on this repository rather than in public issues.

## Threat model and guarantees

### AgentPing never acts on the agent's behalf

- Hooks only **observe**. The hook process writes nothing to stdout, never returns a
  permission decision, and always exits 0, so it cannot approve, deny or block anything.
- There is no remote approval, remote reply, or command execution in this version. Messaging
  providers are treated as **untrusted** output channels only. See
  [docs/remote-interaction.md](docs/remote-interaction.md) for the opt-in design.

### Secrets

- Discord webhook URLs and Telegram bot tokens are stored in the OS credential store (Windows
  Credential Manager, macOS Keychain, Secret Service). They are never written to
  `config.toml`, the SQLite database, event history or logs.
- A plaintext `secrets.toml` is used only when the user passes `--insecure-store` and the
  credential store is unavailable; it is created with owner-only permissions (Unix `0600`;
  per-user profile ACLs on Windows) and `doctor` warns about it.
- `Secret` values redact themselves in `Debug`/`Display`. HTTP errors are stripped of URLs
  (`reqwest::Error::without_url`) and passed through pattern redaction before being logged or
  stored. `doctor`, `providers` and `logs` output are redacted.
- Interactive prompts hide input; `--from-env` / `--stdin` keep secrets out of shell history.

### Outbound requests

- Discord webhook URLs must be `https://` on `discord.com`, `discordapp.com`,
  `ptb.discord.com` or `canary.discord.com` with the `/api/webhooks/<id>/<token>` shape; ports,
  credentials, queries and other hosts are rejected. This prevents the config from being used
  to exfiltrate event data to arbitrary servers.
- Telegram requests go only to `https://api.telegram.org`.
- The HTTP client does not follow redirects and uses short timeouts.
- Discord messages set `allowed_mentions: {parse: []}` so agent text cannot ping `@everyone`.

### Local API

- Binds to loopback only; non-loopback `daemon.host` values fail validation.
- All endpoints except `GET /v1/health` require a 256-bit bearer token stored in the user's
  data directory (owner-only), compared in constant time.
- Requests whose `Host` header is not a loopback name with the daemon's port are rejected
  (DNS rebinding); requests carrying an `Origin` header are rejected (browser CSRF).
- 256 KiB body limit; events are schema-validated and size-limited.
- No endpoint executes commands or reads arbitrary files.

Any process running as the same OS user can read the token, which is by design: same-user
processes are inside the trust boundary.

### Data minimization

- Prompts typed into Claude Code are never read or stored (`UserPromptSubmit` is used only as a
  status signal).
- Stored: event type, agent, session id, project name/path, the agent's notification text,
  Claude's final message on `Stop`, and `AskUserQuestion` text. Use `agentping privacy`,
  `notifications.include_message = false`, `general.retention_days` and
  `agentping events clear`.
- Notifications include project names but not paths by default, and only the first 8 characters
  of session ids. Message text is redacted for common secret formats and truncated.
- No telemetry, analytics or update checks.

### Claude Code settings

`agentping setup claude` backs up `settings.json` before writing, merges instead of replacing,
writes atomically, refuses to touch files that are not valid JSON, and `--remove` deletes only
entries whose command is `agentping … hook claude`.

### Unsafe code

The workspace denies `unsafe_code`. The single exception is a documented call to
`SetHandleInformation` on Windows that marks the hook's standard handles non-inheritable
before spawning a background process.
