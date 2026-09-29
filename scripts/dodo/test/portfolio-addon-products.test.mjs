// PY1.1: the add-on product script is test mode by default, idempotent, reads the
// price from plans.json and never prints the key.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { METERS, PRODUCT_ENV, TEST_BASE, portfolioFrom, productBody, run } from "../portfolio-addon-products.mjs";

const SCRIPT = join(dirname(fileURLToPath(import.meta.url)), "../portfolio-addon-products.mjs");
const plans = { addons: { portfolio: { priceUsd: 10, interval: "month", dodoProductEnv: PRODUCT_ENV, slots: ["deploy"] } } };
const KEY = "sk_test_do_not_print_0123456789";

function fakeDodo({ products = [], meters = [] } = {}) {
  const state = { products: [...products], meters: [...meters], calls: [] };
  const fetchImpl = async (url, init) => {
    state.calls.push({ url, method: init.method, body: init.body ? JSON.parse(init.body) : null, auth: init.headers.authorization });
    const u = new URL(url);
    if (u.origin !== TEST_BASE) throw new Error(`unexpected host ${u.origin}`);
    if (init.method === "GET" && u.pathname === "/products") return new Response(JSON.stringify({ items: state.products }));
    if (init.method === "GET" && u.pathname === "/meters") return new Response(JSON.stringify({ items: state.meters }));
    if (init.method === "POST" && u.pathname === "/products") {
      const p = { ...JSON.parse(init.body), product_id: `pdt_${state.products.length + 1}` };
      state.products.push(p);
      return new Response(JSON.stringify(p), { status: 200 });
    }
    if (init.method === "POST" && u.pathname === "/meters") {
      const m = { ...JSON.parse(init.body), id: `mtr_${state.meters.length + 1}` };
      state.meters.push(m);
      return new Response(JSON.stringify(m), { status: 201 });
    }
    return new Response("{}", { status: 404 });
  };
  return { state, fetchImpl };
}

test("the price comes from plans.json; the product is a monthly subscription at priceUsd x 100 cents", () => {
  assert.equal(portfolioFrom(plans).priceCents, 1000);
  const b = productBody(1000);
  assert.equal(b.price.type, "recurring_price");
  assert.equal(b.price.price, 1000);
  assert.equal(b.price.payment_frequency_interval, "Month");
  assert.equal(b.metadata.scribase_addon, "portfolio");
  assert.throws(() => portfolioFrom({}), /addons.portfolio/);
  assert.throws(() => portfolioFrom({ addons: { portfolio: { ...plans.addons.portfolio, priceUsd: 0 } } }), /priceUsd/);
});

test("dry run sends nothing", async () => {
  const f = fakeDodo();
  const lines = await run({ plans, env: { DODO_API_KEY: KEY }, fetchImpl: f.fetchImpl });
  assert.equal(f.state.calls.length, 0);
  assert.match(lines[0], /dry run \(test mode\)/);
});

test("apply in test mode creates the product and the five Scribase meters once; a second run reuses them", async () => {
  const f = fakeDodo();
  const first = await run({ plans, apply: true, env: { DODO_API_KEY: KEY }, fetchImpl: f.fetchImpl });
  assert.equal(f.state.products.length, 1);
  assert.deepEqual(f.state.meters.map((m) => m.event_name), METERS.map((m) => m.event_name));
  assert.equal(first.at(-1), `${PRODUCT_ENV}=pdt_1`);
  const second = await run({ plans, apply: true, env: { DODO_API_KEY: KEY }, fetchImpl: f.fetchImpl });
  assert.equal(f.state.products.length, 1);
  assert.equal(f.state.meters.length, METERS.length);
  assert.ok(second.some((l) => l.startsWith("product exists")));
  assert.ok(f.state.calls.every((c) => c.auth === `Bearer ${KEY}`));
  assert.ok(![...first, ...second].join("\n").includes(KEY), "the key is never printed");
});

test("live mode needs --live and SCRIBASE_DODO_LIVE=1; otherwise nothing is sent", async () => {
  const f = fakeDodo();
  await assert.rejects(run({ plans, apply: true, live: true, env: { DODO_API_KEY: KEY }, fetchImpl: f.fetchImpl }), /SCRIBASE_DODO_LIVE=1/);
  assert.equal(f.state.calls.length, 0);
  await assert.rejects(run({ plans, apply: true, env: {}, fetchImpl: f.fetchImpl }), /DODO_API_KEY/);
});

test("the CLI refuses without --plans and prints no key on error", () => {
  const r = spawnSync(process.execPath, [SCRIPT], { encoding: "utf8", env: { ...process.env, DODO_API_KEY: KEY } });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /--plans/);
  const dir = mkdtempSync(join(tmpdir(), "plans-"));
  writeFileSync(join(dir, "plans.json"), JSON.stringify(plans));
  const out = execFileSync(process.execPath, [SCRIPT, "--plans", join(dir, "plans.json")], { encoding: "utf8", env: { ...process.env, DODO_API_KEY: KEY } });
  assert.match(out, /dry run \(test mode\)/);
  assert.ok(!out.includes(KEY));
});
