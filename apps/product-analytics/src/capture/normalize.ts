// From a decoded payload to validated events. Shapes accepted:
//   one event object, an array of events, or {batch: [...], api_key, sent_at}.
// The token is the app's public site key: api_key, token, $token or
// properties.token. Any invalid event fails the whole request (never partly
// stored). `$ip` is removed and no address is kept; the country comes from
// the CF-IPCountry header.

import { randomUUID } from "node:crypto";
import { isUuid } from "../db.ts";

export const MAX_EVENTS_PER_REQUEST = 1000;
export const MAX_NAME = 200;
const FUTURE_TOLERANCE_MS = 23 * 3600 * 1000;

export class EventError extends Error {
  status: 400 | 401 | 413;
  constructor(status: 400 | 401 | 413, message: string) {
    super(message);
    this.status = status;
  }
}

export interface CapturedEvent {
  uuid: string;
  event: string;
  distinct_id: string;
  timestamp: Date;
  properties: Record<string, unknown>;
  set: Record<string, unknown> | null;
  set_once: Record<string, unknown> | null;
  unset: string[] | null;
  elements_chain: string | null;
  session_id: string | null;
  window_id: string | null;
  /** group type -> key, from properties.$groups */
  groups: Record<string, string>;
}

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

/** Properties never kept: client addresses. */
export const DROPPED_PROPERTIES = ["$ip", "$raw_ip", "ip"] as const;

function str(v: unknown): string | null {
  if (typeof v === "string") return v;
  if (typeof v === "number" && Number.isFinite(v)) return String(v);
  return null;
}

function parseTime(v: unknown): number | null {
  if (typeof v === "number" && Number.isFinite(v)) return v > 1e12 ? v : v * 1000;
  if (typeof v === "string" && v) {
    const t = Date.parse(v);
    return Number.isNaN(t) ? null : t;
  }
  return null;
}

/** Clock-skew correction as the SDKs expect: now + (timestamp - sent_at), else now - offset. */
export function eventTime(ev: Obj, props: Obj, sentAt: number | null, now: number): Date {
  const ts = parseTime(ev.timestamp ?? props.$time);
  let t: number;
  if (ts !== null && sentAt !== null && props.$ignore_sent_at !== true) t = now + (ts - sentAt);
  else if (typeof ev.offset === "number" && Number.isFinite(ev.offset)) t = now - ev.offset;
  else t = ts ?? now;
  if (t > now + FUTURE_TOLERANCE_MS) t = now;
  return new Date(t);
}

// The `$elements` array some SDK versions send, as the elements chain string.
export function elementsToChain(elements: unknown): string | null {
  if (!Array.isArray(elements) || elements.length === 0) return null;
  const esc = (s: string) => s.replace(/"/g, '\\"');
  return elements
    .filter(isObj)
    .map((el) => {
      let out = typeof el.tag_name === "string" ? el.tag_name : "";
      const classes = Array.isArray(el.attr__class) ? el.attr__class : typeof el.attr__class === "string" ? el.attr__class.split(" ") : [];
      for (const c of classes) if (typeof c === "string" && c) out += `.${c.replace(/"/g, "")}`;
      const attrs: string[] = [];
      if (typeof el.$el_text === "string") attrs.push(`text="${esc(el.$el_text)}"`);
      if (typeof el.attr__href === "string") attrs.push(`href="${esc(el.attr__href)}"`);
      if (typeof el.attr__id === "string") attrs.push(`attr_id="${esc(el.attr__id)}"`);
      if (typeof el.nth_child === "number") attrs.push(`nth-child="${el.nth_child}"`);
      if (typeof el.nth_of_type === "number") attrs.push(`nth-of-type="${el.nth_of_type}"`);
      for (const [k, v] of Object.entries(el)) if (k.startsWith("attr__") && k !== "attr__class" && typeof v === "string") attrs.push(`${k}="${esc(v)}"`);
      return attrs.length ? `${out}:${attrs.sort().join("")}` : out;
    })
    .join(";");
}

/** A copy without address keys (also used for nested $set / $set_once). */
export function withoutIp(o: Obj): Obj {
  const out: Obj = { ...o };
  for (const k of DROPPED_PROPERTIES) delete out[k];
  return out;
}

function objOrNull(v: unknown): Obj | null {
  return isObj(v) && Object.keys(v).length > 0 ? withoutIp(v) : null;
}

export interface Normalized {
  token: string;
  events: CapturedEvent[];
}

export function tokenOf(payload: unknown, first: Obj | null, queryToken: string | null): string | null {
  const p = isObj(payload) ? payload : {};
  const props = first && isObj(first.properties) ? first.properties : {};
  const t = p.api_key ?? p.token ?? p.$token ?? first?.api_key ?? first?.token ?? first?.$token ?? props.token ?? props.$token ?? queryToken;
  return typeof t === "string" && t ? t : null;
}

export function normalizeEvents(payload: unknown, opts: { now: Date; country: string | null; queryToken: string | null; sentAtQuery: number | null }): Normalized {
  let list: unknown[];
  let sentAt: number | null = opts.sentAtQuery;
  if (Array.isArray(payload)) list = payload;
  else if (isObj(payload) && Array.isArray(payload.batch)) {
    list = payload.batch;
    sentAt = parseTime(payload.sent_at) ?? sentAt;
  } else if (isObj(payload) && typeof payload.event === "string") list = [payload];
  else throw new EventError(400, "Send one event, a list of events, or a batch.");
  if (list.length === 0) throw new EventError(400, "The batch is empty.");
  if (list.length > MAX_EVENTS_PER_REQUEST) throw new EventError(413, `Send at most ${MAX_EVENTS_PER_REQUEST} events per request.`);
  if (isObj(payload) && !Array.isArray(payload) && sentAt === null) sentAt = parseTime(payload.sent_at);

  const token = tokenOf(payload, isObj(list[0]) ? list[0] : null, opts.queryToken);
  if (!token) throw new EventError(401, "The project token is missing.");

  const now = opts.now.getTime();
  const events = list.map((raw, i): CapturedEvent => {
    if (!isObj(raw)) throw new EventError(400, `Event ${i} is not an object.`);
    const event = raw.event;
    if (typeof event !== "string" || event.length === 0 || event.length > MAX_NAME) throw new EventError(400, `Event ${i} needs a name of 1 to ${MAX_NAME} characters.`);
    const props: Obj = isObj(raw.properties) ? { ...raw.properties } : {};
    const evToken = raw.api_key ?? raw.token ?? props.token;
    if (typeof evToken === "string" && evToken !== token) throw new EventError(400, "Every event of one request must use the same project token.");
    const distinct = str(raw.distinct_id ?? props.distinct_id ?? raw.$distinct_id);
    if (!distinct || distinct.trim() === "" || distinct.length > MAX_NAME) throw new EventError(400, `Event ${i} needs a distinct_id of 1 to ${MAX_NAME} characters.`);
    let uuid = raw.uuid ?? props.$insert_id;
    if (uuid !== undefined && !isUuid(uuid)) throw new EventError(400, `Event ${i} has an invalid uuid.`);
    uuid = typeof uuid === "string" ? uuid.toLowerCase() : randomUUID();

    for (const k of DROPPED_PROPERTIES) delete props[k];
    for (const k of ["$set", "$set_once"]) if (isObj(props[k])) props[k] = withoutIp(props[k] as Obj);
    delete props.token;
    delete props.distinct_id;
    if (opts.country && /^[A-Z]{2}$/.test(opts.country) && opts.country !== "XX" && opts.country !== "T1") props.$geoip_country_code = opts.country;

    let chain = typeof props.$elements_chain === "string" ? props.$elements_chain : typeof raw.elements_chain === "string" ? raw.elements_chain : null;
    if (!chain) chain = elementsToChain(props.$elements ?? raw.elements);
    delete props.$elements;
    delete props.$elements_chain;

    const set = objOrNull(props.$set) ?? objOrNull(raw.$set);
    const setOnce = objOrNull(props.$set_once) ?? objOrNull(raw.$set_once);
    const unsetRaw = props.$unset ?? raw.$unset;
    const unset = Array.isArray(unsetRaw) ? unsetRaw.filter((k): k is string => typeof k === "string") : isObj(unsetRaw) ? Object.keys(unsetRaw) : null;

    const groups: Record<string, string> = {};
    if (isObj(props.$groups)) {
      for (const [type, key] of Object.entries(props.$groups)) {
        const k = str(key);
        if (type && type.length <= 100 && k && k.length <= 400) groups[type] = k;
      }
    }
    return {
      uuid: uuid as string,
      event,
      distinct_id: distinct,
      timestamp: eventTime(raw, props, sentAt, now),
      properties: props,
      set,
      set_once: setOnce,
      unset: unset && unset.length ? unset : null,
      elements_chain: chain,
      session_id: str(props.$session_id)?.slice(0, 200) ?? null,
      window_id: str(props.$window_id)?.slice(0, 200) ?? null,
      groups,
    };
  });
  return { token, events };
}
