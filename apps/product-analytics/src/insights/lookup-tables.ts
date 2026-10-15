// Lookup tables (Mixpanel parity checklist row 4): a CSV the project owner
// uploads, keyed by its first column, joined into event/person/group
// properties at query time by filters.ts's Joins.lookup() (filters.ts's
// "lookup" PropertyFilter) -- the same join-then-compare path "group"
// filters already use, just against pa.lookup_table_rows instead of
// pa.groups. Reuses cohorts.ts's CSV reader rather than a second one.

import type { Client, Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { parseCsv } from "./cohorts.ts";
import { name } from "./store.ts";
import { QueryError } from "./types.ts";

export const MAX_LOOKUP_ROWS = 100_000;
export const MAX_LOOKUP_COLUMNS = 50;
export const MAX_LOOKUP_CSV_BYTES = 5 * 1024 * 1024;

export interface LookupTableSummary {
  id: string;
  name: string;
  key_column: string;
  columns: string[];
  row_count: number;
  created_at: string;
  updated_at: string;
}

interface ParsedLookup {
  keyColumn: string;
  columns: string[];
  rows: { key: string; data: Record<string, string> }[];
}

/** The CSV's first column is the join key; every other header becomes a `data` column read back as `->> 'column'`. */
function parseLookupCsv(csv: string): ParsedLookup {
  if (Buffer.byteLength(csv, "utf8") > MAX_LOOKUP_CSV_BYTES) throw new QueryError("Keep the file under 5 MB.");
  const table = parseCsv(csv.replace(/^﻿/, ""));
  if (table.length < 2) throw new QueryError("The file needs a header row and at least one row.");
  const [header, ...body] = table;
  const trimmedHeader = header.map((h) => h.trim());
  if (trimmedHeader.length < 1 || trimmedHeader.some((h) => !h)) throw new QueryError("Every column needs a name in the header row.");
  if (trimmedHeader.length > MAX_LOOKUP_COLUMNS) throw new QueryError(`A lookup table has at most ${MAX_LOOKUP_COLUMNS} columns.`);
  if (new Set(trimmedHeader).size !== trimmedHeader.length) throw new QueryError("Column names must be unique.");
  if (body.length > MAX_LOOKUP_ROWS) throw new QueryError(`A lookup table has at most ${MAX_LOOKUP_ROWS} rows.`);
  const [keyColumn, ...columns] = trimmedHeader;
  const seen = new Set<string>();
  const rows = body.map((r) => {
    const key = (r[0] ?? "").trim();
    if (!key) throw new QueryError("Every row needs a key in the first column.");
    if (key.length > 400) throw new QueryError("A lookup key is at most 400 characters.");
    if (seen.has(key)) throw new QueryError(`Duplicate lookup key: ${key}.`);
    seen.add(key);
    const data: Record<string, string> = {};
    columns.forEach((col, i) => {
      data[col] = (r[i + 1] ?? "").trim();
    });
    return { key, data };
  });
  return { keyColumn, columns, rows };
}

async function replaceRows(c: Client, projectId: string, lookupTableId: string, rows: ParsedLookup["rows"]): Promise<void> {
  await c.query("delete from pa.lookup_table_rows where project_id = $1 and lookup_table_id = $2", [projectId, lookupTableId]);
  const BATCH = 1000;
  for (let i = 0; i < rows.length; i += BATCH) {
    const chunk = rows.slice(i, i + BATCH);
    const values: string[] = [];
    const params: unknown[] = [projectId, lookupTableId];
    for (const r of chunk) {
      const base = params.length;
      values.push(`($1, $2, $${base + 1}, $${base + 2})`);
      params.push(r.key, JSON.stringify(r.data));
    }
    await c.query(`insert into pa.lookup_table_rows (project_id, lookup_table_id, key, data) values ${values.join(", ")}`, params);
  }
}

export async function listLookupTables(db: Db, projectId: string): Promise<LookupTableSummary[]> {
  return db.asProject(
    projectId,
    async (c) => (await c.query<LookupTableSummary>("select id, name, key_column, columns, row_count, created_at, updated_at from pa.lookup_tables where project_id = $1 order by name", [projectId])).rows,
  );
}

export async function getLookupTable(db: Db, projectId: string, id: string): Promise<LookupTableSummary | null> {
  if (!isUuid(id)) return null;
  return db.asProject(
    projectId,
    async (c) =>
      (await c.query<LookupTableSummary>("select id, name, key_column, columns, row_count, created_at, updated_at from pa.lookup_tables where project_id = $1 and id = $2", [projectId, id])).rows[0] ??
      null,
  );
}

export async function createLookupTable(db: Db, projectId: string, input: { name: unknown; csv: unknown }): Promise<LookupTableSummary> {
  const n = name(input.name, "lookup table");
  if (typeof input.csv !== "string" || !input.csv.trim()) throw new QueryError("Upload a CSV.");
  const parsed = parseLookupCsv(input.csv);
  try {
    return await db.asProject(projectId, async (c) => {
      const t = (
        await c.query<LookupTableSummary>(
          "insert into pa.lookup_tables (project_id, name, key_column, columns, row_count) values ($1, $2, $3, $4, $5) returning id, name, key_column, columns, row_count, created_at, updated_at",
          [projectId, n, parsed.keyColumn, JSON.stringify(parsed.columns), parsed.rows.length],
        )
      ).rows[0];
      await replaceRows(c, projectId, t.id, parsed.rows);
      return t;
    });
  } catch (err) {
    if ((err as { code?: string }).code === "23505") throw new QueryError("A lookup table with that name already exists.");
    throw err;
  }
}

/** Re-uploads a lookup table's CSV, replacing every row (and its columns, if the header changed). */
export async function replaceLookupTableCsv(db: Db, projectId: string, id: string, input: { csv: unknown }): Promise<LookupTableSummary | null> {
  if (!isUuid(id)) return null;
  if (typeof input.csv !== "string" || !input.csv.trim()) throw new QueryError("Upload a CSV.");
  const parsed = parseLookupCsv(input.csv);
  return db.asProject(projectId, async (c) => {
    const exists = await c.query("select 1 from pa.lookup_tables where project_id = $1 and id = $2", [projectId, id]);
    if (!exists.rowCount) return null;
    await replaceRows(c, projectId, id, parsed.rows);
    return (
      await c.query<LookupTableSummary>(
        "update pa.lookup_tables set key_column = $3, columns = $4, row_count = $5, updated_at = now() where project_id = $1 and id = $2 returning id, name, key_column, columns, row_count, created_at, updated_at",
        [projectId, id, parsed.keyColumn, JSON.stringify(parsed.columns), parsed.rows.length],
      )
    ).rows[0];
  });
}

export async function deleteLookupTable(db: Db, projectId: string, id: string): Promise<boolean> {
  if (!isUuid(id)) return false;
  return db.asProject(projectId, async (c) => ((await c.query("delete from pa.lookup_tables where project_id = $1 and id = $2", [projectId, id])).rowCount ?? 0) > 0);
}
