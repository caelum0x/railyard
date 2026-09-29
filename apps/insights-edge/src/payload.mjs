// Umami-compatible payloads ({ type, payload: { website, url, ... } }) to the
// Railyard event shape. Only listed fields pass; lengths are clamped.

export const MAX_BATCH = 100;
export const MAX_BODY_BYTES = 64 * 1024;
const TYPES = new Set(["event", "identify"]);

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
  return {
    siteKey,
    event: {
      type,
      name: str(p.name, 50),
      url,
      hostname: str(p.hostname, 100),
      referrer: str(p.referrer, 1000),
      title: str(p.title, 500),
      language: str(p.language, 35),
      screen: str(p.screen, 11),
      data: cleanData(p.data),
    },
  };
}
