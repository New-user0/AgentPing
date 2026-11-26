# Remote interaction (design, not implemented)

Status: **design only.** Nothing in AgentPing v0.1 can approve, answer, or send input to an
agent remotely. This document constrains how that may be added.

## What the agent actually supports

Claude Code hooks offer limited, well-defined control surfaces:

- `PermissionRequest` hooks may return `{"hookSpecificOutput": {"hookEventName":
  "PermissionRequest", "decision": {"behavior": "allow" | "deny", …}}}`, but only while the hook
  process is still running, i.e. the hook would have to **block** until a remote answer arrives
  (within its timeout).
- `PreToolUse` for `AskUserQuestion` may return `updatedInput` with `answers` to answer a
  question programmatically, again only synchronously from the hook.
- There is no documented API to inject free-form instructions into a running interactive
  session. Free-form remote instructions would require starting a new `claude -p --resume`
  process, which is a different and more dangerous capability.

Any remote feature must map to one of these documented mechanisms; AgentPing must not pretend
to support anything else.

## Principles

1. **Off by default**, enabled per agent and per action type in config, with a CLI warning.
2. **Providers are not identities.** A Discord or Telegram message is not proof of who sent it.
   Remote actions require a separate authentication step (for example a per-device key pair
   enrolled locally, or a TOTP code), not just "a message came from the right chat".
3. **Short-lived, single-use action tokens.** Each actionable notification carries a random
   token bound to (event id, allowed actions, expiry ≤ hook timeout). Tokens are stored hashed,
   consumed atomically, and replay is rejected.
4. **Allow-list of actions**: `deny` is always safe; `allow` limited to tool categories the user
   opts into (never blanket); question answers limited to the offered options.
5. **No shell, ever.** No free-form command execution from messaging providers.
6. **Confirmation for sensitive actions** and a full local audit log (who, what, when, result),
   without secrets.
7. **Fail closed.** Timeout or any validation error falls back to the normal local prompt.

## Sketch

```text
PermissionRequest hook ──(blocking, ≤ timeout)──▶ daemon: create ActionRequest {id, event, allowed, expires}
                                                   │
                     notification with deep link ◀─┘  (no secrets in the link; token hashed at rest)
                                                   │
           authenticated reply (signed, nonce) ──▶ daemon verifies device key + token + expiry
                                                   │
hook returns decision JSON ◀───────────────────────┘  or exits silently on timeout → normal prompt
```

Inbound replies need a reachable endpoint; for a local-first tool this likely means polling a
provider (Telegram `getUpdates`) rather than exposing a public webhook.
