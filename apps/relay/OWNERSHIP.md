# Relay ownership boundary

Relay is a Railyard product built on a maintained Apache-2.0 TensorZero fork.
Railyard controls the complete Relay product surface; it does not claim
TensorZero's upstream authorship or copyrights.

## Railyard-owned product surface

- Relay's public HTTP contracts, catalog and quote semantics.
- AITP challenge ownership, authorization binding, and replay policy.
- Integer-microdollar pricing, Mint hold/capture/release integration, verified
  settlement evidence, restart-safe outboxes, and cost events.
- Provider hedging policy, health persistence, readiness composition, and
  billing fail-closed rules.
- Rust, JavaScript/TypeScript, Python, and Ruby Relay SDK behavior.
- Relay migrations, operator configuration, release criteria, and product
  documentation.

These live primarily in `crates/relay-*`, `clients/relay-*`, the Relay-specific
gateway modules, Relay Postgres migrations, `tools/`, and `docs/RELAY-*`.

## Upstream foundation

TensorZero supplies the multi-provider inference engine, provider adapters,
streaming, observability, evaluation, optimization, MCP, and its original UI
and documentation. Those sources retain their existing Apache-2.0 notices and
history. Relay may modify, distribute, and commercially operate that permissive
foundation under the checked-in `LICENSE`, but attribution is preserved.

## Independence rules

1. Railyard applications integrate only through `relay-contract`, `relay-sdk`,
   or a `clients/relay-*` package; they do not import TensorZero gateway internals.
2. Product-critical payment vocabulary lives in `relay-aitp-wire`, not Mint,
   Agent Runtime, or another application implementation.
3. Mint is an external settlement authority behind an owned protocol boundary;
   Relay pins verification keys and independently verifies terminal evidence.
4. Catalog, billing, SDK, evidence, and recovery behavior must remain testable
   without a hosted TensorZero service or proprietary upstream control plane.
5. Upstream changes are adopted as attributed engine updates. They cannot
   silently redefine Relay's public contract or money invariants.

This is what “own Relay” means operationally: Railyard can build, test, run,
price, evolve, and migrate the product without depending on TensorZero's hosted
services or surrendering control of Relay's customer-facing behavior.
