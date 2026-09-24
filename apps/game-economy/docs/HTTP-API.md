# game-economy HTTP API

The contract our Expo games (and anyone else's game) call. The server is
`game-economy-server` (default bind `0.0.0.0:8102`). A zero-dependency
TypeScript client lives at [`../clients/ts/game-economy.ts`](../clients/ts/game-economy.ts).

## Model

- **Wallet / premium currency.** A player's spendable balance is their Mint
  account balance, in integer microdollars (µ$, 1 USD = 1,000,000). Mint's
  journal is the ledger: every purchase is a hold -> capture with a signed
  receipt, every refund and IAP credit is an idempotent Mint transfer. Games
  display it as coins/gems by dividing by a fixed rate of their choosing.
- **Soft currency** (earned in play, not bought) stays in the game's own save
  data. It has no cash value, so it does not belong on the money rail.
- **Goods.** Card packs (server-rolled, auditable seed), cosmetics
  (single-owner) and battle passes (tiers advanced by the game server).
- **IAP.** App Store / Google Play purchases are validated upstream (RevenueCat
  webhook or your game backend), then credited to the player's Mint account
  from `GAME_IAP_FUNDING_ACCOUNT`. Credit amounts come from the operator's
  `GAME_IAP_PRODUCTS` table, never from the request.

## Envelope and errors

```json
{ "success": true,  "data": { ... } }
{ "success": false, "error": { "code": "insufficient_funds", "message": "..." } }
```

| Status | Codes |
|---|---|
| 400 | `bad_request` |
| 401 | `unauthorized` |
| 402 | `insufficient_funds`, `currency_already_spent` |
| 403 | `forbidden` (token is for another player) |
| 404 | `unknown_product`, `receipt_not_found`, `pack_not_owned`, `battle_pass_not_owned`, `mint_account_not_found` |
| 409 | `already_owned`, `pack_already_opened`, `idempotency_conflict`, `already_reversed`, `catalog_conflict` |
| 410 | `product_unavailable` (archived SKU) |
| 422 | `unknown_iap_product`, `anonymous_app_user`, `invalid_catalog` |
| 502 | `mint_error`, `fulfillment_failed`, `iap_funding_exhausted` |
| 503 | `server_auth_not_configured`, `iap_not_configured`, `revenuecat_not_configured` |

## Auth

| Caller | Header | Where the secret lives |
|---|---|---|
| Player (the app) | `Authorization: Bearer <player token>` | Issued by `POST /v1/tokens`; expires (default 24 h) |
| Game backend | `X-Game-Server-Key: <GAME_SERVER_SECRET>` | Server only, never in an app bundle |
| RevenueCat | `Authorization: <GAME_REVENUECAT_WEBHOOK_AUTH>` | Set in the RevenueCat webhook settings |
| Operator console | `Authorization: Bearer <GAME_ADMIN_TOKEN>` | Operator only |

## Player routes

### `GET /v1/catalog` (public)
`{ products: [{ sku, price_micros, kind, name, details }], pools }`

### `POST /v1/purchase`
Body `{ player, product_sku, idempotency_key }`. `player` must equal the
token's player. Returns `{ entitlement_id, product_sku, charged_micros,
receipt_id, settlement_seq, settlement_proof, idempotent_replay }`. Retrying
with the same `idempotency_key` returns the original result and never charges
twice; generate the key once per purchase attempt and persist it.

### `POST /v1/packs/open`
Body `{ player, entitlement_id }` -> `{ items: [{ slot, rarity, name, item_id }], seed }`.
The seed is derived by the server and revealed for audit.

### `GET /v1/wallet/:player`
The shop screen's single read: the player's Mint balance plus inventory.
`{ player, available_micros, held_micros, inventory }`. `available_micros` is
spendable now; `held_micros` is reserved by purchases still in flight. A player
Mint has never credited reads as `0`. `player` must equal the token's player
(`403 forbidden` otherwise).

### `GET /v1/inventory/:player`
`{ packs: { <entitlement_id>: {...} }, cosmetics: { <item_id>: name }, battle_pass }`

### `GET /v1/purchases/:player`, `GET /v1/purchases/:player/:receipt_id`
Receipt history with `status` (`completed` / `refunded` / `revoked`).

## Game-backend routes (`X-Game-Server-Key`)

| Route | Body | Result |
|---|---|---|
| `POST /v1/tokens` | `{ player, ttl_seconds? }` | `{ player, token, expires_at }` |
| `POST /v1/players` | `{ owner_ref }` (your own player id) | `{ account_id, owner_ref }`: opens a Mint user account. Call once per player and store `account_id`; every call opens a new account |
| `POST /v1/iap/credit` | `{ player, platform, transaction_id, product_id }` | `{ direction: "credit", amount_micros, settlement_seq, receipt_hash, idempotent_replay, ... }` |
| `POST /v1/iap/reverse` | same | store refund: moves the credit back; `402 currency_already_spent` if the player already spent it |
| `POST /v1/pass/advance` | `{ player, pass_sku, tier }` | `{ pass_sku, tier }` (monotonic, clamped) |
| `POST /v1/refunds` | `{ player, receipt_id, reason? }` | refund through Mint and remove the grant |
| `POST /v1/entitlements/revoke` | `{ player, receipt_id, reason? }` | chargeback: remove the grant, no money moved |

IAP idempotency key in Mint: `iap:<platform>:<transaction_id>` (reverse:
`...:reverse`). A replayed credit returns `idempotent_replay: true` and moves
no money.

### Which IAP path to use

Railyard has two ways to turn a store purchase into spendable balance. Pick
ONE per product; pointing both at the same RevenueCat products credits twice.

| Path | Credits | Verification | Use when |
|---|---|---|---|
| Mint on-ramp `POST <mint>/v1/deposits/revenuecat/webhook` (also `adapty`) | the USD price paid | webhook secret + re-confirmation against the RevenueCat REST API | 1 USD paid = 1 USD of balance |
| game-economy `/v1/iap/*` (below) | the operator's `GAME_IAP_PRODUCTS` amount, from a funding account | webhook secret (RevenueCat) or your backend's own receipt check | currency packs whose grant differs from price (bonus packs), or games without RevenueCat |

## RevenueCat webhook: `POST /v1/iap/revenuecat`

Point a RevenueCat webhook at this route and set its Authorization header to
`GAME_REVENUECAT_WEBHOOK_AUTH`. RevenueCat has already validated the receipt
with Apple/Google.

- `INITIAL_PURCHASE`, `NON_RENEWING_PURCHASE`, `RENEWAL` for a product listed
  in `GAME_IAP_PRODUCTS` -> credit. `REFUND` -> reverse.
- Any other event type, or a product not in the table (e.g. a subscription
  handled by RevenueCat entitlements), -> `200 { ignored: true }`.
- `app_user_id` must be the player's Mint account id: call
  `Purchases.logIn(<mint account id>)` before selling currency. Anonymous ids
  answer `422 anonymous_app_user`, which shows as a failed delivery in the
  RevenueCat dashboard so it can be fixed and resent.
- The platform label is `revenuecat-<store>` (e.g. `revenuecat-app_store`).

## Configuration

See `crates/game-economy-server/src/main.rs` for the full list. IAP adds:

| Env | Meaning |
|---|---|
| `GAME_IAP_FUNDING_ACCOUNT` | Mint account credits are paid from. Enables `/v1/iap/*`. The operator funds it in Mint and reconciles it against App Store / Play payouts. |
| `GAME_IAP_PRODUCTS` | `store_product_id=micros,...`, e.g. `coins_100=990000,coins_550=4990000` |
| `GAME_REVENUECAT_WEBHOOK_AUTH` | Exact webhook Authorization value (>= 16 chars). Enables `/v1/iap/revenuecat`. |

## Expo usage

```ts
import { GameEconomyPlayer, GameEconomyError, newIdempotencyKey } from "./lib/railyard/game-economy";

// Your backend returns { player, token } after its own sign-in.
const store = new GameEconomyPlayer(process.env.EXPO_PUBLIC_GAME_ECONOMY_URL!, player, () => session.token);

const { products } = await store.catalog();
const key = newIdempotencyKey(); // persist with the pending purchase
try {
  const bought = await store.purchase("pack.standard", key);
  const { items } = await store.openPack(bought.entitlement_id);
} catch (e) {
  if (e instanceof GameEconomyError && e.code === "insufficient_funds") showBuyCurrencySheet();
  else throw e;
}
```

Game backend (Node / Workers), after verifying a receipt itself:

```ts
import { GameEconomyServer } from "./game-economy";
const economy = new GameEconomyServer(process.env.GAME_ECONOMY_URL!, process.env.GAME_SERVER_SECRET!);
const { token } = await economy.issueToken(player);
await economy.creditIap({ player, platform: "app_store", transactionId, productId: "coins_550" });
```

## Known gaps

- There is no balance-read route yet. The purchase path reports
  `balance_after_micros` only inside the Mint capture; games that need a
  wallet display must read the Mint account balance through their backend.
- game-economy does not parse raw Apple/Google receipts itself; it relies on
  RevenueCat or the game backend for that verification.
- Mint accounts for players must exist before the first credit (created by the
  game backend at sign-up).
