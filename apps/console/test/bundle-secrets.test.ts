// No secret reaches the browser (RYP-D1 acceptance 4).
// 1. Source graph: no "use client" file reaches a server-only module or
//    process.env through its value imports (type-only imports are erased).
// 2. Build: `next build` with sentinel secret values, then grep every file
//    under .next/static for the sentinels and the secret names. Skipped with
//    a reason when next is not installed (disk rule); the source check runs always.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { SECRET_ENV } from "../lib/config.ts";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SERVER_ONLY = ["lib/config.ts", "lib/deps.ts", "lib/current.ts", "lib/session.ts", "lib/dodo.ts", "lib/postgrest-repo.ts", "lib/scribase-auth.ts", "lib/tokens.ts", "lib/webhook.ts", "lib/billing.ts", "lib/signin.ts"];

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    if (["node_modules", ".next", ".open-next", "test"].includes(name)) continue;
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (/\.(ts|tsx)$/.test(name)) out.push(p);
  }
  return out;
}

function valueImports(file: string): string[] {
  const src = readFileSync(file, "utf8");
  const out: string[] = [];
  for (const m of src.matchAll(/^\s*import\s+(type\s+)?[^'"]*?from\s+["'](\.[^"']+)["']/gm)) {
    if (m[1]) continue;
    out.push(resolve(dirname(file), m[2]));
  }
  return out;
}

test("no client component reaches a server-only module or process.env", () => {
  const clients = walk(ROOT).filter((f) => /^\s*["']use client["']/.test(readFileSync(f, "utf8")));
  assert.ok(clients.length >= 3, "client components found");
  for (const c of clients) {
    const seen = new Set<string>();
    const stack = [c];
    while (stack.length) {
      const f = stack.pop()!;
      if (seen.has(f)) continue;
      seen.add(f);
      const rel = relative(ROOT, f);
      assert.ok(!SERVER_ONLY.includes(rel), `${relative(ROOT, c)} imports server-only ${rel}`);
      assert.doesNotMatch(readFileSync(f, "utf8"), /process\.env/, `${rel} (reached from ${relative(ROOT, c)}) reads process.env`);
      stack.push(...valueImports(f).filter((p) => existsSync(p)));
    }
  }
});

test("next.config.mjs inlines no env into the client", () => {
  const cfg = readFileSync(join(ROOT, "next.config.mjs"), "utf8");
  assert.doesNotMatch(cfg, /\benv\s*:/);
  assert.doesNotMatch(cfg, /NEXT_PUBLIC_/);
});

test("build: the client bundle holds no secret value or secret name", (t) => {
  if (!existsSync(join(ROOT, "node_modules/next/package.json"))) return t.skip("next is not installed here (disk rule); run pnpm install then npm run test:bundle");
  const sentinels = Object.fromEntries(SECRET_ENV.map((n) => [n, `SENTINEL_${n}_7f3a9c`]));
  const env = { ...process.env, ...sentinels, SCRIBASE_URL: "https://sentinel.scribase.test", DODO_PRODUCT_ID_CARD_CHECK: "pdt_s1", DODO_PRODUCT_ID_MONTH: "pdt_s2", DODO_PRODUCT_ID_YEAR: "pdt_s3", NEXT_TELEMETRY_DISABLED: "1" };
  execFileSync(join(ROOT, "node_modules/.bin/next"), ["build"], { cwd: ROOT, env, stdio: "pipe" });
  const files = walkAll(join(ROOT, ".next/static"));
  assert.ok(files.length > 0);
  for (const f of files) {
    const text = readFileSync(f, "utf8");
    for (const [name, value] of Object.entries(sentinels)) {
      assert.ok(!text.includes(value), `${relative(ROOT, f)} contains the value of ${name}`);
      assert.ok(!text.includes(name), `${relative(ROOT, f)} names ${name}`);
    }
  }
});

function walkAll(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walkAll(p, out);
    else out.push(p);
  }
  return out;
}
