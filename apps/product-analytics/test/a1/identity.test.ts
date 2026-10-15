// Identify, alias, merge refusal, $set/$set_once/$unset and groups, driven
// through the capture endpoint (the SDK tests drive the same paths with the
// unmodified SDKs).
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { personIdFor, isDistinctIdIllegal } from "../../src/persons/ids.ts";
import { RESOLVED_PERSON_ID, RESOLVED_PERSON_JOIN } from "../../src/persons/overrides.ts";
import { applyOps, eventOps } from "../../src/persons/properties.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

let pg: ScratchPg;
let run: Running;
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
});

let t0 = Date.parse("2026-09-30T10:00:00Z");
async function send(event: string, distinct_id: string, properties: Record<string, unknown> = {}, extra: Record<string, unknown> = {}) {
  t0 += 1000;
  const res = await fetch(`${run.base}/batch/`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ api_key: site.token, batch: [{ event, distinct_id, uuid: randomUUID(), properties, timestamp: new Date(t0).toISOString(), ...extra }] }),
  });
  assert.equal(res.status, 200, await res.text());
  await run.svc.deps.writer.flush();
}

const q = (sql: string) => pg.psql(sql);
const personOf = (d: string) => q(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = '${d}'`);
const resolvedCount = (personId: string) =>
  Number(q(`select count(*) from pa.events e ${RESOLVED_PERSON_JOIN} where e.project_id = '${site.project_id}' and ${RESOLVED_PERSON_ID} = '${personId}'`));

test("the illegal-id list and the property ops follow upstream", () => {
  for (const id of ["anonymous", "Guest", '"null"', "0", "[object Object]", "  "]) assert.ok(isDistinctIdIllegal(id), id);
  for (const id of ["nan", "user-1", "a@b.co"]) assert.ok(!isDistinctIdIllegal(id), id);
  const ops = eventOps({ event: "x", properties: { $browser: "Firefox", utm_source: "news" }, set: { plan: "pro" }, set_once: { first_seen: "mon" }, unset: ["old"] });
  assert.deepEqual(applyOps({ old: 1, first_seen: "sun" }, ops), { first_seen: "sun", $initial_browser: "Firefox", $initial_utm_source: "news", $browser: "Firefox", utm_source: "news", plan: "pro" });
  assert.equal(applyOps({ plan: "pro" }, { set: { plan: "pro" }, setOnce: {}, unset: [], add: {}, append: {}, union: {}, remove: {} }), null);
  assert.equal(personIdFor(site.project_id, "a"), personIdFor(site.project_id, "a"));
  assert.notEqual(personIdFor(site.project_id, "a"), personIdFor(randomUUID(), "a"));
});

test("anonymous events then identify resolve to one person; earlier events count for that person", { skip: skipNoPg }, async () => {
  await send("$pageview", "anon-1", { $current_url: "https://shop.example/" });
  await send("viewed_item", "anon-1");
  await send("$identify", "user-1", { $anon_distinct_id: "anon-1", $set: { email: "one@shop.example" } });
  await send("purchase", "user-1");
  const p = personOf("user-1");
  assert.equal(personOf("anon-1"), p);
  assert.equal(q(`select is_identified::text || '|' || (properties ->> 'email') from pa.persons where id = '${p}'`), "true|one@shop.example");
  assert.equal(resolvedCount(p), 4);
  assert.equal(q(`select count(*) from pa.persons where project_id = '${site.project_id}'`), "1");
});

test("personless events (identified-only SDK default) are linked to the person on identify", { skip: skipNoPg }, async () => {
  await send("$pageview", "anon-2", { $process_person_profile: false });
  await send("clicked", "anon-2", { $process_person_profile: false });
  assert.equal(personOf("anon-2"), "");
  await send("$identify", "user-2", { $anon_distinct_id: "anon-2" });
  const p = personOf("user-2");
  assert.equal(personOf("anon-2"), p);
  assert.equal(resolvedCount(p), 3);
});

test("two anonymous people merge on identify; the target's properties win", { skip: skipNoPg }, async () => {
  await send("$pageview", "anon-3", { $set: { theme: "dark", tier: "free" } });
  await send("$pageview", "user-3", { $set: { tier: "pro" } });
  const anonPerson = personOf("anon-3");
  await send("$identify", "user-3", { $anon_distinct_id: "anon-3" });
  const p = personOf("user-3");
  assert.notEqual(p, anonPerson);
  assert.equal(personOf("anon-3"), p);
  assert.equal(q(`select (properties ->> 'theme') || '|' || (properties ->> 'tier') from pa.persons where id = '${p}'`), "dark|pro");
  assert.equal(q(`select count(*) from pa.persons where id = '${anonPerson}'`), "0");
  assert.equal(q(`select new_person_id from pa.person_overrides where project_id = '${site.project_id}' and old_person_id = '${anonPerson}'`), p);
  assert.equal(resolvedCount(p), 3);
});

test("alias links a second id to the same person", { skip: skipNoPg }, async () => {
  await send("signed_up", "user-4");
  await send("$create_alias", "user-4", { alias: "crm-4" });
  await send("renewed", "crm-4");
  assert.equal(personOf("crm-4"), personOf("user-4"));
  assert.equal(resolvedCount(personOf("user-4")), 3);
});

test("merging two identified people is refused; $merge_dangerously still merges", { skip: skipNoPg }, async () => {
  await send("$identify", "user-5a", { $anon_distinct_id: "anon-5a" });
  await send("$identify", "user-5b", { $anon_distinct_id: "anon-5b" });
  const a = personOf("user-5a");
  const b = personOf("user-5b");
  await send("$identify", "user-5b", { $anon_distinct_id: "user-5a" });
  assert.equal(personOf("user-5a"), a);
  assert.equal(personOf("user-5b"), b);
  await send("$create_alias", "user-5b", { alias: "user-5a" });
  assert.equal(personOf("user-5a"), a, "alias is refused too");
  await send("$merge_dangerously", "user-5b", { alias: "user-5a" });
  assert.equal(personOf("user-5a"), b);
});

test("an illegal id never merges", { skip: skipNoPg }, async () => {
  await send("$identify", "anonymous", { $anon_distinct_id: "anon-6" });
  assert.notEqual(personOf("anon-6"), personOf("anonymous"));
});

test("$set overwrites, $set_once keeps the first value, $unset removes (events and /engage/)", { skip: skipNoPg }, async () => {
  await send("x", "user-7", { $set: { plan: "free", old: 1 }, $set_once: { first_plan: "free" } });
  await send("x", "user-7", { $set: { plan: "pro" }, $set_once: { first_plan: "pro" }, $unset: ["old"] });
  const p = personOf("user-7");
  assert.equal(q(`select properties::text from pa.persons where id = '${p}'`), JSON.stringify({ plan: "pro", first_plan: "free" }).replace(/,/g, ", ").replace(/:/g, ": "));
  const res = await fetch(`${run.base}/engage/`, { method: "POST", body: JSON.stringify({ $token: site.token, $distinct_id: "user-7", $set: { seats: 3 }, $set_once: { first_plan: "x" } }) });
  assert.equal(res.status, 200);
  assert.equal(q(`select (properties ->> 'seats') || '|' || (properties ->> 'first_plan') from pa.persons where id = '${p}'`), "3|free");
});

test("group properties and the $groups columns land; a sixth group type is ignored", { skip: skipNoPg }, async () => {
  await send("$groupidentify", "$company_acme", { $group_type: "company", $group_key: "acme", $group_set: { name: "Acme", seats: 10 } });
  await send("$groupidentify", "$company_acme", { $group_type: "company", $group_key: "acme", $group_set: { seats: 12 } });
  await send("used_feature", "user-8", { $groups: { company: "acme", team: "t1" } });
  assert.equal(q(`select group_type || ':' || group_type_index from pa.group_types where project_id = '${site.project_id}' order by group_type_index`), "company:0\nteam:1");
  assert.equal(q(`select properties::text from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), '{"name": "Acme", "seats": 12}');
  assert.equal(q(`select "$group_0" || '|' || "$group_1" from pa.events where project_id = '${site.project_id}' and event = 'used_feature'`), "acme|t1");
  const res = await fetch(`${run.base}/groups/`, { method: "POST", body: JSON.stringify({ api_key: site.token, group_type: "company", group_key: "globex", properties: { name: "Globex" } }) });
  assert.equal(res.status, 200);
  assert.equal(q(`select properties ->> 'name' from pa.groups where project_id = '${site.project_id}' and group_key = 'globex'`), "Globex");
  await send("more", "user-8", { $groups: { a: "1", b: "2", c: "3", d: "4" } });
  assert.equal(q(`select count(*) from pa.group_types where project_id = '${site.project_id}'`), "5");
  assert.equal(q(`select coalesce("$group_2", '') || coalesce("$group_3", '') || coalesce("$group_4", '') from pa.events where project_id = '${site.project_id}' and event = 'more'`), "123");
});
