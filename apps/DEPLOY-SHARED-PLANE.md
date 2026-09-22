# Railyard shared plane — co-deploy on the A1 box

How the four money/metering services — **rago**, **relay-gateway**, **mint**,
**abacus** — run side by side on the single Oracle A1 (arm64) box, behind one
**Caddy** front door, sharing **one Postgres** (distinct databases) and one
**ClickHouse** for event/observability storage.

Each service is self-contained: it binds a distinct host port, owns its own
database, and **runs its schema migrations at process startup** — so a deploy is
"pull image, set env, restart"; there is no separate migration step to forget.

---

## 1. Topology

```
                          ┌──────────────── Caddy (:80/:443, TLS) ───────────────┐
                          │  rago.<domain>    → 127.0.0.1:3000  (rago API+GraphQL)│
   internet ───────────►  │  relay.<domain>   → 127.0.0.1:3100  (relay-gateway)   │
                          │  mint.<domain>    → 127.0.0.1:8080  (mint-server)     │
                          │  abacus.<domain>  → 127.0.0.1:8100  (abacus-proxy)    │
                          └───────────────────────┬──────────────────────────────┘
                                                  │  (all services bind 127.0.0.1 only)
        ┌─────────────────────────────────────────┼───────────────────────────────┐
        │                                          │                               │
   ┌────▼─────┐   ┌──────────────┐   ┌─────────────▼┐   ┌──────────┐        ┌───────▼────────┐
   │  rago    │   │ relay-gateway│   │    mint      │   │  abacus  │        │   Postgres     │
   │  :3000   │   │   :3100      │   │    :8080     │   │  :8100   │        │  (one server)  │
   │  db=rago │   │ ClickHouse   │   │   db=mint    │   │ db=abacus│        │  rago/mint/    │
   └────┬─────┘   └──────┬───────┘   └──────┬───────┘   └────┬─────┘        │  abacus DBs    │
        │                │                  │                │              └────────────────┘
        └── Postgres ────┘  ClickHouse ─────┴── Postgres ────┘ ── Postgres          │
                            (rago + relay share the ClickHouse event store)   ┌─────▼──────┐
                                                                              │ ClickHouse │
                                                                              └────────────┘
```

### Port map (host)

| Service        | Host port | Bind env / config                     | Notes |
|----------------|-----------|---------------------------------------|-------|
| rago (API+GraphQL+worker) | **3000** | `RAGO_API_ADDR=0.0.0.0:3000` | one process: API + billing daemon + webhook re-delivery |
| relay-gateway (TensorZero) | **3100** | `config/tensorzero.toml → bind_address` **or** container port map `3100:3000` | TensorZero's own default is `:3000`, which collides with rago — **remap to 3100** |
| mint-server    | **8080** | `MINT_BIND_ADDR=0.0.0.0:8080` | AITP-402 ledger |
| abacus-proxy   | **8100** | `ABACUS_BIND_ADDR=0.0.0.0:8100` | agent spend proxy (default is already `:8100`) |

> **Why relay moves to 3100:** relay-gateway is TensorZero, whose
> `bind_address` defaults to `0.0.0.0:3000` — the same port rago claims. On the
> shared box pick a distinct host port for relay. Either edit
> `apps/relay/config/tensorzero.toml` `bind_address = "0.0.0.0:3100"`, or keep
> the internal `:3000` and publish it on the host as `3100:3000` in the
> container runtime. This doc assumes host **3100**.

Bind every service to `127.0.0.1` (loopback) and let Caddy be the only public
listener. The table shows `0.0.0.0` because that is each app's in-container
default; publish them as `127.0.0.1:<port>:<port>` so nothing but Caddy is
reachable from the internet.

---

## 2. One Postgres, distinct databases

A single Postgres server (managed, e.g. Neon, or a local container on the box)
hosts one database per stateful service. Isolating by **database** (rather than a
shared database with per-app schemas) keeps each app's embedded migrator as the
sole owner of its DDL and avoids cross-app migration-history collisions —
important because rago was previously dump-restored and its `_prisma_migrations`
were empty (see memory). Distinct databases sidestep that class of drift.

```sql
-- run once as a superuser, then hand each app its own least-privilege role
CREATE DATABASE rago;
CREATE DATABASE mint;
CREATE DATABASE abacus;

CREATE ROLE rago_app   LOGIN PASSWORD '...';
CREATE ROLE mint_app   LOGIN PASSWORD '...';
CREATE ROLE abacus_app LOGIN PASSWORD '...';

GRANT ALL PRIVILEGES ON DATABASE rago   TO rago_app;
GRANT ALL PRIVILEGES ON DATABASE mint   TO mint_app;
GRANT ALL PRIVILEGES ON DATABASE abacus TO abacus_app;
```

- If your provider allows only **one** database (some free tiers do), fall back
  to one database with a **schema per app** (`rago`, `mint`, `abacus`) and set
  each DSN's `search_path` / `options=-csearch_path=<schema>`. Keep the roles
  separate so one app cannot read another's tables.
- **relay-gateway (TensorZero) does not use Postgres.** Its state store is
  **ClickHouse** (inference/observability). rago can *optionally* use the same
  ClickHouse for its event store. Point both at one ClickHouse database.

### Migrations at startup — confirmed per app

| Service | Mechanism | Trigger |
|---------|-----------|---------|
| rago    | embedded `sqlx::migrate!("./migrations")` (`rago-db` `MIGRATOR`), run on boot | automatic when `DATABASE_URL` set |
| mint    | server runs migrations at startup | automatic when `DATABASE_URL` set (production mode) |
| abacus  | migrated Postgres product store | automatic when `ABACUS_DATABASE_URL` set |
| relay   | TensorZero applies its ClickHouse migrations at gateway startup | automatic when `CLICKHOUSE_URL` set |

No app needs a manual pre-deploy `migrate` command; restarting the process is
sufficient. After importing rago seeds (see `rago/seeds/`), no migration change
is involved — seeds go over the REST API, not the schema.

---

## 3. Env matrix

Shared building blocks (define once, reuse in each app's env):

```bash
PG_HOST=<postgres-host>          # e.g. ep-xxx.eu-central-1.aws.neon.tech
PG_SSL=require                   # rago defaults SSL ON in prod
CH_URL=http://127.0.0.1:8123     # shared ClickHouse
```

### rago  (`apps/rago/.env`)

| Var | Value | Required |
|-----|-------|----------|
| `DATABASE_URL` | `postgres://rago_app:***@$PG_HOST:5432/rago?sslmode=require` | yes |
| `RAGO_API_ADDR` | `127.0.0.1:3000` | yes |
| `RAGO_JWT_SECRET` | `openssl rand -hex 32` (refuses to boot without it in release) | yes |
| `RAGO_TICK_SECS` | `15` | no |
| `RAGO_BILLING_HOUR` | `0` (UTC) | no |
| `RAGO_RATE_LIMIT` | `6000` | no |
| `CLICKHOUSE_URL` / `CLICKHOUSE_DATABASE` | `$CH_URL` / `rago` | optional event store |
| `RUST_LOG` | `info` | no |

### relay-gateway  (`apps/relay`)

| Var / config | Value | Required |
|--------------|-------|----------|
| `bind_address` (`config/tensorzero.toml`) | `0.0.0.0:3100` (or map `3100:3000`) | yes |
| `CLICKHOUSE_URL` | `$CH_URL` (its primary datastore) | yes |
| provider keys (OpenAI/Anthropic/etc.) | per `config/tensorzero.toml` model providers | as used |

### mint  (`apps/mint/.env`)

| Var | Value | Required |
|-----|-------|----------|
| `MINT_BIND_ADDR` | `127.0.0.1:8080` | yes |
| `DATABASE_URL` | `postgres://mint_app:***@$PG_HOST:5432/mint?sslmode=require` | yes (enables production mode) |
| `MINT_API_KEY` | long random shared key for non-health routes | yes |
| `MINT_SIGNING_KEY` | hex-encoded 32-byte receipt-signing seed | yes |
| `MINT_PAYER_KEYS_JSON` | AITP-402 payer verification keys (JSON array) | yes (for paid traffic) |
| `MINT_KYC_PROVIDER` (+ `PERSONA_API_KEY` / `STRIPE_API_KEY`) | real KYC for cash-out (else redeem is DENIED) | for redemptions |
| `USDC_RPC_URL` / `USDC_TOKEN_CONTRACT` | crypto on/off-ramp | optional |

### abacus  (`apps/abacus`)

| Var | Value | Required |
|-----|-------|----------|
| `ABACUS_BIND_ADDR` | `127.0.0.1:8100` | yes |
| `ABACUS_DATABASE_URL` | `postgres://abacus_app:***@$PG_HOST:5432/abacus?sslmode=require` | yes (enables shared Postgres store + hold recovery) |
| `ABACUS_API_KEY` | proxy secret (`X-API-Key`) | yes |
| `UPSTREAM_BASE_URL` | e.g. `https://api.openai.com` | yes |
| `ABACUS_PROVIDER` | `openai` (default) | no |
| `ABACUS_AGENT_CONFIG` | path to agent → treasury/payee JSON | yes |
| `ABACUS_PRICING_CONFIG` | per-provider/model rate overlay JSON | optional |
| `ABACUS_RECEIPT_LOG` | path for signed receipt projection | no |
| Mint linkage | `MINT_*` base URL + API key so abacus holds settle against mint | yes (for real spend) |

---

## 4. Caddy front door

`/etc/caddy/Caddyfile` — subdomain routing, automatic TLS:

```caddyfile
rago.example.com {
	encode zstd gzip
	reverse_proxy 127.0.0.1:3000
}

relay.example.com {
	encode zstd gzip
	reverse_proxy 127.0.0.1:3100
}

mint.example.com {
	encode zstd gzip
	reverse_proxy 127.0.0.1:8080
}

abacus.example.com {
	encode zstd gzip
	reverse_proxy 127.0.0.1:8100
}
```

Path-based routing on a single host is also fine if you prefer one domain — but
mind that these apps use absolute paths (`/events`, `/plans`, `/v1/...`,
`/v1/proxy`), so a `handle_path` strip prefix is required per app; subdomains
avoid that entirely and are the recommended layout.

Reload after edits: `caddy reload --config /etc/caddy/Caddyfile`.

---

## 5. Process supervision (PM2 / systemd)

The box already runs PM2 (see deploy memory). Each service is one long-lived
process; PM2 restarts it and re-runs its startup migrations automatically.

```bash
# examples — adjust binary paths to the shipped standalone builds
pm2 start ./rago-server        --name rago    --cwd apps/rago         # binds :3000
pm2 start ./relay-gateway      --name relay   --cwd apps/relay        # binds :3100
pm2 start ./mint-server        --name mint    --cwd apps/mint         # binds :8080
pm2 start ./abacus-proxy       --name abacus  --cwd apps/abacus       # binds :8100
pm2 save
```

> A1 is arm64 with limited RAM/disk — build the standalone binaries off-box
> (linux/arm64) and ship the artifacts, as with the vcpeer deploy method. Do not
> compile on the box.

---

## 6. Deploy order & smoke test

1. Provision Postgres (3 DBs + roles) and ClickHouse.
2. Start **rago**, **mint**, **abacus** — each migrates its DB on boot.
3. Start **relay-gateway** (ClickHouse migrations on boot).
4. Bring up Caddy.
5. Health checks:
   ```bash
   curl -fsS https://rago.example.com/health        # rago
   curl -fsS https://mint.example.com/healthz       # mint
   curl -fsS https://relay.example.com/health       # relay-gateway (TensorZero)
   curl -fsS https://abacus.example.com/healthz     # abacus
   ```
6. Seed rago catalogs per consumer (see `rago/seeds/README.md`):
   ```bash
   RAGO_API_URL=https://rago.example.com RAGO_API_KEY=<autogtm-key>      ./rago/seeds/import.sh rago/seeds/autogtm.json
   RAGO_API_URL=https://rago.example.com RAGO_API_KEY=<menivor-key>      ./rago/seeds/import.sh rago/seeds/menivor.json
   RAGO_API_URL=https://rago.example.com RAGO_API_KEY=<opentypesafe-key> ./rago/seeds/import.sh rago/seeds/opentypesafe.json
   ```

---

## 7. Notes & gotchas

- **Port collision (rago ↔ relay):** both default to `:3000`. relay must move to
  `3100` (§1). This is the single most likely footgun on the shared box.
- **Neon migration history:** the rago DB was dump-restored with an empty
  `_prisma_migrations`; keeping rago on its **own** database lets its embedded
  sqlx migrator own the schema cleanly. Reconcile any pre-existing drift with
  idempotent additive SQL before first boot (see memory).
- **Secrets:** never commit `.env`; supply `RAGO_JWT_SECRET`, `MINT_API_KEY`,
  `MINT_SIGNING_KEY`, `ABACUS_API_KEY` from the secret store. Rotate anything
  ever pasted in chat.
- **Mint is the authoritative money ledger;** abacus holds settle against it.
  Keep their API keys and the abacus agent-config treasury/payee identities in
  sync, or abacus falls back to single-replica JSON snapshots.
