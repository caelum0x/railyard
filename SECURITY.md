# Security Policy

Railyard moves real money (Mint's ledger, Rago's billing, Vend/Turnstile/
Abacus's metered holds). Treat anything that touches a money path, an
auth/signing boundary, or a secret as security-sensitive.

## Reporting a vulnerability

**Please do not open a public GitHub issue for a security vulnerability.**

Instead, use [GitHub's private vulnerability reporting](https://github.com/caelum0x/railyard/security/advisories/new)
for this repository, or email the maintainer directly (see the GitHub profile
at [github.com/caelum0x](https://github.com/caelum0x) for a contact method).

Please include:

- Which app (`apps/<name>`) is affected.
- A minimal reproduction (request/response, or a failing test) if you have one.
- The impact as you see it — e.g. "lets a caller capture a Mint hold twice,"
  "bypasses the per-tenant row-level-security key," "forges an AITP-402
  receipt."

We'll acknowledge within a few days and keep you posted as we fix it. Please
give us a reasonable window to ship a fix before any public disclosure.

## Scope

Every app under `apps/*` in this monorepo is in scope, with money-path and
auth-path bugs (Mint, Relay, Rago, Vend, Turnstile, Abacus, Ledgerline,
Context Passport) treated as highest severity. `apps/rithub-forge` (vendored
Forgejo) and `apps/gitea` (vendored Gitea) should be reported upstream to
their own projects unless the issue is specifically in Railyard's billing
bridge or runner integration around them.

## Supported versions

This is a single, continuously-deployed monorepo on `main` — there are no
older maintained release branches. Fixes land on `main` and are deployed from
there; there is no backport policy to maintain separately.
