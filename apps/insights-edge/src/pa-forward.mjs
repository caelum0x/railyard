// Page-tag events into product analytics (QY1.2, A18): with
// PRODUCT_ANALYTICS_URL set, every stored /s.js event is also sent to the
// product analytics service in its capture protocol, so the web view
// (pageviews, top pages, referrers, UTM, page speed) reads the same store as
// funnels, replay and experiments. The site key is the project's public token.
//
// Cookieless stays cookieless: an anonymous visitor's id is the salted daily
// visitor hash (no person profile is made for it), and the session is that
// visitor's day. No IP address is ever part of an event.

export const PA_MAX_BATCH = 500;
export const PA_LIB = "railyard-page-tag";

const VITALS = { lcp: "LCP", inp: "INP", cls: "CLS", fcp: "FCP", ttfb: "TTFB" };
const UTM = ["utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content"];

function pageUrl(e) {
  const host = typeof e.hostname === "string" && /^[A-Za-z0-9.-]{1,100}(:[0-9]{1,5})?$/.test(e.hostname) ? e.hostname : "app.invalid";
  try {
    return new URL(e.url.startsWith("/") ? e.url : `/${e.url}`, `https://${host}`);
  } catch {
    return null;
  }
}

function referringDomain(ref) {
  if (!ref) return "$direct";
  try {
    return new URL(ref).hostname || "$direct";
  } catch {
    return "$direct";
  }
}

/** One collector event (already parsed, with visitor, country and ts) as a capture-protocol event. */
export function toPaEvent(e, uuid) {
  const anon = `anon_${String(e.visitor ?? "").slice(0, 40)}`;
  const url = pageUrl(e);
  const props = {
    $lib: PA_LIB,
    $session_id: `${anon}_${String(e.ts ?? "").slice(0, 10)}`,
    $process_person_profile: Boolean(e.user),
    ...(url ? { $current_url: url.href, $host: url.host, $pathname: url.pathname } : {}),
    $referrer: e.referrer || "$direct",
    $referring_domain: referringDomain(e.referrer),
    ...(e.title ? { title: e.title } : {}),
    ...(e.language ? { $browser_language: e.language } : {}),
    ...(e.country && /^[A-Z]{2}$/.test(e.country) ? { $geoip_country_code: e.country } : {}),
  };
  if (url) for (const k of UTM) if (url.searchParams.get(k)) props[k] = url.searchParams.get(k).slice(0, 200);
  if (e.screen && /^[0-9]{1,5}x[0-9]{1,5}$/.test(e.screen)) {
    const [w, h] = e.screen.split("x").map(Number);
    props.$screen_width = w;
    props.$screen_height = h;
  }
  let event;
  if (e.type === "performance") {
    event = "$web_vitals";
    for (const [k, name] of Object.entries(VITALS)) if (typeof e[k] === "number") props[`$web_vitals_${name}_value`] = e[k];
  } else if (e.type === "identify") {
    event = "$identify";
    props.$anon_distinct_id = anon;
    if (e.data) props.$set = e.data;
  } else if (e.name) {
    event = e.name;
    if (e.data) Object.assign(props, Object.fromEntries(Object.entries(e.data).filter(([k]) => !k.startsWith("$"))));
  } else {
    event = "$pageview";
  }
  return { uuid, event, distinct_id: e.user ?? anon, timestamp: e.ts, properties: props };
}

/**
 * Sends one site's events. Answers "sent", "drop" (refused for good: unknown
 * token or a bad batch, logged) or "retry" (network or server error).
 */
export async function sendToPa(baseUrl, siteKey, events, now) {
  let res;
  try {
    res = await fetch(`${baseUrl.replace(/\/$/, "")}/batch`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ api_key: siteKey, sent_at: new Date(now).toISOString(), batch: events }),
    });
  } catch {
    return "retry";
  }
  if (res.ok) return "sent";
  return res.status >= 500 || res.status === 429 ? "retry" : "drop";
}
