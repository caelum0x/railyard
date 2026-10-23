# Railyard Watch's box 1 checks agent (PY2.2)

This is the piece `apps/console/README.md`'s `RAILYARD_WATCH_LIVE` /
`WATCH_AGENT_TOKEN` rows, and `apps/console/lib/watch/types.ts`'s own comment,
already assumed existed — "a Gatus instance of their own on box 1 ... an
agent next to it pulls the list of checks from this console and pushes each
result back" — and, before this change, did not. The console side
(`lib/watch/service.ts`, `lib/watch/http.ts`, `app/api/v1/watch/agent/*`) was
built and tested in an earlier round; this directory is the other half.

Nothing here is customer-facing. A customer only ever sees the console's
Watch screen and `/status/<slug>`; this box runs the actual probes.

## What runs here

- **`gatus`** (`docker-compose.yml`) — the probe engine
  ([TwiN/gatus](https://github.com/TwiN/gatus), MIT), bound to
  `127.0.0.1:8083` only. It never faces the internet and is never the
  console's own uptime monitor — just the engine behind customer checks.
- **`agent.sh`** — the bridge, run by `watch-agent.service`:
  1. `GET <console>/api/v1/watch/agent/checks` (bearer `WATCH_AGENT_TOKEN`)
  2. rewrites `config/endpoints.generated.yaml` from that list; if the list
     changed, recreates the `gatus` container so the new list takes effect
  3. `GET` gatus's own `/api/v1/endpoints/statuses` (loopback only)
  4. `POST`s the results batch to `<console>/api/v1/watch/agent/results`

  The console dedupes incoming results by `last_checked_at`
  (`lib/watch/service.ts::recordResults`), so resending results it has
  already applied is always safe — `agent.sh` keeps no state of its own.

## Install (box 1, owner or deploy agent)

```bash
sudo ./install.sh            # units + env sample, start gatus, enable the agent
sudo ./install.sh run        # one pass now, see the result
sudo ./install.sh status     # unit + container status
```

Settings are in `/etc/products/watch-agent.env` (template `agent.env.sample`).
You need:

| Var | Where it also lives | Notes |
|---|---|---|
| `WATCH_CONSOLE_URL` | — | the console's public base URL |
| `WATCH_AGENT_TOKEN` | console's `WATCH_AGENT_TOKEN` env | >= 32 random chars, **the exact same value in both places**. Generate with `openssl rand -hex 32`. |

Until both are set correctly, every pass logs "unauthorized" or "Watch is not
live" and keeps retrying — nothing pages, nothing breaks, Watch just stays
dark, same as every other `RAILYARD_WATCH_LIVE`-gated surface.

## The actual flip

1. Mint a token: `openssl rand -hex 32`.
2. Set it as `WATCH_AGENT_TOKEN` in the console's env (owner-only secret step,
   same gating pattern as Mint/OIDC/Gitea) **and** in box 1's
   `/etc/products/watch-agent.env`.
3. Run `sudo ./install.sh` on box 1.
4. Once `sudo ./install.sh run` shows a pushed result for a real check,
   set `RAILYARD_WATCH_LIVE=1` in the console's env and restart it.

## Why Gatus, not a hand-rolled prober

Research-and-reuse first: Gatus is a maintained, widely used uptime-check
engine (HTTP/TCP/DNS/ICMP conditions, its own small storage, its own JSON
API) — reusing it means `agent.sh` is ~150 lines of translation, not a new
HTTP client with retry/timeout/TLS edge cases to get right and keep right.
Nothing about Gatus is customer-visible or named in any Railyard product
surface (denylist rule in `policy/copy-denylist.json`); the console's own
copy only ever says "Watch".

## Verifying this without touching production

`agent.sh`'s two `jq` transforms (console checks -> Gatus YAML, Gatus
statuses -> console results) and the end-to-end pass were exercised against
a real `ghcr.io/twin/gatus:stable` container and the real
`apps/console/lib/watch/http.ts` handlers during development — see the
build log for the exact commands. To repeat: run `docker compose up -d` in
this directory, point `WATCH_CONSOLE_URL` at a `next dev` console with a
`WATCH_AGENT_TOKEN` set and `RAILYARD_WATCH_LIVE=1`, create one check for a
real app from the console UI, then `./agent.sh once` a couple of times a
check-interval apart and watch `/status/<slug>` go from "Waiting for the
first check" to "Operational".
