# Two-way remote interaction (Discord)

Status: **implemented, opt-in** (disabled by default). Reply from Discord and Claude Code acts on
it, using only mechanisms the Claude Code hooks API documents. There is **no shell access** from
Discord: everything goes through Claude's own permission and question flow.

## What you can do from Discord

| Action | How | Claude Code mechanism |
|---|---|---|
| **Approve / deny a permission** | Tap ✅ Approve / ❌ Deny, or reply `approve`, `deny`, or a reason (`deny: use npm ci`); the reason is shown to Claude | `PermissionRequest` hook returns `decision.behavior = allow/deny` (+ `message`) |
| **Answer a question** | Tap an option (single choice) or use the menu (multiple choice); or reply with a number, a label, or your own answer | `PreToolUse(AskUserQuestion)` returns `permissionDecision: allow` with `updatedInput.answers` |
| **Reply to continue** | Reply to a ✅ finished / ⏳ waiting / ⚠️ failed ping with new instructions | A background `Stop` hook with `asyncRewake` exits 2; Claude wakes with your text |
| **Commands** | `status`, `away`, `back`, `help` | handled by the daemon |

Replies are matched to the right session: replying to a specific notification targets that
session; a plain message goes to the most recent open prompt.

## Setup

1. Create a bot: <https://discord.com/developers/applications> → **New Application** → **Bot** →
   **Reset Token** (copy it).
2. **Bot → Privileged Gateway Intents → Message Content Intent**: on.
3. **OAuth2 → URL Generator**: scope `bot`; permissions *View Channels, Send Messages, Read
   Message History, Embed Links*; open the URL and add the bot to your server.
4. Run:

   ```sh
   agentping setup discord-bot                 # hidden prompt, or --from-env DISCORD_BOT_TOKEN
   ```

   It validates the token, uses your webhook's channel (or `--channel-id`), and **pairs your
   account**: you type a one-time code like `AP-123456` in the channel and only that Discord
   account is allowed to answer. It then enables `[remote]`, re-installs the Claude Code hooks
   with suitable timeouts and restarts the daemon.
5. Keep the daemon running (`agentping daemon start` or `agentping daemon install`). It holds the
   Discord Gateway connection; no public URL or port forwarding is needed.

## When prompts go to Discord

Projects and event types you muted for notifications (`[notifications]` toggles,
`notifications.filters`, `[[rules]]` with `notify = false`) are never routed: their prompts stay
in the terminal and finished sessions don't listen for replies. Quiet hours and provider
selection only affect notifications.

While a permission request or question waits for a Discord answer, Claude Code does **not**
show its local dialog (verified on Claude Code 2.1.332). So by default (`route = "when_away"`)
they are routed to Discord only when you are away:

- after `agentping away` (or sending `away` in Discord), until `agentping back` / `back`;
- or automatically when no prompt has been typed locally for `away_after_minutes` (default 5).

With `route = "always"`, every permission request and question is sent to Discord as well.
Back at your desk with a prompt still waiting in Discord? Run `agentping back` (or send `back`):
open permission requests and questions are withdrawn and the normal terminal prompt appears
immediately. If nobody answers within `decision_timeout_minutes` (default 10), the same happens
automatically. Typing in the terminal, finishing the turn or ending the session
withdraws open prompts; the Discord message is updated accordingly.

"Reply to continue" never blocks: after each turn a background listener waits up to
`reply_timeout_minutes` (default 60) and is replaced by the next turn's listener.

## Security model

- **Off by default.** Requires `remote.enabled = true`, a bot token in the OS credential store,
  a channel id and at least one allow-listed Discord user id.
- **Providers are not identities.** Only messages and button presses from allow-listed user ids,
  in the configured channel, are accepted; everything else is ignored (and logged). Pairing proves
  control of both the terminal and the Discord account.
- **Single-use, expiring prompts.** Each prompt has a random 128-bit id embedded in its buttons;
  it is resolved atomically exactly once, and late or replayed answers are rejected.
- **Nothing beyond the hook API.** The only effects are a permission decision, question answers,
  or a message to Claude. No commands are executed by AgentPing.
- **Audit trail.** Every remote answer is stored (who, what, when) in the `interactions` table and
  logged.
- Free-text replies are capped at 4,000 characters and passed to Claude labelled as a remote
  reply.

## Configuration

```toml
[remote]
enabled = true
discord_channel_id = "123456789012345678"
discord_user_ids = ["987654321098765432"]
continue_replies = true
permissions = true
questions = true
route = "when_away"         # or "always"
away_after_minutes = 5      # 0 = only explicit `away`
reply_timeout_minutes = 60
decision_timeout_minutes = 10
```

After changing `[remote]`, run `agentping setup claude` (hook timeouts depend on it) and
`agentping daemon restart`.

## Limitations

- The daemon must be running; without it hooks behave as notify-only.
- The local dialog is hidden while a prompt waits for Discord; use `agentping back` to bring it
  back immediately (or wait for `decision_timeout_minutes`). `route = "always"` is therefore only
  advisable if you rarely work at the keyboard.
- A reply to continue reaches Claude as a system reminder (that is how `asyncRewake` delivers
  output), not as a typed user message; Claude treats it as your next instruction.
- `-p` (headless) runs end immediately, so reply-to-continue applies to interactive sessions.
- Interrupting a running turn from Discord is not possible through the hooks API.
