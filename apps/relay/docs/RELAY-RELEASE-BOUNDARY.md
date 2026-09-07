# Relay release boundary

Relay is code-complete for the credential-free phase. This file separates
implemented product behavior from the live checks that require infrastructure
or credentials, so another application is not started while hidden Relay code
work remains.

## Implemented and locally verified

- TensorZero routing, native/OpenAI inference, embeddings, SSE, and batches.
- Model-aware exact microdollar quoting and success-only Mint settlement.
- Principal-owned AITP challenge/authorization and pinned Mint attestation
  verification.
- Relay-owned AITP authorization/receipt wire vocabulary and Mint golden-vector
  verification; Relay's payment core has no dependency on Agent Runtime.
- TensorZero selectors resolve to canonical catalog identities. Function routes
  and named-model routes require an explicit operator price and settlement stays
  bound to that retail identity regardless of the runtime-selected provider
  variant. Configured routes are first-class catalog entries and return the same
  public quote the billing middleware uses. Paid startup rejects any configured
  model, embedding model, or non-internal function missing from that catalog.
- `/health` includes Relay's catalog and billing authority. The Mint check is a
  protected, non-mutating ledger read, so dead storage and revoked credentials
  produce `503` instead of a false healthy result.
- Restart-durable reserve/capture/release outbox, immutable evidence, cost-event
  delivery, provider-health checkpoints, and paid-batch ownership.
- Authenticated HTTP/1 acceptance journey covering 401, AITP 402, signed paid
  retry, single provider execution, single Mint capture, durable evidence
  lookup, and settled-request replay recovery without a second call or debit.
- New durable evidence preserves every field in Mint's signed receipt, so a
  replay can return the exact original `AITP-Receipt`. Legacy reduced evidence
  remains readable but never causes Relay to fabricate a signed receipt header.
- Paid startup applies the Relay Postgres schema in one advisory-locked
  transaction and records each migration once.
- Fault-injected recovery retains a hold through a Mint outage and replays the
  identical capture command to one replacement worker without a free release or
  duplicate debit.
- Rust, JavaScript/TypeScript, synchronous/asynchronous Python, and Ruby SDKs,
  including binary multipart, exact-body one-retry AITP flows, and typed
  already-settled recovery evidence. Every SDK now requires a caller-controlled
  pinned-key verifier before promoting Relay's projection to trusted settlement;
  missing or rejected verification fails closed. Direct settlement lookup is
  verified by default; raw projection access is explicitly named `unverified`.

## Verification snapshot (2026-08-28)

- 189 tests across the nine Relay-owned Rust crates.
- 54 focused gateway tests: 42 billing, 3 cost feed, 1 provider-health, 5 AITP,
  2 catalog/quote, and 1 Relay-health test.
- 35 JavaScript tests plus strict TypeScript check and package build.
- 22 Python tests plus bytecode compilation.
- 36 Ruby tests / 103 assertions plus gem build.
- Full gateway `cargo check` and strict all-target/all-feature clippy pass.
- `git diff --check` passes.

## Deferred live acceptance checks

These are not substitute coding tasks and cannot be honestly marked executed in
the managed workspace:

1. Run `python3 tools/relay_postgres_smoke.py` on a host that permits Postgres
   shared memory. The managed sandbox rejects Postgres bootstrap at `shmget`
   even when both shared-memory modes are forced to `mmap`.
2. Start the gateway with authenticated access, `TENSORZERO_POSTGRES_URL`,
   `RELAY_BILLING_ENABLED=1`, `MINT_SERVER_URL`, `MINT_API_KEY`,
   `MINT_PAYEE_ACCOUNT`, `RELAY_AITP_PAYEE`, and
   `MINT_VERIFICATION_KEYS_JSON`. `MINT_PAYER_ACCOUNT` is optional.
3. Verify the advertised AITP address resolves to the configured Mint payee,
   then run funded, insufficient, provider-failure, stream-disconnect, batch,
   process-restart, and duplicate-request journeys against real services.
4. Load the production provider/catalog snapshot and induce provider latency,
   disconnects, and circuit recovery. Confirm one winner, one charge, loser
   cancellation, and cost-feed delivery/dead-letter recovery.

The managed sandbox also rejects binding a loopback TCP listener with
`Operation not permitted`. The credential-free acceptance journey therefore
runs over a real Hyper HTTP/1 connection backed by `tokio::io::duplex`; the
normal-host checklist still includes repeating it over TCP.

Paid startup fails closed if any required money-path dependency or schema step
is unavailable. No live check should weaken that behavior to make deployment
appear green.
