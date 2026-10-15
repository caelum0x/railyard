// Mixpanel ingestion compat (plan task: "Mixpanel ingestion compat with real
// mixpanel-js and mixpanel-python tests"), checked with the real, unmodified
// `mixpanel` npm package (official Node SDK, MIT) against the local service.
// No mock of the client: it is pointed at this service's own host:port with
// `protocol: "http"` and sends real GET requests with a base64 `data=` query
// param, exactly as it would to api.mixpanel.com.
//
// Result (2026-10-15), each assertion below is evidence for the row it names:
// - mixpanel.track(): lands through the shared PostHog-shaped capture path
//   (event/properties/distinct_id/token are read the same way for both
//   protocols) -- compatible.
// - mixpanel.people.set/set_once/unset: land through /engage -- compatible,
//   because this service's /engage already reads $distinct_id, $token,
//   $set, $set_once and $unset, which is exactly what the real SDK sends.
// - mixpanel.people.increment/append/union/remove: NOT compatible. The real
//   SDK sends $add, $append, $union and $remove; this service's /engage
//   handler only implements $set, $set_once and $unset, so these calls are
//   accepted (200) but silently produce no property change.
// - mixpanel.groups.set(): NOT compatible. The real SDK sends $group_key for
//   the group TYPE (e.g. "company") and $group_id for the group KEY (e.g.
//   "acme"); this service's /groups handler reads $group_type for the type
//   and $group_key for the key -- the opposite field for $group_key, and a
//   field ($group_id) it never reads at all. A real mixpanel.groups.set()
//   call is refused with 400 ("group_type is required"), not silently
//   dropped, so a caller finds out immediately.
// - Response body: also NOT compatible by default. A real (non-verbose)
//   Mixpanel client treats the response body as a failure unless it is the
//   literal text "1"; this service always answers PostHog-shaped JSON
//   (`{"status":1}`), so every call below uses `verbose: true` (the SDK's
//   own opt-in to parse JSON and check `result.status`) to see past that
//   and test the ingestion shape on its own. Without `verbose: true`, every
//   real Mixpanel SDK call against this service reports an error today,
//   track and engage included.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import Mixpanel from "mixpanel";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";

let pg: ScratchPg;
let run: Running;
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
});

function client(): Mixpanel.Mixpanel {
  const { hostname, port } = new URL(run.base);
  return Mixpanel.init(site.token, { protocol: "http", host: port ? `${hostname}:${port}` : hostname, verbose: true });
}

/** mixpanel-node calls are fire-and-forget GETs; wait for the service to flush its insert buffer. */
async function settle(): Promise<void> {
  await new Promise((r) => setTimeout(r, 50));
  await run.svc.deps.writer.flush();
}

test("mixpanel.track(): the real Node SDK's event lands through /track like a capture call", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.track("signed_up", { distinct_id: "mp-1", plan: "pro" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(
    count(pg, `project_id = '${site.project_id}' and event = 'signed_up' and distinct_id = 'mp-1' and properties ->> 'plan' = 'pro' and properties ->> 'mp_lib' = 'node'`),
    1,
  );
});

test("mixpanel.people.set/set_once/unset: the real Node SDK's profile calls land through /engage", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.people.set("mp-2", { plan: "pro" }, (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.set_once("mp-2", { first_plan: "free" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  const person = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'mp-2'`);
  assert.ok(person, "a person row exists for the engaged distinct_id");
  assert.equal(pg.psql(`select (properties ->> 'plan') || '|' || (properties ->> 'first_plan') from pa.persons where id = '${person}'`), "pro|free");

  await new Promise<void>((resolve, reject) => mp.people.unset("mp-2", "plan", (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties ->> 'plan' from pa.persons where id = '${person}'`), "");
});

test("mixpanel.people.increment(): accepted but not applied -- $add is not one of /engage's three supported ops", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.people.set("mp-3", { page_views: 1 }, (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.increment("mp-3", "page_views", 1, (err) => (err ? reject(err) : resolve())));
  await settle();
  const person = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'mp-3'`);
  // Still 1, not 2: the $add op the real SDK sent was accepted (200) and ignored.
  assert.equal(pg.psql(`select properties ->> 'page_views' from pa.persons where id = '${person}'`), "1");
});

test("mixpanel.groups.set(): refused -- $group_key/$group_id are not this service's $group_type/$group_key", { skip: skipNoPg }, async () => {
  const { hostname, port } = new URL(run.base);
  // mixpanel-node has no error channel for a non-2xx GET beyond the parsed
  // body ("1" on success); call the service directly to see the real status.
  const payload = { $token: site.token, $group_key: "company", $group_id: "acme", $set: { name: "Acme" } };
  const data = Buffer.from(JSON.stringify(payload)).toString("base64");
  const res = await fetch(`http://${hostname}:${port}/groups?data=${encodeURIComponent(data)}`);
  assert.equal(res.status, 400);
  const body = (await res.json()) as { detail: string };
  assert.match(body.detail, /group_type is required/);
  assert.equal(pg.psql(`select count(*) from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), "0");
});
