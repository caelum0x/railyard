// Session replay storage (2026-10-15): session metadata lives in D1 (so the
// dashboard's session list is one cheap query), the recorded rrweb events
// themselves live in R2 as one object per chunk (R2 has no append, and a
// session can arrive over many small POSTs). Nothing here ever sees a raw IP
// address; `visitor` is the same salted daily hash the rest of Insights uses.

export const REPLAY_SCHEMA = [
  `CREATE TABLE IF NOT EXISTS replay_sessions (
    site_key TEXT NOT NULL,
    session_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    url TEXT,
    hostname TEXT,
    country TEXT,
    visitor TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    event_count INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0,
    chunk_count INTEGER NOT NULL DEFAULT 0,
    ended INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (site_key, session_id)
  )`,
  "CREATE INDEX IF NOT EXISTS replay_sessions_site_started ON replay_sessions (site_key, started_at)",
];

let ready = null;
export function ensureReplaySchema(db) {
  if (!ready) ready = db.batch(REPLAY_SCHEMA.map((s) => db.prepare(s))).catch((err) => {
    ready = null;
    throw err;
  });
  return ready;
}

/** R2 object key for one chunk of one session. Zero-padded seq keeps a `list()` call in order. */
export function chunkKey(siteKey, sessionId, seq) {
  return `${siteKey}/${sessionId}/${String(seq).padStart(6, "0")}.json`;
}

export function sessionPrefix(siteKey, sessionId) {
  return `${siteKey}/${sessionId}/`;
}

/** New sessions this UTC month for a site (for the sessions_per_month cap). */
export async function sessionsThisMonth(db, siteKey, now = Date.now()) {
  await ensureReplaySchema(db);
  const month = `${new Date(now).toISOString().slice(0, 7)}-01T00:00:00.000Z`;
  const row = await db.prepare("SELECT COUNT(*) AS n FROM replay_sessions WHERE site_key = ? AND started_at >= ?").bind(siteKey, month).first();
  return row?.n ?? 0;
}

export async function sessionExists(db, siteKey, sessionId) {
  await ensureReplaySchema(db);
  const row = await db.prepare("SELECT 1 AS x FROM replay_sessions WHERE site_key = ? AND session_id = ?").bind(siteKey, sessionId).first();
  return Boolean(row);
}

/**
 * Creates or updates one session's metadata row for an incoming chunk.
 * Idempotent per `seq` is the caller's job (the ingest route decides whether
 * to accept a chunk at all); this just records the running totals.
 */
export async function upsertSessionChunk(db, { siteKey, sessionId, now, url, hostname, country, visitor, eventCount, bytes, ended, truncated }) {
  await ensureReplaySchema(db);
  const ts = new Date(now).toISOString();
  const existing = await db.prepare("SELECT started_at FROM replay_sessions WHERE site_key = ? AND session_id = ?").bind(siteKey, sessionId).first();
  if (!existing) {
    await db
      .prepare(
        `INSERT INTO replay_sessions (site_key, session_id, started_at, last_seen_at, url, hostname, country, visitor, duration_ms, event_count, bytes, chunk_count, ended, truncated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, 1, ?, ?)`,
      )
      .bind(siteKey, sessionId, ts, ts, url ?? null, hostname ?? null, country ?? null, visitor ?? null, eventCount, bytes, ended ? 1 : 0, truncated ? 1 : 0)
      .run();
    return;
  }
  const durationMs = Math.max(0, Date.parse(ts) - Date.parse(existing.started_at));
  await db
    .prepare(
      `UPDATE replay_sessions SET last_seen_at = ?, duration_ms = ?, event_count = event_count + ?, bytes = bytes + ?, chunk_count = chunk_count + 1,
       ended = CASE WHEN ? = 1 THEN 1 ELSE ended END, truncated = CASE WHEN ? = 1 THEN 1 ELSE truncated END
       WHERE site_key = ? AND session_id = ?`,
    )
    .bind(ts, durationMs, eventCount, bytes, ended ? 1 : 0, truncated ? 1 : 0, siteKey, sessionId)
    .run();
}

/** Sessions for one site in [since, until), newest first, for the dashboard list. */
export async function listSessions(db, siteKey, since, until, limit = 200) {
  await ensureReplaySchema(db);
  const { results } = await db
    .prepare(
      `SELECT session_id, started_at, last_seen_at, url, hostname, country, duration_ms, event_count, bytes, ended, truncated
       FROM replay_sessions WHERE site_key = ? AND started_at >= ? AND started_at < ? ORDER BY started_at DESC LIMIT ?`,
    )
    .bind(siteKey, since, until, limit)
    .all();
  return results;
}

export async function getSession(db, siteKey, sessionId) {
  await ensureReplaySchema(db);
  return db
    .prepare(
      `SELECT session_id, started_at, last_seen_at, url, hostname, country, duration_ms, event_count, bytes, chunk_count, ended, truncated
       FROM replay_sessions WHERE site_key = ? AND session_id = ?`,
    )
    .bind(siteKey, sessionId)
    .first();
}

/** All of one session's rrweb events, chunks concatenated in order. Null when the bucket has nothing (deleted, or never recorded). */
export async function readSessionEvents(bucket, siteKey, sessionId) {
  const listed = await bucket.list({ prefix: sessionPrefix(siteKey, sessionId) });
  const keys = listed.objects.map((o) => o.key).sort();
  if (!keys.length) return null;
  const events = [];
  for (const key of keys) {
    const obj = await bucket.get(key);
    if (!obj) continue;
    try {
      const chunk = JSON.parse(await obj.text());
      if (Array.isArray(chunk)) events.push(...chunk);
    } catch {
      // A corrupt chunk is skipped rather than failing the whole playback.
    }
  }
  return events;
}

/** Removes one session's stored events and its metadata row (used by site deletion and by the dashboard's own retention sweep later). */
export async function deleteSession(env, siteKey, sessionId) {
  if (env.REPLAY) {
    const listed = await env.REPLAY.list({ prefix: sessionPrefix(siteKey, sessionId) });
    await Promise.all(listed.objects.map((o) => env.REPLAY.delete(o.key)));
  }
  if (env.DB) {
    await ensureReplaySchema(env.DB);
    await env.DB.prepare("DELETE FROM replay_sessions WHERE site_key = ? AND session_id = ?").bind(siteKey, sessionId).run();
  }
}

/** Removes every replay session (events + metadata) for a whole site — used when a site is deleted. */
export async function deleteAllSessions(env, siteKey) {
  if (env.REPLAY) {
    let cursor;
    do {
      const listed = await env.REPLAY.list({ prefix: `${siteKey}/`, cursor });
      await Promise.all(listed.objects.map((o) => env.REPLAY.delete(o.key)));
      cursor = listed.truncated ? listed.cursor : undefined;
    } while (cursor);
  }
  if (env.DB) {
    await ensureReplaySchema(env.DB);
    await env.DB.prepare("DELETE FROM replay_sessions WHERE site_key = ?").bind(siteKey).run();
  }
}
