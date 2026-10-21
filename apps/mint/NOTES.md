# Mint deploy blockers (owner-only)

- 2026-10-21: `apps/mint` on `railyard-box` is pinned at image
  `railyard-mint:6200c2e1de31`, 4 commits behind `main` (includes
  `1bab00e7` Google Play direct payments and the playstore transient-lookup
  fix in `a67af8af`). This is intentional, not an oversight: `mint` is the
  money service, and per `infra-box/ci/README.md` / `ci.env.example`
  (`CI_APPROVAL_SERVICES=mint`) it only ships from a GitHub Actions
  "Run workflow" dispatch with `approve=mint` -- the one approval click is
  an owner step by design, so this session did not deploy it. Run the
  `ship` workflow with `service=mint`, `approve=mint` when ready.
