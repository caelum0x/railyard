// Experiments on the flags slot (flags merge pending: the slot is the
// wave-p2-railyard client API contract, served here by a stand-in).
//  - the service's hash equals the SDK core's; for 10 000 distinct ids the
//    unmodified server SDK's local evaluation and the service's /flags answer
//    give the same variant;
//  - the service never writes a flag: it hands the console the flag change,
//    and reads it back through the slot's public client API (flag_state);
//  - results match a hand count, the stats match the ported methods, and a
//    60/40 split on a 50/50 plan raises the sample-ratio warning;
//  - roles and projects are enforced.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { PostHog } from "posthog-node";
import { getFeatureFlagHash } from "../../node_modules/.pnpm/node_modules/@posthog/core/dist/index.mjs";
import { MemoryFlagsReader, type SlotFlag } from "../../src/flags/client.ts";
import { flagHash, variantFor, variantOf } from "../../src/flags/variants.ts";
import { buildFlagValue, checkVariants, parseFlagValue, VariantError } from "../../src/experiments/flag-value.ts";
import { frequentistTest, bayesianTest } from "../../src/experiments/stats/methods.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, type Row } from "./helpers.ts";

const KEY = "ry_flags_experimentsabcdefghijklmnop0123";
const OTHER_KEY = "ry_flags_otherprojectabcdefghijklmnop012";
const slot: Record<string, Record<string, SlotFlag>> = { [KEY]: {}, [OTHER_KEY]: {} };
const ids = Array.from({ length: 10_000 }, (_, i) => (i % 3 === 0 ? `user_${i}` : i % 3 === 1 ? `${i}@shop.example` : `anon-${(i * 2654435761) % 4294967296}`));

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, flags: new MemoryFlagsReader(slot) });
  for (const [s, k] of [[site, KEY], [other, OTHER_KEY]] as const) {
    const r = await api(run, s, "/settings", { method: "PUT", body: { flags_client_key: k } });
    assert.equal(r.status, 200);
  }
});

test("flag values: variants are validated, built and read back; other values stay plain", () => {
  assert.throws(() => checkVariants([{ key: "a", weight: 50 }, { key: "b", weight: 50 }]), /control/);
  assert.throws(() => checkVariants([{ key: "control", weight: 50 }, { key: "test", weight: 40 }]), /add up to 100/);
  assert.throws(() => checkVariants([{ key: "control", weight: 100 }]), VariantError);
  assert.throws(() => checkVariants([{ key: "control", weight: 50 }, { key: "Bad Key", weight: 50 }]), /lowercase/);
  const v = { rollout: 100, variants: [{ key: "control", weight: 50 }, { key: "test", weight: 50 }], experiment: "e1" };
  const s = buildFlagValue(v);
  assert.equal(s, '{"rollout":100,"variants":[{"key":"control","weight":50},{"key":"test","weight":50}],"experiment":"e1"}');
  assert.deepEqual(parseFlagValue(s), v);
  for (const plain of [null, "", "{}", '{"color":"green"}', "not json", '{"experiment":"e","variants":[{"key":"a","weight":60}]}']) assert.equal(parseFlagValue(plain), null, String(plain));
  assert.equal(variantFor("checkout", { enabled: true, value: '{"color":"green"}' }, "u1"), null);
  assert.equal(variantFor("checkout", { enabled: false, value: s }, "u1"), null);
  assert.ok(["control", "test"].includes(variantFor("checkout", { enabled: true, value: s }, "u1")!));
});

test("the service hash equals the SDK core hash for 10 000 ids", async () => {
  for (const id of ids) {
    assert.equal(flagHash("new_checkout", id), await getFeatureFlagHash("new_checkout", id));
    assert.equal(flagHash("new_checkout", id, "variant"), await getFeatureFlagHash("new_checkout", id, "variant"));
  }
});

test("the server SDK's local evaluation and the service's /flags give the same variant for 10 000 ids", { skip: skipNoPg, timeout: 240_000 }, async () => {
  const variants = [
    { key: "control", weight: 34 },
    { key: "test", weight: 33 },
    { key: "test_b", weight: 33 },
  ];
  const value = buildFlagValue({ rollout: 90, variants, experiment: "local-eval" });
  slot[KEY] = { three_way: { enabled: true, value } };
  // The flag definition the SDK evaluates locally, from the same plan.
  const defs = createServer((req, res) => {
    if (!(req.url ?? "").startsWith("/flags/definitions")) return void res.writeHead(404).end();
    res.writeHead(200, { "content-type": "application/json" }).end(
      JSON.stringify({
        flags: [{ id: 1, key: "three_way", active: true, filters: { groups: [{ properties: [], rollout_percentage: 90 }], multivariate: { variants: variants.map((v) => ({ key: v.key, rollout_percentage: v.weight })) } } }],
        group_type_mapping: {},
        cohorts: {},
      }),
    );
  });
  await new Promise<void>((r) => defs.listen(0, "127.0.0.1", () => r()));
  const sdk = new PostHog(site.token, { host: `http://127.0.0.1:${(defs.address() as AddressInfo).port}`, personalApiKey: "phs_local_eval_test", featureFlagsPollingInterval: 3_600_000, flushAt: 1000 });
  try {
    await sdk.waitForLocalEvaluationReady?.(10_000);
    const local = new Map<string, string | null>();
    for (const id of ids) {
      const v = await sdk.getFeatureFlag("three_way", id, { onlyEvaluateLocally: true, sendFeatureFlagEvents: false });
      local.set(id, typeof v === "string" ? v : null);
    }
    const counts = new Map<string | null, number>();
    for (const v of local.values()) counts.set(v, (counts.get(v) ?? 0) + 1);
    assert.ok((counts.get(null) ?? 0) > 800 && (counts.get(null) ?? 0) < 1200, "about 10% outside the rollout");
    for (const k of ["control", "test", "test_b"]) assert.ok((counts.get(k) ?? 0) > 2700, k);
    let mismatches = 0;
    let next = 0;
    const worker = async () => {
      for (;;) {
        const i = next++;
        if (i >= ids.length) return;
        const res = await fetch(`${run.base}/flags/?v=2`, { method: "POST", body: JSON.stringify({ token: site.token, distinct_id: ids[i] }) });
        const body = (await res.json()) as { flags: Record<string, { variant: string | null }> };
        if (body.flags.three_way.variant !== local.get(ids[i])) mismatches += 1;
        assert.equal(variantOf("three_way", parseFlagValue(value)!, ids[i]), local.get(ids[i]));
      }
    };
    await Promise.all(Array.from({ length: 24 }, worker));
    assert.equal(mismatches, 0);
    const legacy = (await (await fetch(`${run.base}/decide/?v=3`, { method: "POST", body: JSON.stringify({ api_key: site.token, distinct_id: ids[0] }) })).json()) as { featureFlags: Record<string, unknown> };
    assert.equal(legacy.featureFlags.three_way, local.get(ids[0]) ?? true);
  } finally {
    await sdk.shutdown();
    defs.close();
  }
});

// A stand-in for the console's call to the flags slot's setFlag (the only writer).
const writes: { env: string; key: string; enabled: boolean; value: string }[] = [];
function setFlagThroughSlot(env: string, flag: { key: string; enabled: boolean; value: string }) {
  writes.push({ env, ...flag });
  slot[KEY] = { ...slot[KEY], [flag.key]: { enabled: flag.enabled, value: flag.value } };
}

test("create, start through the slot, read back, stop; roles and projects are enforced", { skip: skipNoPg }, async () => {
  const body = { name: "Checkout copy", flag_key: "new_checkout", flags_env: "production", metric: { kind: "conversion", event: "purchase" } };
  assert.equal((await api(run, site, "/experiments", { body, role: "viewer" })).status, 403);
  assert.equal((await api(run, site, "/experiments", { body: { ...body, variants: [{ key: "control", weight: 70 }] } })).status, 400);
  const created = await api(run, site, "/experiments", { body });
  assert.equal(created.status, 201, JSON.stringify(created.body));
  const exp = created.body.experiment;
  assert.deepEqual(exp.variants, [{ key: "control", weight: 50 }, { key: "test", weight: 50 }]);
  assert.equal((await api(run, site, "/experiments", { body })).status, 409, "names are unique");
  assert.equal((await api(run, other, `/experiments/${exp.id}`)).status, 404, "another project cannot see it");
  const foreign = await fetch(`${run.base}/v1/projects/${other.project_id}/experiments/${exp.id}`, { headers: { authorization: `Bearer ${await (await import("./helpers.ts")).token(site)}` } });
  assert.equal(foreign.status, 403);

  let detail = await api(run, site, `/experiments/${exp.id}`);
  assert.equal(detail.body.flag_state, "missing");
  const change = await api(run, site, `/experiments/${exp.id}/flag-value?for=start`);
  assert.equal(change.status, 200);
  assert.deepEqual(change.body.flag, { key: "new_checkout", enabled: true, value: buildFlagValue({ rollout: 100, variants: exp.variants, experiment: exp.id }) });
  assert.equal((await api(run, site, `/experiments/${exp.id}/flag-value?for=start`, { role: "viewer" })).status, 403);
  assert.equal(writes.length, 0, "the service wrote no flag");
  setFlagThroughSlot(change.body.flags_env, change.body.flag);
  const started = await api(run, site, `/experiments/${exp.id}/status`, { body: { status: "running" } });
  assert.equal(started.body.experiment.status, "running");
  detail = await api(run, site, `/experiments/${exp.id}`);
  assert.equal(detail.body.flag_state, "live");
  assert.equal((await api(run, site, `/experiments/${exp.id}`, { method: "DELETE" })).status, 409);
  assert.equal((await api(run, site, `/experiments/${exp.id}`, { method: "PUT", body: { flag_key: "other_flag", name: "Checkout copy v2" } })).body.experiment.flag_key, "new_checkout", "a running experiment keeps its flag");
  const second = await api(run, site, "/experiments", { body: { ...body, name: "Same flag" } });
  setFlagThroughSlot("production", (await api(run, site, `/experiments/${second.body.experiment.id}/flag-value?for=start`)).body.flag);
  assert.equal((await api(run, site, `/experiments/${second.body.experiment.id}/status`, { body: { status: "running" } })).status, 409, "one running experiment per flag");
  slot[KEY].new_checkout = { enabled: true, value: change.body.flag.value };

  const stop = await api(run, site, `/experiments/${exp.id}/flag-value?for=stop&variant=test`);
  assert.equal(JSON.parse(stop.body.flag.value).variants[0].key, "test");
  assert.equal((await api(run, site, `/experiments/${exp.id}/flag-value?for=stop&variant=nope`)).status, 400);
  setFlagThroughSlot("production", stop.body.flag);
  assert.equal((await api(run, site, `/experiments/${exp.id}/status`, { body: { status: "stopped" } })).body.experiment.status, "stopped");
  assert.equal(variantFor("new_checkout", slot[KEY].new_checkout, "anyone"), "test", "after stopping, everyone gets the kept variant");
  assert.equal((await api(run, site, `/experiments/${exp.id}`, { method: "DELETE" })).status, 200);
});

function exposure(distinct: string, flag: string, variant: string, at: Date): Row {
  return { event: "$feature_flag_called", distinct_id: distinct, timestamp: at, properties: { $feature_flag: flag, $feature_flag_response: variant } };
}

test("results match a hand count and the ported stats; a 60/40 split of a 50/50 plan warns", { skip: skipNoPg }, async () => {
  const create = async (name: string, flag: string, metric: object, method = "frequentist") => {
    const r = await api(run, site, "/experiments", { body: { name, flag_key: flag, flags_env: "production", metric, method } });
    const id = r.body.experiment.id as string;
    await api(run, site, `/experiments/${id}/status`, { body: { status: "running" } });
    return id;
  };
  const conv = await create("Conversion", "pay_button", { kind: "conversion", event: "purchase" });
  // Every row falls inside the experiments' run: after they started, before the query.
  let t0 = new Date(Date.now() + 5);
  const at = (step: number) => new Date(t0.getTime() + step * 10);
  const settle = () => new Promise((r) => setTimeout(r, 120));
  const rows: Row[] = [];
  const plan = parseFlagValue(buildFlagValue({ rollout: 100, variants: [{ key: "control", weight: 50 }, { key: "test", weight: 50 }], experiment: conv }))!;
  const hand = { control: { n: 0, conv: 0 }, test: { n: 0, conv: 0 } };
  for (let i = 0; i < 2000; i += 1) {
    const id = `buyer_${i}`;
    const v = variantOf("pay_button", plan, id) as "control" | "test";
    rows.push(exposure(id, "pay_button", v, at(1)));
    if (i % 7 === 0) rows.push(exposure(id, "pay_button", v, at(2)));
    hand[v].n += 1;
    const converts = v === "test" ? i % 8 === 0 : i % 11 === 0;
    if (converts) {
      rows.push({ event: "purchase", distinct_id: id, timestamp: at(3), properties: { amount: 10 } });
      hand[v].conv += 1;
    }
    // A purchase before exposure does not count.
    if (i % 13 === 0) rows.push({ event: "purchase", distinct_id: id, timestamp: at(0.5) });
  }
  // Someone seen in both variants is left out.
  rows.push(exposure("switcher", "pay_button", "control", at(1)), exposure("switcher", "pay_button", "test", at(2)), { event: "purchase", distinct_id: "switcher", timestamp: at(3) });
  // Another project's rows never count.
  await insertEvents(pg, other.project_id, [exposure("buyer_1", "pay_button", "test", at(1)), { event: "purchase", distinct_id: "buyer_1", timestamp: at(2) }]);
  await insertEvents(pg, site.project_id, rows);
  await settle();
  const r = (await api(run, site, `/experiments/${conv}/results`)).body.results;
  const got = Object.fromEntries(r.variants.map((v: { variant: string }) => [v.variant, v]));
  assert.equal(got.control.exposed, hand.control.n);
  assert.equal(got.test.exposed, hand.test.n);
  assert.equal(got.control.converted, hand.control.conv);
  assert.equal(got.test.converted, hand.test.conv);
  assert.equal(r.multiple_variants, 1);
  assert.equal(r.srm.mismatch, false);
  const want = frequentistTest({ kind: "proportion", n: hand.test.n, sum: hand.test.conv }, { kind: "proportion", n: hand.control.n, sum: hand.control.conv }, { difference: "relative", alpha: 0.05 });
  assert.ok(Math.abs(r.comparisons[0].p_value - want.pValue) < 1e-12);
  assert.ok(Math.abs(r.comparisons[0].effect - want.pointEstimate) < 1e-12);

  const sum = await create("Revenue", "pay_button2", { kind: "sum", event: "purchase", property: "amount" }, "bayesian");
  t0 = new Date(Date.now() + 5);
  const rows2: Row[] = [];
  const hand2 = { control: { n: 0, s: 0, ss: 0 }, test: { n: 0, s: 0, ss: 0 } };
  for (let i = 0; i < 1000; i += 1) {
    const id = `payer_${i}`;
    // 600 control, 400 test: a 60/40 split of a 50/50 plan.
    const v = i < 600 ? "control" : "test";
    rows2.push(exposure(id, "pay_button2", v, at(1)));
    hand2[v].n += 1;
    const amounts = i % 3 === 0 ? [5, v === "test" ? 9 : 6] : i % 3 === 1 ? ["12.5"] : [];
    let total = 0;
    for (const a of amounts) {
      rows2.push({ event: "purchase", distinct_id: id, timestamp: at(4), properties: { amount: a } });
      total += Number(a);
    }
    hand2[v].s += total;
    hand2[v].ss += total * total;
  }
  await insertEvents(pg, site.project_id, rows2);
  await settle();
  const r2 = (await api(run, site, `/experiments/${sum}/results`)).body.results;
  const g2 = Object.fromEntries(r2.variants.map((v: { variant: string }) => [v.variant, v]));
  for (const k of ["control", "test"] as const) {
    assert.equal(g2[k].exposed, hand2[k].n);
    assert.ok(Math.abs(g2[k].sum - hand2[k].s) < 1e-9, `${k} sum`);
    assert.ok(Math.abs(g2[k].sum_squares - hand2[k].ss) < 1e-9, `${k} sum of squares`);
  }
  assert.equal(r2.srm.mismatch, true);
  assert.match(r2.srm.warning, /sample ratio mismatch/);
  const b = bayesianTest({ kind: "mean", n: hand2.test.n, sum: hand2.test.s, sumSquares: hand2.test.ss }, { kind: "mean", n: hand2.control.n, sum: hand2.control.s, sumSquares: hand2.control.ss }, { difference: "relative", ciLevel: 0.95 });
  assert.ok(Math.abs(r2.comparisons[0].chance_to_win - b.chanceToWin) < 1e-12);

  const empty = await create("Nothing yet", "quiet_flag", { kind: "count", event: "purchase" });
  const r3 = (await api(run, site, `/experiments/${empty}/results`)).body.results;
  assert.match(r3.comparisons[0].error, /No exposures yet/);
  assert.equal(r3.srm.mismatch, false);
});
