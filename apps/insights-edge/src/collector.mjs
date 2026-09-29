// OrgCollector: one Durable Object per Railyard org. It counts events against
// each site's monthly cap, buffers the stored ones, and flushes every 2 s in
// ONE request to Insights ingest (so each site sees at most one downstream
// request every 2 s). Over-cap events are counted, never stored. Once a
// minute it reports the org's usage to Rago as an `insights_events` event
// (a $0 shadow meter). It never receives an IP address.

import { capDecision } from "./caps.ts";

export const FLUSH_MS = 2000;
export const MAX_BUFFER_PER_SITE = 10_000;
/** Client event ids remembered per site (a retried delivery is stored once). */
export const SEEN_TTL_MS = 24 * 3600 * 1000;
export const MAX_SEEN_PER_SITE = 20_000;

const monthKey = (ms) => new Date(ms).toISOString().slice(0, 7);

export class OrgCollector {
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.orgId = null;
    this.buffers = new Map(); // siteKey -> { appId, events: [] }
    this.counts = new Map(); // `${siteKey}|${month}` -> number
    this.minutes = new Map(); // minute -> { events, stored, capped, dropped }
    this.dirty = new Set();
    this.seen = new Map(); // siteKey -> Map(event id -> expiry ms)
    this.seenDirty = new Set();
    // Usage bucket for Rago: one minute (RAGO_BUCKET_MS exists for tests only).
    this.bucketMs = Number(env.RAGO_BUCKET_MS ?? 60_000);
  }

  bucket(ms) {
    return new Date(Math.floor(ms / this.bucketMs) * this.bucketMs).toISOString();
  }

  now() {
    return Date.now();
  }

  async count(siteKey, month) {
    const k = `${siteKey}|${month}`;
    if (!this.counts.has(k)) this.counts.set(k, (await this.state.storage.get(`count:${k}`)) ?? 0);
    return this.counts.get(k);
  }

  minute(ms) {
    const k = this.bucket(ms);
    if (!this.minutes.has(k)) this.minutes.set(k, { events: 0, stored: 0, capped: 0, dropped: 0 });
    return this.minutes.get(k);
  }

  async fetch(request) {
    const body = await request.json();
    const now = this.now();
    if (this.orgId !== body.orgId) {
      this.orgId = body.orgId;
      await this.state.storage.put("orgId", body.orgId);
    }
    const { site } = body;
    const events = await this.fresh(site.key, body.events, now);
    const duplicates = body.events.length - events.length;
    const month = monthKey(now);
    const used = await this.count(site.key, month);
    const decision = capDecision(used, site.events_limit ?? 0, events.length);
    const stored = events.slice(0, decision.stored);
    const capped = decision.capped;
    this.counts.set(`${site.key}|${month}`, used + events.length);
    this.dirty.add(`${site.key}|${month}`);
    const m = this.minute(now);
    m.events += events.length;
    m.capped += capped;
    if (stored.length) {
      const buf = this.buffers.get(site.key) ?? { appId: site.app_id, events: [] };
      buf.events.push(...stored);
      const over = buf.events.length - MAX_BUFFER_PER_SITE;
      if (over > 0) {
        buf.events.splice(0, over);
        m.dropped += over;
      }
      this.buffers.set(site.key, buf);
      m.stored += stored.length;
    }
    await this.schedule();
    return Response.json({ stored: stored.length, capped, duplicates });
  }

  /** Drops events whose client id was seen in the last 24 h; remembers the new ones (bounded). */
  async fresh(siteKey, events, now) {
    if (!events.some((e) => e.id)) return events;
    if (!this.seen.has(siteKey)) this.seen.set(siteKey, new Map((await this.state.storage.get(`seen:${siteKey}`)) ?? []));
    const seen = this.seen.get(siteKey);
    for (const [id, until] of seen) if (until <= now) seen.delete(id);
    const out = [];
    for (const e of events) {
      if (e.id && seen.has(e.id)) continue;
      if (e.id) seen.set(e.id, now + SEEN_TTL_MS);
      out.push(e);
    }
    while (seen.size > MAX_SEEN_PER_SITE) seen.delete(seen.keys().next().value);
    this.seenDirty.add(siteKey);
    return out;
  }

  async schedule() {
    if (this.alarmAt && this.alarmAt > this.now() - FLUSH_MS) return;
    if ((await this.state.storage.getAlarm()) === null) await this.state.storage.setAlarm(this.now() + FLUSH_MS);
    this.alarmAt = this.now() + FLUSH_MS;
  }

  async alarm() {
    this.alarmAt = 0;
    if (!this.orgId) this.orgId = await this.state.storage.get("orgId");
    const now = this.now();
    await this.flushIngest(now);
    for (const k of this.dirty) await this.state.storage.put(`count:${k}`, this.counts.get(k));
    this.dirty.clear();
    for (const k of this.seenDirty) await this.state.storage.put(`seen:${k}`, [...(this.seen.get(k) ?? new Map())]);
    this.seenDirty.clear();
    await this.flushRago(now);
    const pending = [...this.buffers.values()].some((b) => b.events.length) || this.minutes.size > 0;
    if (pending) {
      await this.state.storage.setAlarm(now + FLUSH_MS);
      this.alarmAt = now + FLUSH_MS;
    }
  }

  async flushIngest(now) {
    const batches = [...this.buffers.entries()].filter(([, b]) => b.events.length).map(([siteKey, b]) => ({ site_key: siteKey, app_id: b.appId, events: b.events }));
    if (!batches.length || !this.env.INGEST_URL) return;
    this.buffers = new Map();
    try {
      const res = await fetch(`${this.env.INGEST_URL.replace(/\/$/, "")}/v1/ingest/batch`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${this.env.INGEST_TOKEN ?? ""}` },
        body: JSON.stringify({ org_id: this.orgId, sent_at: new Date(now).toISOString(), batches }),
      });
      if (!res.ok) throw new Error(`ingest ${res.status}`);
    } catch (err) {
      // Put the events back in front of anything that arrived meanwhile; retried at the next alarm.
      for (const b of batches) {
        const cur = this.buffers.get(b.site_key) ?? { appId: b.app_id, events: [] };
        cur.events = [...b.events, ...cur.events].slice(-MAX_BUFFER_PER_SITE);
        this.buffers.set(b.site_key, cur);
      }
      console.error(`insights-edge: ingest flush for org ${this.orgId} failed: ${err.message}`);
    }
  }

  async flushRago(now) {
    const current = this.bucket(now);
    for (const [minute, m] of [...this.minutes.entries()]) {
      if (minute >= current) continue;
      this.minutes.delete(minute);
      if (!this.env.RAGO_URL || !m.events) continue;
      try {
        const res = await fetch(`${this.env.RAGO_URL.replace(/\/$/, "")}/api/v1/events`, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${this.env.RAGO_API_KEY ?? ""}` },
          body: JSON.stringify({
            event: {
              transaction_id: `insights:${this.orgId}:${minute}`,
              external_subscription_id: `railyard-org-${this.orgId}`,
              code: "insights_events",
              timestamp: Math.floor(Date.parse(minute) / 1000),
              properties: { events: m.events, stored: m.stored, capped: m.capped, dropped: m.dropped },
            },
          }),
        });
        if (!res.ok && res.status !== 409) throw new Error(`rago ${res.status}`);
      } catch (err) {
        // Shadow meter: a lost minute is logged, never retried into a double count.
        console.error(`insights-edge: rago usage for org ${this.orgId} ${minute} failed: ${err.message}`);
      }
    }
  }
}
