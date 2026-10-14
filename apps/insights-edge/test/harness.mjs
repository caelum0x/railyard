// Runs the collector in Miniflare (workerd) with a KV of sites and an
// outbound mock that records every request the Worker sends downstream.
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { stripTypeScriptTypes } from "node:module";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/** Every .mjs/.ts module under src/, recursively (Miniflare's module converter does not follow imports, so every file index.mjs transitively imports — including src/replay/* and src/vendor/* — must be listed by hand). */
function collectSrcModules(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...collectSrcModules(full));
    } else if ((entry.endsWith(".mjs") || entry.endsWith(".ts")) && full !== join(ROOT, "src", "index.mjs")) {
      out.push(full);
    }
  }
  return out;
}

export async function startCollector({ bindings = {}, sites = {}, respond = () => 200, d1 = false, r2 = false } = {}) {
  const persist = mkdtempSync(join(tmpdir(), "ry-insights-"));
  const outbound = [];
  // Miniflare 5 takes its new option shape; the converter keeps the classic one readable.
  const opts = convertV4MiniflareOptions({
    workers: [
      {
        name: "insights-edge",
        // Every module listed, entry first (the converter does not follow imports).
        modules: [join(ROOT, "src", "index.mjs"), ...collectSrcModules(join(ROOT, "src"))].map((full) => ({
          type: "ESModule",
          path: full,
          // workerd runs JavaScript: TypeScript modules (caps.ts) have their types stripped, as wrangler's bundle does.
          contents: full.endsWith(".ts") ? stripTypeScriptTypes(readFileSync(full, "utf8")) : readFileSync(full, "utf8"),
        })),
        modulesRoot: ROOT,
        compatibilityDate: "2026-09-20",
        durableObjects: { ORG_COLLECTOR: { className: "OrgCollector", useSQLite: true } },
        kvNamespaces: ["SITES"],
        ...(d1 ? { d1Databases: ["DB"] } : {}),
        ...(r2 ? { r2Buckets: ["REPLAY"] } : {}),
        bindings: {
          VISITOR_SALT_SECRET: "test-salt-secret",
          INGEST_URL: "https://ingest.test",
          INGEST_TOKEN: "ingest-token",
          RAGO_URL: "https://rago.test",
          RAGO_API_KEY: "rago-key",
          ...bindings,
        },
        outboundService: async (req) => {
          const entry = { url: req.url, at: Date.now(), body: await req.text(), auth: req.headers.get("authorization") };
          outbound.push(entry);
          // `respond` returns either a bare status (body defaults to "{}") or
          // `{status, body}` so a test can mock a real JSON reply (e.g. Dodo's
          // `checkout_url`) per outbound call.
          const r = respond(entry);
          const { status, body } = typeof r === "object" && r !== null ? r : { status: r, body: "{}" };
          return new Response(typeof body === "string" ? body : JSON.stringify(body), { status });
        },
      },
    ],
  });
  // Keep cf-connecting-ip (the dev default strips it), so the no-raw-IP tests see a real IP.
  for (const w of opts.workers) w.dev = { ...w.dev, stripCfConnectingIp: false };
  const mf = new Miniflare({ ...opts, logRequests: false, resourcePersistencePath: persist });
  const base = (await mf.ready).href.replace(/\/$/, "");
  const kv = await mf.getKVNamespace("SITES");
  for (const [key, value] of Object.entries(sites)) await kv.put(`site:${key}`, JSON.stringify(value));
  return {
    mf,
    outbound,
    persist,
    base,
    // Plain HTTP to the local workerd socket (keep-alive), as a browser would call it.
    send: (body, headers = {}, path = "/api/send") =>
      fetch(`${base}${path}`, {
        method: "POST",
        headers: { "content-type": "text/plain", "user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Safari/605.1.15", "cf-connecting-ip": "203.0.113.77", ...headers },
        body: typeof body === "string" ? body : JSON.stringify(body),
      }),
    async close() {
      await mf.dispose();
      rmSync(persist, { recursive: true, force: true });
    },
  };
}

export const ev = (site, url = "/", extra = {}) => ({ type: "event", payload: { website: site, url, hostname: "shop.example", title: "Shop", language: "en-US", screen: "1440x900", ...extra } });
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
