# WhatsApp (not implemented)

WhatsApp delivery is **not available** in AgentPing v0.1. `providers.whatsapp.enabled = true`
is rejected by config validation, and `agentping setup whatsapp` explains the status. This
page records the requirements for a future implementation.

## Approach

Only Meta's official **WhatsApp Business Platform (Cloud API)** will be supported.
Unofficial libraries that automate WhatsApp Web violate WhatsApp's terms, break frequently and
can get numbers banned, so they will not be used.

## Requirements for users

- A Meta (Facebook) Business account and a Meta developer app with the WhatsApp product.
- A phone number registered to the WhatsApp Business Platform (it can't be simultaneously used
  in the regular WhatsApp app); a free test number is available for development.
- A **permanent access token** (System User token) with `whatsapp_business_messaging`
  permission, and the **phone number ID**.
- The recipient's number, in international format.

## Constraints that shape the design

- **Template messages.** Business-initiated messages sent outside the 24-hour customer service
  window (opened when the user messages the business) must use a **pre-approved message
  template**. Notifications would therefore use a utility template such as
  `"{{1}} needs your attention in {{2}}: {{3}}"`, which limits formatting and free text.
- **Pricing.** Meta bills template conversations/messages by category (utility, marketing,
  authentication) and country. Pricing changes over time; check Meta's current pricing page.
- **Rate limits and quality rating.** New numbers start with limited daily
  business-initiated conversations and are subject to quality ratings.
- **Webhooks for replies** (future remote interaction) require a publicly reachable HTTPS
  endpoint, which conflicts with AgentPing's local-first design and would need a relay.

## Planned shape

- Secrets: access token in the OS credential store; `phone_number_id`, `recipient`,
  `template_name`, `template_language` in `[providers.whatsapp]`.
- Request: `POST https://graph.facebook.com/<version>/<phone_number_id>/messages` with a
  `template` message; strict host allow-listing like the other providers.
- Failure classification consistent with Discord/Telegram (429/5xx retry, auth errors permanent).
