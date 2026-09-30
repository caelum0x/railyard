// Statement helpers shared by every builder. User input only ever reaches
// SQL as a bound parameter ($n); identifiers and keywords come from closed
// lists in this file.

import { QueryError, type DateRange, type Interval, type WindowUnit } from "./types.ts";

export class Params {
  values: unknown[] = [];
  /** Binds a value and returns its placeholder. */
  add(v: unknown): string {
    this.values.push(v);
    return `$${this.values.length}`;
  }
}

/** The one place person ids are resolved: an event's person is its merge target when there is one. */
export const PERSON = "coalesce(o.new_person_id, e.person_id)";
export const PERSON_JOIN = "left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id";

export const INTERVALS: Interval[] = ["hour", "day", "week", "month"];
export const INTERVAL_SQL: Record<Interval, string> = { hour: "1 hour", day: "1 day", week: "1 week", month: "1 month" };
export const WINDOW_SQL: Record<WindowUnit, string> = { minute: "minute", hour: "hour", day: "day", week: "week" };

const DAY = 86_400_000;
const REL = /^-(\d{1,4})([hdwmy])$/;
const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/;
const ISO_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d{1,6})?)?(Z|[+-]\d{2}:\d{2})$/;

export interface ResolvedRange {
  from: Date;
  to: Date;
}

function parsePoint(v: string, now: Date, end: boolean): Date {
  const rel = REL.exec(v);
  if (rel) {
    const n = Number(rel[1]);
    const d = new Date(now.getTime());
    switch (rel[2]) {
      case "h":
        return new Date(now.getTime() - n * 3600_000);
      case "d":
        d.setUTCDate(d.getUTCDate() - n);
        break;
      case "w":
        d.setUTCDate(d.getUTCDate() - 7 * n);
        break;
      case "m":
        d.setUTCMonth(d.getUTCMonth() - n);
        break;
      case "y":
        d.setUTCFullYear(d.getUTCFullYear() - n);
        break;
    }
    // Relative day ranges start at the beginning of that day (upstream behaviour).
    return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()));
  }
  if (ISO_DATE.test(v)) {
    const d = new Date(`${v}T00:00:00Z`);
    if (Number.isNaN(d.getTime())) throw new QueryError(`${v} is not a date.`);
    return end ? new Date(d.getTime() + DAY - 1) : d;
  }
  if (ISO_TIME.test(v)) {
    const d = new Date(v);
    if (Number.isNaN(d.getTime())) throw new QueryError(`${v} is not a date.`);
    return d;
  }
  throw new QueryError(`Use a date like 2026-09-01 or a relative range like -30d, not ${JSON.stringify(v).slice(0, 40)}.`);
}

export const MAX_RANGE_DAYS = 3 * 366;

export function resolveRange(r: DateRange, now: Date): ResolvedRange {
  const from = parsePoint(r.date_from, now, false);
  const to = r.date_to ? parsePoint(r.date_to, now, true) : now;
  if (from.getTime() > to.getTime()) throw new QueryError("The start date is after the end date.");
  if (to.getTime() - from.getTime() > MAX_RANGE_DAYS * DAY) throw new QueryError("Pick a range of at most three years.");
  return { from, to };
}

/** The same length of time right before the range (compare to previous). */
export function previousRange(r: ResolvedRange): ResolvedRange {
  const len = r.to.getTime() - r.from.getTime();
  return { from: new Date(r.from.getTime() - len - 1), to: new Date(r.from.getTime() - 1) };
}

export function checkTimezone(tz: string | undefined): string {
  const z = tz ?? "UTC";
  if (z.length > 64 || !/^[A-Za-z0-9_+\-/]+$/.test(z)) throw new QueryError("Pick a time zone such as UTC or Europe/Berlin.");
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: z });
  } catch {
    throw new QueryError("Pick a time zone such as UTC or Europe/Berlin.");
  }
  return z;
}

/** True when every offset of the zone over the range is a whole hour (hourly rollups line up). */
export function wholeHourZone(tz: string, r: ResolvedRange): boolean {
  if (tz === "UTC") return true;
  const offset = (d: Date) => {
    const parts = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "longOffset" }).formatToParts(d);
    const name = parts.find((p) => p.type === "timeZoneName")?.value ?? "GMT";
    const m = /GMT([+-])(\d{2}):(\d{2})/.exec(name);
    return m ? Number(m[3]) : 0;
  };
  for (let t = r.from.getTime(); t <= r.to.getTime() + DAY; t += 30 * DAY) if (offset(new Date(t)) !== 0) return false;
  return offset(r.to) === 0;
}

/** Local bucket start of a timestamp expression, as `timestamp without time zone` in the zone. */
export function bucketExpr(expr: string, interval: Interval, tzParam: string): string {
  return `date_trunc('${interval}', (${expr}) at time zone ${tzParam})`;
}

/** Formats a local bucket start (timestamp without time zone, as returned by pg) as an ISO-like label. */
export function bucketLabel(v: unknown, interval: Interval): string {
  const d = v instanceof Date ? v : new Date(String(v));
  // pg returns timestamp without time zone as a local Date; rebuild from its parts.
  const iso = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes())).toISOString();
  return interval === "hour" ? iso.slice(0, 16).replace("T", " ") : iso.slice(0, 10);
}

export function checkKey(key: unknown, what = "property"): string {
  if (typeof key !== "string" || key.length < 1 || key.length > 200) throw new QueryError(`Name the ${what} (1 to 200 characters).`);
  return key;
}
