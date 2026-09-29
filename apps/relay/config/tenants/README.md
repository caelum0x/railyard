# Relay tenants: Scribase projects (Wave P PY1.4)

Scribase projects call AI models through the Railyard console's gateway
(`apps/console/lib/ai/gateway.ts`, `POST /api/v1/ai/chat/completions`), which
forwards to Relay with one tenant key. The rules, all enforced by tests:

| Rule | Where |
|---|---|
| **BYOK only** (ADR RY-0003). A call is forwarded only when the org connected its own account for the provider the model names. Otherwise 403 `byok_required`, and nothing reaches Relay. | `lib/ai/gateway.ts`, `gateway.test.ts` |
| **Never our keys.** The console puts the customer's key in that provider's `dynamic::` slot and a refusal marker in every other slot of `tensorzero::credentials`. Relay lets a caller-supplied credential win over its stored and environment keys (`crates/gateway/src/provider_keys.rs`, `existing.entry(name).or_insert(value)`), so no request on the tenant key can use ours, not even on failover. A test compares the console's slot list with `PROVIDER_KEY_SLOTS`. | `gateway.test.ts` |
| **Credentials never travel with a request.** A body that carries `tensorzero::credentials` is refused; keys are typed once on `/connect/ai` (one-time link) and sealed with AES-256-GCM, bound to org and provider. | `lib/ai/provision.ts` |
| **Caps.** Per project key: tokens per UTC day and per month (429 with the period). The tenant key itself has a Relay spend cap against runaway loops (`relay-tenancy.sh tenant-key`). | `gateway.ts`, infra-box `stacks/railyard/relay-tenancy.sh` |
| **Traces by key.** Each call records model, provider, status, tokens and latency per project key, and carries `tensorzero::tags` (`tenant=scribase`, the org and the key id) so Relay's own traces can be filtered the same way. | `ai_traces`, bridge `ai_usage` |

The tenant key is created on the box with `relay-tenancy.sh tenant-key` (saved
0600, never printed) and given to the console as `RELAY_TENANT_KEY` by the
owner (`wrangler secret put`, reading the file).
