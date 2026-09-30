// Runs customer SQL. Three guards, each enough on its own for isolation:
//   1. checkSql (sandbox-parser.ts): one SELECT over the pa_view views.
//   2. A separate connection that logs in as pa_sql_login, which can read
//      only the pa_view views: no pa schema, no role to switch to, a
//      read-only transaction and a 10 s statement timeout.
//   3. The views show only the project registered for this backend's pid in
//      pa.sql_sandbox_sessions, which the sandbox role cannot touch; the
//      owner writes that row right before the query and removes it after.
// Answers stop at 10 000 rows.

import { randomBytes } from "node:crypto";
import pg from "pg";
import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { checkSql, SqlRejected } from "./sandbox-parser.ts";

export const SQL_TIMEOUT_MS = 10_000;
export const SQL_MAX_ROWS = 10_000;
export const SANDBOX_ROLE = "pa_sql_login";

export interface SqlResult {
  columns: string[];
  rows: unknown[][];
  truncated: boolean;
}

export interface SandboxOptions {
  timeoutMs?: number;
  maxRows?: number;
}

export class SqlUnavailable extends Error {
  status = 503;
}

function jsonSafe(v: unknown): unknown {
  if (v === null || v === undefined) return null;
  if (v instanceof Date) return v.toISOString();
  if (typeof v === "bigint") return v.toString();
  if (Buffer.isBuffer(v)) return `\\x${v.toString("hex")}`;
  return v;
}

/** Postgres errors a person can act on, in plain words; anything else is generic. */
function plain(err: unknown, timeoutMs: number): Error {
  const e = err as { code?: string; message?: string };
  if (e.code === "57014") return new SqlRejected(`The query took longer than ${Math.round(timeoutMs / 1000)} seconds and was stopped. Narrow the time range or add a limit.`);
  if (e.code === "42501") return new SqlRejected(`Not allowed: ${e.message ?? "permission denied"}.`);
  if (e.code === "25006") return new SqlRejected("Only reading is allowed here: cannot write in a read-only transaction.");
  if (e.code && /^(42|22|21|0A|25|54)/.test(e.code)) return new SqlRejected(e.message ?? "The query could not run.");
  if (e.code === "42601" || /multiple commands|cannot insert/.test(e.message ?? "")) return new SqlRejected("Run one statement at a time: cannot insert multiple commands.");
  return err instanceof Error ? err : new Error(String(err));
}

export class SqlSandbox {
  owner: Db;
  opts: Required<SandboxOptions>;
  pool: pg.Pool | null = null;
  starting: Promise<pg.Pool> | null = null;

  constructor(owner: Db, opts: SandboxOptions = {}) {
    this.owner = owner;
    this.opts = { timeoutMs: opts.timeoutMs ?? SQL_TIMEOUT_MS, maxRows: opts.maxRows ?? SQL_MAX_ROWS };
  }

  /** Gives the sandbox login a fresh random password (as the owner) and opens its pool. */
  private async open(): Promise<pg.Pool> {
    if (this.pool) return this.pool;
    if (!this.starting) {
      this.starting = (async () => {
        const password = randomBytes(24).toString("base64url");
        try {
          await this.owner.query("select 1 from pg_roles where rolname = $1", [SANDBOX_ROLE]).then((r) => {
            if (!r.length) throw new Error("the SQL role is missing; run the migrations");
          });
          const [{ stmt }] = await this.owner.query<{ stmt: string }>("select format('alter role %I with login password %L', $1::text, $2::text) as stmt", [SANDBOX_ROLE, password]);
          await this.owner.query(stmt);
        } catch (err) {
          this.starting = null;
          throw new SqlUnavailable(`SQL is not available on this service yet (${(err as Error).message}).`);
        }
        const base = String((this.owner.pool as unknown as { options: { connectionString?: string } }).options.connectionString ?? "");
        const url = new URL(base);
        url.username = SANDBOX_ROLE;
        url.password = password;
        this.pool = new pg.Pool({ connectionString: url.toString(), max: 2, idleTimeoutMillis: 10_000, allowExitOnIdle: true, application_name: "product-analytics-sql" });
        this.pool.on("error", () => undefined);
        return this.pool;
      })();
    }
    return this.starting;
  }

  /** Checks the text with the parser, then runs it. */
  async run(projectId: string, sql: string): Promise<SqlResult> {
    return this.runUnchecked(projectId, checkSql(sql));
  }

  /** Runs text without the parser (tests prove the database guards hold alone). */
  async runUnchecked(projectId: string, sql: string): Promise<SqlResult> {
    if (!isUuid(projectId)) throw new SqlRejected("Unknown project.");
    const pool = await this.open();
    const client = await pool.connect();
    let pid = 0;
    let broken = false;
    try {
      pid = Number((await client.query<{ pid: number }>("select pg_backend_pid() as pid")).rows[0].pid);
      await this.owner.query(
        "insert into pa.sql_sandbox_sessions (pid, project_id) values ($1, $2) on conflict (pid) do update set project_id = excluded.project_id, started_at = now()",
        [pid, projectId],
      );
      await client.query("begin transaction read only");
      await client.query(`set local statement_timeout = ${Math.max(100, Math.floor(this.opts.timeoutMs))}`);
      // A bound parameter forces the extended protocol: one statement only.
      const res = await withDeadline(
        client.query({ text: `select * from (${sql}\n) as q limit $1`, values: [this.opts.maxRows + 1], rowMode: "array" }),
        this.opts.timeoutMs + 2000,
        () => {
          broken = true;
        },
      );
      await client.query("rollback");
      const rows = (res.rows as unknown[][]).map((r) => r.map(jsonSafe));
      return { columns: res.fields.map((x) => x.name), rows: rows.slice(0, this.opts.maxRows), truncated: rows.length > this.opts.maxRows };
    } catch (err) {
      await client.query("rollback").catch(() => {
        broken = true;
      });
      throw plain(err, this.opts.timeoutMs);
    } finally {
      client.release(broken);
      if (pid) await this.owner.query("delete from pa.sql_sandbox_sessions where pid = $1", [pid]).catch(() => undefined);
    }
  }

  async close(): Promise<void> {
    if (this.pool) await this.pool.end();
    this.pool = null;
    this.starting = null;
  }
}

async function withDeadline<T>(p: Promise<T>, ms: number, onLate: () => void): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      p,
      new Promise<T>((_, reject) => {
        timer = setTimeout(() => {
          onLate();
          reject(Object.assign(new Error("timeout"), { code: "57014" }));
        }, ms);
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

const FORMULA_START = /^[=+\-@\t\r]/;

function cell(v: unknown): string {
  if (v === null || v === undefined) return "";
  let s = typeof v === "object" ? JSON.stringify(v) : String(v);
  // Spreadsheet formula injection: a leading = + - @ is shown as text.
  if (typeof v === "string" && FORMULA_START.test(s)) s = `'${s}`;
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function toCsv(r: SqlResult): string {
  return [r.columns, ...r.rows].map((row) => row.map(cell).join(",")).join("\r\n") + "\r\n";
}
