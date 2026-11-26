# Telegram

## Setup

1. Message [@BotFather](https://t.me/BotFather), send `/newbot`, copy the token
   (`123456789:AA…`).
2. Send any message to your new bot (or add it to a group/channel and post there).
3. `agentping setup telegram` (or `--from-env VAR` / `--stdin`). AgentPing calls `getUpdates` to
   list chats that messaged the bot and lets you pick one; `--chat-id` skips discovery.

The token goes to the OS credential store; `providers.telegram.chat_id` is stored in config.

## What is sent

`POST https://api.telegram.org/bot<token>/sendMessage` with `parse_mode: HTML`: bold title,
the (HTML-escaped, redacted, truncated) message, the Project/Agent/Event fields and an italic
footer, within Telegram's 4096-character limit. Link previews are disabled. With
`silent_low_severity = true`, low-severity events are sent without sound.

## Errors

| Response | Handling |
|---|---|
| `ok: true` | delivered |
| 429 / `parameters.retry_after` | retry after the requested delay |
| 5xx, timeouts | retry with backoff |
| 401 | permanent (invalid token) |
| 400 `chat not found` | permanent (check `chat_id`, message the bot first) |
| 403 | permanent (bot blocked or removed) |

Health check: `getMe`. The token never appears in errors or logs (URLs are stripped from
transport errors and token patterns are redacted).
