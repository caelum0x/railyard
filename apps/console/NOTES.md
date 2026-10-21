# Console deploy blockers (owner-only)

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
