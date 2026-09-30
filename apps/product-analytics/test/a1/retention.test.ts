// Retention on a moved clock: whole monthly partitions older than the
// longest plan are dropped; a shorter plan (or setting) is trimmed per
// project; nothing inside a project's window is removed. Receipts expire.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { dropExpiredPartitions, ensurePartitions, purgeReceipts, runRetention, trimProject } from "../../src/jobs/retention.ts";
import { LONGEST_RETENTION_DAYS } from "../../src/plans.ts";
import { putSettings, DEFAULT_SETTINGS } from "../../src/tenancy/settings.ts";
import { MemorySiteDirectory } from "../../src/tenancy/site-directory.ts";
import { Db } from "../../src/db.ts";
import { makeSite, migratedPg, skipNoPg } from "./helpers.ts";

const DAY = 86_400_000;

test("partitions and per-project trims on a moved clock", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const db = new Db(pg.url);
  try {
    const long = makeSite({ plan: "railyard", retention_days: 365 });
    const short = makeSite({ plan: "trial", retention_days: 30 });
    const custom = makeSite({ plan: "railyard", retention_days: 365 });
    const sites = new MemorySiteDirectory([long, short, custom]);
    await putSettings(db, custom, { ...DEFAULT_SETTINGS, retention_days: 90 });
    const now = new Date("2027-09-15T03:10:00Z");
    const ages = [1, 29, 31, 89, 91, 200, 364, 366, 400, 430];
    for (const s of [long, short, custom]) {
      for (const d of ages) {
        const ts = new Date(now.getTime() - d * DAY).toISOString();
        pg.psql(`select pa.ensure_month('${ts}'::date); insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp") values ('${randomUUID()}', '${s.project_id}', 'age_${d}', 'd', gen_random_uuid(), '${ts}')`);
      }
    }
    const ages_of = (p: string) => pg.psql(`select string_agg(replace(event, 'age_', ''), ',' order by (replace(event, 'age_', ''))::int) from pa.events where project_id = '${p}'`);
    const out = await runRetention(db, sites, now);
    // Partition drop: only whole months that end before now - 365 days (2026-09-15).
    assert.deepEqual(out.dropped.sort(), ["events_y2026m07", "events_y2026m08"]);
    assert.equal(ages_of(long.project_id), "1,29,31,89,91,200,364");
    assert.equal(ages_of(short.project_id), "1,29");
    assert.equal(ages_of(custom.project_id), "1,29,31,89");
    assert.deepEqual(out.trimmed, { [long.project_id]: 1, [short.project_id]: 6, [custom.project_id]: 4 });
    // The next night nothing inside the window moves (364 days became 365: still inside).
    const nextNight = await runRetention(db, sites, new Date(now.getTime() + DAY));
    assert.deepEqual(nextNight.trimmed, {});
    assert.equal(ages_of(long.project_id), "1,29,31,89,91,200,364");
    // Two nights later they are outside and go.
    const next = await runRetention(db, sites, new Date(now.getTime() + 2 * DAY));
    assert.deepEqual(next.dropped, []);
    assert.equal(ages_of(long.project_id), "1,29,31,89,91,200", "364 days became 366");
    assert.equal(ages_of(short.project_id), "1", "29 days became 31");
    // The partitions for this month and the next two exist.
    const made = await ensurePartitions(db, now);
    assert.deepEqual(made, ["events_y2027m09", "events_y2027m10", "events_y2027m11"]);
    assert.ok(LONGEST_RETENTION_DAYS >= 365);
    assert.deepEqual(await dropExpiredPartitions(db, now), []);
    assert.equal(await trimProject(db, long.project_id, 365, now), 0);
  } finally {
    await db.close();
  }
});

test("receipts older than the retry window are purged; recent ones stay", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const db = new Db(pg.url);
  try {
    const p = randomUUID();
    pg.psql(`insert into pa.ingest_receipts (project_id, uuid, received_at) values ('${p}', gen_random_uuid(), '2026-08-01'), ('${p}', gen_random_uuid(), '2026-09-29')`);
    assert.equal(await purgeReceipts(db, new Date("2026-09-30T00:00:00Z")), 1);
    assert.equal(pg.psql("select count(*) from pa.ingest_receipts"), "1");
  } finally {
    await db.close();
  }
});
