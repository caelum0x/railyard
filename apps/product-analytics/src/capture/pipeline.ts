// capture -> caps -> person -> insert buffer. One org at a time (a promise
// chain per org), so the cap decision and the person merges never race.
//   paused org      counted as received and dropped; nothing stored or metered
//   outside window  events older than the plan's retention are dropped
//   too large       an event over MAX_EVENT_BYTES (properties and person updates) is dropped
//   at the cap      counted (received, capped, metered) and not stored
//   otherwise       persons resolved in one project transaction, rows buffered

import { capDecision } from "../caps.ts";
import type { Db } from "../db.ts";
import { groupColumns, groupTypeIndex, setOps, upsertGroup } from "../groups/groups.ts";
import type { Logger } from "../log.ts";
import { personIdFor } from "../persons/ids.ts";
import { processPerson } from "../persons/service.ts";
import type { Site } from "../tenancy/site-directory.ts";
import type { Meter } from "./meter.ts";
import { withoutIp, type CapturedEvent } from "./normalize.ts";
import type { UsageCounter } from "./usage.ts";
import { monthOf, type EventRow, type EventWriter } from "./writer.ts";

const DAY_MS = 86_400_000;
/** Largest stored event (its properties and person updates as JSON). */
export const MAX_EVENT_BYTES = 1024 * 1024;

/** True when the event's stored JSON would exceed MAX_EVENT_BYTES. */
export function tooLarge(e: CapturedEvent): boolean {
  let n = e.elements_chain?.length ?? 0;
  for (const part of [e.properties, e.set, e.set_once]) {
    if (!part) continue;
    n += Buffer.byteLength(JSON.stringify(part), "utf8");
    if (n > MAX_EVENT_BYTES) return true;
  }
  return n > MAX_EVENT_BYTES;
}

export interface IngestResult {
  received: number;
  accepted: number;
  capped: number;
  dropped: number;
  paused: boolean;
}

export interface PipelineDeps {
  db: Db;
  writer: EventWriter;
  usage: UsageCounter;
  meter: Meter;
  log: Logger;
  now: () => Date;
}

export class CapturePipeline {
  deps: PipelineDeps;
  chains = new Map<string, Promise<unknown>>();

  constructor(deps: PipelineDeps) {
    this.deps = deps;
  }

  /** Runs `fn` after every earlier call for the same key has settled. */
  serial<T>(key: string, fn: () => Promise<T>): Promise<T> {
    const prev = this.chains.get(key) ?? Promise.resolve();
    const next = prev.then(fn, fn);
    const tail = next.catch(() => undefined);
    this.chains.set(key, tail);
    void tail.then(() => {
      if (this.chains.get(key) === tail) this.chains.delete(key);
    });
    return next;
  }

  /** `historical`: skip the retention-window cutoff (the /import endpoint's job -- backfilling events older than the plan's retention, unlike /track's live path). */
  ingest(site: Site, events: CapturedEvent[], opts: { historical?: boolean } = {}): Promise<IngestResult> {
    return this.serial(`org:${site.org_id}`, () => this.ingestNow(site, events, opts));
  }

  async ingestNow(site: Site, events: CapturedEvent[], opts: { historical?: boolean } = {}): Promise<IngestResult> {
    const { writer, usage, meter, now: clock } = this.deps;
    const now = clock();
    const month = monthOf(now);
    const received = events.length;
    const base = { project_id: site.project_id, org_id: site.org_id, month, received };
    if (site.paused) {
      writer.add([], { ...base, capped: 0, dropped: received });
      return { received, accepted: 0, capped: 0, dropped: received, paused: true };
    }
    const oldest = now.getTime() - site.retention_days * DAY_MS;
    const inWindow = events.filter((e) => (opts.historical || e.timestamp.getTime() >= oldest) && !tooLarge(e));
    const expired = received - inWindow.length;
    const used = await usage.used(site.org_id);
    const decision = capDecision(used, site.events_limit, inWindow.length);
    const toStore = inWindow.slice(0, decision.stored);
    const rows = toStore.length ? await this.resolve(site, toStore) : [];
    writer.add(rows, { ...base, capped: decision.capped, dropped: expired });
    meter.record(site.scribase_org_id, now, rows.length, decision.capped);
    return { received, accepted: rows.length, capped: decision.capped, dropped: expired, paused: false };
  }

  /** People and groups for the events to store, in one project transaction. */
  resolve(site: Site, events: CapturedEvent[]): Promise<EventRow[]> {
    return this.deps.db.asProject(site.project_id, async (c) => {
      const cache = new Map<string, number | null>();
      const rows: EventRow[] = [];
      for (const ev of events) {
        // Group updates carry no person (upstream skips person processing for them).
        const person = ev.event === "$groupidentify" ? { person_id: personIdFor(site.project_id, ev.distinct_id) } : await processPerson(c, site.project_id, ev);
        if (ev.event === "$groupidentify") {
          const type = ev.properties.$group_type;
          const key = ev.properties.$group_key;
          const set = ev.properties.$group_set;
          if (typeof type === "string" && type && (typeof key === "string" || typeof key === "number") && String(key)) {
            const idx = await groupTypeIndex(c, site.project_id, type, cache);
            if (idx !== null) await upsertGroup(c, site.project_id, idx, String(key).slice(0, 400), setOps(set && typeof set === "object" && !Array.isArray(set) ? withoutIp(set as Record<string, unknown>) : {}), ev.timestamp);
          }
        }
        rows.push({
          uuid: ev.uuid,
          project_id: site.project_id,
          org_id: site.org_id,
          event: ev.event,
          distinct_id: ev.distinct_id,
          person_id: person.person_id,
          timestamp: ev.timestamp.toISOString(),
          properties: ev.properties,
          elements_chain: ev.elements_chain,
          session_id: ev.session_id,
          window_id: ev.window_id,
          groups: await groupColumns(c, site.project_id, ev.groups, cache),
        });
      }
      return rows;
    });
  }
}
