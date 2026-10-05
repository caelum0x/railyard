// Railyard Flags (Wave P PY2.1): environments, flags, evaluation, branch
// copies, tenant isolation, both backends (the Flagsmith one against a fake
// of its admin API) and the PostgREST store's requests.
//   node --test lib/flags/flags.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";

import { MemoryFlagsStore, PostgrestFlagsStore } from "./store.ts";
import { TableFlagsBackend } from "./table-backend.ts";
import { FlagsmithBackend } from "./flagsmith-backend.ts";
import { deleteSegment, ensureEnv, evaluate, firstRead, FlagsError, listFlags, listSegments, parseFlagInput, setFlag, setSegment, type FlagsDeps } from "./service.ts";
import { clientFlags, consoleSegment, readContext } from "./http.ts";
import { evaluateFlag } from "./targeting.mjs";
import { RailyardFlagsProvider } from "../../../../packages/openfeature-railyard/src/index.mjs";
import { COHORT_A, RULE_SETS, SEGMENTS, cohortsOf, idOf, propsOf } from "../../../../packages/openfeature-railyard/test/fixtures/rule-sets.mjs";

const NOW = new Date("2026-09-29T12:00:00Z");

function tableDeps(): FlagsDeps {
  const store = new MemoryFlagsStore();
  const backend = new TableFlagsBackend(store);
  return { store, backend, backends: { table: backend } };
}

/** A fake of the Flagsmith admin and flags API (the calls FlagsmithBackend makes). */
function fakeFlagsmith(token = "admin-token") {
  let id = 0;
  const orgs = new Map<number, string>();
  const projects = new Map<number, { org: number; name: string }>();
  const envs = new Map<string, { id: number; project: number; name: string }>();
  const features = new Map<number, { id: number; project: number; name: string; description: string }>();
  const states = new Map<number, { id: number; env: string; feature: number; enabled: boolean; feature_state_value: string | null }>();
  const calls: string[] = [];
  const reply = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
  const fetchImpl = (async (url: string, init: RequestInit) => {
    const u = new URL(url);
    const h = new Headers(init.headers);
    const body = init.body ? JSON.parse(String(init.body)) : null;
    const path = u.pathname.replace(/^\/api\/v1/, "");
    calls.push(`${init.method} ${path}`);
    if (path === "/flags/") {
      const env = envs.get(h.get("x-environment-key") ?? "");
      if (!env) return reply({ detail: "bad key" }, 403);
      return reply(
        [...states.values()]
          .filter((s) => s.env === h.get("x-environment-key"))
          .map((s) => ({ enabled: s.enabled, feature_state_value: s.feature_state_value, feature: features.get(s.feature) })),
      );
    }
    if (h.get("authorization") !== `Token ${token}`) return reply({ detail: "auth" }, 401);
    if (init.method === "POST" && path === "/organisations/") {
      orgs.set(++id, body.name);
      return reply({ id }, 201);
    }
    if (init.method === "POST" && path === "/projects/") {
      if (!orgs.has(body.organisation)) return reply({}, 400);
      projects.set(++id, { org: body.organisation, name: body.name });
      return reply({ id }, 201);
    }
    if (init.method === "POST" && path === "/environments/") {
      const key = `env_key_${++id}_${"x".repeat(20)}`;
      envs.set(key, { id, project: body.project, name: body.name });
      for (const f of features.values()) if (f.project === body.project) states.set(++id, { id, env: key, feature: f.id, enabled: false, feature_state_value: null });
      return reply({ id, api_key: key }, 201);
    }
    let m = /^\/projects\/(\d+)\/features\/$/.exec(path);
    if (m) {
      const project = Number(m[1]);
      if (init.method === "GET") {
        const q = u.searchParams.get("search") ?? "";
        return reply({ results: [...features.values()].filter((f) => f.project === project && f.name.includes(q)) });
      }
      const f = { id: ++id, project, name: body.name, description: body.description };
      features.set(f.id, f);
      for (const [key, e] of envs) if (e.project === project) states.set(++id, { id, env: key, feature: f.id, enabled: false, feature_state_value: null });
      return reply(f, 201);
    }
    m = /^\/environments\/([^/]+)\/featurestates\/$/.exec(path);
    if (m) {
      const feature = Number(u.searchParams.get("feature"));
      return reply({ results: [...states.values()].filter((s) => s.env === m![1] && s.feature === feature) });
    }
    m = /^\/environments\/([^/]+)\/featurestates\/(\d+)\/$/.exec(path);
    if (m && init.method === "PATCH") {
      const s = states.get(Number(m[2]));
      if (!s || s.env !== m[1]) return reply({}, 404);
      Object.assign(s, body);
      return reply(s);
    }
    return reply({ detail: "not found" }, 404);
  }) as typeof fetch;
  return { fetchImpl, calls, orgs, projects, envs };
}

function flagsmithDeps(fake = fakeFlagsmith()) {
  const store = new MemoryFlagsStore();
  const backend = new FlagsmithBackend({ apiUrl: "https://flags.test", adminToken: "admin-token", fetchImpl: fake.fetchImpl });
  return { deps: { store, backend, backends: { flagsmith: backend } } as FlagsDeps, fake };
}

for (const [label, make] of [
  ["table", () => ({ deps: tableDeps() })],
  ["flagsmith", () => flagsmithDeps()],
] as const) {
  test(`${label}: create, flip and read a flag through the client key, same surface`, async () => {
    const { deps } = make();
    const { env, created } = await ensureEnv(deps, { orgId: "org_a", orgName: "Acme", app: "shop", env: "production" }, NOW);
    assert.equal(created, true);
    assert.match(env.client_key, /^ry_flags_[A-Za-z0-9_-]{32}$/);
    assert.equal((await ensureEnv(deps, { orgId: "org_a", orgName: "Acme", app: "shop", env: "production" }, NOW)).created, false);
    await setFlag(deps, env, { key: "new_checkout", enabled: false, value: "blue", description: "The new checkout" }, "user:1", NOW);
    let read = await evaluate(deps, env.client_key, NOW);
    assert.deepEqual(read.flags, { new_checkout: { enabled: false, value: "blue" } });
    await setFlag(deps, env, { key: "new_checkout", enabled: true }, "user:1", NOW);
    read = await evaluate(deps, env.client_key, NOW);
    assert.deepEqual(read.flags.new_checkout, { enabled: true, value: "blue" }, "a flip keeps the value");
    assert.equal(read.env, "production");
    assert.deepEqual((await listFlags(deps, env)).map((f) => f.key), ["new_checkout"]);
    assert.deepEqual(await firstRead(deps.store, "org_a"), { kind: "first_flag_read", id: "shop/production", at: NOW.toISOString() });
  });

  test(`${label}: a branch gets its own environment, seeded from its parent, then changes alone`, async () => {
    const { deps } = make();
    const prod = (await ensureEnv(deps, { orgId: "org_b", orgName: "B", app: "shop", env: "production" }, NOW)).env;
    await setFlag(deps, prod, { key: "banner", enabled: true, value: "hello" }, "u", NOW);
    const branch = (await ensureEnv(deps, { orgId: "org_b", orgName: "B", app: "shop", env: "feature-x", copyFrom: "production" }, NOW)).env;
    assert.notEqual(branch.client_key, prod.client_key);
    assert.equal(branch.copied_from, prod.id);
    assert.deepEqual((await evaluate(deps, branch.client_key, NOW)).flags.banner, { enabled: true, value: "hello" });
    await setFlag(deps, branch, { key: "banner", enabled: false }, "u", NOW);
    assert.equal((await evaluate(deps, branch.client_key, NOW)).flags.banner.enabled, false);
    assert.equal((await evaluate(deps, prod.client_key, NOW)).flags.banner.enabled, true, "the parent is untouched");
  });

  test(`${label}: tenant isolation - one client key reads one environment of one org`, async () => {
    const { deps } = make();
    const a = (await ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "shop", env: "production" }, NOW)).env;
    const b = (await ensureEnv(deps, { orgId: "org_b", orgName: "B", app: "shop", env: "production" }, NOW)).env;
    await setFlag(deps, a, { key: "secret_a", enabled: true }, "u", NOW);
    await setFlag(deps, b, { key: "secret_b", enabled: true }, "u", NOW);
    assert.deepEqual(Object.keys((await evaluate(deps, a.client_key, NOW)).flags), ["secret_a"]);
    assert.deepEqual(Object.keys((await evaluate(deps, b.client_key, NOW)).flags), ["secret_b"]);
    for (const bad of [null, "", "ry_flags_short", `ry_flags_${"z".repeat(32)}`, "anything"]) {
      await assert.rejects(evaluate(deps, bad, NOW), (e: FlagsError) => e.status === 401);
    }
    assert.equal(await deps.store.findEnv("org_a", "shop", "production").then((e) => e?.id), a.id);
    assert.equal((await deps.store.listEnvs("org_b")).length, 1);
  });
}

// ---------------------------------------------------------------- QY1.3 targeting

const SEG_MAP = Object.fromEntries(Object.entries(SEGMENTS).map(([k, v]) => [k, { conditions: v.conditions }]));

async function seeded(deps: FlagsDeps, orgId = "org_t") {
  const env = (await ensureEnv(deps, { orgId, orgName: "T", app: "shop", env: "production" }, NOW)).env;
  for (const seg of Object.values(SEGMENTS)) await setSegment(deps, env, seg, "user:1", NOW);
  for (const [key, f] of Object.entries(RULE_SETS)) await setFlag(deps, env, { key, enabled: f.enabled, value: f.value, targeting: f.targeting }, "user:1", NOW);
  return env;
}

for (const [label, make] of [
  ["table", () => ({ deps: tableDeps() })],
  ["flagsmith", () => flagsmithDeps()],
] as const) {
  test(`QY1.3 ${label}: rules and % rollouts are stored, listed, answered per person and cleared - the same features on both backends`, async () => {
    const { deps } = make();
    const env = await seeded(deps);
    const listed = await listFlags(deps, env);
    assert.equal(listed.length, 20);
    assert.deepEqual(listed.find((f) => f.key === "r16_two_rules")?.targeting, RULE_SETS.r16_two_rules.targeting);
    for (let i = 0; i < 300; i += 1) {
      const ctx = { id: idOf(i), properties: propsOf(i), cohorts: cohortsOf(i) };
      const read = await evaluate(deps, env.client_key, NOW, ctx);
      for (const [key, f] of Object.entries(RULE_SETS)) assert.equal(read.flags[key].enabled, evaluateFlag(key, f, ctx, SEG_MAP).enabled, `${key} ${i}`);
    }
    // Without a person, a targeted flag is off (old clients stay safe) and its rules travel for local evaluation.
    const anon = await evaluate(deps, env.client_key, NOW);
    assert.equal(anon.flags.r04_pro.enabled, false);
    assert.deepEqual(anon.flags.r04_pro.targeting?.rules, RULE_SETS.r04_pro.targeting.rules);
    assert.deepEqual(Object.keys(anon.segments).sort(), ["eu_adults", "paying"]);
    // A flip keeps the rules; null makes the flag plain again.
    await setFlag(deps, env, { key: "r04_pro", enabled: false }, "u", NOW);
    assert.deepEqual((await listFlags(deps, env)).find((f) => f.key === "r04_pro")?.targeting, RULE_SETS.r04_pro.targeting);
    await setFlag(deps, env, { key: "r04_pro", enabled: true, targeting: null }, "u", NOW);
    const plain = await evaluate(deps, env.client_key, NOW);
    assert.deepEqual(plain.flags.r04_pro, { enabled: true, value: null });
  });
}

test("QY1.3: segments - unknown references refused, a used segment cannot be removed, a branch copies rules and segments", async () => {
  const deps = tableDeps();
  const env = await seeded(deps);
  await assert.rejects(setFlag(deps, env, { key: "x", enabled: true, targeting: { rules: [{ conditions: [{ type: "segment", key: "nope" }] }] } }, "u", NOW), (e: FlagsError) => e.status === 400 && /no segment "nope"/.test(e.message));
  await assert.rejects(deleteSegment(deps, env, "paying"), (e: FlagsError) => e.status === 409 && /r15_segment/.test(e.message));
  await assert.rejects(setSegment(deps, env, { key: "bad", conditions: [{ type: "cohort", id: COHORT_A }] }, "u", NOW), (e: FlagsError) => e.status === 400);
  await setSegment(deps, env, { key: "spare", conditions: [{ type: "property", key: "plan", operator: "exact", value: "free" }] }, "u", NOW);
  assert.equal(await deleteSegment(deps, env, "spare"), true);
  const branch = (await ensureEnv(deps, { orgId: "org_t", orgName: "T", app: "shop", env: "feature-y", copyFrom: "production" }, NOW)).env;
  assert.deepEqual((await listSegments(deps, branch)).map((s) => s.key), ["eu_adults", "paying"]);
  assert.deepEqual((await listFlags(deps, branch)).find((f) => f.key === "r15_segment")?.targeting, RULE_SETS.r15_segment.targeting);
  // The console's segment handler: the user's own org only.
  assert.equal((await consoleSegment(deps, { org: "org_other", uid: "x" }, { app: "shop", env: "production", segment: SEGMENTS.paying }, NOW)).status, 404);
  assert.equal((await consoleSegment(deps, null, {}, NOW)).status, 401);
  const ok = await consoleSegment(deps, { org: "org_t", uid: "1" }, { app: "shop", env: "production", segment: { key: "vip", name: "VIP", conditions: [{ type: "property", key: "plan", operator: "exact", value: "enterprise" }] } }, NOW);
  assert.equal(ok.status, 200);
});

test("QY1.3: tenant isolation - one environment's rules and segments never reach another's people", async () => {
  const deps = tableDeps();
  const a = await seeded(deps, "org_a");
  const b = (await ensureEnv(deps, { orgId: "org_b", orgName: "B", app: "shop", env: "production" }, NOW)).env;
  await setFlag(deps, b, { key: "r15_segment", enabled: true }, "u", NOW);
  // B has no segments: a rule naming A's segment is refused in B.
  await assert.rejects(setFlag(deps, b, { key: "r15_segment", enabled: true, targeting: RULE_SETS.r15_segment.targeting }, "u", NOW), (e: FlagsError) => e.status === 400);
  const readB = await evaluate(deps, b.client_key, NOW, { id: "user_1", properties: { plan: "pro" } });
  assert.deepEqual(Object.keys(readB.flags), ["r15_segment"]);
  assert.deepEqual(readB.segments, {});
  assert.equal(readB.flags.r15_segment.targeting, undefined, "B's flag is plain; A's rules are not B's");
  const readA = await evaluate(deps, a.client_key, NOW);
  assert.equal(Object.keys(readA.flags).length, 20);
});

test("QY1.3: the read endpoint takes the person from the query, refuses bad context, and caches per URL", async () => {
  const deps = tableDeps();
  const env = await seeded(deps);
  const props = encodeURIComponent(JSON.stringify({ plan: "enterprise" }));
  const res = await clientFlags(deps, new Request(`https://c/api/v1/flags/client?key=${env.client_key}&id=user_9&props=${props}`), NOW);
  assert.equal(res.status, 200);
  const body = (await res.json()) as { flags: Record<string, { enabled: boolean; value: string | null }> };
  assert.deepEqual(body.flags.r16_two_rules, { enabled: true, value: "enterprise", targeting: { on: true, value: "default", rules: RULE_SETS.r16_two_rules.targeting.rules } });
  for (const bad of ["props=%7B", "props=%5B%5D", `props=${"x".repeat(2100)}`, "cohorts=nope", `id=${"i".repeat(201)}`]) {
    const r = await clientFlags(deps, new Request(`https://c/api/v1/flags/client?key=${env.client_key}&${bad}`), NOW);
    assert.equal(r.status, 400, bad);
  }
  assert.deepEqual(readContext(new URLSearchParams(`id=u&cohorts=${COHORT_A}`)), { id: "u", cohorts: [COHORT_A] });
});

test("QY1.3: 10 000 ids x 20 rule sets - the OpenFeature provider's local answer equals the read endpoint's answer for that person", { timeout: 240_000 }, async () => {
  const deps = tableDeps();
  const env = await seeded(deps);
  const read = async (url: string) => clientFlags(deps, new Request(url), NOW);
  const provider = new RailyardFlagsProvider({ key: env.client_key, url: "https://c/api/v1/flags/client", fetch: ((u: string) => read(u)) as typeof fetch, setTimeout: (() => 0) as unknown as typeof setTimeout, clearTimeout: (() => {}) as typeof clearTimeout });
  await provider.initialize();
  let mismatches = 0;
  let on = 0;
  for (let i = 0; i < 10_000; i += 1) {
    const props = propsOf(i);
    const q = `id=${encodeURIComponent(idOf(i))}&props=${encodeURIComponent(JSON.stringify(props))}${cohortsOf(i).length ? `&cohorts=${cohortsOf(i).join(",")}` : ""}`;
    const server = (await (await read(`https://c/api/v1/flags/client?key=${env.client_key}&${q}`)).json()) as { flags: Record<string, { enabled: boolean; value: string | null }> };
    for (const key of Object.keys(RULE_SETS)) {
      const local = provider.resolveBooleanEvaluation(key, false, { targetingKey: idOf(i), ...props, cohorts: cohortsOf(i) });
      if (local.value !== server.flags[key].enabled) mismatches += 1;
      if (local.value) on += 1;
      if (key === "r16_two_rules" && local.value) {
        const v = provider.resolveStringEvaluation(key, "", { targetingKey: idOf(i), ...props, cohorts: cohortsOf(i) }).value;
        if (v !== server.flags[key].value) mismatches += 1;
      }
    }
  }
  assert.equal(mismatches, 0);
  assert.ok(on > 20_000 && on < 180_000, `a real mix of on and off (${on})`);
  await provider.onClose();
});

test("flagsmith: one organisation per Railyard org, one project per app, one environment per env", async () => {
  const { deps, fake } = flagsmithDeps();
  await ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "shop", env: "production" }, NOW);
  await ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "shop", env: "preview" }, NOW);
  await ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "blog", env: "production" }, NOW);
  await ensureEnv(deps, { orgId: "org_b", orgName: "B", app: "shop", env: "production" }, NOW);
  assert.equal(fake.orgs.size, 2);
  assert.equal(fake.projects.size, 3);
  assert.equal(fake.envs.size, 4);
  const envs = await deps.store.listEnvs("org_a");
  assert.ok(envs.every((e) => e.backend === "flagsmith" && e.backend_ref && e.backend_org === envs[0].backend_org));
});

test("flagsmith: a service error is a plain 502 that changes nothing and names no host or key", async () => {
  const broken = (async () => new Response("{}", { status: 500 })) as typeof fetch;
  const store = new MemoryFlagsStore();
  const backend = new FlagsmithBackend({ apiUrl: "https://flags.test", adminToken: "t", fetchImpl: broken });
  const deps: FlagsDeps = { store, backend, backends: { flagsmith: backend } };
  await assert.rejects(ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "shop", env: "production" }, NOW), (e: FlagsError) => e.status === 502 && !/flags\.test|Token/.test(e.message));
  assert.equal(store.envs.length, 0, "no half-made environment");
});

test("an environment made on a backend this deployment no longer serves answers 503, not someone else's data", async () => {
  const deps = tableDeps();
  const env = (await ensureEnv(deps, { orgId: "org_a", orgName: "A", app: "shop", env: "production" }, NOW)).env;
  const moved: FlagsDeps = { ...deps, backends: {} };
  await assert.rejects(evaluate(moved, env.client_key, NOW), (e: FlagsError) => e.status === 503);
});

test("input is validated at the boundary", async () => {
  assert.equal(typeof parseFlagInput({ key: "Bad Key", enabled: true }), "string");
  assert.equal(typeof parseFlagInput({ key: "ok", enabled: "yes" }), "string");
  assert.equal(typeof parseFlagInput({ key: "ok", enabled: true, value: 3 }), "string");
  assert.equal(typeof parseFlagInput({ key: "ok", enabled: true, value: "x".repeat(2001) }), "string");
  assert.equal(typeof parseFlagInput([]), "string");
  assert.deepEqual(parseFlagInput({ key: "ok", enabled: true, value: null }), { key: "ok", enabled: true, value: null });
  const deps = tableDeps();
  await assert.rejects(ensureEnv(deps, { orgId: "o", orgName: "O", app: "Bad App", env: "production" }, NOW), (e: FlagsError) => e.status === 400);
  await assert.rejects(ensureEnv(deps, { orgId: "o", orgName: "O", app: "shop", env: "../x" }, NOW), (e: FlagsError) => e.status === 400);
  const env = (await ensureEnv(deps, { orgId: "o", orgName: "O", app: "shop", env: "production" }, NOW)).env;
  await assert.rejects(setFlag(deps, env, { key: "x y", enabled: true }, "u", NOW), (e: FlagsError) => e.status === 400);
});

test("the PostgREST store: schema railyard, conflicts ignored on create, first read set once", async () => {
  const calls: { method: string; url: string; prefer: string | null; body: unknown }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ method: String(init.method), url, prefer: new Headers(init.headers).get("prefer"), body: init.body ? JSON.parse(String(init.body)) : null });
    if (init.method === "PATCH") return new Response(calls.length === 1 ? "[{}]" : "[]", { status: 200 });
    return new Response("[]", { status: 200 });
  }) as typeof fetch;
  const store = new PostgrestFlagsStore({ url: "https://p", serviceRoleKey: "k", fetchImpl });
  assert.equal(await store.markFirstRead("e1", NOW.toISOString()), true);
  assert.equal(await store.markFirstRead("e1", NOW.toISOString()), false);
  assert.equal(calls[0].url, "https://p/rest/v1/flag_envs?id=eq.e1&first_read_at=is.null");
  await store.findEnvByClientKey("ry_flags_a&b");
  assert.equal(calls[2].url, "https://p/rest/v1/flag_envs?client_key=eq.ry_flags_a%26b&limit=1");
  await assert.rejects(store.createEnv({ org_id: "o", app: "a", env: "e", client_key: "k", backend: "table", backend_ref: null, backend_project: null, backend_org: null, copied_from: null }), /could not create/);
  assert.equal(calls[3].prefer, "resolution=ignore-duplicates,return=representation");
});
