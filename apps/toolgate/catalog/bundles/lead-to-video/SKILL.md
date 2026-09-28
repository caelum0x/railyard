---
name: lead-to-video
description: Turn a lead filter into personalised short videos sent by email, using Switchyard tools (Rally + Menivor) on one key.
---

# Lead to video

Use this when the user wants outbound that stands out: a short personalised
video per prospect, delivered by email. Every step is a Switchyard tool call
(`sy call <id>` in a shell, or `call_tool` over MCP). Never ask the user for
provider keys; Switchyard injects credentials server-side.

## Steps

1. **Find companies.** `rally.leads.search` with `{"kind":"companies","filters":{...},"limit":10}`.
   Describe it first (`sy describe rally.leads.search`) to see the price per result.
2. **Enrich.** For each company domain, `rally.company.enrich` with `{"domain":"..."}`.
   Skip companies that come back with `company: null` (not billed).
3. **Verify the address.** `rally.email.verify` with `{"email":"..."}`. Only continue
   with `result: "deliverable"`.
4. **Render.** `menivor.video.render` with a `subject` naming the company and a
   15 to 30 second `script` that references one concrete fact from step 2.
   Keep `duration_s` small; the price is per second.
5. **Wait.** Poll `menivor.video.status` with the `job_id` every 20 seconds (free)
   until `videos` has a URL.
6. **Send.** `rally.email.send` with a verified `from`, the prospect in `to`, and the
   video URL in the body. Always pass an `Idempotency-Key` (for example
   `lead-to-video:<domain>`) so a retry never sends twice.

## Guardrails

- Check `sy balance` before step 4; renders are the expensive step.
- Stop and report if any call returns 402 (top up) or 429 `cap_exceeded` (ask the owner to raise the project cap).
- Do not send to role accounts (info@, sales@) or to addresses marked `risky`.
