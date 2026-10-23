# apps/gitea — Railyard's code hosting (Rithub), on Gitea

**Decision (2026-10-15):** Railyard's code-hosting service ("Rithub + Rit" on
the roadmap, Dec 21/28 milestone) is **go-gitea/gitea** (MIT, 58k+★, actively
maintained) — composed as its own container, not hand-built. This closes the
roadmap gap named in `plans/OSS-ADOPTION-2026-10-15.md` ("Railyard" section,
row 1): packages, LFS, wikis, and Actions CI were each a multi-week
from-scratch build; Gitea ships all four today.

## Inventory: what existed, what Gitea covers, what's kept

Two prior, independent efforts exist in this repo for the same product slot.
**Neither is deleted** (owner rule: never delete sources/features) — both stay
on disk, now documented as superseded by this decision for new work:

| Path | What it is | Status | Disposition |
|---|---|---|---|
| `apps/rit` | Library-first Rust reimplementation of Git's object/index/ref/pack model (hash-object, pack receive/upload, interop-tested against real `git`) | Phase-complete for its own scope; genuinely impressive, byte-for-byte Git-compatible | **Kept, not deleted.** No longer the dependency of a forge build — see below. |
| `apps/rithub` | Self-hostable Rust git **forge** built on `rit`: full collaboration core (issues, PRs with merge/squash/rebase, review, forks + cross-repo PRs), web UI, RBAC, protected branches, webhooks, releases, audit log, search, Rago/Mint billing hook. Self-contained git plane (no system `git` binary needed). Its own README states it directly: **"Actions/runners, packages, wikis/projects, OAuth/SSO, and a SPA remain ahead"** — i.e., exactly this roadmap milestone's scope was its open gap. | Phase 7 of its own roadmap; not deployed anywhere | **Kept, not deleted; superseded for this milestone.** The remaining gap (packages/LFS/wikis/Actions/OAuth) is now closed by `apps/gitea` instead of being built from scratch in Rust. If `rithub`'s deeper collaboration/billing model (native Rago/Mint integration, no `git` binary dependency) is wanted later, it remains available to finish independently — the two are not mutually exclusive, just not both being built at once. |
| `apps/rithub-forge` | An **uncommitted**, in-progress Forgejo (Gitea's AGPL/GPL community fork) adoption from an earlier session: full Forgejo source checkout (~593 MB, its own nested git repo cloned from `codeberg.org/forgejo/forgejo`), rebranded via `custom/` overlay, with a real `integrations/railyard/` billing bridge (webhook → Rago → Mint) and SSO/OIDC docs. Its own README already flagged itself as "a separate, optional... deployment kept for teams that need Forgejo-only features," distinct from the Rust `rithub` which it called "the shipped Rithub product." | Never committed to git (sits as untracked files in the main `~/products/railyard` checkout); never deployed | **Kept on disk, untouched by this change, documented here.** Superseded as *the* adoption decision: the owner's current instruction is explicitly Gitea (MIT), not Forgejo (GPL-3.0-or-later for Forgejo's own code going forward) — license cleanliness was exactly why the OSS-ADOPTION doc picked Gitea over Forgejo as the runner-up. `integrations/railyard/webhook-bridge/bridge.mjs` in *this* directory is adapted from (not copied verbatim into history — the source was never committed) that experiment's bridge, which already handled both Forgejo's and Gitea's webhook header names. |

**Net:** three codebases now exist side by side for the same product slot
(`rit`+`rithub` Rust-native, `rithub-forge` Forgejo/uncommitted, `gitea`
official/deployed). Nothing was deleted. `apps/gitea` is the one wired into
the console and documented for production deploy.

## What's here

- `docker-compose.yml` — official `gitea/gitea:1.23` image + `postgres:17`.
  `app.ini` is generated entirely from `GITEA__<SECTION>__<KEY>` environment
  variables (Gitea's documented environment-to-ini mechanism) — **no app.ini
  is committed**. See the file's own comments for two non-obvious gotchas
  found while proving this locally:
  1. The Docker image's `CustomPath` **is** `/data/gitea` itself, not
     `/data/gitea/custom` — branding overlays bind-mount
     `./custom/public → /data/gitea/public` and
     `./custom/templates → /data/gitea/templates` individually, never the
     whole `/data/gitea` (that would shadow `conf/`, `ssh/`, the sqlite/log
     dirs that also live there).
  2. Root-level `app.ini` keys with no `[section]` (like `APP_NAME`) map to
     `GITEA____KEY` (empty section segment), not `GITEA__APP_NAME`.
  3. Don't set `GITEA__server__START_SSH_SERVER=true` *and* leave the base
     image's own passthrough OpenSSH running — they fight over container
     port `:22` and crash-loop. Leave `START_SSH_SERVER` unset (the
     passthrough default); `SSH_PORT` is only the port *advertised* in clone
     URLs.
- `.env.example` — copy to `.env`, fill in secrets.
- `custom/` — branding overlay: `public/assets/img/logo.svg` + `favicon.svg`
  (Rithub "R" mark), `public/assets/css/railyard-brand.css` (brand color
  CSS-variable overrides), `templates/custom/header.tmpl` (injects the
  stylesheet), `templates/custom/footer.tmpl` (brand + MIT attribution line).
- `integrations/railyard/` — the Rago/Mint billing bridge (dependency-free
  Node, HMAC-verified both ways) and its unit tests (16/16 passing,
  `node --test integrations/railyard/tests/bridge.test.mjs`).
- `deploy/RUNBOOK.md` — first production deploy on railyard-box (Oracle A1
  ARM64), Cloudflare Tunnel for HTTPS, SSH on a non-22 port via the VCN
  security list, `gitea dump`/`gitea restore` backups, rollback.

## SSO (backlog item 5, closed)

`apps/console` is now a real OIDC provider (`lib/oidc.ts`: RS256 id_token and
a self-contained RS256 access_token, authorization-code grant, single-use
codes in `railyard.oidc_codes`): discovery at
`/.well-known/openid-configuration`, `/oidc/{authorize,token,userinfo,jwks}`.
A visitor who is not yet signed into Console is bounced through its existing
sign-in and comes straight back to finish the OIDC flow, so one session
covers both Console and Rithub. It is gated on two console env vars
(`RAILYARD_OIDC_SIGNING_KEY`, generated with
`apps/console/scripts/gen-oidc-key.mjs`; `RAILYARD_OIDC_CLIENTS`, a
`{client_id: {secret, redirect_uri}}` map) -- unset, every `/oidc/*` route
answers 503 and Gitea just keeps its native accounts. The exact
`gitea admin auth add-oauth` registration command is documented inline in
`docker-compose.yml`'s `railyard-sso` block.

## Local proof (2026-10-15, this session)

Ran via `docker-compose up -d` on this machine (Colima, arm64 host — the
exact target architecture as the Oracle A1 box) against the official
`gitea/gitea:1.23` + `postgres:17` images, both pulled as native arm64
manifests (no emulation):

- Branding: page `<title>` = "Rithub (by Railyard)", custom logo + favicon +
  brand CSS served (200s), custom footer line rendered with MIT attribution
  to Gitea — confirmed via `curl` and three screenshots (desktop 1440×900
  home + repo, mobile 390×844 repo) at `/tmp/railyard-git-*.png`.
- Admin user created (`gitea admin user create --admin`), API token minted.
- Repo created via API; **HTTPS push** (`git push` over `http://` with a
  token in the URL) — works.
- **SSH push** — registered a public key via the API, pushed a second commit
  over `ssh://...@localhost:2222` — works.
- **Git LFS** — `git lfs track "*.bin"`, pushed a 200 KB binary, confirmed
  tracked via `git lfs ls-files` and visible in the repo file list — works.
- **npm package registry** — `npm publish` against
  `http://localhost:3300/api/packages/<owner>/npm/` with a `.npmrc` token —
  published `@railyard/proof-pkg@1.0.0`, confirmed via the packages API —
  works.
- **Wiki** — created a page via the wiki API — works.
- Repo's own tab bar confirms Issues, Pull Requests, **Actions**,
  **Packages**, **Projects**, **Wiki** are all present and enabled.

Local proof was torn down after screenshots (`docker compose down`, image/
volume cleanup) — disk discipline, nothing proof-only is left running.

## Console wiring

`apps/console`'s left nav has a **Code** entry pointing at `RAILYARD_GIT_URL`
(default `https://git.ereboslab.com`) — see `lib/config.ts` and
`components/Nav.tsx`.

## Production deploy (2026-10-19)

Live on railyard-box: `docker compose` stack at `/srv/railyard-gitea`
(web on host port 3350, Postgres on 5434 -- 3300/3400/3100/5433 were already
taken by rago/mint/relay/the shared Postgres), fronted by Caddy
(`infra-box/caddy/sites/gitea.caddy`, `CADDY_SITES` += `gitea`) with a
Let's Encrypt cert, not the Cloudflare Tunnel the original RUNBOOK draft
proposed -- every other box-hosted Railyard service (rago/relay/mint/errors)
already uses plain Caddy + a proxied Cloudflare DNS `A` record to
`130.61.113.116`, so this follows the same, already-proven pattern instead of
introducing a new one. DNS: `git.ereboslab.com` A record added via the
Cloudflare API (not `git.railyard.ereboslab.com` -- no other service here
uses a `railyard.` prefix). Verified live: `https://git.ereboslab.com/` 200s
with the "Rithub (by Railyard)" title, logo/branding assets 200, admin
account `caelum0x` created.

**Not yet done (owner step, OCI Console, networking only):** the Oracle VCN
security list rule for inbound TCP 2222 (git-over-SSH) -- HTTPS clone/push
and the web UI work today; SSH clone needs that one rule added, same as
`deploy/RUNBOOK.md` step 3 already documented.

## CI runner deployed (2026-10-20)

Closes a real gap found in a GitLab CE / GitHub Enterprise Server
comparison: `GITEA__actions__ENABLED=true` has been set since the first
deploy, but with no runner registered the Actions tab was cosmetic — a
pushed `.gitea/workflows/*.yml` job would queue forever. `act_runner`
(`gitea/act_runner:0.6.1`, pinned) is now a real service in
`docker-compose.yml`: DooD via the mounted host docker socket, registered
against the gitea container over the internal compose network
(`http://gitea:3000`, not the public HTTPS hostname), credentials persisted
in the `gitea-runner-data` volume after first boot. See
`deploy/RUNBOOK.md`'s "act_runner" section for the registration command and
an end-to-end smoke-test workflow. Deployed and verified live on
railyard-box. Live proof (2026-10-20, this session): registered the runner
(`docker exec -u git railyard-gitea-gitea-1 gitea actions generate-runner-token`
+ `docker compose up -d act_runner`) — logs show `runner: railyard-box ...
declare successfully` with labels `[ubuntu-latest ubuntu-22.04 ubuntu-24.04]`.
Created a throwaway repo (`caelum0x/ci-runner-proof`), pushed a real
`.gitea/workflows/ci.yml` (`runs-on: ubuntu-latest`, one `echo` step) over
HTTP with a scoped API token, and polled
`GET /api/v1/repos/caelum0x/ci-runner-proof/actions/tasks`: the runner pulled
`docker.gitea.com/runner-images:ubuntu-latest`, ran the job in a sibling
container (`GITEA-ACTIONS-TASK-1-WORKFLOW-JOB-smoke-...`), and the run's
`status` went `queued` -> `running` -> **`success`** within about a minute
of the push. Proof repo deleted afterward (`DELETE /api/v1/repos/...`,
`204`); the two scoped proof tokens remain on the admin account under
Settings -> Applications (safe to revoke manually).
