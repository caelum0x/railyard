// The insert buffer: events wait at most FLUSH_MS or FLUSH_ROWS before one
// insert per project. Receipts make the insert idempotent: an event uuid
// already received for the project is skipped, so a retried batch is stored
// once. A failed flush keeps the rows and tries again on the next tick.

import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";

export const FLUSH_MS = 2000;
export const FLUSH_ROWS = 1000;
export const MAX_BUFFERED_ROWS = 50_000;

export interface EventRow {
  uuid: string;
  project_id: string;
  org_id: string;
  event: string;
  distinct_id: string;
  person_id: string;
  timestamp: string;
  properties: Record<string, unknown>;
  elements_chain: string | null;
  session_id: string | null;
  window_id: string | null;
  groups: (string | null)[];
}

export interface UsageDelta {
  project_id: string;
  org_id: string;
  month: string;
  received: number;
  capped: number;
  dropped: number;
}

export const monthOf = (d: Date) => `${d.toISOString().slice(0, 7)}-01`;

export class EventWriter {
  db: Db;
  log: Logger;
  now: () => Date;
  rows: EventRow[] = [];
  usage = new Map<string, UsageDelta>();
  months = new Set<string>();
  timer: NodeJS.Timeout | null = null;
  flushing: Promise<void> | null = null;
  /** Called after rows are stored, with the count stored per org (caps cache). */
  onStored: (orgId: string, month: string, stored: number) => void = () => undefined;

  constructor(db: Db, log: Logger, now: () => Date = () => new Date()) {
    this.db = db;
    this.log = log;
    this.now = now;
  }

  /** Rows accepted for storing but not yet flushed (or being flushed now), for one org. */
  pendingFor(orgId: string): number {
    let n = 0;
    for (const r of this.rows) if (r.org_id === orgId) n += 1;
    for (const r of this.inFlight) if (r.org_id === orgId) n += 1;
    return n;
  }
  inFlight: EventRow[] = [];

  add(rows: EventRow[], usage: UsageDelta): void {
    this.rows.push(...rows);
    const k = `${usage.project_id}|${usage.month}`;
    const cur = this.usage.get(k);
    this.usage.set(k, cur ? { ...cur, received: cur.received + usage.received, capped: cur.capped + usage.capped, dropped: cur.dropped + usage.dropped } : usage);
    if (this.rows.length > MAX_BUFFERED_ROWS) {
      const lost = this.rows.length - MAX_BUFFERED_ROWS;
      this.rows = this.rows.slice(lost);
      this.log.error("insert buffer full; oldest rows dropped", { lost });
    }
    if (this.rows.length >= FLUSH_ROWS) void this.flush();
  }

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.flush(), FLUSH_MS);
    this.timer.unref();
  }

  async stop(): Promise<void> {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    await this.flush();
  }

  flush(): Promise<void> {
    if (this.flushing) return this.flushing.then(() => this.flush());
    if (this.rows.length === 0 && this.usage.size === 0) return Promise.resolve();
    this.flushing = this.flushNow().finally(() => {
      this.flushing = null;
    });
    return this.flushing;
  }

  async ensureMonths(rows: EventRow[]): Promise<void> {
    const need = [...new Set(rows.map((r) => r.timestamp.slice(0, 7)))].filter((m) => !this.months.has(m));
    for (const m of need) {
      await this.db.query("select pa.ensure_month($1::date)", [`${m}-01`]);
      this.months.add(m);
    }
  }

  async flushNow(): Promise<void> {
    const rows = this.rows;
    const usage = this.usage;
    this.inFlight = rows;
    this.rows = [];
    this.usage = new Map();
    const byProject = new Map<string, EventRow[]>();
    for (const r of rows) {
      const list = byProject.get(r.project_id) ?? [];
      list.push(r);
      byProject.set(r.project_id, list);
    }
    for (const u of usage.values()) if (!byProject.has(u.project_id)) byProject.set(u.project_id, []);
    const failed: EventRow[] = [];
    const failedUsage = new Map<string, UsageDelta>();
    for (const [projectId, list] of byProject) {
      const deltas = [...usage.values()].filter((u) => u.project_id === projectId);
      try {
        await this.ensureMonths(list);
        const stored = await this.writeProject(projectId, list, deltas);
        this.inFlight = this.inFlight.filter((r) => r.project_id !== projectId);
        for (const [key, n] of stored) {
          const [orgId, month] = key.split("|");
          this.onStored(orgId, month, n);
        }
      } catch (err) {
        failed.push(...list);
        for (const d of deltas) failedUsage.set(`${d.project_id}|${d.month}`, d);
        this.log.error("event insert failed; kept for the next flush", { project_id: projectId, rows: list.length, error: (err as Error).message });
      }
    }
    this.inFlight = [];
    if (failed.length || failedUsage.size) {
      this.rows = [...failed, ...this.rows];
      for (const [k, d] of failedUsage) {
        const cur = this.usage.get(k);
        this.usage.set(k, cur ? { ...cur, received: cur.received + d.received, capped: cur.capped + d.capped, dropped: cur.dropped + d.dropped } : d);
      }
    }
  }

  /** One transaction per project as the request role. Returns stored counts per org|month. */
  async writeProject(projectId: string, list: EventRow[], deltas: UsageDelta[]): Promise<Map<string, number>> {
    // One row per uuid within the flush (the receipts dedupe across flushes).
    const seen = new Set<string>();
    const unique = list.filter((r) => (seen.has(r.uuid) ? false : (seen.add(r.uuid), true)));
    const now = this.now().toISOString();
    return this.db.asProject(projectId, async (c) => {
      const stored = new Map<string, number>();
      let inserted: { uuid: string; event: string; timestamp: Date }[] = [];
      if (unique.length) {
        const input = unique.map((r) => ({
          uuid: r.uuid,
          event: r.event,
          distinct_id: r.distinct_id,
          person_id: r.person_id,
          ts: r.timestamp,
          properties: r.properties,
          elements_chain: r.elements_chain,
          session_id: r.session_id,
          window_id: r.window_id,
          g0: r.groups[0],
          g1: r.groups[1],
          g2: r.groups[2],
          g3: r.groups[3],
          g4: r.groups[4],
        }));
        const res = await c.query<{ uuid: string; event: string; timestamp: Date }>(
          `with input as (
             select * from jsonb_to_recordset($2::jsonb) as x(
               uuid uuid, event text, distinct_id text, person_id uuid, ts timestamptz, properties jsonb,
               elements_chain text, session_id text, window_id text, g0 text, g1 text, g2 text, g3 text, g4 text)
           ), r as (
             insert into pa.ingest_receipts (project_id, uuid, received_at)
             select $1, uuid, $3 from input
             on conflict (project_id, uuid) do nothing
             returning uuid
           )
           insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp", properties, elements_chain,
                                  session_id, window_id, "$group_0", "$group_1", "$group_2", "$group_3", "$group_4", inserted_at)
           select i.uuid, $1, i.event, i.distinct_id, i.person_id, i.ts, i.properties, i.elements_chain,
                  i.session_id, i.window_id, i.g0, i.g1, i.g2, i.g3, i.g4, $3
             from input i join r on r.uuid = i.uuid
           returning uuid, event, "timestamp"`,
          [projectId, JSON.stringify(input), now],
        );
        inserted = res.rows;
      }
      const orgOf = unique[0]?.org_id ?? deltas[0]?.org_id;
      const byMonth = new Map<string, number>();
      const nowMonth = monthOf(this.now());
      if (inserted.length) byMonth.set(nowMonth, inserted.length);
      const months = new Set([...deltas.map((d) => d.month), ...byMonth.keys()]);
      for (const month of months) {
        const d = deltas.find((x) => x.month === month);
        const s = byMonth.get(month) ?? 0;
        await c.query(
          `insert into pa.usage_monthly (project_id, org_id, month, received, stored, capped, dropped)
           values ($1, $2, $3, $4, $5, $6, $7)
           on conflict (project_id, month) do update set
             received = pa.usage_monthly.received + excluded.received,
             stored = pa.usage_monthly.stored + excluded.stored,
             capped = pa.usage_monthly.capped + excluded.capped,
             dropped = pa.usage_monthly.dropped + excluded.dropped`,
          [projectId, d?.org_id ?? orgOf, month, d?.received ?? 0, s, d?.capped ?? 0, d?.dropped ?? 0],
        );
        if (s) stored.set(`${d?.org_id ?? orgOf}|${month}`, s);
      }
      if (inserted.length) {
        const first = inserted.reduce((a, b) => (new Date(a.timestamp) <= new Date(b.timestamp) ? a : b));
        await c.query(
          `insert into pa.project_firsts (project_id, org_id, event_uuid, event, at) values ($1, $2, $3, $4, $5)
           on conflict (project_id) do nothing`,
          [projectId, orgOf, first.uuid, first.event, now],
        );
      }
      return stored;
    });
  }
}
