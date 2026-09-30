// A migrated scratch cluster with the shared fixture loaded, a service Db
// (queries run as the request role) and an owner client for hand SQL.

import { after } from "node:test";
import { randomUUID } from "node:crypto";
import pg from "pg";
import { Db } from "../../src/db.ts";
import { migratedPg, type ScratchPg } from "../a1/helpers.ts";
import { loadFixture, type FixtureStats } from "./fixtures.ts";

// Insight jobs are driven by hand in tests.
process.env.PA_INSIGHTS_JOBS = "off";

const closers: (() => Promise<void>)[] = [];
// Registered at import time, before the scratch cluster's own cleanup, so
// the pools close while the server is still up.
after(async () => {
  for (const c of closers.splice(0)) await c();
});

export const NOW = new Date("2026-09-30T00:00:00Z");

export interface FixtureDb {
  pg: ScratchPg;
  db: Db;
  owner: pg.Client;
  projectId: string;
  otherProjectId: string;
  stats: FixtureStats;
  /** Hand SQL as the owner (no row level security): the reference answers. */
  hand<R extends Record<string, unknown> = Record<string, unknown>>(sql: string, params?: unknown[]): Promise<R[]>;
}

export async function fixtureDb(persons = 400, days = 60): Promise<FixtureDb> {
  const scratch = await migratedPg();
  const owner = new pg.Client({ connectionString: scratch.url });
  await owner.connect();
  const projectId = randomUUID();
  const otherProjectId = randomUUID();
  const stats = await loadFixture(owner, { projectId, persons, days, end: NOW, seed: 7 });
  // A second project with the same shape, which must never leak into the first.
  await loadFixture(owner, { projectId: otherProjectId, persons: Math.max(20, Math.floor(persons / 4)), days, end: NOW, seed: 99 });
  const db = new Db(scratch.url, 3);
  closers.push(async () => {
    await db.close();
    await owner.end();
  });
  return {
    pg: scratch,
    db,
    owner,
    projectId,
    otherProjectId,
    stats,
    hand: async (sql, params = []) => (await owner.query(sql, params)).rows,
  };
}

export const round = (n: number, d = 6) => Math.round(n * 10 ** d) / 10 ** d;
