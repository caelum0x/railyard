// QY1.3 (I10, A19) on the real service and Postgres:
//  - 10 000 ids x 20 rule sets: the OpenFeature provider's local answer (the
//    person's properties and cohorts in its context) equals /flags, which
//    reads the person's stored properties and cohort membership itself;
//  - the reference SDK's own local evaluation (posthog-node, unmodified)
//    gives the same answers for the property and rollout rule sets;
//  - request person_properties override stored ones; another project's
//    people and rules never mix in.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import pg from "pg";
import { PostHog } from "posthog-node";
import { MemoryFlagsReader, type SlotFlag } from "../../src/flags/client.ts";
import { answerFlag, evaluateFlag, referencedSegments, type Condition, type SegmentMap } from "../../src/flags/targeting.mjs";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, SECRET } from "../a3/helpers.ts";
import { RailyardFlagsProvider } from "../../../../packages/openfeature-railyard/src/index.mjs";
import { COHORT_A, COHORT_B, RULE_SETS, SEGMENTS, cohortsOf, idOf, propsOf } from "../../../../packages/openfeature-railyard/test/fixtures/rule-sets.mjs";

const KEY = "ry_flags_targetingabcdefghijklmnop0123456";
const OTHER_KEY = "ry_flags_othertargetabcdefghijklmnop01234";
const N = 10_000;
const SEG: SegmentMap = Object.fromEntries(Object.entries(SEGMENTS).map(([k, v]) => [k, { conditions: v.conditions }]));
const answered: Record<string, SlotFlag> = Object.fromEntries(Object.entries(RULE_SETS).map(([k, f]) => [k, answerFlag(k, f, {}, SEG)]));
const readBody = { env: "production", flags: answered, segments: referencedSegments(answered, SEG), fetched_at: new Date().toISOString() };

let db: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });

before(async () => {
  if (skipNoPg) return;
  db = await migratedPg();
  const reader = new MemoryFlagsReader(
    { [KEY]: answered, [OTHER_KEY]: { r04_pro: { enabled: true, value: null } } },
    { [KEY]: readBody.segments, [OTHER_KEY]: {} },
  );
  run = await startService(db, [site, other], { consoleSecret: SECRET, flags: reader });
  for (const [s, k] of [[site, KEY], [other, OTHER_KEY]] as const) assert.equal((await api(run, s, "/settings", { method: "PUT", body: { flags_client_key: k } })).status, 200);
  // 10 000 people with their properties, and the two cohorts' members, as stored rows.
  const c = new pg.Client({ connectionString: db.url });
  await c.connect();
  try {
    await c.query("insert into pa.cohorts (id, project_id, name, kind) values ($1, $3, 'a', 'static'), ($2, $3, 'b', 'static')", [COHORT_A, COHORT_B, site.project_id]);
    for (let start = 0; start < N; start += 1000) {
      const idx = Array.from({ length: Math.min(1000, N - start) }, (_, j) => start + j);
      const persons = await c.query<{ id: string }>(
        "insert into pa.persons (project_id, properties) select $1, p from unnest($2::jsonb[]) with ordinality as t(p, n) order by n returning id",
        [site.project_id, idx.map((i) => JSON.stringify(propsOf(i)))],
      );
      const pids = persons.rows.map((r) => r.id);
      await c.query("insert into pa.person_distinct_ids (project_id, distinct_id, person_id) select $1, d, p from unnest($2::text[], $3::uuid[]) as t(d, p)", [site.project_id, idx.map(idOf), pids]);
      const members: [string, string][] = idx.flatMap((i, j) => cohortsOf(i).map((co) => [co, pids[j]] as [string, string]));
      if (members.length) await c.query("insert into pa.cohort_people (project_id, cohort_id, person_id) select $1, co, p from unnest($2::uuid[], $3::uuid[]) as t(co, p)", [site.project_id, members.map((m) => m[0]), members.map((m) => m[1])]);
    }
    // The other project has a person with the same id who is "pro": its rules are not this project's.
    const op = await c.query<{ id: string }>("insert into pa.persons (project_id, properties) values ($1, '{\"plan\":\"pro\"}') returning id", [other.project_id]);
    await c.query("insert into pa.person_distinct_ids (project_id, distinct_id, person_id) values ($1, $2, $3)", [other.project_id, idOf(1), op.rows[0].id]);
  } finally {
    await c.end();
  }
});

const flagsFor = async (token: string, distinctId: string, extra: Record<string, unknown> = {}) => {
  const res = await fetch(`${run.base}/flags/?v=2`, { method: "POST", body: JSON.stringify({ token, distinct_id: distinctId, ...extra }) });
  assert.equal(res.status, 200);
  return (await res.json()) as { flags: Record<string, { enabled: boolean; metadata: { payload: string | null } }>; errorsWhileComputingFlags: boolean };
};

test("10 000 ids x 20 rule sets: the OpenFeature provider's local answer equals /flags from stored people and cohorts", { skip: skipNoPg, timeout: 600_000 }, async () => {
  const provider = new RailyardFlagsProvider({ key: KEY, fetch: (async () => new Response(JSON.stringify(readBody), { status: 200 })) as typeof fetch, setTimeout: (() => 0) as unknown as typeof setTimeout, clearTimeout: (() => {}) as typeof clearTimeout });
  await provider.initialize();
  let mismatches = 0;
  let compared = 0;
  let next = 0;
  const perFlag = new Map<string, number>();
  const worker = async () => {
    for (;;) {
      const i = next++;
      if (i >= N) return;
      const body = await flagsFor(site.token, idOf(i));
      assert.equal(body.errorsWhileComputingFlags, false);
      const ctx = { targetingKey: idOf(i), ...propsOf(i), cohorts: cohortsOf(i) };
      for (const key of Object.keys(RULE_SETS)) {
        const local = provider.resolveBooleanEvaluation(key, false, ctx).value;
        compared += 1;
        if (local !== body.flags[key].enabled) mismatches += 1;
        if (local) perFlag.set(key, (perFlag.get(key) ?? 0) + 1);
        if (key === "r16_two_rules" && local && provider.resolveStringEvaluation(key, "", ctx).value !== body.flags[key].metadata.payload) mismatches += 1;
      }
    }
  };
  await Promise.all(Array.from({ length: 24 }, worker));
  assert.equal(compared, N * 20);
  assert.equal(mismatches, 0);
  // Not a trivial agreement: the rollouts land near their share and the cohort rules match their members.
  assert.ok(Math.abs((perFlag.get("r01_half") ?? 0) / N - 0.5) < 0.02, `r01 ${perFlag.get("r01_half")}`);
  assert.ok(Math.abs((perFlag.get("r02_ten") ?? 0) / N - 0.1) < 0.015, `r02 ${perFlag.get("r02_ten")}`);
  assert.equal(perFlag.get("r18_off") ?? 0, 0);
  assert.ok((perFlag.get("r19_cohort") ?? 0) >= N / 5, "every member of cohort A is in");
  for (const key of Object.keys(RULE_SETS)) if (key !== "r18_off") assert.ok((perFlag.get(key) ?? 0) > 0, `${key} matched nobody`);
  await provider.onClose();
});

/** A rule set as the reference SDK's flag definition (segments written out as their conditions). */
function definition(key: string, f: (typeof RULE_SETS)[string], id: number) {
  const props = (conds: Condition[]) =>
    conds.flatMap((c) => (c.type === "segment" ? SEGMENTS[c.key].conditions : [c])).map((c) => ({ key: (c as { key: string }).key, operator: (c as { operator: string }).operator, value: (c as { value?: unknown }).value, type: "person" }));
  return { id, key, active: f.enabled, filters: { groups: f.targeting.rules.map((r) => ({ properties: props(r.conditions), rollout_percentage: r.rollout })) } };
}

test("the reference SDK's own local evaluation agrees for the property and rollout rule sets (10 000 complete people)", { skip: skipNoPg, timeout: 600_000 }, async () => {
  const sets = Object.entries(RULE_SETS).filter(([, f]) => !f.cohort);
  const defs = createServer((req, res) => {
    if (!(req.url ?? "").startsWith("/flags/definitions")) return void res.writeHead(404).end();
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ flags: sets.map(([k, f], n) => definition(k, f, n + 1)), group_type_mapping: {}, cohorts: {} }));
  });
  await new Promise<void>((r) => defs.listen(0, "127.0.0.1", () => r()));
  const sdk = new PostHog(site.token, { host: `http://127.0.0.1:${(defs.address() as AddressInfo).port}`, personalApiKey: "phs_local_eval_test", featureFlagsPollingInterval: 3_600_000, flushAt: 1000 });
  try {
    await sdk.waitForLocalEvaluationReady?.(10_000);
    let mismatches = 0;
    let undecided = 0;
    for (let i = 0; i < N; i += 1) {
      const personProperties = propsOf(i, true);
      for (const [key, f] of sets) {
        const theirs = await sdk.isFeatureEnabled(key, idOf(i), { personProperties, onlyEvaluateLocally: true, sendFeatureFlagEvents: false });
        if (theirs === undefined) {
          undecided += 1;
          continue;
        }
        if (theirs !== evaluateFlag(key, f, { id: idOf(i), properties: personProperties }, SEG).enabled) mismatches += 1;
      }
    }
    assert.equal(undecided, 0, "every case decided locally");
    assert.equal(mismatches, 0);
  } finally {
    await sdk.shutdown();
    defs.close();
  }
});

test("request person_properties override stored ones; another project's people and rules never mix in", { skip: skipNoPg }, async () => {
  const i = 1; // propsOf(1).plan is "pro"
  const stored = await flagsFor(site.token, idOf(i));
  assert.equal(stored.flags.r04_pro.enabled, propsOf(i).plan === "pro");
  const overridden = await flagsFor(site.token, idOf(i), { person_properties: { plan: "free" } });
  assert.equal(overridden.flags.r04_pro.enabled, false);
  const unknown = await flagsFor(site.token, "nobody-here", { person_properties: { plan: "pro" } });
  assert.equal(unknown.flags.r04_pro.enabled, true, "an unknown person is evaluated on the given properties");
  // The other project: its own plain flag only; this project's 20 rule sets are not there.
  const theirs = await flagsFor(other.token, idOf(i));
  assert.deepEqual(Object.keys(theirs.flags), ["r04_pro"]);
  assert.equal(theirs.flags.r04_pro.enabled, true);
});
