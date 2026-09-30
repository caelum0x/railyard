// Applies db/migrations/*.sql in order (each file is idempotent). Run by the database owner:
//   DATABASE_URL=... node scripts/migrate.ts
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import pg from "pg";

const DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "db", "migrations");
export const MIGRATIONS = readdirSync(DIR).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort();

export async function migrate(url: string): Promise<string[]> {
  const client = new pg.Client({ connectionString: url });
  await client.connect();
  try {
    for (const f of MIGRATIONS) await client.query(readFileSync(join(DIR, f), "utf8"));
    return MIGRATIONS;
  } finally {
    await client.end();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const url = process.env.DATABASE_URL;
  if (!url) throw new Error("DATABASE_URL is required");
  const done = await migrate(url);
  console.log(`applied ${done.length} migrations`);
}
