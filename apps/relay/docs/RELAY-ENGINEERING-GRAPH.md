# Relay engineering graph

Last verified: 2026-08-28

Relay is a maintained TensorZero fork plus a Railyard-owned overlay. TensorZero
provides the multi-provider inference, observability, evaluation, optimization,
and configuration substrate. The owned crates below contain the product-specific
money, payment, resilience, and cross-application contract. Rust applications
use `relay-sdk`/`relay-contract`; web, Node, and Expo applications use
`clients/relay-js`; Python services use `clients/relay-python`; Ruby services
use `clients/relay-ruby`. None of these integration paths may depend on
`gateway` or TensorZero internals.

## Runtime graph

```text
Railyard application
  -> relay-sdk (Rust) / clients/relay-js (TypeScript) / clients/relay-python / clients/relay-ruby
       -> relay-contract wire vocabulary
       -> Relay HTTP surface
            -> TensorZero authentication
            -> Relay billing middleware
                 -> relay-aitp-wire (owned authorization, receipt, Mint evidence verification)
                 -> relay-catalog (selector, capability, exact quote)
                 -> relay-aitp (principal-owned challenge and replay state)
                 -> relay-billing (hold, capture/release, Mint evidence)
                 -> relay-hedge (winner-only provider execution)
                 -> Postgres settlement outbox
                      -> Mint
                      -> settlement evidence
                      -> relay-cost-feed outbox
                           -> Ledgerline/Abacus webhook consumer
```

## Owned capability matrix

| Layer | Implemented and verified | Next closure work |
|---|---|---|
| `relay-aitp-wire` | Relay-owned integer amount, authorization, receipt, header codec, and independent Mint hold-attestation verifier; fixed Mint golden vector; no dependency on Agent Runtime or Mint implementation code | Add protocol versions here before adopting them in gateway or clients |
| `relay-contract` | Exact microdollars including scientific JSON notation; model/function selectors; normalized usage; AITP-402, status/health, and settlement DTOs; TypeScript exports; checked-in v1 response fixtures | Add fixtures only when a new public response family is introduced; existing v1 shapes are frozen |
| `relay-sdk` | Safe base URL and credential handling; no redirects; bounded JSON; native/OpenAI chat and embeddings; batch start/poll; typed healthy/unhealthy status; catalog/quotes and paginated cost-event inspection/replay; arbitrary custom endpoints; incremental SSE; typed 402 and one-retry paid journeys; verified-by-default settlement lookup plus explicitly unverified recovery access; binary multipart; abandoned-stream recovery worker; already-settled evidence is trusted only through an injected caller-pinned Mint validator | Replace the duplicated Vend and Agent Runtime Relay clients after Relay release |
| `clients/relay-js` | Dependency-free browser/Node/Expo client; bounded JSON/SSE; redirects rejected; live catalog and quotes; typed 200/503 health; chat/native/embeddings/batches; binary multipart; exact-body paid retries; pinned-verifier-gated settlement evidence; paginated cost events and replay; abandoned-stream recovery handoff | Replace application-local TypeScript clients after Relay release |
| `clients/relay-python` | Dependency-free sync/async Python 3.11+ client built on the standard library; injectable transports; redirect rejection; bounded JSON/SSE; exact-body paid retries; pinned-verifier-gated settlement evidence; chat/native/embeddings/batches; binary multipart; typed health; catalog/quotes; paginated cost events; abandoned-stream recovery handoff | Replace Menivor's direct provider calls after Relay release |
| `clients/relay-ruby` | Dependency-free Ruby 2.6+ client built on Net::HTTP; injectable transports; bounded JSON/SSE; exact-body paid retries; pinned-verifier-gated settlement evidence; chat/native/embeddings/batches; binary multipart; typed health; catalog/quotes; paginated cost events; abandoned-stream recovery handoff | Adopt from Ruby consumers only after Relay release |
| `relay-catalog` | All embedded billing models; first-class TensorZero function and named-model identities; exact ceil quotes; capabilities; aliases; provider counts; schema/metadata/provider validation; canonical duplicate rejection; atomic last-good refresh; legacy/full operator catalogs; one shared billing/discovery snapshot; authenticated catalog and quote endpoints; paid-startup coverage check against every configured retail route | Runtime-check production provider credentials against their live upstreams |
| `relay-aitp` | Principal ownership; expiry; issued/authorized/consumed lifecycle; idempotent same replay; conflicting replay rejection; Postgres implementation; bounded retention cleanup; Relay-owned pinned-key Mint attestation verification with receipt and principal/request binding | Run Postgres-backed multi-process contention tests in the provisioned local stack |
| `relay-billing` | Mint-backed holds; exact success-only capture; failure release; signed evidence; batches; streaming; fencing; restart recovery | Run real Mint + Postgres composition tests in a provisioned local stack |
| `relay-hedge` | Progressive hedges; first-success/first-token winner; loser cancellation; cost cap; EWMA/circuit ranking; Postgres health snapshots | Exercise live provider adapters under induced latency and disconnect faults |
| `relay-cost-feed` | Immutable event schema; atomic settlement insertion; deterministic idempotency; leases; retry/backoff; dead letters; env-gated webhook; principal-scoped inspection, state-filtered cursor pagination, and dead-letter replay APIs/SDK; checked-in v1 page/query fixtures | Add Ledgerline/Abacus consumer contract tests after Relay release |
| `relay-storage-postgres` | Independently buildable restart-safe billing coordinator plus principal-bound challenge, immutable settlement-evidence, leased cost-event, and provider-health stores; six-migration Relay-only manifest; transactional, advisory-locked, ledgered migration runner invoked by paid gateway startup; no gateway/V8 dependency | Run database-backed contention and restart tests on a host that permits Postgres shared memory |
| Gateway composition | Catalog-backed pricing; durable AITP ownership; settlement evidence; atomic cost event; credential-ready delivery; Docker-free ephemeral Postgres smoke runner for ownership, transaction, lease, constraint, and checkpoint invariants | Execute the smoke runner on a host that permits Postgres shared memory, then run real Mint/provider restart faults in the provisioned local stack |

## Money and ownership invariants

1. Money is integer microdollars end to end; no binary float participates in a
   hold, capture, release, quote, or cost event.
2. A TensorZero function or configured named model is never billed through an
   implicit generic fallback. The operator must configure its stable retail
   identity, and response model metadata cannot reprice it after authorization.
3. Inference failure or incomplete usage releases the whole hold. Capture is the
   only debit path and occurs only for a successfully observed usage result.
4. The terminal direction is immutable. Concurrent capture/release attempts have
   one winner, and restart recovery replays the persisted command without
   rerunning inference.
5. A 402 challenge belongs to `(authenticated principal, request UUID)`. A
   payer may fund a different principal, but another Relay principal cannot
   steal that signed retry.
6. Mint is authoritative for balances, authorization signatures, receipts, and
   attestations. Relay persists the verified projection and never treats a
   client-supplied receipt as settlement authority.
7. A captured cost event is committed in the same database transaction as Mint
   evidence and billing completion. Delivery is at least once and consumers use
   `event_id` as the idempotency key.
8. Provider hedging can increase attempts but cannot increase billable winners:
   only the response that reaches the billing middleware can settle.

## Verification ledger

- Owned crates: 189 tests passed across wire protocol, contract, SDK, catalog,
  AITP, cost feed, billing, hedge, and Postgres storage packages (unit,
  integration, and doc tests).
- JavaScript/TypeScript SDK: 35 tests passed; strict TypeScript checking and
  package build pass with no runtime dependencies.
- Python SDK: 22 tests passed across sync/async paid requests, binary multipart,
  split SSE, terminal/drop recovery behavior, operator routes, bounds, and
  protocol validation; bytecode compilation passes with no runtime dependencies.
- Ruby SDK: 36 tests and 103 assertions passed across backward compatibility,
  exact-one-retry JSON/multipart payment journeys, paid SSE, terminal/drop
  recovery, operator routes, bounds, URL safety, and protocol validation; gem
  packaging passes.
- Postgres composition runner: `tools/relay_postgres_smoke.py` applies only the
  six Relay migrations to an ephemeral local cluster and checks principal
  ownership, transaction rollback/commit atomicity, event idempotency,
  concurrent `SKIP LOCKED` leases, batch constraints, and provider-health
  checkpoints. This managed sandbox rejects Postgres's bootstrap `shmget` call,
  so the runner is syntax-verified here but its database assertions remain for a
  normal local host.
- Gateway billing: 42 tests passed, covering funded/unfunded, native/OpenAI,
  embeddings, streaming, batches, over-budget capping, evidence, and paid retry.
  The HTTP/1 acceptance journey proves authenticated 402, paid retry, one
  execution/capture, evidence lookup, and deterministic
  `request_already_settled` replay recovery across an in-memory Hyper
  connection. Complete evidence reproduces Mint's exact signed receipt; legacy
  reduced evidence deliberately omits the receipt header instead of synthesizing
  cryptographic fields. The managed sandbox forbids loopback TCP binds.
- Gateway ownership addition: another authenticated principal is rejected before
  Mint verification and cannot capture a stolen AITP authorization.
- Gateway outbox: 6 concurrency/recovery tests passed.
- Gateway AITP codec: 5 tests passed.
- Gateway catalog/quote handlers: 2 tests passed; both routes stay behind the
  gateway authentication middleware and share billing's catalog instance.
- Relay health: the public readiness response includes catalog integrity and a
  protected, non-mutating Mint ledger probe when billing is enabled; disabled
  development is reported explicitly rather than treated as an error.
- Gateway cost-event handlers: authenticated inspection, state-filtered listing,
  principal ownership, dead-letter replay, and unauthenticated rejection are
  covered; the earlier two-handler verification passed before the listing
  addition.
- SDK abandoned-stream recovery: early Drop enqueues without I/O, `[DONE]`
  suppresses recovery, the bounded queue fails visibly under pressure, and up
  to the configured concurrency limit recovers without head-of-line blocking.
  The worker verifies a pending-then-terminal journey; Mint validation rejects
  cross-request replay and receipt tampering.
- Strict lint: all owned crates and the full gateway binary pass
  `clippy --all-targets --all-features -D warnings`; the full gateway also
  passes a fresh `cargo check`. Hash-verified cached upstream V8/TypeScript
  assets made the current offline rebuild possible.

The gateway binary's current focused Relay run passes 54 tests across billing,
cost-event handlers, provider-health persistence, AITP, and catalog/quotes.
The six settlement-state tests moved with their production coordinator into
`relay-storage-postgres`; its ten tests also cover the migration manifest and
prove that a failed Mint capture retains its hold, survives coordinator
replacement, is leased to exactly one recovery worker, and debits actual usage
once. The real Mint HTTP adapter is also fault-injected to prove recovery replays
the byte-identical capture path/body/idempotency key and never substitutes a
free release. The combined focused total is 61. The same three unrelated
failures remain unchanged TensorZero log-capture tests whose helper deliberately
supports one invocation per process because upstream runs them under Nextest.
The earlier whole-binary result was 61/64; no Relay test failed. The broader
gateway integration-test graph was not rebuilt together because Cargo
previously exhausted disk while
materializing it. Only generated `target` artifacts were cleaned. Remaining
environment-backed composition work stays tracked in the matrix rather than
being reported as complete.
