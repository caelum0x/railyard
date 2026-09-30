# RY-0004: Ship on push

- **Status:** accepted. Path A is built; path B stays the written fallback; path C stays
  rejected.
- **Date:** 2026-09-30
- **Package:** PY3.1 (Wave P), which carries RYP-E3 (`gtm/WAVES-RAILYARD-BIDSTAGE-2026-09-29.md`).
- **Owner items:** Y4 (create the Railyard GitHub App from the manifest link), plus the
  Cloudflare OAuth client and `RAILYARD_SHIP_ON_PUSH_LIVE=1` (below).

## Context

Until now a Railyard deploy ran on the user's machine: `railyard deploy` builds and
calls wrangler, which uses Cloudflare's own login (ADR RY-0002: we host nothing).
RYP-E3 asks for deploys from `git push`: `main` goes live within 5 minutes and a
branch gets a preview URL posted on its pull request, with no token shown or pasted at
any step. It named three paths:

- **A**: if Cloudflare offers a third-party OAuth client for account access, "Connect
  Cloudflare" plus a GitHub App; builds run in the customer's own GitHub Actions and
  deploy to the customer's Cloudflare account.
- **B** (the default if A did not exist): `railyard link` mints a Cloudflare API token
  from the user's wrangler session on their machine and writes it into the repository's
  GitHub secret with the user's GitHub OAuth token.
- **C**: hosting in our account through Workers for Platforms. Rejected (Dodo row 23:
  we would be the host and the merchant of record for customer workloads; and its cost).

## Evidence: path A exists

Checked on 2026-09-30 in Cloudflare's documentation:

1. Changelog, 2026-06-03, "Introducing self-managed OAuth clients":
   <https://developers.cloudflare.com/changelog/post/2026-06-03-public-oauth-clients/>.
   "Today we are launching self-managed OAuth, enabling developers to build third-party
   applications that integrate with Cloudflare via OAuth ... after a user grants consent,
   Wrangler can deploy Workers into that account." Clients are created under **Manage
   account > OAuth clients**; they start **private** (usable only by members of the
   creating account) and become **public** after client-domain verification.
2. "Create an OAuth client":
   <https://developers.cloudflare.com/fundamentals/oauth/create-an-oauth-client/>.
   Scopes use the API token permission names; **only the Authorization Code flow** is
   supported for third-party clients (no client credentials, no device flow).
3. Changelog, 2026-04-14, consent and revoke: the user picks which accounts the
   application may use, sees every scope, and can revoke access from their profile.

So path A exists, and it is the better path: no Cloudflare token is ever created by the
user or stored in their repository.

## Decision

Build path A as follows (all in `apps/console/lib/ship/**`, routes under
`apps/console/app/api/v1/github/**`, the CLI in `packages/cli/src/link/**`, migration
`apps/console/db/migrations/0007_ship.sql`):

1. **`railyard link`** (in a folder whose `origin` is on GitHub) asks the console for a
   15-minute, single-use link session and prints one URL.
2. **Browser, at most two consents:** install the Railyard GitHub App on the repository
   (skipped when the App already covers it), then Cloudflare's own consent screen
   (skipped when the org already granted it). The Cloudflare grant uses the
   authorization code flow **with PKCE (S256)**; the code verifier never appears in a URL.
   The installation id GitHub sends back is not trusted: the App's own view of the
   repository (`GET /repos/{repo}/installation`) decides.
   Between the two, the user signs in on GitHub with the App (its user authorization;
   no click after the first time) and the console checks, with that user's own token,
   that they can push to the repository. The token is used for that one check and never
   stored. This is required because the App is shared by every Railyard org: an
   installation on a repository (for example on all of a GitHub organization's
   repositories) says nothing about which Railyard org may link it, and without the check
   any org could link a repository it does not control and receive its pushes
   (verifier finding, Wave P3).
3. **One pull request**, opened by the App on the branch `railyard/ship-on-push`, adds
   `.github/workflows/railyard.yml`. Nothing is pushed to a default branch; merging the
   pull request is the user's consent to change their repository.
4. **The workflow holds no key.** It asks GitHub for an OIDC token with the console as
   the audience (`permissions: id-token: write`) and runs `npx railyard ci`. The console
   verifies the token (issuer, audience, expiry, signature against GitHub's published
   keys), finds the link by the stable `repository_id`, accepts only
   `job_workflow_ref = <repo>/.github/workflows/railyard.yml@...`, only `push` and
   `workflow_dispatch` events, only branch refs, and refuses a paused org. It answers with
   a short-lived Cloudflare access token (refreshed from the sealed refresh token, the
   rotated refresh token sealed again; a cached access token is reused while it has
   10 minutes left, so parallel jobs rarely refresh at once).
5. `railyard ci` masks that token (`::add-mask::`) before anything else is printed,
   deploys exactly as `railyard deploy` does (`<app>` on the default branch,
   `<app>-<branch>` elsewhere), never opens a login, and reports the result with a fresh
   OIDC token. The console records the deploy (branch and commit come from the verified
   token, not the body) and, for a non-default branch with an open pull request, writes
   one preview comment on it, edited on each later push.
6. **Uninstall:** the App's signed webhooks (`installation` deleted,
   `installation_repositories` removed) drop the matching links.
7. **Storage:** the Cloudflare refresh and access tokens are sealed (AES-256-GCM,
   `RAILYARD_CREDENTIALS_KEY`, the org id as associated data), as the AI gateway does for
   provider keys. No table holds a plaintext credential; RLS is on and `anon` and
   `authenticated` have no grants.

The GitHub App asks for: contents write and workflows write (the one workflow pull
request; GitHub requires the workflows permission to add a workflow file), pull requests
write (the preview comment), metadata read. `GET /api/v1/github/manifest` posts the
manifest to GitHub; the page GitHub returns to shows one command that converts the
manifest code and pipes the App id, key and webhook secret straight into
`wrangler secret put`, so no key is displayed.

### Why not path B now

B needs a Cloudflare API token created from the user's wrangler session and stored as a
long-lived repository secret. A has no stored token at all and one fewer step, so B is
not built. It stays the fallback if the OAuth client cannot be made public (domain
verification): the only parts that would change are step 2's Cloudflare consent and
step 4's token source.

### Path C

Still rejected, for the reasons above. Nothing here hosts a customer workload: builds
run on the customer's GitHub Actions minutes and deploy to the customer's Cloudflare
account.

## Consequences

- Cost to us: $0 (the customer's Actions minutes and Cloudflare account).
- A private Cloudflare OAuth client works only for members of our own Cloudflare
  account. For customers, the client must be **public**, which needs domain verification
  of the console's domain (owner step).
- Everything is off until `RAILYARD_SHIP_ON_PUSH_LIVE=1` (denylist rule
  `railyard-slot-ship-on-push` keeps the public copy "arriving" until then).
- Known limits: two jobs that both find the cached access token stale may refresh at
  once; with a rotating refresh token one of them fails and the next push succeeds. Fork
  pull requests get no OIDC token from GitHub, so they never deploy (by design).
- The "within 5 minutes" and "preview on the pull request" timings are measured on a
  real test repository and a real Cloudflare account by the owner (recorded in the P3
  deploy checklist); the tests use a fake GitHub, a fake Cloudflare and a mocked wrangler.

## Owner steps

1. Create the Cloudflare OAuth client in our account (Manage account > OAuth clients):
   redirect URI `<console>/api/v1/github/cloudflare/callback`, grant type
   `authorization_code`, the scopes in `CLOUDFLARE_OAUTH_SCOPES` (default: account read,
   user read, Workers and Workers scripts and routes write, `offline_access`), then verify
   the console domain and make the client public. Put `CLOUDFLARE_OAUTH_CLIENT_ID` and
   `CLOUDFLARE_OAUTH_CLIENT_SECRET` into the console's secrets.
2. Y4: open `<console>/api/v1/github/manifest` (add `?org=<github-org>` to create it
   under a GitHub organization), confirm on GitHub, and run the one command the next page
   shows.
3. Apply migration `0007_ship.sql` after `0006`.
4. Set `RAILYARD_SHIP_ON_PUSH_LIVE=1` on staging, run `railyard link` in a test
   repository, merge the pull request, push to `main` and to a branch, record both times,
   then set it in production.
