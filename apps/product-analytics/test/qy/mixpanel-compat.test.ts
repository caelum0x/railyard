// Mixpanel ingestion compat (plan task: "Mixpanel ingestion compat with real
// mixpanel-js and mixpanel-python tests"), checked with the real, unmodified
// `mixpanel` npm package (official Node SDK, MIT) against the local service.
// No mock of the client: it is pointed at this service's own host:port with
// `protocol: "http"` and sends real GET requests with a base64 `data=` query
// param, exactly as it would to api.mixpanel.com.
//
// Result (2026-10-15, updated same day to close the $add/$append/$union/
// $remove and /groups gaps), each assertion below is evidence for the row
// it names:
// - mixpanel.track(): lands through the shared PostHog-shaped capture path
//   (event/properties/distinct_id/token are read the same way for both
//   protocols) -- compatible.
// - mixpanel.people.set/set_once/unset/increment/append/union/remove: all
//   land through /engage -- compatible. $set/$set_once/$unset already
//   worked; $add/$append/$union/$remove now apply through the same
//   PropertyOps pipeline (src/persons/properties.ts) that $set uses.
// - mixpanel.groups.set()/union()/remove(): compatible. The real SDK sends
//   $group_key for the group TYPE (e.g. "company") and $group_id for the
//   group KEY (e.g. "acme"); this service's own wire protocol instead uses
//   $group_type/$group_key for the same two things. /groups now
//   disambiguates by the presence of $group_id (a field only the Mixpanel
//   shape ever sends) and applies $set/$set_once/$unset/$add/$append/
//   $union/$remove the same way /engage does.
// - Response body: still NOT compatible by default. A real (non-verbose)
//   Mixpanel client treats the response body as a failure unless it is the
//   literal text "1"; this service always answers PostHog-shaped JSON
//   (`{"status":1}`), so every call below uses `verbose: true` (the SDK's
//   own opt-in to parse JSON and check `result.status`) to see past that
//   and test the ingestion shape on its own. Without `verbose: true`, every
//   real Mixpanel SDK call against this service reports an error today,
//   track and engage included.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
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

/** `key` makes the real SDK attach `api_key` -- the stronger, query-level credential /import requires (the SDK forces HTTPS for its `secret`/service-account auth, which this local http test server can't exercise). */
function importClient(): Mixpanel.Mixpanel {
  const { hostname, port } = new URL(run.base);
  return Mixpanel.init(site.token, { protocol: "http", host: port ? `${hostname}:${port}` : hostname, verbose: true, key: site.token });
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

test("mixpanel.people.increment(): $add now applies through the same ops pipeline as $set", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.people.set("mp-3", { page_views: 1 }, (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.increment("mp-3", "page_views", 1, (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.increment("mp-3", "credits_left", -2, (err) => (err ? reject(err) : resolve())));
  await settle();
  const person = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'mp-3'`);
  assert.equal(pg.psql(`select properties ->> 'page_views' from pa.persons where id = '${person}'`), "2");
  assert.equal(pg.psql(`select properties ->> 'credits_left' from pa.persons where id = '${person}'`), "-2");
});

test("mixpanel.people.append()/union()/remove(): list-valued properties now work through /engage", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.people.append("mp-4", "pages_visited", "home", (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.append("mp-4", "pages_visited", "pricing", (err) => (err ? reject(err) : resolve())));
  await settle();
  const person = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'mp-4'`);
  assert.equal(pg.psql(`select properties -> 'pages_visited' from pa.persons where id = '${person}'`), '["home", "pricing"]');

  await new Promise<void>((resolve, reject) => mp.people.union("mp-4", { browsers: ["chrome", "firefox"] }, (err) => (err ? reject(err) : resolve())));
  await new Promise<void>((resolve, reject) => mp.people.union("mp-4", { browsers: ["firefox", "safari"] }, (err) => (err ? reject(err) : resolve())));
  await settle();
  // de-duplicated: firefox from the second call is not repeated.
  assert.equal(pg.psql(`select properties -> 'browsers' from pa.persons where id = '${person}'`), '["chrome", "firefox", "safari"]');

  await new Promise<void>((resolve, reject) => mp.people.remove("mp-4", { browsers: "chrome" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties -> 'browsers' from pa.persons where id = '${person}'`), '["firefox", "safari"]');
});

test("mixpanel.groups.set()/union()/remove(): /groups now disambiguates $group_key/$group_id from this service's own $group_type/$group_key", { skip: skipNoPg }, async () => {
  const mp = client();
  await new Promise<void>((resolve, reject) => mp.groups.set("company", "acme", { name: "Acme" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties ->> 'name' from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), "Acme");
  assert.equal(pg.psql(`select group_type from pa.group_types where project_id = '${site.project_id}' and group_type_index = (select group_type_index from pa.groups where project_id = '${site.project_id}' and group_key = 'acme')`), "company");

  await new Promise<void>((resolve, reject) => mp.groups.union("company", "acme", { products: ["widgets", "anvils"] }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties -> 'products' from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), '["widgets", "anvils"]');

  await new Promise<void>((resolve, reject) => mp.groups.remove("company", "acme", { products: "widgets" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties -> 'products' from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), '["anvils"]');
});

test("mixpanel.import(): historical events land through /import -- unlike /track, not dropped by the retention window -- and are idempotent by $insert_id", { skip: skipNoPg }, async () => {
  const mp = importClient();
  const old = new Date(Date.now() - 400 * 24 * 3600 * 1000); // older than this site's 365-day retention
  const insertId = randomUUID();
  await new Promise<void>((resolve, reject) =>
    mp.import("legacy_signup", old, { distinct_id: "mp-import-1", $insert_id: insertId, plan: "legacy" }, (err) => (err ? reject(err) : resolve())),
  );
  await settle();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'legacy_signup' and distinct_id = 'mp-import-1'`), 1);
  assert.equal(
    pg.psql(`select date_trunc('day', "timestamp")::date = date_trunc('day', '${old.toISOString()}'::timestamptz)::date from pa.events where project_id = '${site.project_id}' and event = 'legacy_signup'`),
    "t",
  );

  // Re-importing the same event (same $insert_id) is idempotent: still exactly one row.
  await new Promise<void>((resolve, reject) =>
    mp.import("legacy_signup", old, { distinct_id: "mp-import-1", $insert_id: insertId, plan: "legacy" }, (err) => (err ? reject(err) : resolve())),
  );
  await settle();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'legacy_signup' and distinct_id = 'mp-import-1'`), 1);

  // The same age of event through the live /track path is dropped as outside the retention window -- /import exists precisely because /track won't take this.
  const live = client();
  await new Promise<void>((resolve, reject) =>
    live.track("legacy_signup_live", { distinct_id: "mp-import-1", time: Math.floor(old.getTime() / 1000) }, (err) => (err ? reject(err) : resolve())),
  );
  await settle();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'legacy_signup_live'`), 0);
});

test("mixpanel /import: refused without Basic auth or an api_key/token query parameter -- a token inside the event body alone (what /track accepts) is not enough", { skip: skipNoPg }, async () => {
  const { hostname, port } = new URL(run.base);
  const body = { event: "legacy_signup_refused", properties: { token: site.token, distinct_id: "mp-import-2", time: Math.floor(Date.now() / 1000) } };
  const data = Buffer.from(JSON.stringify(body)).toString("base64");
  const res = await fetch(`http://${hostname}:${port}/import?data=${encodeURIComponent(data)}`);
  assert.equal(res.status, 401);
  const resBody = (await res.json()) as { detail: string };
  assert.match(resBody.detail, /Basic authentication|api_key/);
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'legacy_signup_refused'`), 0);
});
