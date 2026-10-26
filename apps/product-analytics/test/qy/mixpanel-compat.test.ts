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
// - Response body (2026-10-26): now compatible in both modes. The real SDK
//   always sends an explicit `verbose` query param (`0` or `1`, never
//   absent); `src/capture/routes.ts`'s `responseMode()` reads it and
//   answers the literal text "1"/"0" when the caller opted into the plain
//   contract (`verbose=0`, the SDK's own default) and JSON `{status:1}` /
//   `{type,detail}` when it asked for `verbose=1` -- the same JSON this
//   service's own PostHog-shaped SDKs still get unconditionally, since they
//   never send a `verbose` param at all. Most calls above still pass
//   `verbose: true` because that is what lets a thrown SDK error carry
//   Mixpanel's own `result.error` text; the dedicated tests at the bottom
//   of this file cover the real SDK with its real default
//   (`verbose: false`) end to end, plus the raw plain-text wire shape.
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

/** The real SDK's actual default: `verbose` left unset, which the SDK itself turns into an explicit `verbose=0` on the wire. A call here only succeeds if the response body is the literal text "1" -- this is the exact contract a team's existing Mixpanel code (that never set `verbose: true`) relies on. */
function plainClient(): Mixpanel.Mixpanel {
  const { hostname, port } = new URL(run.base);
  return Mixpanel.init(site.token, { protocol: "http", host: port ? `${hostname}:${port}` : hostname });
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

// The plain (non-verbose) response contract (2026-10-26): a real Mixpanel
// SDK that never opts into `verbose: true` -- i.e. every SDK's own default
// -- still sends an explicit `verbose=0` on the wire and treats anything
// but the literal text "1" as a thrown error. Below, `plainClient()` is the
// SDK exactly as a team would already have it configured; a callback error
// here means this service is not actually usable by that team's existing
// code. This is the exact gap `docs/MIXPANEL-PARITY-CHECKLIST.md` flagged
// as MISSING before `src/capture/routes.ts`'s `responseMode()`/`okResponse()`.

test("mixpanel.track() with the SDK's real default (verbose unset -> verbose=0 on the wire): no error, plain '1' body", { skip: skipNoPg }, async () => {
  const mp = plainClient();
  await new Promise<void>((resolve, reject) => mp.track("plain_signed_up", { distinct_id: "mp-plain-1", plan: "pro" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'plain_signed_up' and distinct_id = 'mp-plain-1'`), 1);
});

test("mixpanel.people.set() with the SDK's real default: no error, plain '1' body", { skip: skipNoPg }, async () => {
  const mp = plainClient();
  await new Promise<void>((resolve, reject) => mp.people.set("mp-plain-2", { plan: "pro" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  const person = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'mp-plain-2'`);
  assert.equal(pg.psql(`select properties ->> 'plan' from pa.persons where id = '${person}'`), "pro");
});

test("mixpanel.groups.set() with the SDK's real default: no error, plain '1' body", { skip: skipNoPg }, async () => {
  const mp = plainClient();
  await new Promise<void>((resolve, reject) => mp.groups.set("company", "plain-acme", { name: "Plain Acme" }, (err) => (err ? reject(err) : resolve())));
  await settle();
  assert.equal(pg.psql(`select properties ->> 'name' from pa.groups where project_id = '${site.project_id}' and group_key = 'plain-acme'`), "Plain Acme");
});

test("the raw wire contract: verbose=0 answers the literal text \"1\" on success and \"0\" on a refused token, never JSON", { skip: skipNoPg }, async () => {
  const { hostname, port } = new URL(run.base);
  const okBody = { event: "plain_wire_ok", properties: { token: site.token, distinct_id: "mp-plain-3" } };
  const okData = Buffer.from(JSON.stringify(okBody)).toString("base64");
  const okRes = await fetch(`http://${hostname}:${port}/track?verbose=0&data=${encodeURIComponent(okData)}`);
  assert.equal(okRes.status, 200);
  assert.equal(await okRes.text(), "1");
  assert.match(okRes.headers.get("content-type") ?? "", /text\/plain/);

  const badBody = { event: "plain_wire_bad", properties: { token: "site_does_not_exist00", distinct_id: "mp-plain-4" } };
  const badData = Buffer.from(JSON.stringify(badBody)).toString("base64");
  const badRes = await fetch(`http://${hostname}:${port}/track?verbose=0&data=${encodeURIComponent(badData)}`);
  assert.equal(badRes.status, 401);
  assert.equal(await badRes.text(), "0");
});

test("the raw wire contract: verbose=1 still answers JSON with a status field, same as before this change", { skip: skipNoPg }, async () => {
  const { hostname, port } = new URL(run.base);
  const body = { event: "verbose_wire_ok", properties: { token: site.token, distinct_id: "mp-plain-5" } };
  const data = Buffer.from(JSON.stringify(body)).toString("base64");
  const res = await fetch(`http://${hostname}:${port}/track?verbose=1&data=${encodeURIComponent(data)}`);
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { status: 1 });
});

test("a caller that never mentions verbose at all (this service's own PostHog-shaped SDKs) keeps today's unconditional JSON body -- the plain contract is opt-in, not a breaking change", { skip: skipNoPg }, async () => {
  const { hostname, port } = new URL(run.base);
  const body = { event: "no_verbose_param", properties: { token: site.token, distinct_id: "mp-plain-6" } };
  const data = Buffer.from(JSON.stringify(body)).toString("base64");
  const res = await fetch(`http://${hostname}:${port}/track?data=${encodeURIComponent(data)}`);
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { status: 1 });
});
