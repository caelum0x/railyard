// Test harness: a scratch Postgres cluster from initdb (never a hosted
// database), the migrations, and the service on a random local port.

import { after } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import type { AddressInfo } from "node:net";
import { serve } from "@hono/node-server";
import { migrate } from "../../scripts/migrate.ts";
import { createService, type Service, type ServiceOptions } from "../../src/app.ts";
import { silentLogger } from "../../src/log.ts";
import { MemorySiteDirectory, type Site } from "../../src/tenancy/site-directory.ts";

export const havePg = spawnSync("pg_ctl", ["--version"]).status === 0 && spawnSync("initdb", ["--version"]).status === 0;
export const skipNoPg = havePg ? false : "pg_ctl/initdb not installed";

export interface ScratchPg {
  url: string;
  dir: string;
  psql(sql: string): string;
  /** Several statements in one session (each its own -c); returns all output. */
  session(...stmts: string[]): string;
  stop(): void;
}

export function scratchPg(): ScratchPg {
  const dir = mkdtempSync(join(tmpdir(), "pa-pg-"));
  const port = String(40000 + Math.floor(Math.random() * 9000));
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
  execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses='' -c fsync=off -c timezone=UTC`, "-w", "start"], { stdio: "ignore" });
  const psql = (sql: string) => execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", "-c", sql], { encoding: "utf8" }).trim();
  const session = (...stmts: string[]) =>
    execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", ...stmts.flatMap((s) => ["-c", s])], { encoding: "utf8" }).trim();
  psql("create role anon nologin; create role authenticated nologin;");
  let stopped = false;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "immediate", "stop"], { stdio: "ignore" });
    rmSync(dir, { recursive: true, force: true });
  };
  return { url: `postgresql://postgres@localhost/postgres?host=${encodeURIComponent(dir)}&port=${port}`, dir, psql, session, stop };
}

/** A migrated scratch cluster, stopped when the calling file's tests end. */
export async function migratedPg(): Promise<ScratchPg> {
  const pg = scratchPg();
  cleanups.push(async () => pg.stop());
  await migrate(pg.url);
  return pg;
}

const cleanups: (() => Promise<void>)[] = [];
after(async () => {
  for (const c of cleanups.reverse()) await c();
});

export function makeSite(over: Partial<Site> = {}): Site {
  return {
    project_id: randomUUID(),
    org_id: randomUUID(),
    token: `site_${randomUUID().replace(/-/g, "").slice(0, 20)}`,
    name: "shop",
    plan: "railyard",
    events_limit: 100_000,
    paused: false,
    retention_days: 365,
    scribase_org_id: null,
    ...over,
  };
}

export interface Running {
  base: string;
  svc: Service;
  sites: MemorySiteDirectory;
  close(): Promise<void>;
}

export async function startService(pg: ScratchPg, sites: Site[], opts: Partial<ServiceOptions> = {}): Promise<Running> {
  const dir = new MemorySiteDirectory(sites);
  const svc = createService({ databaseUrl: pg.url, sites: dir, log: silentLogger, ...opts });
  const server = serve({ fetch: svc.app.fetch, port: 0, hostname: "127.0.0.1" });
  await new Promise((r) => server.once("listening", r));
  const port = (server.address() as AddressInfo).port;
  let closed = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    await new Promise((r) => server.close(r));
    await svc.close();
  };
  cleanups.push(close);
  return { base: `http://127.0.0.1:${port}`, svc, sites: dir, close };
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Flushes the insert buffer and returns a count from psql. */
export async function flushed(r: Running): Promise<void> {
  await r.svc.deps.writer.flush();
}

export const count = (pg: ScratchPg, where: string) => Number(pg.psql(`select count(*) from pa.events where ${where}`));
