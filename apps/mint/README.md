# apps/mint — AI Token settlement layer

> **Status: implemented standalone Rust workspace.** Eleven crates compile and the
> workspace test suite passes. It remains excluded from the root desktop Cargo
> workspace; run validation from `apps/mint` with `cargo test --workspace`.

## Run the product

For a disposable local ledger:

```bash
cd apps/mint
MINT_API_KEY=local-secret MINT_ALLOW_INMEMORY=1 cargo run -p mint-server
curl http://127.0.0.1:8080/healthz
```

Production requires `DATABASE_URL`, `MINT_API_KEY`, and a unique 32-byte
`MINT_SIGNING_KEY` encoded as hex. The server refuses to use its deterministic
development signer when Postgres is configured. AITP-402 also needs the payer
verification-key registry, supplied as a JSON array:

```bash
export MINT_PAYER_KEYS_JSON='[{"payer":"acct_customer","pubkey_id":"key_1","public_key_hex":"<64 hex chars>"}]'
```

The server runs migrations at startup. Payment on-ramp adapters are activated by
their provider environment variables; with none configured, deposit confirmation
uses the development ramp and must not be treated as production funding.

## Fund an account

`POST /v1/checkouts` creates a provider-hosted checkout for an existing Mint
account. The amount must be positive whole USD cents expressed as integer
microdollars. Mint binds the account ID into provider metadata; the provider's
verified webhook is the only operation that credits the account.

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/checkouts \
  -H "x-api-key: $MINT_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"provider":"stripe","account_id":"acct_customer","amount_micros":20000000,
       "success_url":"https://app.example.com/billing/success",
       "cancel_url":"https://app.example.com/billing"}'
```

`success_url` / `cancel_url` are optional per request; otherwise Mint uses
`MINT_CHECKOUT_SUCCESS_URL` / `MINT_CHECKOUT_CANCEL_URL`, then its own hosted
`/checkout/success` and `/checkout/cancel` pages under `MINT_PUBLIC_URL`.

Configure at least one checkout-capable provider and its webhook secret:

| Provider | Keys | Notes |
|---|---|---|
| Stripe | `STRIPE_API_KEY`, `STRIPE_WEBHOOK_SECRET` | credits on `payment_intent.succeeded` |
| PayPal | `PAYPAL_CLIENT_ID`, `PAYPAL_CLIENT_SECRET`, `PAYPAL_WEBHOOK_ID`, `MINT_PUBLIC_URL` | buyer returns to `/checkout/paypal/return`, which captures and credits (order id = deposit ref) |
| Creem | `CREEM_API_KEY`, `CREEM_WEBHOOK_SECRET`, `CREEM_PRODUCT_ID`, optional `CREEM_UNIT_PRICE_CENTS` (default 100) | a fixed-price credit product bought in units; amount must be a multiple of the unit price |
| Polar | `POLAR_ACCESS_TOKEN`, `POLAR_WEBHOOK_SECRET`, `POLAR_PRODUCT_ID` | the product must be custom-price ("pay what you want") |
| USDC | `USDC_RPC_URL`, `COINBASE_COMMERCE_API_KEY`, `COINBASE_COMMERCE_WEBHOOK_SECRET` | |
| Iyzico | `IYZICO_API_KEY`, `IYZICO_SECRET_KEY`, `IYZICO_WEBHOOK_SECRET` | |

Mobile IAP providers confirm store purchases but do not expose this
arbitrary hosted-checkout route.

## Cash out, payouts and reconciliation

`POST /v1/redeem` burns AIT and pays out. The payout is checked for
routability BEFORE the burn (known active `<provider>:` prefix on `payout_ref`,
a payout-capable provider, a destination kind it supports, whole cents): an
unroutable payout returns `422 payout_unroutable` and nothing is burned.
Destinations: `{"kind":"stripe","account":"acct_…"}`,
`{"kind":"paypal","email":"…"}`, `{"kind":"crypto","chain":"ethereum","address":"0x…"}`.

Every payout attempt is persisted:

- `GET /v1/payouts?status=failed|pending|paid&account_id=&cursor=` — the payout log;
- `GET /v1/payouts/{payout_ref}` — one payout (pending ones are refreshed from
  the provider: PayPal batch status, on-chain receipt);
- `POST /v1/payouts/{payout_ref}/retry` — re-send a failed payout with the same
  provider idempotency key (paid/pending payouts are never re-sent);
- `GET /v1/reconciliation` — per-provider deposits minted vs. burns vs. payout
  outcomes, the burned-but-unpaid worklist and the supply identity check.

## KYC onboarding

- `GET /v1/accounts/{id}/kyc` — provider-verified status (`unverified`,
  `pending`, `approved`, `declined`, or `unavailable` with no provider);
- `POST /v1/accounts/{id}/kyc/session` `{"return_url":"https://…"}` — starts a
  hosted Persona inquiry (needs `PERSONA_INQUIRY_TEMPLATE_ID`) or Stripe
  Identity session and returns its URL.

## Statements and accounts

- `GET /v1/accounts?after=&limit=` — accounts with live balances;
- `GET /v1/accounts/{id}/statement?from_ms=&to_ms=` — opening balance, every
  entry with its signed delta, credit/debit totals and closing balance;
- `GET /v1/accounts/{id}/receipts?after_seq=&limit=&from_ms=&to_ms=` — a page of
  the hash-linked receipt chain.

## Operator console

`GET /admin` serves the operator console (reserve and solvency, Merkle audit
root, reconciliation, accounts and statements, KYC, payouts with retry, hosted
checkout creation). It holds no data itself; every panel calls the API with the
operator's `X-API-Key`.

## API keys

`MINT_API_KEY` is the master key. Give each consuming service its own scoped
key instead:

```bash
export MINT_API_KEYS_JSON='[
  {"name":"ledgerline","key":"<32+ random chars>","scopes":["read","spend"]},
  {"name":"abacus","key":"<32+ random chars>","scopes":["read","spend"]},
  {"name":"autogtm","key":"<32+ random chars>","scopes":["read","accounts","spend","funding"]}
]'
```

Scopes: `read` (all GETs), `accounts`, `spend` (transfers/holds/streams/
allowances/AITP), `funding` (checkouts, mint), `payouts` (redeem, retry),
`admin`. Rotate a key by adding the new one under the same name, redeploying
callers, then removing the old one.

Mint turns AI credits into a first-class **transferable token (AIT)** —
sendable/receivable/deliverable between accounts, agents, providers, and models —
with **web3 mechanics and zero crypto** (addresses, signed transfers,
conservation-of-supply, verifiable hash-linked receipts; fiat-backed, off-chain,
no gas). It's meant to become the canonical settlement layer that ReelForge
(`apps/api`), **Relay** (`apps/relay`), and desktop all spend on.

Create a shareable receive address with
`POST /v1/accounts/{account_id}/addresses`, then pass the returned `ait1…`
value as `to` in `POST /v1/transfers`. An address is routing only: Mint resolves
it to the authoritative recipient before committing the ledger operation, while
the sender remains an authorized `acct_…` supplied by the trusted product
backend. Transfer responses contain the exact signed sender receipt plus a
privacy-safe `mint-transfer-attestation-v1`; exact idempotent replay returns the
same evidence. The Rust, Python, and TypeScript SDKs all require that proof
shape. The dynamic-language clients additionally reject a nominally successful
response when its sender, amount, operation, or journal sequence does not match
the submitted transfer. Cryptographic acceptance still requires an
operator-pinned key from the public rotation document; discovery alone is not a
trust decision.

## Design docs (source of truth)
- **[docs/mint/PLAN.md](../../docs/mint/PLAN.md)** — architecture, crates, phases, invariants, product surfaces.
- **[docs/mint/AITP-402-SPEC.md](../../docs/mint/AITP-402-SPEC.md)** — the wire protocol: AIT unit, addresses, ledger ops, the AITP-402 payment handshake, hash-linked receipts.
- **[docs/mint/IMPLEMENTATION.md](../../docs/mint/IMPLEMENTATION.md)** — build-grade detail: Postgres schema, per-phase task lists, wallet integration/migration, reserve reconciliation, fee math, threat model, compliance tiers, SLOs, conformance suite.

## Implemented crates
`mint-core` · `mint-ledger` · `mint-payments` · `mint-protocol` ·
`mint-receipts` · `mint-reserve` · `mint-server` · `mint-store` ·
`mint-store-postgres` · `mint-stream` · `mint-sdk`

## Product gaps

Payer-key and API-key rotation is config-and-restart rather than an
authenticated control plane, and scoped keys are per-capability rather than
per-account. Redemption fails closed until a real KYC provider is configured: set
`MINT_KYC_PROVIDER=persona|stripe_identity` plus the provider key
(`PERSONA_API_KEY` / `STRIPE_API_KEY`) to install the `VerifiedKycGate`, which
authorizes each cash-out against the account's provider-verified identity and a
per-tx payout ceiling; with nothing configured every redeem is denied and no
fake verification is ever used in production. Cash redemption also depends on a
payout-capable provider (USDC pays out an ERC-20 `transfer` to the configured
token contract; Creem and mobile IAP are deposit-only).
The longer-term custody, anchoring, fee, and multi-currency decisions remain in
`IMPLEMENTATION.md`.

## Product family
**ReelForge** (video) · **Relay** (inference gateway, `apps/relay`) · **Mint** (this).
All inherit ADR-0001 (integer-microdollar money) + ADR-0002 (success-only billing).
