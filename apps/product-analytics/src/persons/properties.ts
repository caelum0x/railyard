// Person property updates, following PostHog's person semantics (MIT, commit
// cbb1e9253027, nodejs/src/common/persons/person-property-utils.ts and the
// person update steps): $set overwrites, $set_once only fills missing keys,
// $unset removes. Event properties in EVENT_TO_PERSON_PROPERTIES are copied
// onto the person ($set) and their first value is kept as $initial_* ($set_once).

const EVENT_TO_PERSON_PROPERTIES = new Set([
  "$app_build",
  "$app_name",
  "$app_namespace",
  "$app_version",
  "$browser",
  "$browser_version",
  "$device_type",
  "$current_url",
  "$pathname",
  "$os",
  "$os_name",
  "$os_version",
  "$referring_domain",
  "$referrer",
  "$screen_height",
  "$screen_width",
  "$viewport_height",
  "$viewport_width",
  "$raw_user_agent",
  "$geoip_country_code",
]);

const CAMPAIGN_PROPERTIES = new Set([
  "utm_source",
  "utm_medium",
  "utm_campaign",
  "utm_content",
  "utm_name",
  "utm_term",
  "gclid",
  "gad_source",
  "gclsrc",
  "dclid",
  "gbraid",
  "wbraid",
  "fbclid",
  "msclkid",
  "twclid",
  "li_fat_id",
  "mc_cid",
  "igshid",
  "ttclid",
  "rdt_cid",
  "irclid",
  "_kx",
  "epik",
  "qclid",
  "sccid",
]);

export const eventToPersonProperties = new Set([...EVENT_TO_PERSON_PROPERTIES, ...CAMPAIGN_PROPERTIES]);

export const initialKey = (key: string) => `$initial_${key.replace("$", "")}`;

export interface PropertyOps {
  set: Record<string, unknown>;
  setOnce: Record<string, unknown>;
  unset: string[];
  /** Mixpanel `$add`: numeric increment (negative to decrement). */
  add: Record<string, unknown>;
  /** Mixpanel `$append`: push one value onto a list property, creating it if absent. */
  append: Record<string, unknown>;
  /** Mixpanel `$union`: merge value(s) into a list property, de-duplicated. */
  union: Record<string, unknown>;
  /** Mixpanel `$remove`: drop one scalar value from a list property. */
  remove: Record<string, unknown>;
}

type Obj = Record<string, unknown>;

const EMPTY_OBJ: Obj = {};

/** The ops an event applies to its person. `updateAll` is off for personless events. */
export function eventOps(ev: {
  properties: Obj;
  set: Obj | null;
  set_once: Obj | null;
  unset: string[] | null;
  event: string;
  add?: Obj | null;
  append?: Obj | null;
  union?: Obj | null;
  remove?: Obj | null;
}): PropertyOps {
  const set: Obj = {};
  const setOnce: Obj = {};
  // $pageview-style properties first, so an explicit $set of the same key wins.
  if (ev.event !== "$groupidentify") {
    for (const [k, v] of Object.entries(ev.properties)) {
      if (!eventToPersonProperties.has(k) || v === undefined) continue;
      set[k] = v;
      setOnce[initialKey(k)] = v;
    }
  }
  Object.assign(setOnce, ev.set_once ?? {});
  Object.assign(set, ev.set ?? {});
  return { set, setOnce, unset: ev.unset ?? [], add: ev.add ?? EMPTY_OBJ, append: ev.append ?? EMPTY_OBJ, union: ev.union ?? EMPTY_OBJ, remove: ev.remove ?? EMPTY_OBJ };
}

/** New properties after the ops, or null when nothing changes. Never mutates `current`. Order: set_once, set, add, append, union, remove, unset. */
export function applyOps(current: Obj, ops: PropertyOps): Obj | null {
  const next: Obj = { ...current };
  let changed = false;
  for (const [k, v] of Object.entries(ops.setOnce)) {
    if (!(k in next)) {
      next[k] = v;
      changed = true;
    }
  }
  for (const [k, v] of Object.entries(ops.set)) {
    if (JSON.stringify(next[k]) !== JSON.stringify(v)) {
      next[k] = v;
      changed = true;
    }
  }
  for (const [k, v] of Object.entries(ops.add)) {
    const by = typeof v === "number" ? v : Number(v);
    if (!Number.isFinite(by)) continue;
    const cur = typeof next[k] === "number" ? (next[k] as number) : 0;
    next[k] = cur + by;
    changed = true;
  }
  for (const [k, v] of Object.entries(ops.append)) {
    const cur = Array.isArray(next[k]) ? (next[k] as unknown[]) : next[k] !== undefined ? [next[k]] : [];
    next[k] = [...cur, v];
    changed = true;
  }
  for (const [k, raw] of Object.entries(ops.union)) {
    const values = Array.isArray(raw) ? raw : [raw];
    const cur = Array.isArray(next[k]) ? (next[k] as unknown[]) : [];
    const merged = [...cur];
    for (const v of values) if (!merged.some((x) => JSON.stringify(x) === JSON.stringify(v))) merged.push(v);
    if (merged.length !== cur.length) {
      next[k] = merged;
      changed = true;
    }
  }
  for (const [k, v] of Object.entries(ops.remove)) {
    if (!Array.isArray(next[k])) continue;
    const filtered = (next[k] as unknown[]).filter((x) => JSON.stringify(x) !== JSON.stringify(v));
    if (filtered.length !== (next[k] as unknown[]).length) {
      next[k] = filtered;
      changed = true;
    }
  }
  for (const k of ops.unset) {
    if (k in next) {
      delete next[k];
      changed = true;
    }
  }
  return changed ? next : null;
}
