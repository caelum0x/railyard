# Pricing & currency model — store

**SCAFFOLD.** Reference for the two-currency model the store renders and `services/economy` enforces.

## Currencies

| Currency | Type | Earned | Spent on |
|----------|------|--------|----------|
| `shards` | Soft | Gameplay, quests, battle-pass | Champions, some cosmetics |
| `aeon`   | Hard | Purchased with real money | Cosmetics, bundles, premium pass |

## Price tiers (aeon)

| Tier | aeon | Typical item |
|------|------|--------------|
| T1   | 390  | Ward skins, emotes |
| T2   | 975  | Standard champion / basic skin |
| T3   | 1350 | Epic skin |
| T4   | 1820 | Legendary skin |
| T5   | 2500 | Bundle / ultimate |

## Rules

- The **authoritative price** is resolved server-side at `POST /v1/orders` time; the store price is a hint.
- Prices are region-aware; the store requests a localized catalog from `services/inventory`.
- No loot-box / random-purchase mechanics are modeled here (compliance — see `web/compliance`).
- Every purchasable SKU maps to a concrete, disclosed grant (no probabilistic rewards for money).
