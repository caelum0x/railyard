# game-economy

A standalone Railyard storefront for card packs, cosmetics, and battle passes.
Prices are integer microdollars and every purchase uses Mint's hold → capture or
release lifecycle.

The entitlement path is restart-safe:

1. persist a principal/SKU/idempotency-key purchase intent;
2. create the deterministic Mint hold;
3. capture only after fulfillment succeeds, otherwise release;
4. require Mint's signed `AITP-Receipt` on the terminal response;
5. atomically commit the completed purchase and player inventory.

If the process stops after Mint captures but before the local commit, retrying
the same purchase replays the same Mint keys and recovers ownership without a
second charge. `game-economy-store::JsonFileGameState` provides atomic local
durability for a single service or desktop process. Multi-instance servers
should implement `GameStateRepository` with a transactional database.

## HTTP surface (`game-economy-server`)

| Route | Auth | Purpose |
|---|---|---|
| `GET /healthz`, `GET /v1/catalog` | public | Liveness; SKUs with prices, odds tables and item pools |
| `GET /store` | page | Player storefront: shop, inventory, pack opening, receipt history |
| `POST /v1/purchase` | player token | Mint hold -> fulfillment -> capture -> signed receipt |
| `POST /v1/packs/open` | player token | Open an owned pack; the roll seed is server-committed (keyed by the server secret) and revealed in the response for audit — players cannot choose it |
| `GET /v1/inventory/:player` | player token | Granted entitlements |
| `GET /v1/purchases/:player[/:receipt_id]` | player token | Receipt history / one receipt |
| `POST /v1/tokens` | game-server key | Issue an expiring player token |
| `POST /v1/pass/advance` | game-server key | Report the battle-pass tier a player earned |
| `POST /v1/entitlements/revoke` | game-server key | Remove a purchase's grant after a refund/chargeback |

Server-to-server routes require `X-Game-Server-Key: <GAME_SERVER_SECRET>`; a
player token is never accepted there, so players cannot advance their own pass.

Configuration: `GAME_CATALOG_PATH` (JSON: `{"products":[{"CardPack":{...}}, ...],
"pools":{"Common":[...]}}`, validated at boot; `GAME_CATALOG=demo` serves the
sample catalog), `GAME_FULFILLMENT_URL` + `GAME_FULFILLMENT_SECRET` (signed
delivery webhook — capture happens only after it returns 2xx), `GAME_SERVER_SECRET`,
`GAME_TOKEN_TTL_SECS`, `GAME_ALLOW_LEGACY_TOKENS`, plus the Mint settings listed in
`crates/game-economy-server/src/main.rs`.

```bash
cd apps/game-economy
cargo test --workspace --offline
```

The real HTTP adapter is `game-economy-mint-client`; tests exercise it against a
local HTTP Mint contract and verify that locally synthesized receipts are never
accepted as entitlement proof.
