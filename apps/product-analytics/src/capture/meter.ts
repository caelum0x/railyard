// Usage meter `scribase.analytics.events` (dispatch section 5.3 consumer
// contract, tests/contract/consumers/contract.json). Counts per org per UTC
// minute; a finished minute is posted once as
//   {transaction_id: "scribase:scribase.analytics.events:<scribase org>:<minute>:pa",
//    external_customer_id, external_subscription_id: "scribase-portfolio-<customer>",
//    code, timestamp, properties: {events, stored, capped}}
// only when RAILYARD_LIVE=1 and the org is bound to a Scribase org. With the
// switch off the service's own counts (pa.usage_monthly) are the record.
// A repeated transaction_id is a no-op at the meter; a failed minute is
// logged and not retried into a double count (same rule as the edge).

import type { Logger } from "../log.ts";

export const METER_CODE = "scribase.analytics.events";

export interface MeterConfig {
  live: boolean;
  url: string | null;
  apiKey: string | null;
}

interface Minute {
  customer: string;
  minute: string;
  events: number;
  stored: number;
  capped: number;
}

export const minuteOf = (d: Date) => `${d.toISOString().slice(0, 16)}Z`;

export function meterEvent(m: Minute) {
  return {
    transaction_id: `scribase:${METER_CODE}:${m.customer}:${m.minute}:pa`,
    external_customer_id: m.customer,
    external_subscription_id: `scribase-portfolio-${m.customer}`,
    code: METER_CODE,
    timestamp: Math.floor(Date.parse(m.minute) / 1000),
    properties: { events: m.events, stored: m.stored, capped: m.capped },
  };
}

export class Meter {
  cfg: MeterConfig;
  log: Logger;
  fetchImpl: typeof fetch;
  minutes = new Map<string, Minute>();

  constructor(cfg: MeterConfig, log: Logger, fetchImpl: typeof fetch = fetch) {
    this.cfg = cfg;
    this.log = log;
    this.fetchImpl = fetchImpl;
  }

  get on(): boolean {
    return this.cfg.live && !!this.cfg.url;
  }

  /** `counted` = stored + capped (a paused org's dropped events are not metered). */
  record(scribaseOrgId: string | null, at: Date, stored: number, capped: number): void {
    if (!this.on || !scribaseOrgId || stored + capped === 0) return;
    const minute = minuteOf(at);
    const k = `${scribaseOrgId}|${minute}`;
    const cur = this.minutes.get(k) ?? { customer: scribaseOrgId, minute, events: 0, stored: 0, capped: 0 };
    this.minutes.set(k, { ...cur, events: cur.events + stored + capped, stored: cur.stored + stored, capped: cur.capped + capped });
  }

  /** Posts every finished minute (all of them when `all`). */
  async flush(now: Date, all = false): Promise<number> {
    const current = minuteOf(now);
    let sent = 0;
    for (const [k, m] of [...this.minutes.entries()]) {
      if (!all && m.minute >= current) continue;
      this.minutes.delete(k);
      try {
        const res = await this.fetchImpl(`${this.cfg.url!.replace(/\/$/, "")}/api/v1/events`, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${this.cfg.apiKey ?? ""}` },
          body: JSON.stringify({ event: meterEvent(m) }),
        });
        if (!res.ok && res.status !== 409) throw new Error(`meter answered ${res.status}`);
        sent += 1;
      } catch (err) {
        this.log.error("usage meter minute not sent", { customer: m.customer, minute: m.minute, error: (err as Error).message });
      }
    }
    return sent;
  }
}
