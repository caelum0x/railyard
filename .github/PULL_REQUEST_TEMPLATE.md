## What does this change, and why?

<!-- Which `apps/<name>` is this in? What problem does it fix or what capability does it add? -->

## How was this tested?

<!--
Each app is its own workspace — there is no single root test command.
Run the affected app's own suite, e.g.:
  cd apps/<name> && cargo test --workspace     (Rust apps)
  cd apps/<name> && pnpm test                  (Node/TS apps)
-->

## Checklist

- [ ] I ran the affected app's own test suite and it passes.
- [ ] If this touches a money path (anything that calls Mint, or anything in
      `apps/mint` itself), I added or updated a test that exercises it.
- [ ] If this adds a new app or a new external dependency, I checked
      [`docs/OSS_FOUNDATIONS.md`](../docs/OSS_FOUNDATIONS.md) for an existing
      permissively-licensed foundation first, and (if it's GPL/AGPL) isolated
      it the way `apps/rithub-forge` is isolated, with its own LICENSE.
- [ ] This keeps the design invariants intact (integer microdollars,
      success-only billing, Mint as the only vault, injectable transport
      traits) — see [`CONTRIBUTING.md`](../CONTRIBUTING.md).
