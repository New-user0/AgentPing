# Discord

## Setup

1. Pick (or create) a server and channel. **Server Settings → Integrations → Webhooks → New
   Webhook**, select the channel, **Copy Webhook URL**.
2. `agentping setup discord` (hidden prompt), or `--from-env VAR`, or `--stdin`.
3. In the Discord mobile app, set the channel's notifications to **All Messages**.

## What is sent

`POST https://discord.com/api/webhooks/<id>/<token>?wait=true` with:

- `content`: the title plus project (this drives the phone's push preview)
- one embed: title, body (message text, redacted/truncated), fields *Project*, *Agent*,
  *Event* (and *Path* only if `include_project_path = true`), a color by event type, the event
  timestamp and an `AgentPing · session abcd1234` footer
- `allowed_mentions: {"parse": []}` so agent text can't mention anyone
- `username` from `providers.discord.username`

Discord limits (title 256, description 4096, field 1024, 25 fields, 6000 total, content 2000) are
enforced before sending.

## Errors

| Response | Handling |
|---|---|
| 2xx | delivered |
| 429 | retry after `retry_after` (body) or `Retry-After` |
| 5xx, timeouts, connection errors | retry with exponential backoff |
| 401/403/404 | permanent: webhook deleted or invalid; run `agentping setup discord` |
| other 4xx | permanent, Discord's message is recorded |

`agentping providers --check` / `doctor` perform a `GET` on the webhook (returns its name without
posting a message).
