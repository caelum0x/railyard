// Ported from PostHog nodejs/src/common/persons/person-utils.ts (MIT, commit
// cbb1e9253027): ids no merge may involve. Plus the deterministic person id
// of a distinct id, so events sent before a person row exists (personless
// events) and the person created later agree, and merges link them through
// pa.person_overrides.

import { createHash } from "node:crypto";

const BARE_CASE_INSENSITIVE_ILLEGAL_IDS = [
  "anonymous",
  "guest",
  "distinctid",
  "distinct_id",
  "id",
  "not_authenticated",
  "email",
  "undefined",
  "true",
  "false",
];

const BARE_CASE_SENSITIVE_ILLEGAL_IDS = ["[object Object]", "NaN", "None", "none", "null", "0", "undefined"];

const quoted = (ids: string[]) => [...ids, ...ids.map((id) => `'${id}'`), ...ids.map((id) => `"${id}"`)];

const CASE_INSENSITIVE_ILLEGAL_IDS = new Set(quoted(BARE_CASE_INSENSITIVE_ILLEGAL_IDS));
const CASE_SENSITIVE_ILLEGAL_IDS = new Set(quoted(BARE_CASE_SENSITIVE_ILLEGAL_IDS));

export function isDistinctIdIllegal(id: string): boolean {
  return id.trim() === "" || CASE_INSENSITIVE_ILLEGAL_IDS.has(id.toLowerCase()) || CASE_SENSITIVE_ILLEGAL_IDS.has(id);
}

export function isDistinctIdUnmergeable(id: string): boolean {
  return isDistinctIdIllegal(id) || id.includes("\u0000") || [...id].length > 400;
}

/** UUID v5 of `distinct_id` in the namespace of the project id. */
export function personIdFor(projectId: string, distinctId: string): string {
  const ns = Buffer.from(projectId.replace(/-/g, ""), "hex");
  const b = createHash("sha1").update(ns).update(distinctId, "utf8").digest().subarray(0, 16);
  b[6] = (b[6] & 0x0f) | 0x50;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = b.toString("hex");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
