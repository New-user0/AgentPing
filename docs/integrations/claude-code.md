# Claude Code integration

Based on the official hooks reference: <https://code.claude.com/docs/en/hooks>
(verified against Claude Code 2.1.332).

## How it works

Claude Code runs configured `command` hooks and passes a JSON object on **stdin** with common
fields (`session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode`, …) plus
event-specific fields. AgentPing registers:

| Event | Matcher | Why |
|---|---|---|
| `Notification` | (none) | `notification_type` distinguishes `permission_prompt`, `idle_prompt`, `elicitation_dialog`, `elicitation_url_dialog`, `agent_needs_input`, `auth_*`, `quota_*`, … |
| `PreToolUse` | `AskUserQuestion` | Claude asks multiple-choice questions through this tool; `tool_input.questions[]` carries the text |
| `PostToolUse` | `AskUserQuestion` | Cancels the pending question notification once answered (matched by `tool_use_id`) |
| `Stop` | — | Task finished; `last_assistant_message` becomes the summary |
| `StopFailure` | — | Turn ended by an API error (`error`: `rate_limit`, `server_error`, …; `error_details`) |
| `SessionStart` | — | Session tracking (`source`) |
| `SessionEnd` | — | Session tracking (`reason`); timeout 5 s because SessionEnd hooks share a short budget |
| `UserPromptSubmit` | — | Marks the session "working". The `prompt` field is ignored |

### Why not the `PermissionRequest` hook?

`PermissionRequest` fires the moment a permission dialog appears, even if you are at the
keyboard. The `Notification` hook's `permission_prompt` type fires only after the prompt has been
waiting ~6 s, which is the "you are away" signal AgentPing wants, and using both would create
duplicates. AgentPing never returns a decision from any hook.

### Delivery behaviour

| Native | Event | Severity | Notes |
|---|---|---|---|
| `permission_prompt` | `permission_request` | high | |
| `AskUserQuestion` | `user_question` | high | delayed `question_delay_secs` (15), cancelled if answered |
| `elicitation_dialog`, `elicitation_url_dialog`, `agent_needs_input` | `user_question` | high | |
| `idle_prompt` | `agent_idle` | low | repeat-suppressed |
| `Stop` | `task_completed` | normal | fires after every turn |
| `StopFailure` | `task_failed` | high | |
| others | `notification` | low/normal | |

## Hook command

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "C:/Users/me/.cargo/bin/agentping.exe hook claude", "timeout": 10, "shell": "bash" } ] }
    ]
  }
}
```

- macOS/Linux: `"/path/to/agentping" hook claude` (shell quoting applied when needed).
- Windows with Git Bash: forward-slash path, `"shell": "bash"`.
- Windows without Git Bash: `& 'C:\path\agentping.exe' hook claude` with `"shell": "powershell"`.

The exec form (`args`) is not used yet because of reported Windows issues with it in some
Claude Code versions.

## Contract

- Reads at most 1 MiB from stdin; tolerant JSON parsing (unknown fields ignored, wrong types
  treated as missing).
- Writes **nothing** to stdout/stderr (stdout would become context for `SessionStart` /
  `UserPromptSubmit`) and always exits 0, including for malformed input.
- Sends the normalized event to the daemon (1.5 s budget) or stores it locally and spawns a
  detached sender. Typical runtime: 30–60 ms.
- Project = folder name of `CLAUDE_PROJECT_DIR`, else the git root of `cwd`, else `cwd`.

## Install / remove

```sh
agentping setup claude [--scope user|project|local] [--dry-run]
agentping setup claude --remove [--scope …]
```

- `user`: `~/.claude/settings.json` (respects `CLAUDE_CONFIG_DIR`)
- `project`: `./.claude/settings.json` (committed, shared with your team; use with care)
- `local`: `./.claude/settings.local.json`

Existing settings and hooks are preserved, a timestamped backup
(`settings.json.agentping-backup-YYYYMMDD-HHMMSS`) is written first, re-running is idempotent and
updates a moved binary path, and invalid JSON is never modified. Restart Claude Code sessions
after installing; `agentping doctor` verifies registration.

## Limitations

- Permission prompts answered within ~6 s never notify (by design of `permission_prompt`).
- Whether `PostToolUse` fires for `AskUserQuestion` is not explicitly documented; if it does not,
  question notifications are still sent after the delay.
- `Stop` fires at the end of every turn, so quick back-and-forth at the keyboard produces
  completion pings; tune with `repeat_window_secs`, rules, or `task_completed = false`.
