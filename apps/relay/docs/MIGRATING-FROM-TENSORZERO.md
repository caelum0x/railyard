# Migrating from TensorZero to Relay

TensorZero archived its repository on 2026-06-12, so it no longer receives
fixes, provider updates or security patches. Relay is a maintained fork of
TensorZero **2026.6.0**. This guide covers what stays the same, what Relay adds,
and how to switch. Relay is not affiliated with or endorsed by TensorZero (see
[`NOTICE`](../NOTICE)).

## What stays the same

Relay keeps the TensorZero engine in `crates/` with its crate names
(`tensorzero-core`, `gateway`, ...). Nothing below was renamed:

| Surface | TensorZero 2026.6.0 | Relay |
|---|---|---|
| Binary | `gateway` | `gateway` |
| CLI | `--config-file <path-or-glob>`, `--default-config` | same |
| Config | `tensorzero.toml` schema (`[gateway]`, `[models.*]`, `[functions.*]`, `[variants.*]`, ...) | same file, same schema |
| Inference API | `POST /inference`, `POST /batch_inference`, `POST /feedback` | same |
| OpenAI-compatible API | `POST /openai/v1/chat/completions`, `/openai/v1/embeddings`, `tensorzero::model_name::*` / `tensorzero::function_name::*` selectors | same |
| Storage | `TENSORZERO_CLICKHOUSE_URL`, `TENSORZERO_POSTGRES_URL` | same variables |
| Auth | gateway API keys (`TENSORZERO_API_KEY` for clients) | same |
| Clients | TensorZero Python / Node clients and any OpenAI SDK | keep working against the same endpoints |

With billing and hedging left at their defaults (off), Relay serves a
TensorZero config exactly as TensorZero 2026.6.0 would.

## What Relay adds (all opt-in)

- **Success-only billing on Mint.** `RELAY_BILLING_ENABLED=1` puts a hold on
  the payer before inference and captures the exact provider cost afterwards.
  Failures, refusals and disconnects release the hold. Startup fails closed on
  any missing billing setting (see the [README](../README.md)).
- **A price catalog**, `relay-catalog`, with the authenticated endpoints
  `GET /v1/relay/catalog` and `POST /v1/relay/quotes`. The same catalog drives
  billing.
- **AITP-402 payment challenges**, so agents can pay per call.
- **Provider hedging, health tracking, spend caps, a cost feed** into Rago
  metering, and **per-key provider credentials**.
- **Maintained Relay SDKs** for Rust, JS/TS, Python and Ruby in `clients/relay-*`.
- **White-label resale.** [Vend](../../vend/README.md) runs your own branded
  LLM API on top of Relay, with end-user wallets, per-model markup, spend caps
  and payouts.

## Switch in four steps

1. **Build or pull the image.** From `apps/relay`, run
   `docker build -t relay -f Dockerfile .`. Or build the binary with
   `cd crates && cargo build -p gateway --release`.
2. **Keep your config.** Mount your existing `tensorzero.toml` and start with
   `gateway --config-file /app/config/tensorzero.toml`. The bind address still
   comes from `[gateway] bind_address`.
3. **Keep your environment.** Set the same `TENSORZERO_*` storage variables
   and the same provider API-key variables your config references.
4. **Point clients at Relay.** Client code does not change. Only the base URL
   does.

Then smoke-test the same requests you run in production (`/inference`, and
`/openai/v1/chat/completions` if you use it) before moving traffic over.

## What is different or not carried over

- **TensorZero Autopilot.** This was a hosted, paid upstream service. Its
  client crates (`autopilot-*`) are vendored, but no hosted Autopilot backend
  exists for them to call. The operator UI hides every Autopilot entry point
  and `/autopilot` shows a "not available in Relay" page.
- **Upstream website links and community channels** are not carried over: the
  Relay README keeps only an attribution section, and UI help links point to
  the engine docs pinned at the 2026.6.0 tag (override with
  `VITE_RELAY_DOCS_URL` at UI build time). Report Relay issues in the Railyard
  repository.
- **UI log level.** `RELAY_UI_LOG_LEVEL` sets the UI log level;
  `TENSORZERO_UI_LOG_LEVEL` still works as an alias.
- **Versioning.** Relay versions its own releases from the 2026.6.0 baseline.
  Upstream fixes after the archive date do not exist. Relay applies provider
  and security updates itself.

## Keeping the fork healthy

Relay's own code is kept separate from the TensorZero engine
([`OWNERSHIP.md`](../OWNERSHIP.md)). Railyard apps call Relay only through
`relay-contract`, `relay-sdk` or `clients/relay-*`. They never import TensorZero
internals, so maintaining the engine does not break Relay's product surface.
