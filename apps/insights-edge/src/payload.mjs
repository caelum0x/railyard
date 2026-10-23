// Umami-compatible payloads ({ type, payload: { website, url, ... } }) to the
// Railyard event shape. Only listed fields pass; lengths are clamped.

import { AGENT_FAILURE_EVENT, agentFailureRecord } from "./agent-failure.mjs";

export const MAX_BATCH = 100;
export const MAX_BODY_BYTES = 64 * 1024;
const TYPES = new Set(["event", "identify", "performance"]);
/** Web vitals of one page view (PY3.3), with the upstream collector's ranges. */
export const VITALS = { lcp: 60000, inp: 60000, cls: 100, fcp: 60000, ttfb: 60000 };

/** The app's own user id (PY3.2): short and plain; the same pattern Insights ingest checks. */
export const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;

const str = (v, max) => (typeof v === "string" ? v.slice(0, max) : null);

function cleanData(data) {
  if (!data || typeof data !== "object" || Array.isArray(data)) return null;
  const out = {};
  let n = 0;
  for (const [k, v] of Object.entries(data)) {
    if (n >= 20) break;
    if (typeof k !== "string" || k.length > 50) continue;
    if (typeof v === "string") out[k] = v.slice(0, 500);
    else if (typeof v === "number" && Number.isFinite(v)) out[k] = v;
    else if (typeof v === "boolean") out[k] = v;
    else continue;
    n += 1;
  }
  return n ? out : null;
}

/** @returns {{ siteKey: string, event: object } | { error: string }} */
export function parseOne(item) {
  if (!item || typeof item !== "object") return { error: "invalid event" };
  const type = item.type ?? "event";
  if (!TYPES.has(type)) return { error: "invalid type" };
  const p = item.payload;
  if (!p || typeof p !== "object") return { error: "missing payload" };
  const siteKey = str(p.website, 80);
  if (!siteKey || !/^[A-Za-z0-9_-]{8,80}$/.test(siteKey)) return { error: "missing site key" };
  const url = str(p.url, 1000);
  if (!url) return { error: "missing url" };
  // An optional client event id (the Expo clients' offline queue, PY2.5): a
  // retried delivery of the same id is stored once.
  const id = typeof p.id === "string" && /^[A-Za-z0-9_-]{8,64}$/.test(p.id) ? p.id : null;
  // Identify (PY3.2): `user` names the signed-in person; an identify without one is refused.
  if (p.user !== undefined && p.user !== null && !(typeof p.user === "string" && USER_ID.test(p.user))) return { error: "invalid user" };
  const user = typeof p.user === "string" ? p.user : null;
  if (type === "identify" && !user) return { error: "identify needs user" };
  const vitals = {};
  if (type === "performance") {
    for (const [k, max] of Object.entries(VITALS)) {
      const v = p[k];
      if (v === undefined || v === null) continue;
      if (typeof v !== "number" || !Number.isFinite(v) || v < 0 || v > max) return { error: `invalid ${k}` };
      vitals[k] = v;
    }
    if (!Object.keys(vitals).length) return { error: "performance needs a metric" };
  }
  const name = str(p.name, 50);
  const data = cleanData(p.data);
  // $agent_failure keeps only the agent, the model and the error name.
  const failure = name === AGENT_FAILURE_EVENT ? agentFailureRecord({ name, data: p.data }) : null;
  if (name === AGENT_FAILURE_EVENT && !failure) return { error: "agent failure needs an agent and an error name" };
  return {
    siteKey,
    event: {
      type,
      ...(id ? { id } : {}),
      ...(user ? { user } : {}),
      ...vitals,
      name,
      url,
      hostname: str(p.hostname, 100),
      referrer: str(p.referrer, 1000),
      title: str(p.title, 500),
      language: str(p.language, 35),
      screen: str(p.screen, 11),
      data: failure ?? data,
    },
  };
}
