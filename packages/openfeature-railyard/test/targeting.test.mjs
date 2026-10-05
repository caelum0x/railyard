// QY1.3 (I10, A19): rules by person property, cohort or segment and % rollouts
// with a stable hash, evaluated locally by the client and both OpenFeature
// providers with the code the server runs. The copies in the console and in
// product analytics are byte-equal to this one.
//   node --test test/*.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createFlagsClient, RailyardFlagsProvider, RailyardServerFlagsProvider } from "../src/index.mjs";
import { answerFlag, evaluateFlag, flagHash, matchProperty, parseSegment, parseTargeting, referencedSegments, sha1Hex } from "../src/targeting.mjs";
import { COHORT_A, RULE_SETS, SEGMENTS, cohortsOf, idOf, propsOf } from "./fixtures/rule-sets.mjs";

const KEY = `ry_flags_${"t".repeat(32)}`;
const SEG = Object.fromEntries(Object.entries(SEGMENTS).map(([k, s]) => [k, { conditions: s.conditions }]));

test("the copies in the console and product analytics are byte-equal to the package's", () => {
  for (const f of ["targeting.mjs", "targeting.d.mts"]) {
    const mine = readFileSync(new URL(`../src/${f}`, import.meta.url), "utf8");
    for (const copy of [`../../../apps/console/lib/flags/${f}`, `../../../apps/product-analytics/src/flags/${f}`]) {
      assert.equal(readFileSync(new URL(copy, import.meta.url), "utf8"), mine, copy);
    }
  }
});

test("the hash is SHA-1 of '<flag>.<id>' as the reference SDKs compute it", () => {
  for (const s of ["", "a", "new_checkout.user_1", "é漢字", "x".repeat(200)]) assert.equal(sha1Hex(s), createHash("sha1").update(s, "utf8").digest("hex"));
  const h = flagHash("new_checkout", "user_1");
  assert.equal(h, parseInt(createHash("sha1").update("new_checkout.user_1").digest("hex").slice(0, 15), 16) / 0xfffffffffffffff);
  assert.ok(h >= 0 && h <= 1);
});

test("a % rollout is stable per id and close to its share over 10 000 ids", () => {
  for (const [rollout, lo, hi] of [[10, 0.08, 0.12], [50, 0.48, 0.52], [33.3, 0.31, 0.355]]) {
    const flag = { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout }] } };
    let on = 0;
    for (let i = 0; i < 10_000; i += 1) {
      const a = evaluateFlag("rollout_check", flag, { id: idOf(i) }).enabled;
      assert.equal(evaluateFlag("rollout_check", flag, { id: idOf(i) }).enabled, a, "stable");
      if (a) on += 1;
    }
    assert.ok(on / 10_000 > lo && on / 10_000 < hi, `${rollout}% gave ${on}`);
  }
  // Raising the rollout only adds people: nobody inside 10% is outside 50%.
  const at = (r, i) => evaluateFlag("grow", { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout: r }] } }, { id: idOf(i) }).enabled;
  for (let i = 0; i < 2000; i += 1) if (at(10, i)) assert.ok(at(50, i));
  // Without an id only a full rollout includes a person.
  assert.equal(evaluateFlag("x", { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout: 99 }] } }, {}).enabled, false);
  assert.equal(evaluateFlag("x", { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout: 100 }] } }, {}).enabled, true);
});

test("property operators follow the reference matcher", () => {
  const P = { plan: "Pro", age: 31, age_text: "31", beta: "TRUE", email: "Ann@ACME.example", gone: null };
  const m = (operator, key, value) => matchProperty({ type: "property", key, operator, value }, P);
  assert.ok(m("exact", "plan", "pro"));
  assert.ok(m("exact", "plan", ["team", "PRO"]));
  assert.ok(!m("is_not", "plan", "pro"));
  assert.ok(m("icontains", "email", "@acme"));
  assert.ok(m("not_icontains", "email", "test"));
  assert.ok(m("regex", "email", "^Ann@"));
  assert.ok(m("not_regex", "email", "^bob"));
  assert.ok(m("gt", "age", 30) && m("gt", "age_text", "30") && !m("lt", "age", 30) && m("gte", "age", 31) && m("lte", "age_text", 31));
  assert.ok(m("exact", "beta", true), "true/false compare by truthiness");
  assert.ok(m("is_set", "plan") && !m("is_not_set", "plan"));
  assert.ok(m("is_not_set", "missing") && !m("exact", "missing", "x") && !m("is_not", "missing", "x"), "a missing property matches only is_not_set");
  assert.ok(m("is_set", "gone") && !m("icontains", "gone", "x") && m("is_not", "gone", "x"));
});

test("rules: first match wins with its value; conditions AND; segments and cohorts; a switched-off flag is off", () => {
  const f = RULE_SETS.r16_two_rules;
  assert.deepEqual(evaluateFlag("r16_two_rules", f, { id: "u", properties: { plan: "enterprise" } }, SEG), { enabled: true, value: "enterprise", reason: "TARGETING_MATCH", rule: 0 });
  const eu = Array.from({ length: 400 }, (_, i) => evaluateFlag("r16_two_rules", f, { id: `eu_${i}`, properties: { plan: "free", country: "FR", age: 40 } }, SEG));
  assert.ok(eu.some((r) => r.value === "eu") && eu.every((r) => !r.enabled || r.value === "eu" || r.value === "default"));
  const kid = evaluateFlag("r16_two_rules", f, { id: "kid", properties: { plan: "free", country: "FR", age: 12 } }, SEG);
  assert.ok(kid.rule === null || kid.rule === 2, "a minor is not in the EU-adults segment");
  assert.equal(evaluateFlag("r19_cohort", RULE_SETS.r19_cohort, { id: "c1", cohorts: [COHORT_A] }).enabled, true);
  assert.equal(evaluateFlag("r19_cohort", RULE_SETS.r19_cohort, { id: "c1", cohorts: [] }).rule === 0, false);
  assert.equal(evaluateFlag("r18_off", RULE_SETS.r18_off, { id: "x" }).reason, "DISABLED");
  // A segment of another environment is never consulted: an unknown key never matches.
  assert.equal(evaluateFlag("r15_segment", RULE_SETS.r15_segment, { id: "x", properties: { plan: "pro" } }, {}).enabled, false);
});

test("validation: bounds, operators, segment references and clean output", () => {
  assert.equal(parseTargeting(null), null);
  assert.match(parseTargeting({ rules: [] }), /1 to 20 rules/);
  assert.match(parseTargeting({ rules: Array.from({ length: 21 }, () => ({ conditions: [] })) }), /1 to 20 rules/);
  assert.match(parseTargeting({ rules: [{ rollout: 101 }] }), /rollout/);
  assert.match(parseTargeting({ rules: [{ conditions: [{ type: "property", key: "plan", operator: "like", value: "x" }] }] }), /operator/);
  assert.match(parseTargeting({ rules: [{ conditions: [{ type: "property", key: "plan", operator: "regex", value: "(" }] }] }), /regular expression/);
  assert.match(parseTargeting({ rules: [{ conditions: [{ type: "property", key: "plan", operator: "icontains", value: ["a"] }] }] }), /exact and is_not/);
  assert.match(parseTargeting({ rules: [{ conditions: [{ type: "segment", key: "nope" }] }] }, new Set(["paying"])), /no segment "nope"/);
  assert.match(parseTargeting({ rules: [{ conditions: [{ type: "cohort", id: "12" }] }] }), /cohort id/);
  assert.deepEqual(parseTargeting({ rules: [{ conditions: [{ type: "property", key: "plan", value: "pro", extra: 1 }] }], other: 1 }), { rules: [{ conditions: [{ type: "property", key: "plan", operator: "exact", value: "pro" }], rollout: 100 }] });
  assert.deepEqual(parseSegment({ key: "paying", conditions: SEGMENTS.paying.conditions }), { key: "paying", name: "paying", conditions: SEGMENTS.paying.conditions });
  assert.match(parseSegment({ key: "x", conditions: [{ type: "segment", key: "y" }] }), /property conditions only/);
  assert.match(parseSegment({ key: "x", conditions: [] }), /at least one condition/);
});

test("the read answer: context-free values for old clients, rules for local evaluation", () => {
  const a = answerFlag("r04_pro", RULE_SETS.r04_pro);
  assert.deepEqual(a, { enabled: false, value: null, targeting: { on: true, value: null, rules: RULE_SETS.r04_pro.targeting.rules } });
  assert.deepEqual(answerFlag("plain", { enabled: true, value: "x" }), { enabled: true, value: "x" });
  assert.deepEqual(referencedSegments({ a: answerFlag("r15_segment", RULE_SETS.r15_segment) }, SEG), { paying: { conditions: SEGMENTS.paying.conditions } });
});

function readEndpoint() {
  const flags = Object.fromEntries(Object.entries(RULE_SETS).map(([k, f]) => [k, answerFlag(k, f, {}, SEG)]));
  const body = { env: "production", flags, segments: referencedSegments(flags, SEG), fetched_at: "2026-10-05T12:00:00.000Z" };
  return async () => new Response(JSON.stringify(body), { status: 200 });
}

test("client and both providers answer per person from the evaluation context, the same as the evaluator", async () => {
  const client = createFlagsClient({ key: KEY, fetch: readEndpoint(), setTimeout: () => 0, clearTimeout: () => {} });
  await client.ready();
  const provider = new RailyardFlagsProvider({ key: KEY, fetch: readEndpoint(), setTimeout: () => 0, clearTimeout: () => {} });
  await provider.initialize();
  const server = new RailyardServerFlagsProvider({ key: KEY, fetch: readEndpoint() });
  await server.initialize();
  for (let i = 0; i < 500; i += 1) {
    const ctx = { targetingKey: idOf(i), ...propsOf(i), cohorts: cohortsOf(i) };
    for (const [key, flag] of Object.entries(RULE_SETS)) {
      const want = evaluateFlag(key, flag, { id: idOf(i), properties: propsOf(i), cohorts: cohortsOf(i) }, SEG);
      assert.equal(client.evaluate(key, { id: idOf(i), properties: propsOf(i), cohorts: cohortsOf(i) }).enabled, want.enabled, `${key} ${i}`);
      assert.equal(provider.resolveBooleanEvaluation(key, false, ctx).value, want.enabled, `${key} ${i}`);
      assert.equal((await server.resolveBooleanEvaluation(key, false, ctx)).value, want.enabled, `${key} ${i}`);
    }
  }
  const ent = provider.resolveStringEvaluation("r16_two_rules", "none", { targetingKey: "e", plan: "enterprise" });
  assert.deepEqual([ent.value, ent.reason], ["enterprise", "TARGETING_MATCH"]);
  const off = provider.resolveStringEvaluation("r04_pro", "none", { targetingKey: "f", plan: "free" });
  assert.deepEqual([off.value, off.reason], ["none", "DEFAULT"]);
  assert.equal(provider.resolveBooleanEvaluation("missing", true, {}).errorCode, "FLAG_NOT_FOUND");
  // The context-free answer is safe for old clients: a targeted flag is off.
  assert.equal(client.isEnabled("r04_pro"), false);
  await provider.onClose();
  client.close();
});

test("a cached start keeps rules and segments", async () => {
  const store = new Map();
  const storage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => void store.set(k, v) };
  const first = createFlagsClient({ key: KEY, fetch: readEndpoint(), storage, setTimeout: () => 0, clearTimeout: () => {} });
  await first.ready();
  first.close();
  const offline = createFlagsClient({ key: KEY, fetch: async () => { throw new TypeError("offline"); }, storage, setTimeout: () => 0, clearTimeout: () => {} });
  await offline.ready();
  assert.equal(offline.source(), "cache");
  assert.equal(offline.evaluate("r15_segment", { id: "u", properties: { plan: "pro" } }).enabled, evaluateFlag("r15_segment", RULE_SETS.r15_segment, { id: "u", properties: { plan: "pro" } }, SEG).enabled);
  assert.deepEqual(Object.keys(offline.segments()).sort(), ["eu_adults", "paying"]);
  offline.close();
});
