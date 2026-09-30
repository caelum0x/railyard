// App A can never read or write app B through the API: its token only ever
// writes its own project, and a console token for A is refused for B (and
// for a forged app list across orgs). The SQL-level half is in
// migrations.test.ts (RLS as the request role).
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const SECRET = "s".repeat(40);
let pg: ScratchPg;
let run: Running;
const a = makeSite({ name: "app-a" });
const b = makeSite({ name: "app-b" });

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [a, b], { consoleSecret: SECRET });
  for (const s of [a, b]) {
    await fetch(`${run.base}/batch/`, { method: "POST", body: JSON.stringify({ api_key: s.token, batch: [{ event: `hello_${s.name}`, distinct_id: "same-id", uuid: randomUUID(), properties: { $set: { email: `${s.name}@x.example` } } }] }) });
  }
  await run.svc.deps.writer.flush();
});

test("a token writes only its own project, even for the same distinct id", { skip: skipNoPg }, async () => {
  assert.equal(count(pg, `project_id = '${a.project_id}' and event = 'hello_app-b'`), 0);
  assert.equal(count(pg, `project_id = '${b.project_id}' and event = 'hello_app-a'`), 0);
  // A body that names app B's project id cannot move an event there: the token decides.
  await fetch(`${run.base}/e/`, { method: "POST", body: JSON.stringify({ event: "sneak", uuid: randomUUID(), properties: { token: a.token, distinct_id: "same-id", project_id: b.project_id, team_id: b.project_id } }) });
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${b.project_id}' and event = 'sneak'`), 0);
  assert.equal(count(pg, `project_id = '${a.project_id}' and event = 'sneak'`), 1);
  // The same distinct id is two different people in two apps.
  assert.equal(pg.psql("select count(distinct person_id) from pa.person_distinct_ids where distinct_id = 'same-id'"), "2");
});

test("a console token for app A cannot read or change app B", { skip: skipNoPg }, async () => {
  const now = new Date();
  const tokA = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, now);
  const get = (path: string, tok: string, init: RequestInit = {}) => fetch(`${run.base}${path}`, { ...init, headers: { authorization: `Bearer ${tok}`, ...(init.headers ?? {}) } });
  assert.equal((await get(`/v1/projects/${a.project_id}/events`, tokA)).status, 200);
  const feed = (await (await get(`/v1/projects/${a.project_id}/events`, tokA)).json()) as { events: { event: string }[] };
  assert.ok(feed.events.every((e) => e.event !== "hello_app-b"));
  for (const path of ["events", "persons", "persons/same-id", "persons/same-id/export", "privacy/jobs", "settings", "status", "groups"]) {
    assert.equal((await get(`/v1/projects/${b.project_id}/${path}`, tokA)).status, 403, path);
  }
  assert.equal((await get(`/v1/projects/${b.project_id}/persons/same-id/delete`, tokA, { method: "POST" })).status, 403);
  assert.equal((await get(`/v1/projects/${b.project_id}/settings`, tokA, { method: "PUT", body: "{}" })).status, 403);
  // A token of org A that lists app B's id is still refused: B belongs to another org.
  const forged = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id, b.project_id], role: "owner" }, now);
  assert.equal((await get(`/v1/projects/${b.project_id}/persons`, forged)).status, 403);
  // Wrong secret, expired, too long-lived, or missing: 401.
  assert.equal((await get(`/v1/projects/${a.project_id}/events`, await signConsoleToken("x".repeat(40), { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, now))).status, 401);
  assert.equal((await get(`/v1/projects/${a.project_id}/events`, await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, new Date(now.getTime() - 301_000)))).status, 401);
  assert.equal((await get(`/v1/projects/${a.project_id}/events`, await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, now, 3600))).status, 401);
  assert.equal((await fetch(`${run.base}/v1/projects/${a.project_id}/events`)).status, 401);
  // A viewer can read but not delete.
  const viewer = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "viewer" }, now);
  assert.equal((await get(`/v1/projects/${a.project_id}/persons`, viewer)).status, 200);
  assert.equal((await get(`/v1/projects/${a.project_id}/persons/same-id/delete`, viewer, { method: "POST" })).status, 403);
});

test("person reads through the API only see the project's own person", { skip: skipNoPg }, async () => {
  const tokA = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, new Date());
  const res = await fetch(`${run.base}/v1/projects/${a.project_id}/persons/same-id`, { headers: { authorization: `Bearer ${tokA}` } });
  const body = (await res.json()) as { person: { properties: { email: string } }; event_count: number };
  assert.equal(body.person.properties.email, "app-a@x.example");
  assert.equal(body.event_count, 2);
});
