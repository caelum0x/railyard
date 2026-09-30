// Request-path queries (asProject) stop at a default statement timeout, so a
// slow console read cannot hold one of the service's few pooled connections
// (shared with capture) for minutes. Code that needs longer sets its own.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { Db, PROJECT_STATEMENT_TIMEOUT_MS } from "../../src/db.ts";
import { migratedPg, skipNoPg } from "./helpers.ts";

test("asProject queries have a default statement timeout; asOwner jobs and explicit settings are not cut", { skip: skipNoPg }, async () => {
  assert.ok(PROJECT_STATEMENT_TIMEOUT_MS >= 30_000 && PROJECT_STATEMENT_TIMEOUT_MS <= 120_000);
  const pg = await migratedPg();
  const db = new Db(pg.url, 2, { projectTimeoutMs: 300 });
  try {
    await assert.rejects(db.asProject(randomUUID(), (c) => c.query("select pg_sleep(1)")), (e: unknown) => (e as { code?: string }).code === "57014");
    await db.asProject(randomUUID(), async (c) => {
      await c.query("set local statement_timeout = 5000");
      await c.query("select pg_sleep(0.6)");
    });
    await db.asOwner((c) => c.query("select pg_sleep(0.6)"));
  } finally {
    await db.close();
  }
});
