// SQL over events: only one SELECT over pa_view.{events,persons,groups,
// sessions} passes the parser; the database still holds the line when the
// parser is skipped (hostile strings go straight to the runner): other
// schemas, set_config, set role, several statements, writes and the session
// table are all refused, and every view shows one project. 10 s timeout,
// 10 000 rows, CSV download.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { checkSql, SqlRejected } from "../../src/insights/sandbox-parser.ts";
import { SqlSandbox, toCsv } from "../../src/insights/sandbox.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { fixtureDb, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
let box: SqlSandbox;
before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb(120, 30);
  box = new SqlSandbox(f.db);
});

test("the parser accepts one SELECT over the four views, with CTEs, joins and functions", () => {
  for (const ok of [
    "select event, count(*) from pa_view.events group by 1 order by 2 desc",
    "with s as (select * from pa_view.sessions) select count(*) from s",
    "select p.properties ->> 'email', count(*) from pa_view.events e join pa_view.persons p on p.id = e.person_id group by 1",
    "select date_trunc('day', timestamp) as d, count(distinct person_id) from pa_view.events where timestamp > now() - interval '7 days' group by 1;",
    "select g.group_key, g.properties from pa_view.groups g",
  ]) {
    assert.doesNotThrow(() => checkSql(ok), ok);
  }
});

test("the parser refuses writes, several statements, other schemas and system functions with a plain message", () => {
  const bad: [string, RegExp][] = [
    ["delete from pa_view.events", /Only SELECT/],
    ["select 1; select 2", /one statement/],
    ["select * from pa.events", /pa_view/],
    ["select * from pa_view.secret", /pa_view/],
    ["select * from events", /pa_view/],
    ["select * from pa_view.events e, pg_catalog.pg_roles r", /pa_view/],
    ["select pg_sleep(20)", /pg_sleep/],
    ["select set_config('pa.project_id', 'x', true)", /set_config/],
    ["select current_setting('pa.project_id')", /current_setting/],
    ["select * into t from pa_view.events", /INTO/],
    ["select (select count(*) from pa.persons) from pa_view.events", /pa_view/],
    ["select public.f(1)", /schema/],
    ["select * from pa_view.events for update", /lock/],
    ["select", /read/],
    ["x".repeat(21_000), /20 000/],
  ];
  for (const [sql, msg] of bad) {
    assert.throws(() => checkSql(sql), (e: unknown) => e instanceof SqlRejected && msg.test(e.message), sql.slice(0, 60));
  }
});

test("a query returns rows of one project only, with column names", { skip: skipNoPg }, async () => {
  const r = await box.run(f.projectId, "select count(*) as n, count(distinct person_id) as people from pa_view.events");
  const [{ n }] = await f.hand<{ n: string }>("select count(*) as n from pa.events where project_id = $1", [f.projectId]);
  assert.deepEqual(r.columns, ["n", "people"]);
  assert.equal(Number(r.rows[0][0]), Number(n));
  const [{ p }] = await f.hand<{ p: string }>(
    "select count(distinct coalesce(o.new_person_id, e.person_id)) as p from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id where e.project_id = $1",
    [f.projectId],
  );
  assert.equal(Number(r.rows[0][1]), Number(p), "persons are resolved through merges");
  const s = await box.run(f.projectId, "select count(*) from pa_view.sessions");
  const [{ k }] = await f.hand<{ k: string }>("select count(distinct session_id) as k from pa.events where project_id = $1 and session_id is not null", [f.projectId]);
  assert.equal(Number(s.rows[0][0]), Number(k));
  const people = await box.run(f.projectId, "select count(*) from pa_view.persons");
  const [{ m }] = await f.hand<{ m: string }>("select count(*) as m from pa.persons where project_id = $1", [f.projectId]);
  assert.equal(Number(people.rows[0][0]), Number(m));
  const g = await box.run(f.projectId, "select count(*) from pa_view.groups where group_type = 'company'");
  assert.equal(Number(g.rows[0][0]), 50);
});

test("with the parser skipped, the database still refuses everything outside one project's views", { skip: skipNoPg }, async () => {
  const hostile: [string, RegExp][] = [
    ["select count(*) from pa.events", /permission denied/],
    ["select count(*) from pa.sql_sandbox_sessions", /permission denied/],
    ["select set_config('role', 'postgres', true)", /permission denied|must be/],
    ["select 1; delete from pa.events", /multiple commands|cannot insert|syntax error/],
    // Closing the wrapper by hand still meets the one-statement protocol.
    ["select 1) as q; delete from pa.events; select * from (select 1", /multiple commands|cannot insert/],
    ["with d as (delete from pa_view.events returning 1) select count(*) from d", /read-only|cannot|permission|top level/],
    ["select pa_view.project(), set_config('transaction_read_only', 'off', true)", /read-write|cannot set/],
  ];
  for (const [sql, msg] of hostile) {
    await assert.rejects(box.runUnchecked(f.projectId, sql), msg, sql);
  }
  // Changing the project setting inside the query does not move the views.
  const moved = await box.runUnchecked(
    f.projectId,
    `with x as materialized (select set_config('pa.project_id', '${f.otherProjectId}', true) as s) select count(*) from x, pa_view.events`,
  );
  const [{ n }] = await f.hand<{ n: string }>("select count(*) as n from pa.events where project_id = $1", [f.projectId]);
  assert.equal(Number(moved.rows[0][0]), Number(n));
  // The other project sees its own rows with the same query.
  const theirs = await box.runUnchecked(f.otherProjectId, "select count(*) from pa_view.events");
  const [{ o }] = await f.hand<{ o: string }>("select count(*) as o from pa.events where project_id = $1", [f.otherProjectId]);
  assert.equal(Number(theirs.rows[0][0]), Number(o));
  assert.notEqual(o, n);
  // After a query the session row is gone.
  assert.equal((await f.hand("select 1 from pa.sql_sandbox_sessions")).length, 0);
});

test("long queries stop at the timeout; large answers stop at 10 000 rows", { skip: skipNoPg }, async () => {
  await assert.rejects(new SqlSandbox(f.db, { timeoutMs: 1000 }).runUnchecked(f.projectId, "select pg_sleep(5)"), /took longer than/);
  const big = await box.run(f.projectId, "select n from generate_series(1, 20000) as n");
  assert.equal(big.rows.length, 10_000);
  assert.equal(big.truncated, true);
  const small = await box.run(f.projectId, "select n from generate_series(1, 3) as n");
  assert.equal(small.truncated, false);
});

test("CSV download escapes quotes, commas, new lines and formula starts", () => {
  const csv = toCsv({ columns: ["a", "b"], rows: [["x,y", 'say "hi"'], ["=1+1", null], [3, { k: 1 }]], truncated: false });
  assert.equal(csv, 'a,b\r\n"x,y","say ""hi"""\r\n\'=1+1,\r\n3,"{""k"":1}"\r\n');
});
