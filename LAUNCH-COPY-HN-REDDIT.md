# Railyard — Show HN / r/selfhosted / r/rust launch copy

**Name-collision check (verified via web search, 2026-11-05):** there is an
unrelated "Railyard" (`railyarddev/railyard`, a Claude Code command-sandbox/
guardrail tool) that already has a live Show HN post
(news.ycombinator.com/item?id=47327033). That is **not this project** — no
relation, different author, different purpose. To avoid getting read as a
duplicate or a me-too post, the title below leads with what this project
*is* (settlement ledger + LLM gateway) rather than just the bare name
"Railyard," and the HN text calls out the name collision itself in the first
line so readers aren't confused mid-thread.

No existing Product Hunt, Show HN, or Reddit post found for
`caelum0x/railyard` itself (verified via web search 2026-11-05) — this is a
first launch for this repo. **Owner-only step:** log into Hacker News /
Reddit once, then post the text below as-is.

## Show HN

**Title:** Show HN: Mint – an open-source settlement ledger + LLM gateway we run in production

**First comment:**

(Note: unrelated to the other "Railyard" posted here recently — different
project, different author.)

Mint is a fiat-backed, gasless, success-only microdollar ledger: double-entry
accounting, Ed25519 hash-linked receipts, reserve conservation, and an
AITP-402 payment seam, written in Rust. It's the core of a small rail we
built (repo name: Railyard) after getting tired of stitching together
Stripe + Lago + a bespoke ledger + Sentry + a web-analytics SaaS for every
new product. Relay (a TensorZero fork) and Rago (a Rust rewrite of Lago)
sit next to it as the model gateway and usage-billing pieces; "success-only
billing" means a failed agent run or failed model call never creates a Mint
movement — you're never charged for work that didn't produce a result.

This isn't a demo — it's real infra, already running our own production
traffic across a handful of products (metering, billing, and model-gateway
calls go through it today). Apache-2.0, each app is its own standalone
Cargo/Node workspace so you can take just the ledger, or just the gateway,
without buying into the whole monorepo.

https://github.com/caelum0x/railyard

Happy to answer questions about the receipt/reserve-conservation design, the
success-only billing invariant, or why we forked TensorZero instead of
writing a gateway from scratch.

## r/selfhosted

**Title:** I open-sourced the billing/metering ledger + LLM gateway behind my AI products (Rust, Apache-2.0, self-hostable)

**Post:**

Posting this because it's the kind of thing I'd want to find before writing
my own: a self-hostable settlement ledger (Mint — double-entry, Ed25519
hash-linked receipts, integer microdollars, no floats anywhere in the money
path) plus an LLM gateway (Relay, a TensorZero fork) and a Rust rewrite of
Lago (Rago) for usage-based billing, all talking to the same ledger so you
don't end up with three different sources of truth for "did we actually get
paid."

It's genuinely running in production for a handful of our own products
today, not a weekend prototype — Apache-2.0, each app ships as its own
workspace so you can self-host just the piece you need. Repo + architecture
doc: https://github.com/caelum0x/railyard

Not trying to sell anything here — looking for people who've had to build
the same "ledger + gateway + usage billing" stack by hand and want to
compare notes, or who'll find a bug before I do.

## r/rust

**Title:** Mint: a Rust settlement ledger with Ed25519 hash-linked receipts and reserve conservation (part of a small OSS billing/LLM-gateway rail)

**Post:**

Sharing the Rust internals angle for this one: Mint enforces integer
microdollars everywhere (no float ever touches a balance), every receipt is
Ed25519-signed and hash-chained to the previous one so a receipt can't be
replayed or forged independently of the chain, and every external call
(Postgres, the signing key, outbound HTTP) sits behind an injectable
transport trait so the whole settlement path is testable without touching a
real network or database. `apps/mint` is a ~10-crate workspace
(`*-core` → `*-store`/`*-store-postgres` → `*-protocol` → clients → services
→ `*-server` → `*-sdk` → `*-testkit`) that the rest of the monorepo's apps
copy the shape of.

Apache-2.0, `cargo test --workspace` from `apps/mint` to see the receipt/
reserve-conservation tests. https://github.com/caelum0x/railyard — feedback
on the crate decomposition or the signing design very welcome.
