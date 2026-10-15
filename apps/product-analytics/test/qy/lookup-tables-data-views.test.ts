// Mixpanel parity checklist rows 4 and 5: lookup tables (a CSV-backed
// key -> columns table joined into properties at query time) and data views
// (a named, reusable set of filters merged into a query by id). Both reuse
// existing infra rather than a new query engine:
//   - lookup tables join through filters.ts's Joins (the same join-then-
//     compare path "group" filters already use) and compareProperty (the
//     same comparison code $set/$add/... never needed to duplicate);
//   - data views are resolved to a plain `properties` filter list in
//     runner.ts before any builder (trends.ts, funnels.ts, ...) sees the
//     query, so they are invisible to -- and require no changes in --
//     the query layer the Mixpanel checklist already marks DONE.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createLookupTable, deleteLookupTable, replaceLookupTableCsv } from "../../src/insights/lookup-tables.ts";
import { runInsight, type TrendsResult } from "../../src/insights/runner.ts";
import { createDataView, deleteDataView } from "../../src/insights/store.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { fixtureDb, NOW, type FixtureDb } from "../a2/setup.ts";

let f: FixtureDb;
const EVENT = "qy_lookup_demo";

before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb(20, 10);
  const [{ id: personId }] = await f.hand<{ id: string }>("select id from pa.persons where project_id = $1 limit 1", [f.projectId]);
  const at = (hoursAgo: number) => new Date(NOW.getTime() - hoursAgo * 3600_000).toISOString();
  const rows = [
    { country: "US", amount: 5 },
    { country: "US", amount: 7 },
    { country: "DE", amount: 50 },
    { country: "DE", amount: 60 },
    { country: "FR", amount: 9 },
    { country: "FR", amount: 2 },
  ];
  for (const [i, r] of rows.entries()) {
    await f.hand(`insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp", properties) values ($1, $2, $3, 'qy-lookup', $4, $5, $6)`, [
      randomUUID(),
      f.projectId,
      EVENT,
      personId,
      at(i + 1),
      JSON.stringify({ country_code: r.country, amount: r.amount }),
    ]);
  }
});

const run = async (q: Record<string, unknown>) =>
  (await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: { date_from: "-3d" }, series: [{ event: EVENT }], ...q }), NOW)) as TrendsResult;

test("lookup tables: a CSV-backed table joins into a filter by an event property, through the same join-then-compare path group filters already use", { skip: skipNoPg }, async () => {
  const lt = await createLookupTable(f.db, f.projectId, { name: "Countries", csv: "country_code,region,is_eu\nUS,NA,false\nDE,EU,true\nFR,EU,true\n" });
  assert.equal(lt.key_column, "country_code");
  assert.deepEqual(lt.columns, ["region", "is_eu"]);
  assert.equal(lt.row_count, 3);

  const euOnly = await run({
    properties: [{ type: "lookup", lookup_table_id: lt.id, source: { type: "event", key: "country_code" }, column: "is_eu", operator: "exact", value: "true" }],
  });
  assert.equal(euOnly.series[0].total, 4); // DE x2 + FR x2

  const naOnly = await run({
    properties: [{ type: "lookup", lookup_table_id: lt.id, source: { type: "event", key: "country_code" }, column: "region", operator: "exact", value: "NA" }],
  });
  assert.equal(naOnly.series[0].total, 2); // US x2

  // Re-uploading the CSV replaces the rows (and can change the columns).
  await replaceLookupTableCsv(f.db, f.projectId, lt.id, { csv: "country_code,region\nUS,NA\nDE,EU\nFR,EU\n" });
  const afterReplace = await run({
    properties: [{ type: "lookup", lookup_table_id: lt.id, source: { type: "event", key: "country_code" }, column: "region", operator: "exact", value: "EU" }],
  });
  assert.equal(afterReplace.series[0].total, 4); // DE x2 + FR x2, same rows the is_eu filter matched above

  await deleteLookupTable(f.db, f.projectId, lt.id);
});

test("data views: a named, reusable set of filters is merged into a query by id -- AND'd with the query's own filters, never replacing them", { skip: skipNoPg }, async () => {
  const dv = await createDataView(f.db, f.projectId, { name: "EU purchases", filters: [{ type: "event", key: "country_code", operator: "exact", value: ["DE", "FR"] }] });
  const viaView = await run({ data_view_id: dv.id });
  assert.equal(viaView.series[0].total, 4); // DE x2 + FR x2

  const combined = await run({ data_view_id: dv.id, properties: [{ type: "event", key: "amount", operator: "gt", value: 20 }] });
  assert.equal(combined.series[0].total, 2); // DE's two events (50, 60); FR's (9, 2) don't clear 20

  await assert.rejects(run({ data_view_id: randomUUID() }), /data view/);
  await deleteDataView(f.db, f.projectId, dv.id);
});
