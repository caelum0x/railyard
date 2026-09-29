#!/usr/bin/env node
// Names only (Wave P PY1.5): the Relay key, Rago org and Mint service key each
// product needs for the Railyard consumer contract, and the steps on box 2
// that create them (each writes straight into the product's env file there).
// No secret is generated, read or printed here.
//
//   node scripts/relay/provision-product-keys.mjs            # the plan, as text
//   node scripts/relay/provision-product-keys.mjs --json     # the same, as JSON

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "../..");

export function plan(contract) {
  return Object.keys(contract.products).map((product) => ({
    product,
    env_file: `/etc/products/${product}.env`,
    relay_key: { name: `${product}-railyard`, env: "RELAY_KEY", cap_monthly_usd: 30, cap_daily_usd: 3 },
    rago: { org_slug: `${product}-portfolio`, env: "RAGO_API_KEY", seed: contract.products[product].seed },
    mint: { key_name: `${product}-mirror`, scope: "mirror", env: "MINT_API_KEY" },
    vars: ["RELAY_URL", "RAGO_URL", "MINT_URL", "RAILYARD_LIVE=0"],
    box_steps: [
      `Relay: docker exec railyard-relay-1 gateway --config-file /app/config/tensorzero.toml --create-api-key, piped into RELAY_KEY of ${product}'s env file, then stacks/railyard/relay-tenancy.sh cap <public id> 30 3`,
      `Rago: one org "${product}-portfolio" with a random token (apps/rago/seeds/README.md step 0) into RAGO_API_KEY, then (cd apps/rago/seeds && ./import.sh portfolio/${contract.products[product].seed.split("/").pop()})`,
      `Mint: POST /v1/keys {name: "${product}-mirror"} with the operator key, the answer piped into MINT_API_KEY`,
    ],
  }));
}

export function render(p) {
  return p
    .map((x) =>
      [
        `${x.product}:`,
        `  ${x.env_file}: ${x.vars.join(", ")}; secrets ${x.relay_key.env}, ${x.rago.env}, ${x.mint.env}`,
        `  Relay key "${x.relay_key.name}" capped at $${x.relay_key.cap_monthly_usd}/month, $${x.relay_key.cap_daily_usd}/day`,
        `  Rago org "${x.rago.org_slug}", catalog ${x.rago.seed}`,
        `  Mint service key "${x.mint.key_name}" (scope ${x.mint.scope})`,
        ...x.box_steps.map((step, i) => `  step ${i + 1} on box 2: ${step}`),
      ].join("\n"),
    )
    .join("\n");
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const contract = JSON.parse(readFileSync(join(ROOT, "tests/contract/consumers/contract.json"), "utf8"));
  const p = plan(contract);
  process.stdout.write(process.argv.includes("--json") ? `${JSON.stringify(p, null, 2)}\n` : `${render(p)}\n`);
}
