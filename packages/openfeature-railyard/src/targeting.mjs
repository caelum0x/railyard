// Railyard Flags targeting (Wave Q QY1.3, I10 and A19): rules by person
// property, cohort or segment, and percentage rollouts with a stable hash.
//
// One file, no dependencies, no I/O: the console's read endpoint, the
// product analytics /flags answer and the OpenFeature providers all run this
// same code, so a person gets the same answer everywhere. Byte copies live in
// apps/console/lib/flags/targeting.mjs and
// apps/product-analytics/src/flags/targeting.mjs (drift tests keep them equal).
//
// Semantics follow the open-source product analytics flag matcher (MIT;
// posthog-node local evaluation: match_property and the condition-group
// loop) so the SDKs' own local evaluation agrees (test/qy in
// product-analytics checks 10 000 ids against it):
//   - a flag has ordered rules; a rule matches when all its conditions match
//     AND the id falls inside its rollout:
//       hash = parseInt(sha1("<flag>.<id>")[0:15], 16) / 0xfffffffffffffff
//       inside when hash <= rollout / 100
//   - the first matching rule wins (its value, else the flag's value);
//   - no rule matches: off. A switched-off flag is off for everyone.
// Segments are named, reusable sets of property conditions per environment
// (the shape of Flagsmith segments, BSD-3-Clause). Cohorts are product
// analytics cohort ids; membership comes from the evaluation context.

export const LONG_SCALE = 0xfffffffffffffff;
export const OPERATORS = Object.freeze(["exact", "is_not", "icontains", "not_icontains", "regex", "not_regex", "gt", "gte", "lt", "lte", "is_set", "is_not_set"]);
export const MAX_RULES = 20;
export const MAX_CONDITIONS = 10;
export const MAX_SEGMENTS = 50;
export const MAX_TEXT = 200;
export const MAX_LIST = 100;
export const SEGMENT_KEY = /^[a-z][a-z0-9_]{0,63}$/;
export const PROPERTY_KEY = /^[A-Za-z0-9$_.:-]{1,100}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

// ---------------------------------------------------------------- hashing

function utf8(text) {
  return new TextEncoder().encode(text);
}

/** SHA-1 of a string, as lowercase hex (synchronous, for client-side resolution). */
export function sha1Hex(text) {
  const bytes = utf8(text);
  const bitLen = bytes.length * 8;
  const words = new Uint32Array((((bytes.length + 8) >> 6) + 1) * 16);
  for (let i = 0; i < bytes.length; i += 1) words[i >> 2] |= bytes[i] << (24 - (i % 4) * 8);
  words[bytes.length >> 2] |= 0x80 << (24 - (bytes.length % 4) * 8);
  words[words.length - 1] = bitLen >>> 0;
  words[words.length - 2] = Math.floor(bitLen / 0x100000000);
  let h0 = 0x67452301;
  let h1 = 0xefcdab89;
  let h2 = 0x98badcfe;
  let h3 = 0x10325476;
  let h4 = 0xc3d2e1f0;
  const w = new Uint32Array(80);
  const rotl = (x, n) => (x << n) | (x >>> (32 - n));
  for (let block = 0; block < words.length; block += 16) {
    for (let t = 0; t < 16; t += 1) w[t] = words[block + t];
    for (let t = 16; t < 80; t += 1) w[t] = rotl(w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16], 1);
    let a = h0;
    let b = h1;
    let c = h2;
    let d = h3;
    let e = h4;
    for (let t = 0; t < 80; t += 1) {
      const f = t < 20 ? (b & c) | (~b & d) : t < 40 ? b ^ c ^ d : t < 60 ? (b & c) | (b & d) | (c & d) : b ^ c ^ d;
      const k = t < 20 ? 0x5a827999 : t < 40 ? 0x6ed9eba1 : t < 60 ? 0x8f1bbcdc : 0xca62c1d6;
      const tmp = (rotl(a, 5) + f + e + k + w[t]) >>> 0;
      e = d;
      d = c;
      c = rotl(b, 30) >>> 0;
      b = a;
      a = tmp;
    }
    h0 = (h0 + a) >>> 0;
    h1 = (h1 + b) >>> 0;
    h2 = (h2 + c) >>> 0;
    h3 = (h3 + d) >>> 0;
    h4 = (h4 + e) >>> 0;
  }
  return [h0, h1, h2, h3, h4].map((x) => x.toString(16).padStart(8, "0")).join("");
}

/** The stable rollout hash of one id for one flag, in [0, 1]. */
export function flagHash(key, id, salt = "") {
  return parseInt(sha1Hex(`${key}.${id}${salt}`).slice(0, 15), 16) / LONG_SCALE;
}

// ---------------------------------------------------------------- validation

const isObj = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
const scalar = (v) => (typeof v === "string" && v.length <= MAX_TEXT) || (typeof v === "number" && Number.isFinite(v)) || typeof v === "boolean";

function parseCondition(raw, i, j, opts) {
  const where = `rule ${i + 1}, condition ${j + 1}`;
  if (!isObj(raw)) return `${where} must be an object`;
  if (raw.type === "property") {
    if (typeof raw.key !== "string" || !PROPERTY_KEY.test(raw.key)) return `${where}: the property name must be 1 to 100 letters, digits or $_.:-`;
    const operator = raw.operator ?? "exact";
    if (!OPERATORS.includes(operator)) return `${where}: the operator must be one of ${OPERATORS.join(", ")}`;
    if (operator === "is_set" || operator === "is_not_set") return { type: "property", key: raw.key, operator };
    const value = raw.value;
    if (Array.isArray(value)) {
      if (!["exact", "is_not"].includes(operator)) return `${where}: a list of values works with exact and is_not only`;
      if (value.length < 1 || value.length > MAX_LIST || !value.every(scalar)) return `${where}: the list needs 1 to ${MAX_LIST} values, each text (${MAX_TEXT} characters at most), a number or true/false`;
    } else if (!scalar(value)) return `${where}: the value must be text (${MAX_TEXT} characters at most), a number or true/false`;
    if (operator === "regex" || operator === "not_regex") {
      try {
        new RegExp(String(value));
      } catch {
        return `${where}: the pattern is not a valid regular expression`;
      }
    }
    if (["gt", "gte", "lt", "lte"].includes(operator) && typeof value === "boolean") return `${where}: compare with a number or text`;
    return { type: "property", key: raw.key, operator, value: Array.isArray(value) ? [...value] : value };
  }
  if (raw.type === "cohort") {
    if (typeof raw.id !== "string" || !UUID.test(raw.id)) return `${where}: the cohort must be a cohort id`;
    return { type: "cohort", id: raw.id };
  }
  if (raw.type === "segment") {
    if (opts.inSegment) return `${where}: a segment holds property conditions only`;
    if (typeof raw.key !== "string" || !SEGMENT_KEY.test(raw.key)) return `${where}: the segment key must start with a letter and use lowercase letters, digits and underscores`;
    if (opts.segments && !opts.segments.has(raw.key)) return `${where}: there is no segment "${raw.key}" in this environment`;
    return { type: "segment", key: raw.key };
  }
  return `${where}: the type must be property, cohort or segment`;
}

function parseConditions(raw, i, opts) {
  if (raw === undefined) return [];
  if (!Array.isArray(raw) || raw.length > MAX_CONDITIONS) return `rule ${i + 1} takes at most ${MAX_CONDITIONS} conditions`;
  const out = [];
  for (let j = 0; j < raw.length; j += 1) {
    const c = parseCondition(raw[j], i, j, opts);
    if (typeof c === "string") return c;
    if (opts.inSegment && c.type !== "property") return `condition ${j + 1}: a segment holds property conditions only`;
    out.push(c);
  }
  return out;
}

/**
 * Validates a flag's targeting. `null` clears it. `segments` (a Set of the
 * environment's segment keys) makes a reference to a missing segment an error.
 * @returns {{rules: object[]} | null | string} the clean targeting, null, or an error sentence
 */
export function parseTargeting(raw, segments) {
  if (raw === null) return null;
  if (!isObj(raw)) return "targeting must be an object with rules, or null";
  const rules = raw.rules;
  if (!Array.isArray(rules) || rules.length < 1 || rules.length > MAX_RULES) return `targeting needs 1 to ${MAX_RULES} rules`;
  const out = [];
  for (let i = 0; i < rules.length; i += 1) {
    const r = rules[i];
    if (!isObj(r)) return `rule ${i + 1} must be an object`;
    const conditions = parseConditions(r.conditions, i, { segments });
    if (typeof conditions === "string") return conditions;
    const rollout = r.rollout ?? 100;
    if (typeof rollout !== "number" || !Number.isFinite(rollout) || rollout < 0 || rollout > 100) return `rule ${i + 1}: rollout must be a percentage from 0 to 100`;
    const rule = { conditions, rollout: Math.round(rollout * 100) / 100 };
    if (r.value !== undefined) {
      if (r.value !== null && (typeof r.value !== "string" || r.value.length > 2000)) return `rule ${i + 1}: value must be text of at most 2000 characters, or null`;
      rule.value = r.value;
    }
    out.push(rule);
  }
  return { rules: out };
}

/** Validates a segment: `{key, name?, conditions}` with property conditions only. */
export function parseSegment(raw) {
  if (!isObj(raw)) return "the segment must be an object";
  if (typeof raw.key !== "string" || !SEGMENT_KEY.test(raw.key)) return "the segment key must start with a letter and use lowercase letters, digits and underscores (64 at most)";
  const name = raw.name === undefined || raw.name === null ? raw.key : raw.name;
  if (typeof name !== "string" || name.length < 1 || name.length > 80) return "the segment name must be 1 to 80 characters";
  if (!Array.isArray(raw.conditions) || raw.conditions.length < 1) return "a segment needs at least one condition";
  const conditions = parseConditions(raw.conditions, 0, { inSegment: true });
  if (typeof conditions === "string") return conditions.replace(/^rule 1, /, "");
  return { key: raw.key, name, conditions };
}

/** Segment keys a targeting refers to. */
export function segmentRefs(targeting) {
  const out = new Set();
  for (const r of targeting?.rules ?? []) for (const c of r.conditions) if (c.type === "segment") out.add(c.key);
  return out;
}

// ---------------------------------------------------------------- matching

// Lowercasing of A-Z only, as the reference matcher does for contains.
const asciiLower = (v) => String(v).replace(/[A-Z]/g, (c) => c.toLowerCase());
const exactString = (v) => (typeof v === "string" ? v : v === null ? "null" : typeof v === "object" ? JSON.stringify(v) : String(v));

function isBoolish(v) {
  if (typeof v === "boolean") return true;
  if (typeof v === "string") return v.toLowerCase() === "true" || v.toLowerCase() === "false";
  return Array.isArray(v) && v.every(isBoolish);
}
function truthy(v) {
  if (typeof v === "boolean") return v;
  if (typeof v === "string") return v.toLowerCase() === "true";
  return Array.isArray(v) && v.every(truthy);
}

function exactMatch(target, actual) {
  // true/false targets compare truthiness ("TRUE" equals true).
  if (isBoolish(target) || (Array.isArray(target) && target.length === 0)) return truthy(target) === truthy(actual);
  const a = exactString(actual).toLowerCase();
  return (Array.isArray(target) ? target : [target]).some((v) => exactString(v).toLowerCase() === a);
}

function compare(actual, expected, op) {
  const b = typeof expected === "number" ? expected : parseFloat(String(expected));
  const a = typeof actual === "number" ? actual : actual !== null && actual !== undefined ? parseFloat(String(actual)) : NaN;
  const [l, r] = Number.isFinite(a) && Number.isFinite(b) ? [a, b] : [String(actual), String(expected)];
  return op === "gt" ? l > r : op === "gte" ? l >= r : op === "lt" ? l < r : l <= r;
}

const regexCache = new Map();
function regex(pattern) {
  if (!regexCache.has(pattern)) {
    if (regexCache.size > 500) regexCache.clear();
    let re = null;
    try {
      re = new RegExp(pattern);
    } catch {
      re = null;
    }
    regexCache.set(pattern, re);
  }
  return regexCache.get(pattern);
}

/**
 * One property condition against the person's properties. A property the
 * person does not have matches only is_not_set (the server-side rule; the
 * reference SDKs ask the server in that case).
 */
export function matchProperty(cond, properties) {
  const has = properties != null && Object.prototype.hasOwnProperty.call(properties, cond.key);
  if (!has) return cond.operator === "is_not_set";
  if (cond.operator === "is_not_set") return false;
  if (cond.operator === "is_set") return true;
  const actual = properties[cond.key];
  if (actual === undefined) return cond.operator === "is_not";
  if (actual === null && cond.operator !== "exact" && cond.operator !== "is_not") return false;
  switch (cond.operator) {
    case "exact":
      return exactMatch(cond.value, actual);
    case "is_not":
      return !exactMatch(cond.value, actual);
    case "icontains":
      return asciiLower(actual).includes(asciiLower(cond.value));
    case "not_icontains":
      return !asciiLower(actual).includes(asciiLower(cond.value));
    case "regex": {
      const re = regex(String(cond.value));
      return re !== null && re.test(String(actual));
    }
    case "not_regex": {
      const re = regex(String(cond.value));
      return re !== null && !re.test(String(actual));
    }
    case "gt":
    case "gte":
    case "lt":
    case "lte":
      return compare(actual, cond.value, cond.operator);
    default:
      return false;
  }
}

function matchCondition(cond, ctx, segments) {
  if (cond.type === "property") return matchProperty(cond, ctx.properties);
  if (cond.type === "cohort") return Array.isArray(ctx.cohorts) && ctx.cohorts.includes(cond.id);
  if (cond.type === "segment") {
    const seg = segments?.[cond.key];
    return Boolean(seg) && seg.conditions.every((c) => matchProperty(c, ctx.properties));
  }
  return false;
}

/**
 * The answer of one flag for one person.
 * @param {string} key the flag key (part of the hash)
 * @param {{enabled: boolean, value: string | null, targeting?: {rules: object[]} | null}} flag
 * @param {{id?: string | null, properties?: Record<string, unknown>, cohorts?: string[]}} ctx
 * @param {Record<string, {conditions: object[]}>} [segments] the environment's segments by key
 * @returns {{enabled: boolean, value: string | null, reason: "DISABLED" | "STATIC" | "TARGETING_MATCH" | "NO_MATCH", rule: number | null}}
 */
export function evaluateFlag(key, flag, ctx = {}, segments = {}) {
  if (!flag || !flag.enabled) return { enabled: false, value: null, reason: "DISABLED", rule: null };
  const rules = flag.targeting?.rules;
  if (!rules || rules.length === 0) return { enabled: true, value: flag.value ?? null, reason: "STATIC", rule: null };
  const id = ctx.id === undefined || ctx.id === null ? "" : String(ctx.id);
  for (let i = 0; i < rules.length; i += 1) {
    const r = rules[i];
    if (!r.conditions.every((c) => matchCondition(c, ctx, segments))) continue;
    // Without an id only a full rollout can include the person.
    const inside = r.rollout >= 100 || (id !== "" && r.rollout > 0 && flagHash(key, id) <= r.rollout / 100);
    if (inside) return { enabled: true, value: r.value !== undefined ? r.value : (flag.value ?? null), reason: "TARGETING_MATCH", rule: i };
  }
  return { enabled: false, value: null, reason: "NO_MATCH", rule: null };
}

// ---------------------------------------------------------------- the read answer

/**
 * One flag as the read endpoint answers it: `enabled` and `value` for the
 * request's own context (none: an anonymous person), and, for a targeted
 * flag, `targeting: {on, value, rules}` so a client can evaluate any other
 * person locally with the same rules.
 */
export function answerFlag(key, flag, ctx = {}, segments = {}) {
  const r = evaluateFlag(key, flag, ctx, segments);
  const out = { enabled: r.enabled, value: r.enabled ? r.value : (flag.value ?? null) };
  if (flag.targeting?.rules?.length) out.targeting = { on: Boolean(flag.enabled), value: flag.value ?? null, rules: flag.targeting.rules };
  return out;
}

/** Evaluates a flag from a read answer for any person (the local evaluation of the SDKs and of /flags). */
export function evaluateAnswer(key, answered, ctx = {}, segments = {}) {
  if (!answered) return { enabled: false, value: null, reason: "DISABLED", rule: null };
  if (answered.targeting && Array.isArray(answered.targeting.rules)) {
    return evaluateFlag(key, { enabled: answered.targeting.on === true, value: answered.targeting.value ?? null, targeting: { rules: answered.targeting.rules } }, ctx, segments);
  }
  return answered.enabled ? { enabled: true, value: answered.value ?? null, reason: "STATIC", rule: null } : { enabled: false, value: null, reason: "DISABLED", rule: null };
}

/** The segments a set of answered flags refers to, from the environment's segments. */
export function referencedSegments(flags, segments) {
  const keys = new Set();
  for (const f of Object.values(flags)) for (const r of f.targeting?.rules ?? []) for (const c of r.conditions ?? []) if (c.type === "segment") keys.add(c.key);
  const out = {};
  for (const k of keys) if (segments[k]) out[k] = { conditions: segments[k].conditions };
  return out;
}
