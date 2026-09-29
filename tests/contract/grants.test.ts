// Portfolio add-on grants (Wave P PY2.4, dispatch 5.4): the Rago seed holds
// each monthly grant as free units, and the debit contract the partners
// (Menivor PM1.5, Rally PR3.1) and Scribase (the carve-out grants.rs) share.
//   node --test tests/contract/grants.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "../..");
const contract = JSON.parse(readFileSync(join(HERE, "fixtures/grants/contract.json"), "utf8"));
const seed = JSON.parse(readFileSync(join(ROOT, "apps/rago/seeds/portfolio/grants.json"), "utf8"));

test("the seed has one count meter per grant and a $0 plan whose free units are the grant", () => {
  const codes = seed.billable_metrics.map((m: { code: string }) => m.code).sort();
  assert.deepEqual(codes, contract.grants.map((g: { meter: string }) => g.meter).sort());
  for (const m of seed.billable_metrics) assert.equal(m.aggregation_type, "count_agg");
  const [plan] = seed.plans;
  assert.equal(plan.amount_cents, 0);
  assert.equal(plan.interval, "monthly");
  for (const g of contract.grants) {
    const charge = plan.charges.find((c: { billable_metric_code: string }) => c.billable_metric_code === g.meter);
    assert.ok(charge, g.meter);
    assert.equal(charge.charge_model, "package");
    assert.equal(charge.properties.amount, "0.0", "grants are never charged");
    assert.equal(charge.properties.free_units, g.allowance, `${g.grant} free units = the monthly grant`);
  }
});

test("the allowances match Scribase's plans.json when both are on disk", () => {
  for (const path of [
    join(ROOT, "../scribase-wave-p2-railyard/config/plans.json"),
    join(ROOT, "../supabase-plus-clone/upstream/supabase/supabase/scribase/config/plans.json"),
  ]) {
    let plans: { addons?: { portfolio?: { grants?: Record<string, number> } } };
    try {
      plans = JSON.parse(readFileSync(path, "utf8"));
    } catch {
      continue;
    }
    const grants = plans.addons?.portfolio?.grants ?? {};
    for (const g of contract.grants) assert.equal(grants[g.allowance_plan_key], g.allowance, `${path}: ${g.allowance_plan_key}`);
  }
});

test("debit events: transaction id per use and period, the grant's meter, the grants subscription", () => {
  const re = new RegExp(contract.transaction_id_pattern);
  for (const d of contract.sample_debits) {
    assert.match(d.transaction_id, re);
    const [aud, , org, period] = d.transaction_id.split(":");
    const g = contract.grants.find((x: { aud: string }) => x.aud === aud);
    assert.equal(d.code, g.meter);
    assert.equal(d.external_customer_id, org);
    assert.equal(d.external_subscription_id, `portfolio-grants-${org}`);
    assert.match(period, new RegExp(contract.grant_period_pattern));
  }
  for (const bad of ["menivor:grant:org:2026-13:1", "rally:grant:org:2026-09:0", "other:grant:org:2026-09:1", "menivor:grant:org x:2026-09:1"]) {
    assert.doesNotMatch(bad, re, bad);
  }
  assert.match(contract.refusal.message, /\{what\}.*\{renews\}/);
  assert.equal(contract.refusal.status, 402);
});
