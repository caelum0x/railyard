# Rithub (Gitea) — Oracle ARM box RUNBOOK

**Status: deployed 2026-10-19, SSH-over-2222 closed out 2026-10-25.** Live at
`https://git.ereboslab.com/` (not `git.railyard.ereboslab.com` — see
`../README.md`'s "Production deploy" section for the actual hostname, ports,
and the Caddy-not-Tunnel path taken). The steps below are the original plan;
they describe the Cloudflare Tunnel approach that was *not* the one used in
the end (plain Caddy + DNS matched every other box service better and needed
no new OWNER dashboard step for the HTTP path).

Step 3 below (OCI VCN security-list rule for port 2222) turned out to **not**
need an owner click: this environment already had a working `oci` CLI config
(`~/.oci/config` + API key), used to add the ingress rule directly via
`oci network security-list update`. `sudo ufw allow 2222/tcp` was applied on
the box the same session. A second real gap was found and fixed while
verifying live: Gitea was advertising
`ssh://git@git.ereboslab.com:2222/...` clone URLs, but `git.ereboslab.com` is
a Cloudflare-proxied (orange-cloud) A record, and Cloudflare's proxy does not
forward arbitrary TCP ports (only 80/443) — so that clone URL timed out for
every real end user even with the firewall correctly open. Fixed by adding a
second, DNS-only (grey-cloud) A record `git-ssh.ereboslab.com` → the same IP
(via the Cloudflare API, `CLOUDFLARE_DNS_API_TOKEN`) and a new
`GITEA_SSH_DOMAIN` env var pointed at it (`docker-compose.yml`,
`.env.example`), independent from the proxied `GITEA_DOMAIN`. Verified with a
real `git push` over `ssh://git@git-ssh.ereboslab.com:2222/...` landing on
the live instance — see `plans/railyard.md`'s 2026-10-25 entry for the full
proof log. Nothing in this runbook's SSH section is an owner-only step
anymore.

Railyard's code-hosting service: `apps/gitea` — the official `gitea/gitea`
image (MIT) + Postgres, proven locally (see `../README.md` for the local
proof log: HTTPS push, SSH push, Git LFS, npm package registry, wiki, all
verified 2026-10-15). Follow the Standard Operating Pattern already used for
Railyard's other box-hosted services (`~/products/infra-box/RUNBOOK-A1.md`,
`~/products/infra-box/stacks/aptabase/deploy/RUNBOOK.md`).

Target box: **railyard-box**, Oracle Always-Free A1 (`VM.Standard.A1.Flex`,
ARM64/aarch64, Ubuntu 24.04), public IP `130.61.113.116` per
`hosting-all-on-oracle-2026-10-13` memory — the same box already running
Rago/Mint/Relay. `docker-compose.yml` in this directory pulls the official
`gitea/gitea:1.23` image, which ships `linux/arm64` manifests, so it runs
native on the A1 (no emulation).

## 0. Pre-flight

- [ ] Confirm headroom on railyard-box: `ssh railyard-box 'docker stats --no-stream'`
      and the box's row in `~/products/infra-box/CAPACITY-PLAN.md`. Gitea + Postgres is
      light (roughly 512 MiB / 0.5 CPU combined at this traffic level; raise if
      `docker stats` shows it pinned, don't pre-provision beyond that).
- [ ] Pick the real domain: `git.railyard.ereboslab.com` (matches `railyard.ereboslab.com`
      console and `insights.railyard.ereboslab.com`, the live Railyard subdomain pattern —
      confirm against whichever is current in `plans/railyard.md` at deploy time).
- [ ] Cloudflare zone `ereboslab.com` is on our account (it already hosts `railyard.ereboslab.com`
      and `insights.ereboslab.com` — reuse the same zone, new subdomain only).
- [ ] `git` and `docker compose` are installed on the box (they are, per `RUNBOOK-A1.md`
      bootstrap).

## 1. Ship the kit

```bash
# [Mac]
rsync -a --exclude .git --exclude node_modules \
  ~/products/railyard/apps/gitea/ railyard-box:/tmp/railyard-gitea/
ssh railyard-box 'sudo mkdir -p /srv/railyard-gitea && sudo rsync -a --delete /tmp/railyard-gitea/ /srv/railyard-gitea/ && sudo chown -R deploy:deploy /srv/railyard-gitea'
```

## 2. Secrets (box, as `deploy`)

```bash
# [railyard-box]
cd /srv/railyard-gitea
cp .env.example .env
# Generate the two required secrets with the gitea binary itself (matches
# what app.ini expects byte-for-byte; never hand-roll these):
docker compose run --rm gitea gitea generate secret SECRET_KEY       # -> GITEA_SECRET_KEY
docker compose run --rm gitea gitea generate secret INTERNAL_TOKEN   # -> GITEA_INTERNAL_TOKEN
# Edit .env: paste both secrets in, set a real GITEA_DB_PASSWORD
# (openssl rand -base64 24), set:
#   GITEA_DOMAIN=git.railyard.ereboslab.com
#   GITEA_ROOT_URL=https://git.railyard.ereboslab.com/
#   GITEA_SSH_PUBLIC_PORT=2222
chmod 600 .env
```

## 3. SSH on a non-22 port (Oracle VCN Security List, OWNER)

railyard-box's system SSH (port 22, the `deploy` admin login) must stay
separate from Gitea's git-over-SSH. Gitea's container maps its internal `:22`
(git-shell-only, no shell/admin access — `DISABLE_SSH=false`,
`SSH_PORT=2222` is just the *advertised* clone-URL port) to **host port
2222**, so:

- [ ] **OWNER**: OCI Console → Networking → Virtual Cloud Networks → (railyard-box's
      VCN) → Security Lists → the list attached to railyard-box's subnet → Add
      Ingress Rule: source `0.0.0.0/0`, protocol TCP, destination port **2222**.
      (Do **not** touch the existing port 22 rule — that stays admin-only, ideally
      already source-restricted per `RUNBOOK-A1.md`'s hardening step.)
- [ ] `sudo ufw allow 2222/tcp` on the box (UFW mirrors the security list; both
      must allow it).

## 4. Cloudflare Tunnel for HTTPS (no inbound 443 needed)

Reuses the pattern from `RUNBOOK-A1.md` step 0's "Box tunnel" path — avoids
opening 80/443 on the VCN security list for this one more hostname.

- [ ] **OWNER**: Cloudflare dashboard → Zero Trust → Networks → Tunnels →
      create tunnel `railyard-gitea` (or reuse an existing box tunnel and add
      a route) → copy the tunnel token.
- [ ] On the box:
  ```bash
  # [railyard-box] — cloudflared as a container, no system install needed
  docker run -d --name railyard-gitea-tunnel --restart unless-stopped \
    --network container:gitea-gitea-1 \
    cloudflare/cloudflared:latest tunnel --no-autoupdate run \
    --token "$CLOUDFLARE_TUNNEL_TOKEN"
  ```
  (`--network container:gitea-gitea-1` puts cloudflared on the same network
  namespace as the `gitea` container so it can reach `127.0.0.1:3000`
  directly — no extra Docker network wiring needed.)
- [ ] **OWNER**: in the tunnel's Public Hostname config, add
      `git.railyard.ereboslab.com` → `http://localhost:3000` (service type HTTP,
      origin `localhost:3000` because of the shared network namespace above).
- [ ] DNS: the tunnel creation step auto-creates the CNAME in the zone; confirm
      `git.railyard.ereboslab.com` resolves to `<tunnel-id>.cfargotunnel.com`.

## 5. Start the stack

```bash
# [railyard-box]
cd /srv/railyard-gitea
docker compose pull
docker compose up -d
docker compose logs -f gitea   # watch for "Starting new Web server" with no [E]/[F] lines
```

## 6. First admin account + verify

```bash
# [railyard-box]
docker compose exec -u git gitea gitea admin user create --admin \
  --username <owner-handle> --password '<strong password>' \
  --email owner@ereboslab.com --must-change-password=false
```

Then from the Mac:
```bash
curl -I https://git.railyard.ereboslab.com/            # 200, title "Rithub (by Railyard)"
curl -I https://git.railyard.ereboslab.com/assets/img/logo.svg   # 200 — branding overlay mounted
git clone https://git.railyard.ereboslab.com/<owner-handle>/<any-repo>.git   # HTTPS works
GIT_SSH_COMMAND="ssh -p 2222" git clone git@git.railyard.ereboslab.com:<owner-handle>/<any-repo>.git  # SSH works
```

## 7. Wire the console link

`apps/console`'s Nav already points its new "Code" entry at `RAILYARD_GIT_URL`
(defaults to `https://git.railyard.ereboslab.com` — see `lib/config.ts`). No
console redeploy is needed unless the real hostname differs from that
default; if it does, `wrangler secret put RAILYARD_GIT_URL` (or the
equivalent env var binding) before the next console deploy.

## 8. Backups

Gitea's own `dump` subcommand produces one portable archive (DB dump +
repos + LFS + custom + config), independent of the Postgres volume:

```bash
# [railyard-box] weekly cron (add to the box's existing backup job, same
# pattern as infra-box's restic/wal-g jobs for other stacks)
docker compose exec gitea gitea dump -c /data/gitea/conf/app.ini \
  --file /data/gitea-dump-$(date +%F).zip
docker compose cp gitea:/data/gitea-dump-$(date +%F).zip /srv/railyard-gitea/backups/
# then feed /srv/railyard-gitea/backups/*.zip into the box's existing
# restic repository (RUNBOOK-A1.md "Copy restic.password into the password
# manager" step already set this up for the box).
```

Also back up the two named Docker volumes (`gitea_gitea-pgdata`,
`gitea_gitea-data`) the same way the box already backs up other stacks'
volumes — the `gitea dump` archive above is the portable/restorable one;
the volumes are the fast local recovery path.

## 9. Rollback

- **Bad deploy / bad config:** `docker compose down`, fix `.env` or
  `docker-compose.yml`, `docker compose up -d` again — both named volumes
  (Postgres data, Gitea data) persist across this.
- **Need to go back further:** restore the most recent `gitea dump` zip:
  ```bash
  docker compose down
  docker volume rm gitea_gitea-pgdata gitea_gitea-data   # only if restoring from the dump, not the volumes
  docker compose up -d gitea-postgres
  docker compose run --rm -v /srv/railyard-gitea/backups:/backups gitea \
    gitea restore --file /backups/gitea-dump-<date>.zip
  docker compose up -d
  ```
- **Take the hostname down without touching data:** delete the tunnel's
  Public Hostname route (step 4) — the box keeps running, just unreachable
  from the internet; `git.railyard.ereboslab.com` stops resolving to it.
- **Full removal:** `docker compose down -v` (drops both volumes — only
  after a verified `gitea dump` backup exists elsewhere).

## act_runner (Gitea Actions CI — deployed 2026-10-20)

Gitea's own Actions (CI) was enabled (`GITEA__actions__ENABLED=true`) from
the first deploy, but the Actions tab was cosmetic without a runner attached
— a pushed `.gitea/workflows/*.yml` would show as queued forever. The
`act_runner` service in `docker-compose.yml` closes that:

```bash
# [railyard-box] one-time: generate a registration token, then start the runner
cd /srv/railyard-gitea
docker compose exec -u git gitea gitea actions generate-runner-token
# paste the token into .env as GITEA_RUNNER_TOKEN, then:
docker compose up -d act_runner
docker compose logs -f act_runner   # watch for "runner: <name> registered successfully"
```

`act_runner` carries `profiles: [actions]`, so a bare `docker compose up -d`
(a fresh box, or recovering from the rollback steps below) never tries to
start it and never requires `GITEA_RUNNER_TOKEN` to be set — only naming it
explicitly (`up -d act_runner`, as above) brings it up. If you do start it
before generating a token, it exits/restarts with a clear "no token" error
instead of anything breaking; it never blocks `gitea`/`gitea-postgres`.

Verify end to end (no stub — a real job actually ran):

```bash
# any existing or new repo on the instance, pushed over HTTPS/SSH as usual
mkdir -p .gitea/workflows
cat > .gitea/workflows/ci.yml <<'EOF'
on: [push]
jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - run: echo "rithub actions runner is alive"
EOF
git add .gitea/workflows/ci.yml && git commit -m "ci: smoke test" && git push
# Site Administration -> Actions -> Runners shows the runner "Idle"/"Active";
# the repo's Actions tab shows the "smoke" job with a green check and the
# echoed log line — that's the real proof, not the tab merely existing.
```

Runners execute workflow YAML in pushed repos with real container access
(`/var/run/docker.sock` is mounted, DooD) — the registration token is only
read on the runner's first boot (it persists signed credentials in the
`gitea-runner-data` volume after that), so `.env`'s `GITEA_RUNNER_TOKEN` can
be blanked again once the runner shows "Idle". Woodpecker CI (the
OSS-ADOPTION runner-up) is not needed on top of this.

**Done (2026-10-20):** deployed and verified live — see `../README.md`'s "CI
runner deployed" section for the full proof log (real repo, real push, real
`.gitea/workflows/ci.yml`, run status `success`). Housekeeping left for the
owner: two scoped personal access tokens (`ci-proof`, `ci-proof2`) created
on the `caelum0x` admin account to drive that proof are still listed under
Settings -> Applications on the live instance — revoke them there whenever
convenient (API self-revocation needs Basic auth with the account password,
which this session didn't have).
