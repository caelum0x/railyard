// Destination transforms (A23): what a destination receives, declared per
// destination and applied by the exports runner before any send, the same for
// every kind (webhook, S3, Slack, Discord, HubSpot, ClickHouse, BigQuery).
//
//   { "where":  [{ "property": "plan", "op": "eq", "value": "pro" }, ...],
//     "drop":   ["$ip", "email"],
//     "keep":   ["plan", "amount"],
//     "rename": { "amount": "revenue" },
//     "set":    { "source": "railyard" } }
//
// `where` keeps the events matching every condition (on a property, or on
// `event` / `distinct_id`); then `keep` (an allow list) or `drop` (a deny
// list), `rename` and `set` reshape the properties. Events are never mutated
// in place. Declarative only: there is no code to run, so a transform can
// never reach the network or the database.

import { ApiError } from "../replay/shared/console-guard.ts";
import type { ExportedEvent } from "./runner.ts";

type Scalar = string | number | boolean;
const OPS = ["eq", "neq", "in", "not_in", "exists", "not_exists", "contains", "gt", "lt"] as const;
type Op = (typeof OPS)[number];

export interface Condition {
  property: string;
  op: Op;
  value?: Scalar | Scalar[];
}

export interface Transform {
  where?: Condition[];
  drop?: string[];
  keep?: string[];
  rename?: Record<string, string>;
  set?: Record<string, Scalar>;
}

const MAX_ITEMS = 50;
const KEY = /^[$A-Za-z0-9_.:\-]{1,200}$/;
const isScalar = (v: unknown): v is Scalar => typeof v === "string" || typeof v === "number" || typeof v === "boolean";

function keyList(v: unknown, what: string): string[] {
  if (!Array.isArray(v) || v.length === 0 || v.length > MAX_ITEMS || !v.every((k) => typeof k === "string" && KEY.test(k))) {
    throw new ApiError(400, `The transform's ${what} must list 1 to ${MAX_ITEMS} property names.`);
  }
  return v as string[];
}

/** Validates a transform from the API; `null` when none is given. */
export function checkTransform(raw: unknown): Transform | null {
  if (raw === undefined || raw === null) return null;
  if (typeof raw !== "object" || Array.isArray(raw)) throw new ApiError(400, "The transform must be an object.");
  const r = raw as Record<string, unknown>;
  const unknown = Object.keys(r).filter((k) => !["where", "drop", "keep", "rename", "set"].includes(k));
  if (unknown.length) throw new ApiError(400, `The transform has unknown fields: ${unknown.join(", ")}.`);
  const out: Transform = {};
  if (r.where !== undefined) {
    if (!Array.isArray(r.where) || r.where.length === 0 || r.where.length > MAX_ITEMS) throw new ApiError(400, `The transform's where must list 1 to ${MAX_ITEMS} conditions.`);
    out.where = r.where.map((c, i) => {
      const cond = (c ?? {}) as Record<string, unknown>;
      if (typeof cond.property !== "string" || !KEY.test(cond.property)) throw new ApiError(400, `Condition ${i + 1} needs a property name.`);
      if (typeof cond.op !== "string" || !(OPS as readonly string[]).includes(cond.op)) throw new ApiError(400, `Condition ${i + 1} has an unknown op; use ${OPS.join(", ")}.`);
      const op = cond.op as Op;
      if (op === "exists" || op === "not_exists") return { property: cond.property, op };
      if (op === "in" || op === "not_in") {
        if (!Array.isArray(cond.value) || cond.value.length === 0 || cond.value.length > MAX_ITEMS || !cond.value.every(isScalar)) {
          throw new ApiError(400, `Condition ${i + 1} needs a list of 1 to ${MAX_ITEMS} values.`);
        }
        return { property: cond.property, op, value: cond.value as Scalar[] };
      }
      if ((op === "gt" || op === "lt") && typeof cond.value !== "number") throw new ApiError(400, `Condition ${i + 1} needs a number.`);
      if (!isScalar(cond.value)) throw new ApiError(400, `Condition ${i + 1} needs a value.`);
      return { property: cond.property, op, value: cond.value };
    });
  }
  if (r.drop !== undefined && r.keep !== undefined) throw new ApiError(400, "Use keep or drop in a transform, not both.");
  if (r.drop !== undefined) out.drop = keyList(r.drop, "drop");
  if (r.keep !== undefined) out.keep = keyList(r.keep, "keep");
  for (const [field, label] of [["rename", "rename"], ["set", "set"]] as const) {
    const m = r[field];
    if (m === undefined) continue;
    if (!m || typeof m !== "object" || Array.isArray(m)) throw new ApiError(400, `The transform's ${label} must be an object.`);
    const entries = Object.entries(m as Record<string, unknown>);
    if (entries.length === 0 || entries.length > MAX_ITEMS) throw new ApiError(400, `The transform's ${label} must have 1 to ${MAX_ITEMS} entries.`);
    for (const [k, v] of entries) {
      if (!KEY.test(k)) throw new ApiError(400, `The transform's ${label} has an invalid property name.`);
      if (field === "rename" && (typeof v !== "string" || !KEY.test(v))) throw new ApiError(400, "The transform's rename must map names to names.");
      if (field === "set" && (!isScalar(v) || (typeof v === "string" && v.length > 1000))) throw new ApiError(400, "The transform's set values must be short text, numbers or true/false.");
    }
    if (field === "rename") out.rename = m as Record<string, string>;
    else out.set = m as Record<string, Scalar>;
  }
  return Object.keys(out).length ? out : null;
}

function valueOf(e: ExportedEvent, property: string): unknown {
  if (property === "event") return e.event;
  if (property === "distinct_id") return e.distinct_id;
  return e.properties?.[property];
}

function matches(e: ExportedEvent, c: Condition): boolean {
  const v = valueOf(e, c.property);
  switch (c.op) {
    case "exists":
      return v !== undefined && v !== null;
    case "not_exists":
      return v === undefined || v === null;
    case "eq":
      return v === c.value || (v !== undefined && v !== null && String(v) === String(c.value));
    case "neq":
      return !(v === c.value || (v !== undefined && v !== null && String(v) === String(c.value)));
    case "in":
      return (c.value as Scalar[]).some((x) => v !== undefined && v !== null && String(v) === String(x));
    case "not_in":
      return !(c.value as Scalar[]).some((x) => v !== undefined && v !== null && String(v) === String(x));
    case "contains":
      return typeof v === "string" && v.includes(String(c.value));
    case "gt":
      return typeof v === "number" ? v > (c.value as number) : Number(v) > (c.value as number);
    case "lt":
      return typeof v === "number" ? v < (c.value as number) : Number(v) < (c.value as number);
  }
}

function reshape(props: Record<string, unknown>, t: Transform): Record<string, unknown> {
  let out: Record<string, unknown> = t.keep
    ? Object.fromEntries(Object.entries(props).filter(([k]) => t.keep!.includes(k)))
    : t.drop
      ? Object.fromEntries(Object.entries(props).filter(([k]) => !t.drop!.includes(k)))
      : { ...props };
  if (t.rename) {
    out = Object.fromEntries(Object.entries(out).map(([k, v]) => [t.rename![k] ?? k, v]));
  }
  if (t.set) out = { ...out, ...t.set };
  return out;
}

/** The events a destination receives: filtered, then reshaped (new objects). */
export function applyTransform<T extends ExportedEvent>(t: Transform | null | undefined, events: T[]): T[] {
  if (!t) return events;
  return events
    .filter((e) => (t.where ?? []).every((c) => matches(e, c)))
    .map((e) => (t.drop || t.keep || t.rename || t.set ? { ...e, properties: reshape(e.properties ?? {}, t) } : e));
}
