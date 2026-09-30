// Person deletion: requested from the console, visible as a job, done by the
// scheduler with the clock moved (well within the 24 h due time); every hook
// runs; other people are untouched. Export returns one JSON document.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import type { GdprHook, PersonRef } from "../../src/gdpr/hooks.ts";
import { GDPR_DELETE_WITHIN_MS } from "../../src/plans.ts";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const SECRET = "g".repeat(40);
let pg: ScratchPg;
let run: Running;
let clock = new Date("2026-09-30T09:00:00Z");
const site = makeSite();
const hookCalls: PersonRef[] = [];
const replayHook: GdprHook = {
  name: "test_replays",
  deletePerson: async (ref) => (hookCalls.push(ref), 2),
  exportPerson: async (ref) => [{ session_id: "s-1", distinct_ids: ref.distinctIds }],
};

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site], { consoleSecret: SECRET, now: () => clock, hooks: [replayHook] });
  const send = (event: string, distinct_id: string, properties: Record<string, unknown> = {}) =>
    fetch(`${run.base}/batch/`, { method: "POST", body: JSON.stringify({ api_key: site.token, batch: [{ event, distinct_id, uuid: randomUUID(), properties, timestamp: clock.toISOString() }] }) });
  await send("$pageview", "anon-g", { $process_person_profile: false });
  await send("$identify", "gone@x.example", { $anon_distinct_id: "anon-g", $set: { email: "gone@x.example" } });
  await send("$create_alias", "gone@x.example", { alias: "crm-g" });
  await send("bought", "crm-g", { $session_id: "s-1" });
  await send("$pageview", "stays@x.example", { $set: { email: "stays@x.example" } });
  await run.svc.deps.writer.flush();
});

const auth = async () => ({ authorization: `Bearer ${await signConsoleToken(SECRET, { org_id: site.org_id, app_ids: [site.project_id], role: "owner" }, clock)}` });

test("export: one JSON file with the person, their ids, every event (merged too) and the recordings list", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/v1/projects/${site.project_id}/persons/crm-g/export`, { headers: await auth() });
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-disposition") ?? "", /attachment/);
  const doc = (await res.json()) as { person: { properties: { email: string } }; distinct_ids: string[]; events: { event: string }[]; replays: unknown[]; test_replays: unknown[] };
  assert.equal(doc.person.properties.email, "gone@x.example");
  assert.deepEqual(doc.distinct_ids.sort(), ["anon-g", "crm-g", "gone@x.example"]);
  assert.deepEqual(doc.events.map((e) => e.event).sort(), ["$create_alias", "$identify", "$pageview", "bought"]);
  assert.deepEqual(doc.test_replays, [{ session_id: "s-1", distinct_ids: doc.distinct_ids }]);
  assert.deepEqual(doc.replays, []);
  assert.equal((await fetch(`${run.base}/v1/projects/${site.project_id}/persons/nobody/export`, { headers: await auth() })).status, 404);
});

test("delete: a visible job, done by the scheduler before it is due, removing only that person", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/v1/projects/${site.project_id}/persons/gone@x.example/delete`, { method: "POST", headers: await auth() });
  assert.equal(res.status, 202);
  const { job } = (await res.json()) as { job: { id: string; status: string; due_at: string; requested_at: string } };
  assert.equal(job.status, "queued");
  assert.equal(Date.parse(job.due_at) - Date.parse(job.requested_at), GDPR_DELETE_WITHIN_MS);
  // Asking again while it is open returns the same job.
  const again = (await (await fetch(`${run.base}/v1/projects/${site.project_id}/persons/gone@x.example/delete`, { method: "POST", headers: await auth() })).json()) as { job: { id: string } };
  assert.equal(again.job.id, job.id);
  const list = (await (await fetch(`${run.base}/v1/projects/${site.project_id}/privacy/jobs`, { headers: await auth() })).json()) as { jobs: { id: string; status: string }[] };
  assert.equal(list.jobs[0].id, job.id);
  assert.equal(list.jobs[0].status, "queued");

  clock = new Date(clock.getTime() + 60 * 60_000);
  await run.svc.scheduler.tick(clock);
  const after = (await (await fetch(`${run.base}/v1/projects/${site.project_id}/privacy/jobs`, { headers: await auth() })).json()) as { jobs: { id: string; status: string; finished_at: string; result: Record<string, number> }[] };
  const done = after.jobs.find((j) => j.id === job.id)!;
  assert.equal(done.status, "done");
  assert.ok(Date.parse(done.finished_at) < Date.parse(job.due_at));
  assert.deepEqual(done.result, { events: 4, overrides: 2, distinct_ids: 3, persons: 1, replays: 0, survey_answers: 0, test_replays: 2 });
  assert.equal(hookCalls.length, 1);
  assert.equal(hookCalls[0].projectId, site.project_id);
  assert.equal(count(pg, `project_id = '${site.project_id}' and distinct_id in ('anon-g', 'gone@x.example', 'crm-g')`), 0);
  assert.equal(pg.psql(`select count(*) from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id in ('anon-g', 'gone@x.example', 'crm-g')`), "0");
  assert.equal(pg.psql(`select count(*) from pa.persons where project_id = '${site.project_id}' and properties ->> 'email' = 'gone@x.example'`), "0");
  assert.equal(pg.psql(`select count(*) from pa.person_overrides where project_id = '${site.project_id}'`), "0");
  // The other person is untouched.
  assert.equal(count(pg, `project_id = '${site.project_id}' and distinct_id = 'stays@x.example'`), 1);
  assert.equal(pg.psql(`select count(*) from pa.persons where project_id = '${site.project_id}' and properties ->> 'email' = 'stays@x.example'`), "1");
});

test("a failing deletion is retried and then marked failed, never silently dropped", { skip: skipNoPg }, async () => {
  const { requestDelete, runDueJobs, MAX_ATTEMPTS } = await import("../../src/gdpr/jobs.ts");
  const { GdprHooks } = await import("../../src/gdpr/hooks.ts");
  const { silentLogger } = await import("../../src/log.ts");
  const broken = new GdprHooks([{ name: "broken", deletePerson: async () => { throw new Error("store offline"); }, exportPerson: async () => [] }]);
  const job = await requestDelete(run.svc.deps.db, site.project_id, "stays@x.example", "test", clock);
  for (let i = 0; i < MAX_ATTEMPTS; i += 1) await runDueJobs(run.svc.deps.db, broken, silentLogger, clock);
  assert.equal(pg.psql(`select status || '|' || attempts || '|' || error from pa.gdpr_jobs where id = '${job.id}'`), `failed|${MAX_ATTEMPTS}|store offline`);
});
