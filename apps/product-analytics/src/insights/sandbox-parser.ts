// The SQL allowlist, first of three guards (then the database role, then
// the per-backend project views; see db/migrations/0011_sql_sandbox.sql).
// It parses with the vendored Postgres build of node-sql-parser 5.4.0
// (Apache-2.0, vendor/node-sql-parser, unmodified; its one dependency
// big-integer is vendored beside it) and accepts exactly one SELECT whose
// tables are pa_view.{events,persons,groups,sessions} or its own CTEs, with
// no INTO, no row locks, no schema-qualified or system functions.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export class SqlRejected extends Error {
  status = 400;
}

export const MAX_SQL_CHARS = 20_000;
export const VIEWS = ["events", "persons", "groups", "sessions"] as const;

interface ParserLike {
  astify(sql: string, opt: { database: string }): unknown;
  tableList(sql: string, opt: { database: string }): string[];
}

let parser: ParserLike | null = null;

function load(): ParserLike {
  if (parser) return parser;
  const vendor = join(dirname(fileURLToPath(import.meta.url)), "vendor");
  const source = readFileSync(join(vendor, "node-sql-parser", "build", "postgresql.js"), "utf8");
  const bigInteger = createRequire(join(vendor, "big-integer", "package.json"))("./BigInteger.js");
  const mod: { exports: Record<string, unknown> } = { exports: {} };
  const requireShim = (name: string) => {
    if (name === "big-integer") return bigInteger;
    throw new Error(`the vendored parser asked for ${name}`);
  };
  // The file is a CommonJS bundle; it runs with its own exports and a require limited to its one dependency.
  new Function("exports", "require", "module", source)(mod.exports, requireShim, mod);
  const Parser = mod.exports.Parser as new () => ParserLike;
  parser = new Parser();
  return parser;
}

/** Function names that reach outside the views, change settings or wait. Also any name starting pg_. */
const DENIED_FUNCTIONS = new Set([
  "set_config",
  "current_setting",
  "dblink",
  "dblink_exec",
  "query_to_xml",
  "query_to_xml_and_xmlschema",
  "table_to_xml",
  "cursor_to_xml",
  "database_to_xml",
  "schema_to_xml",
  "txid_current",
  "lo_import",
  "lo_export",
  "lo_get",
  "lo_open",
  "has_table_privilege",
  "has_schema_privilege",
  "has_database_privilege",
  "has_function_privilege",
  "inet_server_addr",
  "inet_server_port",
  "inet_client_addr",
  // Build one value of up to 1 GB inside the database from a short query.
  "repeat",
  "lpad",
  "rpad",
]);

type Node = Record<string, unknown>;
const isNode = (v: unknown): v is Node => typeof v === "object" && v !== null;

function functionName(n: Node): { name: string; schema: string | null } | null {
  if (n.type === "aggr_func" && typeof n.name === "string") return { name: n.name.toLowerCase(), schema: null };
  if (n.type !== "function" || !isNode(n.name)) return null;
  const parts = (n.name as Node).name;
  const schema = (n.name as Node).schema;
  const name = Array.isArray(parts) ? parts.map((p) => String((p as Node).value ?? "")).join(".") : String(n.name);
  return { name: name.toLowerCase(), schema: isNode(schema) ? String(schema.value ?? "").toLowerCase() : null };
}

function walk(v: unknown, visit: (n: Node) => void): void {
  if (Array.isArray(v)) {
    for (const x of v) walk(x, visit);
    return;
  }
  if (!isNode(v)) return;
  visit(v);
  for (const k of Object.keys(v)) walk(v[k], visit);
}

function cteNames(ast: unknown): Set<string> {
  const out = new Set<string>();
  walk(ast, (n) => {
    if (Array.isArray(n.with)) for (const w of n.with) if (isNode(w) && isNode(w.name)) out.add(String(w.name.value).toLowerCase());
  });
  return out;
}

/** Throws SqlRejected with a plain message unless the text is one allowed SELECT. Returns it without a trailing semicolon. */
export function checkSql(input: string): string {
  if (typeof input !== "string" || input.trim().length === 0) throw new SqlRejected("Write a SELECT query.");
  if (input.length > MAX_SQL_CHARS) throw new SqlRejected("Keep the query under 20 000 characters.");
  const sql = input.trim().replace(/;\s*$/, "");
  if (/\bfor\s+(update|share|no\s+key\s+update|key\s+share)\b/i.test(sql)) throw new SqlRejected("Queries cannot lock rows. Remove FOR UPDATE or FOR SHARE.");
  const p = load();
  let ast: unknown;
  let tables: string[];
  try {
    ast = p.astify(sql, { database: "postgresql" });
    tables = p.tableList(sql, { database: "postgresql" });
  } catch (err) {
    const first = String((err as Error).message ?? "").split(" but ")[1] ?? "";
    throw new SqlRejected(`The query could not be read${first ? ` (near ${first.slice(0, 60)})` : ""}. Check it and try again.`);
  }
  if (Array.isArray(ast)) {
    if (ast.length !== 1) throw new SqlRejected("Run one statement at a time.");
    ast = ast[0];
  }
  if (!isNode(ast) || ast.type !== "select") throw new SqlRejected("Only SELECT queries can run here.");
  const ctes = cteNames(ast);
  for (const t of tables) {
    const [kind, schema, table] = t.split("::");
    if (kind !== "select") throw new SqlRejected("Only SELECT queries can run here.");
    const name = (table ?? "").toLowerCase();
    if ((schema === "null" || schema === undefined) && ctes.has(name)) continue;
    if (schema !== "pa_view" || !(VIEWS as readonly string[]).includes(name)) {
      throw new SqlRejected(`Query the views pa_view.events, pa_view.persons, pa_view.groups and pa_view.sessions only (not ${schema === "null" ? "" : `${schema}.`}${table}).`);
    }
  }
  walk(ast, (n) => {
    if (isNode(n.into) && (n.into as Node).type === "into") throw new SqlRejected("SELECT INTO is not allowed here. Download the answer as CSV instead.");
    const fn = functionName(n);
    if (!fn) return;
    if (fn.schema) throw new SqlRejected(`Call functions without a schema (${fn.schema}.${fn.name} is not allowed).`);
    if (fn.name.startsWith("pg_") || DENIED_FUNCTIONS.has(fn.name) || fn.name.startsWith("lo_") || fn.name.startsWith("dblink")) {
      throw new SqlRejected(`The function ${fn.name} is not allowed here.`);
    }
  });
  return sql;
}
