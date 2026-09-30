// Recording payloads from the browser SDK's recorder: `$snapshot` events
// whose `$snapshot_data` holds rrweb events (MIT format), some with parts
// gzip-compressed into latin1 strings (marker cv "2024-10"). Here each event
// is decompressed back to plain rrweb, every input value is masked in the
// bytes we keep (whatever the page's own settings were), and the index
// counters (clicks, key presses, URLs, first and last timestamp) are read.
// Mobile wireframe inputs are masked too; console log events are dropped and
// network events lose their headers and bodies (the SDK lets the app turn
// these on locally, over the project config).

import { gunzipSync } from "node:zlib";

export type RrwebEvent = { type: number; timestamp: number; data?: unknown; [k: string]: unknown };

export interface SnapshotBatch {
  token: string | null;
  distinctId: string | null;
  sessionId: string;
  windowId: string | null;
  events: RrwebEvent[];
}

export class SnapshotError extends Error {}

const EventType = { FullSnapshot: 2, IncrementalSnapshot: 3, Meta: 4, Plugin: 6 } as const;
const Source = { Mutation: 0, MouseInteraction: 2, Input: 5, StyleSheetRule: 8 } as const;
const MouseClick = 2;
const ElementNode = 2;
const TextNode = 3;

/** Largest decompressed part of one event (a full snapshot of a very large page). */
export const MAX_PART_BYTES = 16 * 1024 * 1024;
const SESSION_ID = /^[A-Za-z0-9._:-]{1,200}$/;

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

function gunzipLatin1(s: string): unknown {
  let out: Buffer;
  try {
    out = gunzipSync(Buffer.from(s, "latin1"), { maxOutputLength: MAX_PART_BYTES });
  } catch (err) {
    if (err instanceof RangeError) throw new SnapshotError("a recording part is too large");
    throw new SnapshotError("a compressed recording part is not valid gzip");
  }
  try {
    return JSON.parse(out.toString("utf8"));
  } catch {
    throw new SnapshotError("a compressed recording part is not valid JSON");
  }
}

const unpack = (v: unknown) => (typeof v === "string" ? gunzipLatin1(v) : v);

/** Undoes the recorder's partial compression; plain events pass through. */
export function decompressEvent(e: RrwebEvent): RrwebEvent {
  if (e.cv === undefined) return e;
  const { cv: _cv, ...rest } = e;
  if (e.type === EventType.FullSnapshot) return { ...rest, data: unpack(e.data) } as RrwebEvent;
  if (e.type === EventType.IncrementalSnapshot && isObj(e.data)) {
    const d = e.data;
    if (d.source === Source.Mutation) {
      return { ...rest, data: { ...d, texts: unpack(d.texts), attributes: unpack(d.attributes), removes: unpack(d.removes), adds: unpack(d.adds) } } as RrwebEvent;
    }
    if (d.source === Source.StyleSheetRule) {
      return { ...rest, data: { ...d, ...(d.adds !== undefined ? { adds: unpack(d.adds) } : {}), ...(d.removes !== undefined ? { removes: unpack(d.removes) } : {}) } } as RrwebEvent;
    }
  }
  return rest as RrwebEvent;
}

const KEEP_VALUE_TYPES = new Set(["button", "submit", "reset", "image"]);
const stars = (v: unknown) => (typeof v === "string" ? "*".repeat(Math.min(v.length, 64)) : v);

/** A serialized node with every form value masked (input, textarea, select), children included. */
function maskNode(n: unknown): unknown {
  if (!isObj(n)) return n;
  let out: Obj = n;
  if (n.type === ElementNode) {
    const tag = typeof n.tagName === "string" ? n.tagName.toLowerCase() : "";
    const attrs = isObj(n.attributes) ? n.attributes : null;
    const isField = tag === "textarea" || tag === "select" || (tag === "input" && !KEEP_VALUE_TYPES.has(String(attrs?.type ?? "text").toLowerCase()));
    if (isField && attrs && "value" in attrs) out = { ...out, attributes: { ...attrs, value: stars(attrs.value) } };
    if (Array.isArray(n.childNodes)) {
      const kids = n.childNodes.map((k) => (tag === "textarea" && isObj(k) && k.type === TextNode ? { ...k, textContent: stars(k.textContent) } : maskNode(k)));
      out = { ...out, childNodes: kids };
    }
  } else if (Array.isArray(n.childNodes)) {
    out = { ...out, childNodes: n.childNodes.map(maskNode) };
  }
  return out;
}

/** Mobile SDK wireframes (React Native, iOS, Android): every input's value masked, buttons kept, children included. */
const KEEP_MOBILE_INPUTS = new Set(["button"]);
function maskWireframe(w: unknown): unknown {
  if (!isObj(w)) return w;
  let out: Obj = w;
  if (w.type === "input" && "value" in w && !KEEP_MOBILE_INPUTS.has(String(w.inputType ?? "").toLowerCase())) out = { ...out, value: stars(w.value) };
  if (Array.isArray(w.childWireframes)) out = { ...out, childWireframes: w.childWireframes.map(maskWireframe) };
  return out;
}
const maskWireframes = (v: unknown) => (Array.isArray(v) ? v.map(maskWireframe) : v);
const maskWireframeRefs = (v: unknown) => (Array.isArray(v) ? v.map((a) => (isObj(a) && "wireframe" in a ? { ...a, wireframe: maskWireframe(a.wireframe) } : a)) : v);

/** Request fields the network plugin can carry that may hold credentials or personal data. */
const NETWORK_PRIVATE = ["requestHeaders", "requestBody", "responseHeaders", "responseBody"];
const CONSOLE_PLUGIN = /^rrweb\/console@/;
const NETWORK_PLUGIN = /^rrweb\/network@/;

/**
 * Plugin events: console logs are not kept (the project's config turns them
 * off, but the app's local config wins in the SDK); network requests keep
 * their timing and address but lose headers and bodies. Returns null to drop.
 */
function maskPlugin(e: RrwebEvent): RrwebEvent | null {
  const d = isObj(e.data) ? e.data : null;
  const plugin = typeof d?.plugin === "string" ? d.plugin : "";
  if (CONSOLE_PLUGIN.test(plugin)) return null;
  if (!NETWORK_PLUGIN.test(plugin) || !d) return e;
  const payload = isObj(d.payload) ? d.payload : null;
  const requests = Array.isArray(payload?.requests)
    ? payload.requests.map((r) => {
        if (!isObj(r)) return r;
        const kept: Obj = {};
        for (const [k, v] of Object.entries(r)) if (!NETWORK_PRIVATE.includes(k)) kept[k] = v;
        return kept;
      })
    : [];
  return { ...e, data: { ...d, payload: { ...(payload ?? {}), requests } } };
}

/** The event with every typed value masked, or null when it is not kept at all. */
export function maskEvent(e: RrwebEvent): RrwebEvent | null {
  if (e.type === EventType.Plugin) return maskPlugin(e);
  if (e.type === EventType.FullSnapshot && isObj(e.data)) {
    const data: Obj = { ...e.data };
    if ("node" in data) data.node = maskNode(data.node);
    if ("wireframes" in data) data.wireframes = maskWireframes(data.wireframes);
    return { ...e, data };
  }
  if (e.type !== EventType.IncrementalSnapshot || !isObj(e.data)) return e;
  const d = e.data;
  if (d.source === Source.Input && "text" in d) return { ...e, data: { ...d, text: stars(d.text) } };
  if (d.source === Source.Mutation) {
    const adds = Array.isArray(d.adds)
      ? d.adds.map((a) => (isObj(a) ? { ...a, ...("node" in a ? { node: maskNode(a.node) } : {}), ...("wireframe" in a ? { wireframe: maskWireframe(a.wireframe) } : {}) } : a))
      : d.adds;
    const attributes = Array.isArray(d.attributes)
      ? d.attributes.map((a) => (isObj(a) && isObj(a.attributes) && "value" in a.attributes ? { ...a, attributes: { ...a.attributes, value: stars(a.attributes.value) } } : a))
      : d.attributes;
    return { ...e, data: { ...d, adds, attributes, ...("updates" in d ? { updates: maskWireframeRefs(d.updates) } : {}) } };
  }
  return e;
}

export interface ChunkStats {
  firstTs: number;
  lastTs: number;
  clicks: number;
  keypresses: number;
  urls: string[];
}

export function statsOf(events: RrwebEvent[]): ChunkStats {
  let firstTs = Infinity;
  let lastTs = -Infinity;
  let clicks = 0;
  let keypresses = 0;
  const urls: string[] = [];
  for (const e of events) {
    if (e.timestamp < firstTs) firstTs = e.timestamp;
    if (e.timestamp > lastTs) lastTs = e.timestamp;
    const d = isObj(e.data) ? e.data : null;
    if (e.type === EventType.IncrementalSnapshot && d) {
      if (d.source === Source.MouseInteraction && d.type === MouseClick) clicks += 1;
      if (d.source === Source.Input) keypresses += 1;
    }
    if (e.type === EventType.Meta && d && typeof d.href === "string" && !urls.includes(d.href)) urls.push(d.href.slice(0, 2000));
  }
  return { firstTs, lastTs, clicks, keypresses, urls };
}

function isEvent(v: unknown): v is RrwebEvent {
  return isObj(v) && typeof v.type === "number" && typeof v.timestamp === "number" && Number.isFinite(v.timestamp);
}

/** `$snapshot` events of a decoded capture payload, grouped per session and window, decompressed and masked. */
export function extractSnapshots(payload: unknown): SnapshotBatch[] {
  const list: unknown[] = Array.isArray(payload) ? payload : isObj(payload) && Array.isArray(payload.batch) ? payload.batch : [payload];
  const out: SnapshotBatch[] = [];
  for (const item of list) {
    if (!isObj(item) || item.event !== "$snapshot" || !isObj(item.properties)) continue;
    const p = item.properties;
    const sessionId = p.$session_id;
    if (typeof sessionId !== "string" || !SESSION_ID.test(sessionId)) throw new SnapshotError("a recording needs a session id");
    const raw = Array.isArray(p.$snapshot_data) ? p.$snapshot_data : [p.$snapshot_data];
    const events = raw
      .filter(isEvent)
      .map((e) => maskEvent(decompressEvent(e)))
      .filter((e): e is RrwebEvent => e !== null);
    if (events.length === 0) continue;
    const token = [p.token, p.$token, item.api_key, item.token, isObj(payload) ? payload.api_key : null].find((t) => typeof t === "string") as string | undefined;
    const distinct = [item.distinct_id, p.distinct_id, p.$distinct_id].find((d) => typeof d === "string" || typeof d === "number");
    out.push({
      token: token ?? null,
      distinctId: distinct === undefined ? null : String(distinct).slice(0, 200),
      sessionId,
      windowId: typeof p.$window_id === "string" ? p.$window_id.slice(0, 200) : null,
      events,
    });
  }
  return out;
}
