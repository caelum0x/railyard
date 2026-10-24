// OrgCollector: one Durable Object per Railyard org. It counts events against
// the ORG'S monthly cap (one shared pool across every site/app of the org —
// the plan's events_limit is per org, not per site, so an org on the 3-app
// plan cannot store 3x its quota by spreading events across 3 site keys),
// buffers the stored ones, and flushes every 2 s in ONE request to Insights
// ingest (so each site sees at most one downstream request every 2 s).
// Over-cap events are counted, never stored. Once a minute it reports the
// org's usage to Rago as an `insights_events` event (a $0 shadow meter). It
// never receives an IP address.

import { sendAgentFailures } from "./agent-failure.mjs";
import { capDecision } from "./caps.ts";
import { PA_MAX_BATCH, sendToPa, toPaEvent } from "./pa-forward.mjs";
import { storeBatches } from "./store.mjs";

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
    this.counts = new Map(); // month -> number, pooled across every site of this org
    this.minutes = new Map(); // minute -> { events, stored, capped, dropped }
    this.dirty = new Set();
    this.seen = new Map(); // siteKey -> Map(event id -> expiry ms)
    this.seenDirty = new Set();
    // QY1.2: stored events in the product analytics protocol, per site (only with PRODUCT_ANALYTICS_URL).
    this.pa = new Map(); // siteKey -> events
    // Usage bucket for Rago: one minute (RAGO_BUCKET_MS exists for tests only).
    this.bucketMs = Number(env.RAGO_BUCKET_MS ?? 60_000);
  }

  bucket(ms) {
    return new Date(Math.floor(ms / this.bucketMs) * this.bucketMs).toISOString();
  }

  now() {
    return Date.now();
  }

  /** Events stored this UTC month, pooled across every site of this org (one DO per org). */
  async count(month) {
    if (!this.counts.has(month)) this.counts.set(month, (await this.state.storage.get(`count:${month}`)) ?? 0);
    return this.counts.get(month);
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
    // Pooled org-wide: usage from every site of this org counts against the
    // same monthly cap, so a 3-app org cannot store 3x its plan's events_limit
    // by spreading events across its sites (RYP per-site pricing penalty fix).
    const used = await this.count(month);
    const decision = capDecision(used, site.events_limit ?? 0, events.length);
    const stored = events.slice(0, decision.stored);
    const capped = decision.capped;
    this.counts.set(month, used + events.length);
    this.dirty.add(month);
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
      if (this.env.PRODUCT_ANALYTICS_URL) {
        const pa = this.pa.get(site.key) ?? [];
        pa.push(...stored.map((e) => toPaEvent(e, crypto.randomUUID())));
        this.pa.set(site.key, pa.slice(-MAX_BUFFER_PER_SITE));
      }
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
    await this.flushPa(now);
    for (const k of this.dirty) await this.state.storage.put(`count:${k}`, this.counts.get(k));
    this.dirty.clear();
    for (const k of this.seenDirty) await this.state.storage.put(`seen:${k}`, [...(this.seen.get(k) ?? new Map())]);
    this.seenDirty.clear();
    await this.flushRago(now);
    const pending = [...this.buffers.values()].some((b) => b.events.length) || [...this.pa.values()].some((l) => l.length) || this.minutes.size > 0;
    if (pending) {
      await this.state.storage.setAlarm(now + FLUSH_MS);
      this.alarmAt = now + FLUSH_MS;
    }
  }

  async flushIngest(now) {
    const batches = [...this.buffers.entries()].filter(([, b]) => b.events.length).map(([siteKey, b]) => ({ site_key: siteKey, app_id: b.appId, events: b.events }));
    if (!batches.length || (!this.env.INGEST_URL && !this.env.DB)) return;
    this.buffers = new Map();
    const requeue = (why) => {
      // Put the events back in front of anything that arrived meanwhile; retried at the next alarm.
      for (const b of batches) {
        const cur = this.buffers.get(b.site_key) ?? { appId: b.app_id, events: [] };
        cur.events = [...b.events, ...cur.events].slice(-MAX_BUFFER_PER_SITE);
        this.buffers.set(b.site_key, cur);
      }
      console.error(`insights-edge: ${why} for org ${this.orgId} failed, will retry`);
    };
    // The $0 sink: D1. When it is bound it is the system of record and the
    // ingest forward below is best effort (a retry would write D1 twice).
    if (this.env.DB) {
      try {
        await storeBatches(this.env.DB, batches);
      } catch (err) {
        return requeue(`d1 write (${err.message})`);
      }
    }
    const reportFailures = () => {
      if (!this.env.FAULTLINE_DSN) return null;
      return sendAgentFailures(this.env.FAULTLINE_DSN, batches, now).catch((err) => {
        console.error(`insights-edge: faultline forward for org ${this.orgId} dropped: ${err.message}`);
      });
    };
    if (this.env.DB) await reportFailures();
    if (!this.env.INGEST_URL) return;
    try {
      const res = await fetch(`${this.env.INGEST_URL.replace(/\/$/, "")}/v1/ingest/batch`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${this.env.INGEST_TOKEN ?? ""}` },
        body: JSON.stringify({ org_id: this.orgId, sent_at: new Date(now).toISOString(), batches }),
      });
      if (!res.ok) throw new Error(`ingest ${res.status}`);
      if (!this.env.DB) await reportFailures();
    } catch (err) {
      if (this.env.DB) console.error(`insights-edge: ingest forward for org ${this.orgId} dropped: ${err.message}`);
      else requeue(`ingest flush (${err.message})`);
    }
  }

  /** QY1.2: the same stored events into product analytics, at most PA_MAX_BATCH per request; a server error is retried at the next alarm. */
  async flushPa(now) {
    if (!this.env.PRODUCT_ANALYTICS_URL || !this.pa.size) return;
    const all = this.pa;
    this.pa = new Map();
    for (const [siteKey, events] of all) {
      for (let i = 0; i < events.length; i += PA_MAX_BATCH) {
        const chunk = events.slice(i, i + PA_MAX_BATCH);
        const r = await sendToPa(this.env.PRODUCT_ANALYTICS_URL, siteKey, chunk, now);
        if (r === "retry") {
          const rest = events.slice(i);
          this.pa.set(siteKey, [...rest, ...(this.pa.get(siteKey) ?? [])].slice(-MAX_BUFFER_PER_SITE));
          console.error(`insights-edge: product analytics flush for org ${this.orgId} will be retried`);
          break;
        }
        if (r === "drop") console.error(`insights-edge: product analytics refused ${chunk.length} events of one site of org ${this.orgId}`);
      }
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
