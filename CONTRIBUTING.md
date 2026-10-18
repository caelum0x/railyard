# Contributing to Railyard

Thanks for looking at this. A few things that will save you a round-trip on
review:

## Design invariants (non-negotiable)

These hold across every app in the repo — see
[`docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md`](docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md)
§0 for the full list and rationale:

1. Each app is its own standalone workspace — don't add it to a repo-root
   build graph.
2. Money math is integer microdollars. No floats in any money path.
3. Billing is success-only: failed work must not create a Mint movement.
4. Mint is the only vault. If your app needs to hold or move money, call
   Mint over HTTP — don't add a second balance column anywhere else.
5. External I/O goes behind an injectable transport trait (fake in tests,
   real `reqwest`/HTTP client in production).

## Before opening a PR

- Run the affected app's own test suite (`cd apps/<app> && cargo test
  --workspace`, or that app's equivalent — see its README). There is no
  single root test command by design.
- If you're touching a money path (anything that calls Mint, or anything in
  `apps/mint` itself), add or update a test that exercises it — reviewers
  will ask for one if it's missing.
- If you're adding a new app or a new external dependency, check
  [`docs/OSS_FOUNDATIONS.md`](docs/OSS_FOUNDATIONS.md) first: is there
  already a permissively-licensed (MIT/Apache-2.0) foundation to build on
  instead of writing it from scratch? GPL/AGPL dependencies and
  non-commercial model weights are flagged there as avoid-or-risk — if your
  change needs one, it belongs in a clearly separated subtree with its own
  LICENSE (see how `apps/rithub-forge` is isolated from the rest of the
  repo), not mixed into Apache-2.0 code.

## Reporting a security issue

Please don't open a public issue for a security vulnerability. See
[`SECURITY.md`](SECURITY.md) if one exists for this app, or reach out
directly to the maintainer first.

## License

By contributing, you agree your contribution is licensed under this
repository's [Apache-2.0 license](LICENSE) (or, for `apps/rithub-forge`
specifically, its own GPLv3 license).
