// Salted daily visitor hash. The IP is used here, in memory, and nowhere else:
// it is never stored, logged or sent downstream. The salt changes every UTC
// day, so the same person cannot be followed from one day to the next.

const enc = new TextEncoder();

const keys = new Map();
async function hmacHex(key, data) {
  let k = keys.get(key);
  if (!k) {
    k = await crypto.subtle.importKey("raw", enc.encode(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    if (keys.size > 8) keys.delete(keys.keys().next().value);
    keys.set(key, k);
  }
  const sig = new Uint8Array(await crypto.subtle.sign("HMAC", k, enc.encode(data)));
  return [...sig].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function utcDay(now) {
  return new Date(now).toISOString().slice(0, 10);
}

const salts = new Map();
export async function dailySalt(secret, now) {
  const id = `${secret}\n${utcDay(now)}`;
  if (!salts.has(id)) {
    if (salts.size > 4) salts.delete(salts.keys().next().value);
    salts.set(id, await hmacHex(secret, `railyard-insights-salt:${utcDay(now)}`));
  }
  return salts.get(id);
}

/** 128-bit hex visitor id for one site on one day. */
export async function visitorHash({ salt, siteKey, ip, userAgent }) {
  return (await hmacHex(salt, `${siteKey}\n${ip}\n${userAgent}`)).slice(0, 32);
}
