// D1 event store (the $0 Insights sink). With a `DB` binding the collector
// writes every stored event here at each 2 s flush, so the edge works end to
// end without the box-hosted Insights service. The visitor id is the daily
// salted hash; no IP address is ever written.

export const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    site_key TEXT NOT NULL,
    app_id TEXT,
    ts TEXT NOT NULL,
    type TEXT NOT NULL,
    name TEXT,
    url TEXT NOT NULL,
    hostname TEXT,
    referrer TEXT,
    title TEXT,
    country TEXT,
    visitor TEXT,
    user_id TEXT,
    data TEXT
  )`,
  "CREATE INDEX IF NOT EXISTS events_site_ts ON events (site_key, ts)",
];

/** D1 caps bound parameters per statement at 100; 13 columns -> 7 rows per INSERT. */
const COLS = ["site_key", "app_id", "ts", "type", "name", "url", "hostname", "referrer", "title", "country", "visitor", "user_id", "data"];
const ROWS_PER_INSERT = Math.floor(100 / COLS.length);

let ready = null;
export function ensureSchema(db) {
  if (!ready) ready = db.batch(SCHEMA.map((s) => db.prepare(s))).catch((err) => {
    ready = null;
    throw err;
  });
  return ready;
}

function row(siteKey, appId, e) {
  const data = e.type === "performance" ? JSON.stringify({ lcp: e.lcp, inp: e.inp, cls: e.cls, fcp: e.fcp, ttfb: e.ttfb }) : e.data ? JSON.stringify(e.data) : null;
  return [siteKey, appId ?? null, e.ts, e.type ?? "event", e.name ?? null, e.url, e.hostname ?? null, e.referrer ?? null, e.title ?? null, e.country ?? null, e.visitor ?? null, e.user ?? null, data];
}

/** Writes batches of `{ site_key, app_id, events }` in one D1 batch. Identify events are not stored as rows. */
export async function storeBatches(db, batches) {
  await ensureSchema(db);
  const rows = [];
  for (const b of batches) for (const e of b.events) if (e.type !== "identify") rows.push(row(b.site_key, b.app_id, e));
  if (!rows.length) return 0;
  const stmts = [];
  for (let i = 0; i < rows.length; i += ROWS_PER_INSERT) {
    const chunk = rows.slice(i, i + ROWS_PER_INSERT);
    const placeholders = chunk.map(() => `(${COLS.map(() => "?").join(",")})`).join(",");
    stmts.push(db.prepare(`INSERT INTO events (${COLS.join(",")}) VALUES ${placeholders}`).bind(...chunk.flat()));
  }
  await db.batch(stmts);
  return rows.length;
}

const hostOf = (ref) => {
  try {
    return new URL(ref).hostname.replace(/^www\./, "");
  } catch {
    return null;
  }
};

/**
 * Per-site funnel for [since, until): visitors -> engaged visitors (2+ pages)
 * -> visitors with a custom event, plus page views, top pages, top referrers
 * and custom event counts.
 */
export async function siteStats(db, siteKey, since, until) {
  await ensureSchema(db);
  const w = "site_key = ? AND ts >= ? AND ts < ?";
  const args = [siteKey, since, until];
  const [totals, engaged, converted, pages, refs, events] = await db.batch([
    db.prepare(`SELECT COUNT(*) AS pageviews, COUNT(DISTINCT visitor || substr(ts,1,10)) AS visitors FROM events WHERE ${w} AND type = 'event' AND name IS NULL`).bind(...args),
    db.prepare(`SELECT COUNT(*) AS n FROM (SELECT visitor, substr(ts,1,10) d FROM events WHERE ${w} AND type = 'event' AND name IS NULL GROUP BY visitor, d HAVING COUNT(DISTINCT url) >= 2)`).bind(...args),
    db.prepare(`SELECT COUNT(DISTINCT visitor || substr(ts,1,10)) AS n FROM events WHERE ${w} AND type = 'event' AND name IS NOT NULL`).bind(...args),
    db.prepare(`SELECT url, COUNT(*) AS n FROM events WHERE ${w} AND type = 'event' AND name IS NULL GROUP BY url ORDER BY n DESC LIMIT 5`).bind(...args),
    db.prepare(`SELECT referrer, hostname, COUNT(*) AS n FROM events WHERE ${w} AND type = 'event' AND name IS NULL AND referrer IS NOT NULL AND referrer != '' GROUP BY referrer, hostname ORDER BY n DESC LIMIT 200`).bind(...args),
    db.prepare(`SELECT name, COUNT(*) AS n FROM events WHERE ${w} AND type = 'event' AND name IS NOT NULL GROUP BY name ORDER BY n DESC LIMIT 10`).bind(...args),
  ]);
  const t = totals.results[0] ?? { pageviews: 0, visitors: 0 };
  const byHost = new Map();
  for (const r of refs.results) {
    const h = hostOf(r.referrer);
    if (h && h !== String(r.hostname ?? "").replace(/^www\./, "")) byHost.set(h, (byHost.get(h) ?? 0) + r.n);
  }
  return {
    site_key: siteKey,
    pageviews: t.pageviews,
    visitors: t.visitors,
    engaged: engaged.results[0]?.n ?? 0,
    converted: converted.results[0]?.n ?? 0,
    top_pages: pages.results.map((r) => ({ url: r.url, n: r.n })),
    top_referrers: [...byHost.entries()].sort((a, b) => b[1] - a[1]).slice(0, 5).map(([host, n]) => ({ host, n })),
    events: events.results.map((r) => ({ name: r.name, n: r.n })),
  };
}
