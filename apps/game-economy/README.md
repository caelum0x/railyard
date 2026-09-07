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

```bash
cd apps/game-economy
cargo test --workspace --offline
```

The real HTTP adapter is `game-economy-mint-client`; tests exercise it against a
local HTTP Mint contract and verify that locally synthesized receipts are never
accepted as entitlement proof.
