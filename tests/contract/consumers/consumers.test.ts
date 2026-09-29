// The Railyard consumer contract (Wave P PY1.5, dispatch 5.3): the only
// contract PM1.5, PR1.5 and the Scribase carve-out metering test against.
//   node --test tests/contract/consumers/consumers.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "../../..");
const contract = JSON.parse(readFileSync(join(HERE, "contract.json"), "utf8"));
const AGG = new Set(["count_agg", "sum_agg", "max_agg", "unique_count_agg", "latest_agg", "weighted_sum_agg", "custom_agg"]);
const NEEDS_FIELD = new Set(["sum_agg", "max_agg", "unique_count_agg", "latest_agg", "weighted_sum_agg"]);

test("every product's seed defines exactly its contract meters, each valid for Rago, under a $0 mirror plan", () => {
  for (const [product, p] of Object.entries<any>(contract.products)) {
    const seed = JSON.parse(readFileSync(join(ROOT, p.seed), "utf8"));
    assert.equal(seed.consumer, product);
    const codes = seed.billable_metrics.map((m: any) => m.code).sort();
    assert.deepEqual(codes, [...p.meters].sort(), product);
    for (const m of seed.billable_metrics) {
      assert.ok(AGG.has(m.aggregation_type), `${m.code}: ${m.aggregation_type}`);
      if (NEEDS_FIELD.has(m.aggregation_type)) assert.ok(m.field_name, `${m.code} needs field_name`);
      assert.ok(m.code.startsWith(`${product}.`), `${m.code} is namespaced by its product`);
    }
    for (const plan of seed.plans) {
      assert.equal(plan.amount_cents, 0, `${plan.code} is a mirror`);
      for (const c of plan.charges) {
        assert.ok(codes.includes(c.billable_metric_code), `${plan.code} charges an unknown metric`);
        assert.equal(c.properties.amount, "0.0");
      }
    }
  }
});

test("sample events: idempotency key shape, known code, and the aggregated property present", () => {
  const re = new RegExp(contract.transaction_id_pattern);
  const seen = new Set<string>();
  for (const [product, p] of Object.entries<any>(contract.products)) {
    const seed = JSON.parse(readFileSync(join(ROOT, p.seed), "utf8"));
    const byCode = new Map(seed.billable_metrics.map((m: any) => [m.code, m]));
    for (const e of p.sample_events) {
      assert.match(e.transaction_id, re);
      assert.ok(e.transaction_id.startsWith(`${product}:${e.code}:`), "transaction_id is <product>:<meter>:<source id>");
      assert.ok(!seen.has(e.transaction_id), "transaction ids are unique");
      seen.add(e.transaction_id);
      const m: any = byCode.get(e.code);
      assert.ok(m, `${e.code} is a seed metric`);
      if (m.field_name) assert.equal(typeof e.properties[m.field_name], "number", `${e.code}.${m.field_name}`);
      assert.ok(Number.isInteger(e.timestamp) && e.external_customer_id);
    }
    assert.deepEqual(p.sample_events.map((e: any) => e.code).sort(), [...p.meters].sort(), "one sample per meter");
  }
});

test("env names and the switch: secrets are named, never valued; RAILYARD_LIVE defaults to 0", () => {
  assert.deepEqual(contract.env, ["RELAY_URL", "RELAY_KEY", "RAGO_URL", "RAGO_API_KEY", "MINT_URL", "MINT_API_KEY", "RAILYARD_LIVE"]);
  assert.equal(contract.switch.default, "0");
  const doc = readFileSync(join(ROOT, "docs/CONSUMERS.md"), "utf8");
  for (const name of contract.env) assert.ok(doc.includes(name), `CONSUMERS.md names ${name}`);
  assert.doesNotMatch(JSON.stringify(contract), /sk-|whsec_|rago_live_[a-z0-9]{8,}/i);
});
