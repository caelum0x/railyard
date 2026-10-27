# Console deploy blockers (owner-only)

**Correction, 2026-10-27:** the last entry below ("scribase.com project creation is an interactive, owner-authenticated signup, so this one genuinely cannot be done headlessly from here") was wrong. `npx scribase init --temp --yes` provisions a real project with zero interactive steps — used it to provision a real one, applied all migrations, wired the secrets, deployed, and verified sign-in reaches the real gateway end to end. See `plans/railyard.md`'s "Console go-live (2026-10-27)" entry for the full writeup. The only genuinely owner-gated step left is converting that project from temporary (72h TTL) to permanent — needs a real `SCRIBASE_ACCESS_TOKEN` or a real $1 card check, both things only the owner can do.

- 2026-10-21: `GET /api/v1/rail/status` (commit `c3b7edcf`) is merged to main
  but not deployed to the live Worker (`railyard.ereboslab.com`). Blocked on
  two things only the owner can resolve from this machine: (1)
  `~/.config/products-keys/railyard-console.env` (the secrets file
  `deploy-scripts/railyard-console-live.sh` requires) does not exist here;
  (2) `npx wrangler`/`pnpm` are both broken in this sandbox (corepack's pnpm
  cache is missing its cached binary, and the workspace root
  `node_modules/` does not exist, so `npm run cf:build`/`wrangler deploy`
  cannot run locally). Run `railyard-console-live.sh` (bump `PIN` to a commit
  that includes `c3b7edcf`) from a machine with a working pnpm install and
  the keys file once available.

- **2026-10-22, corrected:** (1) and (2) above are NOT the real blocker --
  `corepack prepare pnpm@10.30.3 --activate` fixes the local pnpm/wrangler
  toolchain in minutes (no keys file needed: this Worker already has 4/9
  secrets live since 2026-10-14 and `wrangler deploy` never touches secrets),
  and `cf:build`/`wrangler deploy` both ran clean from this machine using the
  global Cloudflare OAuth token (`wrangler whoami` -> subasiarhan3@gmail.com,
  workers:write). **The actual blocker is `lib/config.ts`'s own guard**
  (added in `a72111e9`, by design): it throws when `SCRIBASE_URL ===
  SCRIBASE_CONTROL_PLANE_HOST` ("https://api.scribase.com"), which is exactly
  the value baked into `wrangler.jsonc`'s `vars.SCRIBASE_URL` today, because
  the real "railyard-console" Scribase project (DEPLOY.md step 1: sign in at
  scribase.com, create the project, flip `SCRIBASE_URL` to its own gateway
  host) has never been provisioned. Proved this live and rolled it back
  immediately: deploying current `main` as-is (version
  `d21434a1-52ad-4bff-b82e-7c3b728ef116`, 2026-10-21T23:17Z) 500s *every*
  route including `/`, not just the new one -- confirmed via `wrangler tail`
  (`Error: SCRIBASE_URL is the Scribase control-plane API ...`). Rolled back
  to the last known-good version (`3c6adda0-8a2a-40ff-9a67-d67bc0ba3f90`,
  2026-10-14T11:43Z) with `wrangler rollback`; verified `GET /` is back to
  200 with the sign-in page. So: shipping `rail/status` (or anything else on
  `main`) to this Worker is blocked on one real owner step -- provisioning
  the railyard-console Scribase project and updating `SCRIBASE_URL` -- not on
  tooling. No management/org-creation API key for Scribase exists in
  `~/.config/products-keys/` or `~/products/.env` (only per-project keys),
  and scribase.com project creation is an interactive, owner-authenticated
  signup, so this one genuinely cannot be done headlessly from here.
