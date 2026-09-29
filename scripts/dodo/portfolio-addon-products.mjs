#!/usr/bin/env node
// Creates the Scribase portfolio add-on product and the Scribase usage meters in
// Dodo (Wave P PY1.1, contract 5.2 and 5.3). TEST MODE by default; the live run
// is owner item Y1 and needs both --live and SCRIBASE_DODO_LIVE=1.
//
//   node scripts/dodo/portfolio-addon-products.mjs --plans <scribase>/config/plans.json            # dry run
//   DODO_API_KEY=... node scripts/dodo/portfolio-addon-products.mjs --plans ... --apply             # test mode
//   SCRIBASE_DODO_LIVE=1 DODO_API_KEY=... node ... --plans ... --apply --live                       # owner only (Y1)
//
// Idempotent: an existing product (metadata scribase_addon=portfolio, or the same
// name) and existing meters (same event_name) are reused, never duplicated.
// The price comes from plans.json (addons.portfolio.priceUsd); nothing is typed here.
// Output: the product id as `SCRIBASE_DODO_ADDON_PORTFOLIO_PRODUCT_ID=<id>` (an id,
// not a secret) and one line per meter. The API key is read from the environment
// and never printed.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const TEST_BASE = "https://test.dodopayments.com";
export const LIVE_BASE = "https://live.dodopayments.com";
export const PRODUCT_NAME = "Scribase Portfolio add-on";
export const PRODUCT_ENV = "SCRIBASE_DODO_ADDON_PORTFOLIO_PRODUCT_ID";

/** The Scribase meters of the Railyard consumer contract (dispatch 5.3). */
export const METERS = [
  { event_name: "scribase.addon.slot_active", name: "Scribase add-on slot active", measurement_unit: "slot-days", aggregation: { type: "count" } },
  { event_name: "scribase.ai.tokens", name: "Scribase AI tokens (BYOK, not billed)", measurement_unit: "tokens", aggregation: { type: "sum", key: "tokens" } },
  { event_name: "scribase.analytics.events", name: "Scribase analytics events", measurement_unit: "events", aggregation: { type: "count" } },
  { event_name: "scribase.push.sent", name: "Scribase push notifications sent", measurement_unit: "notifications", aggregation: { type: "count" } },
  { event_name: "scribase.email.sent", name: "Scribase email sent", measurement_unit: "emails", aggregation: { type: "count" } },
];

/** Reads addons.portfolio from Scribase's plans.json and checks it. */
export function portfolioFrom(plans) {
  const p = plans?.addons?.portfolio;
  if (!p) throw new Error("plans.json has no addons.portfolio (PS1.0 lands it)");
  if (!Number.isFinite(p.priceUsd) || p.priceUsd < 1) throw new Error("addons.portfolio.priceUsd must be at least 1");
  if (p.interval !== "month") throw new Error("addons.portfolio.interval must be month");
  if (p.dodoProductEnv !== PRODUCT_ENV) throw new Error(`addons.portfolio.dodoProductEnv must be ${PRODUCT_ENV}`);
  return { priceCents: Math.round(p.priceUsd * 100), slots: p.slots ?? [] };
}

/** The product create body (Dodo POST /products). */
export function productBody(priceCents) {
  return {
    name: PRODUCT_NAME,
    description: "Deploy, analytics, flags, errors, uptime, AI gateway, billing and consent for every project in a Scribase organization. Requires Scribase Pro.",
    tax_category: "saas",
    metadata: { scribase_addon: "portfolio" },
    price: {
      type: "recurring_price",
      price: priceCents,
      currency: "USD",
      discount: 0,
      purchasing_power_parity: false,
      payment_frequency_count: 1,
      payment_frequency_interval: "Month",
      subscription_period_count: 10,
      subscription_period_interval: "Year",
      trial_period_days: 0,
      tax_inclusive: false,
    },
  };
}

export function baseFor({ live, env }) {
  if (!live) return TEST_BASE;
  if (env.SCRIBASE_DODO_LIVE !== "1") throw new Error("--live needs SCRIBASE_DODO_LIVE=1 (owner item Y1); nothing was sent");
  return LIVE_BASE;
}

async function api(fetchImpl, base, key, method, path, body) {
  const res = await fetchImpl(`${base}${path}`, {
    method,
    headers: { authorization: `Bearer ${key}`, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Dodo ${method} ${path.split("?")[0]} answered ${res.status}`);
  return text ? JSON.parse(text) : {};
}

async function listAll(fetchImpl, base, key, path) {
  const out = [];
  for (let page = 0; page < 50; page += 1) {
    const r = await api(fetchImpl, base, key, "GET", `${path}?page_size=100&page_number=${page}`);
    const items = r.items ?? [];
    out.push(...items);
    if (items.length < 100) break;
  }
  return out;
}

/** Plans the work, and with apply=true does it. Returns the lines to print. */
export async function run({ plans, apply = false, live = false, env = process.env, fetchImpl = fetch }) {
  const { priceCents } = portfolioFrom(plans);
  const base = baseFor({ live, env });
  const mode = base === LIVE_BASE ? "live" : "test";
  if (!apply) {
    return [
      `dry run (${mode} mode): would ensure product "${PRODUCT_NAME}" at ${priceCents} cents a month`,
      ...METERS.map((m) => `dry run: would ensure meter ${m.event_name}`),
    ];
  }
  const key = env.DODO_API_KEY;
  if (!key) throw new Error("DODO_API_KEY is not set; nothing was sent");
  const lines = [];
  const products = await listAll(fetchImpl, base, key, "/products");
  let product = products.find((p) => p?.metadata?.scribase_addon === "portfolio") ?? products.find((p) => p?.name === PRODUCT_NAME);
  if (product) {
    lines.push(`product exists (${mode}): ${product.product_id}`);
  } else {
    product = await api(fetchImpl, base, key, "POST", "/products", productBody(priceCents));
    lines.push(`product created (${mode}): ${product.product_id}`);
  }
  const meters = await listAll(fetchImpl, base, key, "/meters");
  for (const m of METERS) {
    const found = meters.find((x) => x.event_name === m.event_name);
    if (found) {
      lines.push(`meter exists (${mode}): ${m.event_name} ${found.id}`);
    } else {
      const created = await api(fetchImpl, base, key, "POST", "/meters", { ...m, description: "Railyard consumer contract (Wave P 5.3)" });
      lines.push(`meter created (${mode}): ${m.event_name} ${created.id}`);
    }
  }
  lines.push(`${PRODUCT_ENV}=${product.product_id}`);
  return lines;
}

function parseArgs(argv) {
  const a = { apply: false, live: false, plans: null };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--apply") a.apply = true;
    else if (argv[i] === "--live") a.live = true;
    else if (argv[i] === "--plans") a.plans = argv[++i];
    else throw new Error(`unknown argument ${argv[i]}`);
  }
  if (!a.plans) throw new Error("--plans <path to Scribase config/plans.json> is required");
  return a;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const a = parseArgs(process.argv.slice(2));
    const plans = JSON.parse(readFileSync(a.plans, "utf8"));
    for (const line of await run({ plans, apply: a.apply, live: a.live })) process.stdout.write(`${line}\n`);
  } catch (e) {
    process.stderr.write(`portfolio-addon-products: ${e.message}\n`);
    process.exit(1);
  }
}
