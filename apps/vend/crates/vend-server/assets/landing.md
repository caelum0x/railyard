# Vend — your own OpenRouter, white-labelled

One OpenAI-compatible endpoint across every model Relay routes. Your brand,
your prices, your margin. A wallet for each of your users, automatic spend
caps, and revenue split paid out on Mint — not OpenRouter's.

[Create a workspace](https://vend.ereboslab.com/console)

## Launch in five calls

1. **Sign up** — Create a workspace, your first app and a live provisioning
   key in one call, or the form at [/console](https://vend.ereboslab.com/console).
2. **Set your price** — Default markup per app, overridable per model. The
   next request bills at the new rate and `/v1/models` reflects it
   immediately.
3. **Give a user a wallet** — `POST /v1/wallets` from your backend, or let
   the console do it. Each user holds their own Mint balance.
4. **Send a top-up link** — Creem, Polar, PayPal, Stripe, iyzico or USDC —
   whichever your market actually uses. The signed webhook deposits straight
   into the wallet.
5. **Call the model** — The user's wallet pays the provider's exact cost;
   your margin and the platform fee settle automatically on every captured
   charge.

```bash
curl $VEND/v1/chat/completions \
  -H "Authorization: Bearer $VK" -H "Vend-End-User: user_42" \
  -H "Content-Type: application/json" \
  -d '{"model":"tensorzero::model_name::openai::gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'
```

## Built for founders OpenRouter does not serve

- **Your brand, not theirs** — Your users never see OpenRouter. The
  endpoint, the invoice and the dashboard are yours.
- **Non-US top-ups** — Mint checkouts cover iyzico and USDC alongside
  Stripe, Creem, Polar and PayPal — markets Stripe alone does not reach.
- **Per-user wallets** — Every user gets a prepaid balance with its own
  daily spend cap, enforced atomically — no shared pool to drain.
- **Success-only billing** — A hold is placed first; only the exact
  provider cost plus your markup is ever captured. Failures release
  automatically.
- **Automatic payouts** — Every charge splits into provider cost, your
  margin and the platform fee on Mint — idempotent, with a reconciler for
  interrupted legs.
- **One API, every model** — The full OpenAI-compatible surface, streaming
  included, across whatever Relay routes — no separate integration per
  provider.

## Pricing

| What you pay Vend | |
|---|---|
| Platform fee | a share of your margin on captured charges, set by the operator at launch — not a per-seat or per-request fee |
| Provider cost | passed through exactly; Vend adds no markup of its own on top of what you charge your users |
| Hosting | run it yourself (open path) or on the operator's infrastructure |

Compare that to reselling through a provider's own billing: you do not
control the price, the invoice, or when you get paid. On Vend you set the
markup and Mint pays you out.

## Source

[github.com/caelum0x/railyard/tree/main/apps/vend](https://github.com/caelum0x/railyard/tree/main/apps/vend)
